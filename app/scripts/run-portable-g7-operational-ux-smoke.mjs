import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import url from "node:url";
import { spawn } from "node:child_process";

import { getPortableOutputDir } from "./ffmpegBundle.mjs";
import {
  buildPortableG5Launch,
  getPortableG5Paths,
} from "./run-portable-g5-smoke.mjs";
import {
  assertTextExcludesPathValues,
  shutdownSmokeProcessAndLogs,
  verifyPortablePreflight,
} from "./run-portable-g6-fast-trim-smoke.mjs";

const __filename = url.fileURLToPath(import.meta.url);

const G7_EVIDENCE_SCHEMA_VERSION = 2;
const SUPPORTED_PLATFORMS = new Set(["linux", "win32"]);
const G7_COPY_DURATION_S = 8;
const G7_TRANSFORM_SOURCE_DURATION_S = 20;
const G7_TRANSFORM_SPEED = 2;
const G7_CANCEL_SPEED = 0.5;
const G7_FRAME_RATE_CAP_FPS = 24;
const G7_GEOMETRY_TOLERANCE_CSS_PX = 1;

const g7SmokeCases = Object.freeze([
  Object.freeze({
    id: "copy-progress",
    operation: "copy-progress",
    fixtureId: "copy-compatible",
    expectedPhases: Object.freeze(["copying", "finalizing"]),
    requireActiveControls: false,
    expectedOutput: Object.freeze({
      width: 640,
      height: 360,
      frameRateFps: 30,
      durationS: G7_COPY_DURATION_S,
      videoAction: "copy",
    }),
  }),
  Object.freeze({
    id: "rotate-speed-cap",
    operation: "rotate-speed-cap",
    fixtureId: "high-motion",
    expectedPhases: Object.freeze(["encoding", "finalizing"]),
    requireActiveControls: true,
    expectedOutput: Object.freeze({
      width: 540,
      height: 960,
      frameRateFps: G7_FRAME_RATE_CAP_FPS,
      durationS: G7_TRANSFORM_SOURCE_DURATION_S / G7_TRANSFORM_SPEED,
      videoAction: "encode",
    }),
  }),
  Object.freeze({
    id: "cancel-drop",
    operation: "cancel-drop",
    fixtureId: "high-motion",
    dropFixtureId: "copy-compatible",
    expectedPhases: Object.freeze(["encoding"]),
    requireActiveControls: true,
    expectedCancellation: true,
  }),
]);

const g7SmokeCaseById = new Map(g7SmokeCases.map((testCase) => [testCase.id, testCase]));

function assertSupportedPlatform(platform) {
  if (!SUPPORTED_PLATFORMS.has(platform)) {
    throw new Error(`Portable G7 smoke supports only Windows and Linux, not ${platform}.`);
  }
}

function resolveG7SmokeCases(caseIds) {
  if (caseIds === undefined || caseIds === null) return [...g7SmokeCases];
  if (!Array.isArray(caseIds) || caseIds.length === 0) {
    throw new Error("Portable G7 smoke caseIds must be a non-empty array when provided.");
  }
  const seen = new Set();
  return caseIds.map((rawCaseId) => {
    const caseId = String(rawCaseId).trim();
    if (seen.has(caseId)) throw new Error(`Portable G7 smoke case is duplicated: ${caseId}`);
    seen.add(caseId);
    const testCase = g7SmokeCaseById.get(caseId);
    if (!testCase) throw new Error(`Unknown portable G7 smoke case: ${caseId}`);
    return testCase;
  });
}

function fixtureOutputPath(fixtureId, fixtureRoot) {
  if (fixtureId === "copy-compatible") return path.resolve(fixtureRoot, "copy-compatible.mp4");
  if (fixtureId === "high-motion") return path.resolve(fixtureRoot, "high-motion.mp4");
  throw new Error(`Unknown portable G7 fixture: ${fixtureId}`);
}

function buildG7FixtureCommand(fixtureId, fixtureRoot) {
  const outputPath = fixtureOutputPath(fixtureId, fixtureRoot);
  const common = ["-y", "-hide_banner", "-loglevel", "error"];
  if (fixtureId === "copy-compatible") {
    return Object.freeze({
      outputPath,
      args: Object.freeze([
        ...common,
        "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30",
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
        "-t", String(G7_COPY_DURATION_S),
        "-map", "0:v:0", "-map", "1:a:0",
        "-c:v", "libx264", "-preset", "ultrafast", "-threads", "1", "-pix_fmt", "yuv420p",
        "-g", "60", "-keyint_min", "60", "-sc_threshold", "0",
        "-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart",
        outputPath,
      ]),
    });
  }
  if (fixtureId === "high-motion") {
    return Object.freeze({
      outputPath,
      args: Object.freeze([
        ...common,
        "-f", "lavfi", "-i", "testsrc2=size=960x540:rate=60",
        "-f", "lavfi", "-i", "sine=frequency=660:sample_rate=48000",
        "-t", String(G7_TRANSFORM_SOURCE_DURATION_S),
        "-map", "0:v:0", "-map", "1:a:0",
        "-c:v", "libx264", "-preset", "ultrafast", "-threads", "1", "-crf", "18", "-pix_fmt", "yuv420p",
        "-g", "120", "-keyint_min", "120", "-sc_threshold", "0",
        "-c:a", "aac", "-b:a", "128k", "-movflags", "+faststart",
        outputPath,
      ]),
    });
  }
  throw new Error(`Unknown portable G7 fixture: ${fixtureId}`);
}

function fixtureBuildOrder(selectedCases) {
  const order = [];
  for (const testCase of selectedCases) {
    for (const fixtureId of [testCase.fixtureId, testCase.dropFixtureId]) {
      if (fixtureId && !order.includes(fixtureId)) order.push(fixtureId);
    }
  }
  return order;
}

function buildG7SmokeEnvironment(testCase, {
  inputPath,
  outputPath,
  statusPath,
  dropPath = null,
  caseRoot,
  platform = process.platform,
  baseEnv = process.env,
} = {}) {
  if (!testCase || !g7SmokeCaseById.has(testCase.id)) {
    throw new Error("Portable G7 smoke requires a canonical test case.");
  }
  assertSupportedPlatform(platform);
  for (const [label, value] of [["inputPath", inputPath], ["outputPath", outputPath], ["statusPath", statusPath], ["caseRoot", caseRoot]]) {
    if (!value || !String(value).trim()) throw new Error(`Portable G7 smoke ${label} is required.`);
  }
  if (testCase.expectedCancellation && !dropPath) {
    throw new Error(`Portable G7 smoke ${testCase.id} requires a dropPath.`);
  }

  const cleanBaseEnv = Object.fromEntries(
    Object.entries(baseEnv).filter(([name]) => (
      !name.startsWith("VFL_SMOKE_") && name !== "VFL_FFMPEG_PATH" && name !== "VFL_FFPROBE_PATH"
    )),
  );
  const env = {
    ...cleanBaseEnv,
    VFL_SMOKE_INPUT: path.resolve(inputPath),
    VFL_SMOKE_OUTPUT: path.resolve(outputPath),
    VFL_SMOKE_STATUS: path.resolve(statusPath),
    VFL_SMOKE_FORMAT: "mp4",
    VFL_SMOKE_SIZE_LIMIT_MB: "0",
    VFL_SMOKE_TRIM_START_S: "0",
    VFL_SMOKE_SKIP_PREVIEW_INTERACTIONS: "1",
    VFL_SMOKE_WORKFLOW_QUEUE: "0",
    VFL_SMOKE_G5_QUEUE_TARGET_MISS: "0",
    VFL_SMOKE_G7_OPERATION: testCase.operation,
    ...(dropPath ? { VFL_SMOKE_G7_DROP_PATH: path.resolve(dropPath) } : {}),
  };
  if (platform === "linux") {
    env.XDG_DATA_HOME = path.resolve(caseRoot, "xdg-data");
    env.XDG_CONFIG_HOME = path.resolve(caseRoot, "xdg-config");
    env.XDG_CACHE_HOME = path.resolve(caseRoot, "xdg-cache");
  } else {
    env.WEBVIEW2_USER_DATA_FOLDER = path.resolve(caseRoot, "webview2-user-data");
  }
  return env;
}

async function exists(filePath) {
  try {
    await fs.access(filePath);
    return true;
  } catch {
    return false;
  }
}

async function runBounded(command, args, {
  cwd,
  env,
  timeoutMs = 120_000,
  maxCaptureBytes = 4 * 1024 * 1024,
} = {}) {
  return await new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      env,
      windowsHide: true,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    let settled = false;
    const append = (current, chunk) => {
      const next = current + chunk.toString();
      if (Buffer.byteLength(next) > maxCaptureBytes) {
        child.kill();
        finish(() => reject(new Error(`${command} exceeded its ${maxCaptureBytes}-byte output limit.`)));
        return current;
      }
      return next;
    };
    const finish = (action) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      action();
    };
    const timer = setTimeout(() => {
      child.kill();
      finish(() => reject(new Error(`${command} timed out after ${timeoutMs} ms.`)));
    }, timeoutMs);
    child.stdout.on("data", (chunk) => { stdout = append(stdout, chunk); });
    child.stderr.on("data", (chunk) => { stderr = append(stderr, chunk); });
    child.on("error", (error) => finish(() => reject(error)));
    child.on("close", (code, signal) => finish(() => {
      if (code === 0) {
        resolve({ stdout, stderr });
        return;
      }
      reject(new Error(`${command} exited with code ${String(code)} (signal ${signal ?? "none"}).\n${stderr}`));
    }));
  });
}

async function readJsonFileOrNull(filePath) {
  try {
    return JSON.parse(await fs.readFile(filePath, "utf8"));
  } catch (error) {
    if (error?.code === "ENOENT" || error instanceof SyntaxError) return null;
    throw error;
  }
}

async function waitForTerminalStatus(child, statusPath, testCase, timeoutSeconds) {
  const deadline = Date.now() + timeoutSeconds * 1000;
  let lastStatus = null;
  let lastStage = null;
  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 200));
    const status = await readJsonFileOrNull(statusPath);
    if (status) {
      lastStatus = status;
      if (status.stage !== lastStage) {
        console.log(`Portable G7 smoke ${testCase.id} stage: ${status.stage}`);
        lastStage = status.stage;
      }
      if (status.stage === "success" || status.stage === "error") {
        return status;
      }
    }
    if ((child.exitCode !== null || child.signalCode !== null) && !lastStatus) {
      throw new Error(`Portable G7 smoke ${testCase.id} app exited before writing status.`);
    }
    if ((child.exitCode !== null || child.signalCode !== null) && lastStatus?.stage !== "success") {
      throw new Error(`Portable G7 smoke ${testCase.id} app exited at stage ${lastStatus?.stage ?? "none"}.`);
    }
  }
  throw new Error(`Portable G7 smoke ${testCase.id} timed out. Last stage: ${lastStatus?.stage ?? "none"}`);
}

function assertOrderedStageSubsequence(history, required, label) {
  let cursor = 0;
  for (const stage of history) {
    if (stage === required[cursor]) cursor += 1;
    if (cursor === required.length) return;
  }
  throw new Error(`${label} stage history omitted ordered stage ${required[cursor] ?? "unknown"}: ${history.join(" -> ")}`);
}

function requiredG7Stages(testCase) {
  return Object.freeze([
    "detected",
    "input-applied",
    "probe-ready",
    "workflow-ready",
    "g7-ui-ready",
    "interaction-ready",
    "encoding",
    ...(testCase.requireActiveControls ? ["g7-controls-ready"] : []),
    ...(testCase.expectedCancellation ? ["g7-drop-queued", "g7-cancel-requested"] : []),
    "success",
  ]);
}

function assertProgressHistory(testCase, history) {
  if (!Array.isArray(history)) {
    throw new Error(`Portable G7 smoke ${testCase.id} progress history is not an array.`);
  }
  if (history.length === 0) {
    throw new Error(`Portable G7 smoke ${testCase.id} retained no encode progress samples.`);
  }
  const identities = new Set();
  let previousOverall = -1;
  for (const [index, sample] of history.entries()) {
    if (!Number.isSafeInteger(sample.attemptId) || sample.attemptId <= 0) {
      throw new Error(`Portable G7 smoke ${testCase.id} progress sample ${index} has an invalid attempt ID.`);
    }
    if (!Number.isSafeInteger(sample.jobId) || sample.jobId <= 0) {
      throw new Error(`Portable G7 smoke ${testCase.id} progress sample ${index} has an invalid job ID.`);
    }
    identities.add(`${sample.attemptId}:${sample.jobId}`);
    if (!Number.isInteger(sample.stepIndex) || !Number.isInteger(sample.stepCount) || sample.stepIndex < 1 || sample.stepIndex > sample.stepCount) {
      throw new Error(`Portable G7 smoke ${testCase.id} progress sample ${index} has invalid step coordinates.`);
    }
    if (!Number.isFinite(sample.passPct) || sample.passPct < 0 || sample.passPct > 1) {
      throw new Error(`Portable G7 smoke ${testCase.id} progress sample ${index} has invalid passPct.`);
    }
    if (!Number.isFinite(sample.overallPct) || sample.overallPct < 0 || sample.overallPct > 1) {
      throw new Error(`Portable G7 smoke ${testCase.id} progress sample ${index} has invalid overallPct.`);
    }
    if (sample.overallPct + 1e-9 < previousOverall) {
      throw new Error(`Portable G7 smoke ${testCase.id} progress regressed at sample ${index}.`);
    }
    previousOverall = sample.overallPct;
  }
  if (identities.size !== 1) {
    throw new Error(`Portable G7 smoke ${testCase.id} mixed encode identities or retained none.`);
  }
  const phases = new Set(history.map((sample) => sample.phase));
  let expectedPhaseIndex = 0;
  for (const sample of history) {
    if (sample.phase === testCase.expectedPhases[expectedPhaseIndex]) expectedPhaseIndex += 1;
    if (expectedPhaseIndex === testCase.expectedPhases.length) break;
  }
  if (expectedPhaseIndex !== testCase.expectedPhases.length) {
    throw new Error(
      `Portable G7 smoke ${testCase.id} never observed ordered progress phase ${testCase.expectedPhases[expectedPhaseIndex]}.`,
    );
  }
  const finalizingIndex = history.findIndex((sample) => sample.phase === "finalizing");
  const activePhase = testCase.operation === "copy-progress" ? "copying" : "encoding";
  if (
    finalizingIndex >= 0 &&
    history.slice(finalizingIndex + 1).some((sample) => sample.phase === activePhase)
  ) {
    throw new Error(`Portable G7 smoke ${testCase.id} returned to ${activePhase} after finalizing.`);
  }
  if (!testCase.expectedCancellation) {
    const finalSample = history.at(-1);
    if (finalSample?.phase !== "finalizing" || Math.abs(finalSample.overallPct - 1) > 1e-6) {
      throw new Error(
        `Portable G7 smoke ${testCase.id} final retained progress sample was not completed finalization.`,
      );
    }
  }
  return Object.freeze({ sampleCount: history.length, phases: Object.freeze([...phases]), identityCount: identities.size });
}

function assertMountedProgressHistory(testCase, history, backendHistory) {
  if (!Array.isArray(history)) {
    throw new Error(`Portable G7 smoke ${testCase.id} mounted progress history is not an array.`);
  }
  if (history.length === 0) {
    throw new Error(`Portable G7 smoke ${testCase.id} retained no mounted progressbar samples.`);
  }
  if (!Array.isArray(backendHistory) || backendHistory.length === 0) {
    throw new Error(`Portable G7 smoke ${testCase.id} cannot link mounted progress to backend events.`);
  }

  const phases = new Set();
  const identities = new Set();
  let previousValue = -1;
  for (const [index, sample] of history.entries()) {
    if (!Number.isSafeInteger(sample?.attemptId) || sample.attemptId <= 0) {
      throw new Error(`Portable G7 smoke ${testCase.id} mounted progress sample ${index} has an invalid attempt ID.`);
    }
    if (!Number.isSafeInteger(sample.jobId) || sample.jobId <= 0) {
      throw new Error(`Portable G7 smoke ${testCase.id} mounted progress sample ${index} has an invalid job ID.`);
    }
    if (!Number.isFinite(sample.sourceOverallPct) || sample.sourceOverallPct < 0 || sample.sourceOverallPct > 1) {
      throw new Error(`Portable G7 smoke ${testCase.id} mounted progress sample ${index} has invalid source progress.`);
    }
    const linkedBackendEvent = backendHistory.some((backendSample) => (
      backendSample.attemptId === sample.attemptId &&
      backendSample.jobId === sample.jobId &&
      backendSample.phase === sample.phase &&
      Math.abs(backendSample.overallPct - sample.sourceOverallPct) <= 1e-9
    ));
    if (!linkedBackendEvent) {
      throw new Error(`Portable G7 smoke ${testCase.id} mounted progress sample ${index} is not linked to a real backend event.`);
    }
    identities.add(`${sample.attemptId}:${sample.jobId}`);

    const phase = sample.phase;
    const expectedPhaseLabel = phase === "copying"
      ? "Copying media"
      : phase === "finalizing"
        ? "Finalizing output"
        : phase === "encoding" && /^Encoding(?: pass \d+ of \d+)?$/.test(String(sample.phaseLabel ?? ""))
          ? sample.phaseLabel
          : null;
    if (!expectedPhaseLabel || sample.phaseLabel !== expectedPhaseLabel) {
      throw new Error(`Portable G7 smoke ${testCase.id} mounted progress sample ${index} has incorrect phase copy.`);
    }
    if (sample.role !== "progressbar" || sample.ariaLabel !== "Encoding progress") {
      throw new Error(`Portable G7 smoke ${testCase.id} mounted progress sample ${index} has an incorrect role or accessible name.`);
    }
    if (sample.valueMin !== 0 || sample.valueMax !== 100) {
      throw new Error(`Portable G7 smoke ${testCase.id} mounted progress sample ${index} has incorrect value bounds.`);
    }
    if (!Number.isSafeInteger(sample.valueNow) || sample.valueNow < 0 || sample.valueNow > 99) {
      throw new Error(`Portable G7 smoke ${testCase.id} mounted progress sample ${index} has an invalid active value.`);
    }
    const expectedVisibleValue = Math.round(Math.min(sample.sourceOverallPct, 0.99) * 100);
    if (sample.valueNow !== expectedVisibleValue) {
      throw new Error(`Portable G7 smoke ${testCase.id} mounted progress sample ${index} does not match its backend event value.`);
    }
    if (
      typeof sample.valueText !== "string" ||
      !sample.valueText.startsWith(sample.phaseLabel) ||
      !sample.valueText.endsWith(`, ${sample.valueNow} percent`)
    ) {
      throw new Error(`Portable G7 smoke ${testCase.id} mounted progress sample ${index} has incorrect value text.`);
    }
    if (sample.visiblePercent !== sample.valueNow || sample.fillWidth !== `${sample.valueNow}%`) {
      throw new Error(`Portable G7 smoke ${testCase.id} mounted progress sample ${index} has inconsistent visible progress.`);
    }
    if (sample.isFinalizing !== (phase === "finalizing")) {
      throw new Error(`Portable G7 smoke ${testCase.id} mounted progress sample ${index} has inconsistent finalizing state.`);
    }
    if (sample.valueNow < previousValue) {
      throw new Error(`Portable G7 smoke ${testCase.id} mounted progress visibly regressed at sample ${index}.`);
    }
    previousValue = sample.valueNow;
    phases.add(phase);
  }
  if (identities.size !== 1) {
    throw new Error(`Portable G7 smoke ${testCase.id} mounted progress mixed encode identities.`);
  }

  const requiredActivePhase = testCase.operation === "copy-progress" ? "copying" : "encoding";
  if (!phases.has(requiredActivePhase)) {
    throw new Error(`Portable G7 smoke ${testCase.id} never mounted its ${requiredActivePhase} phase copy.`);
  }
  if (!testCase.expectedCancellation && !phases.has("finalizing")) {
    throw new Error(`Portable G7 smoke ${testCase.id} never mounted its finalizing phase copy.`);
  }
  return Object.freeze({
    sampleCount: history.length,
    phases: Object.freeze([...phases]),
    identityCount: identities.size,
  });
}

function assertG7Evidence(testCase, evidence) {
  if (!evidence || evidence.operation !== testCase.operation) {
    throw new Error(`Portable G7 smoke ${testCase.id} retained the wrong operation evidence.`);
  }
  for (const field of [
    "resetCancelFocused",
    "resetCancelPreservedSettings",
    "resetCancelRestoredFocus",
    "resetConfirmed",
    "resetConfirmRestoredFocus",
    "frameRateMountedCopyVerified",
    "exportControlStable",
    "exportControlInitiallyFocused",
  ]) {
    if (evidence[field] !== true) {
      throw new Error(`Portable G7 smoke ${testCase.id} did not prove ${field}.`);
    }
  }
  if (evidence.resetDialogRole !== "alertdialog") {
    throw new Error(`Portable G7 smoke ${testCase.id} reset dialog role was not alertdialog.`);
  }
  if (testCase.operation !== "copy-progress") {
    if (evidence.previewRotationDeg !== 90 || !String(evidence.previewTransform ?? "").includes("rotate(90deg)")) {
      throw new Error(`Portable G7 smoke ${testCase.id} did not prove the mounted 90-degree preview.`);
    }
    if (evidence.frameRateCapFps !== G7_FRAME_RATE_CAP_FPS || evidence.frameRateCapApplies !== true) {
      throw new Error(`Portable G7 smoke ${testCase.id} did not prove the post-speed 24 fps cap.`);
    }
  }
  if (testCase.requireActiveControls) {
    for (const field of ["exportControlPreservedIdentity", "exportControlPreservedGeometry", "cancelControlSeparate"]) {
      if (evidence[field] !== true) {
        throw new Error(`Portable G7 smoke ${testCase.id} did not prove ${field}.`);
      }
    }
  }
  if (testCase.expectedCancellation) {
    if (
      evidence.dropActionKind !== "queueInputs" ||
      evidence.dropPreservedInput !== true ||
      evidence.queuedDropCount !== 1 ||
      evidence.cancelInvokeCount !== 1
    ) {
      throw new Error(`Portable G7 smoke ${testCase.id} did not prove queued drop and one-shot cancellation.`);
    }
  }
  const progress = assertProgressHistory(testCase, evidence.progressHistory);
  return Object.freeze({
    ...progress,
    mounted: assertMountedProgressHistory(
      testCase,
      evidence.mountedProgressHistory,
      evidence.progressHistory,
    ),
  });
}

function normalizeRational(raw) {
  if (typeof raw !== "string") return null;
  const [numeratorRaw, denominatorRaw] = raw.split("/");
  const numerator = Number(numeratorRaw);
  const denominator = Number(denominatorRaw ?? 1);
  if (!Number.isFinite(numerator) || !Number.isFinite(denominator) || denominator === 0) return null;
  return numerator / denominator;
}

async function probeMedia(ffprobePath, mediaPath) {
  const { stdout } = await runBounded(ffprobePath, [
    "-hide_banner", "-loglevel", "error",
    "-show_entries", "stream=codec_type,codec_name,width,height,avg_frame_rate,r_frame_rate:format=duration",
    "-of", "json",
    mediaPath,
  ], { timeoutMs: 30_000 });
  return JSON.parse(stdout);
}

function validateOutputProbe(testCase, probe) {
  const expected = testCase.expectedOutput;
  const video = probe?.streams?.find((stream) => stream.codec_type === "video");
  if (!video) throw new Error(`Portable G7 smoke ${testCase.id} output has no video stream.`);
  if (video.width !== expected.width || video.height !== expected.height) {
    throw new Error(`Portable G7 smoke ${testCase.id} output was ${video.width}x${video.height}, expected ${expected.width}x${expected.height}.`);
  }
  const frameRateFps = normalizeRational(video.avg_frame_rate) ?? normalizeRational(video.r_frame_rate);
  if (frameRateFps === null || Math.abs(frameRateFps - expected.frameRateFps) > 0.15) {
    throw new Error(`Portable G7 smoke ${testCase.id} output rate was ${String(frameRateFps)}, expected ${expected.frameRateFps}.`);
  }
  const durationS = Number(probe?.format?.duration);
  if (!Number.isFinite(durationS) || Math.abs(durationS - expected.durationS) > 0.6) {
    throw new Error(`Portable G7 smoke ${testCase.id} output duration was ${String(durationS)}, expected about ${expected.durationS}.`);
  }
  return Object.freeze({
    width: video.width,
    height: video.height,
    frameRateFps,
    durationS,
    codecName: video.codec_name,
  });
}

function assertTextExcludesPaths(raw, protectedPaths, label, { platform = process.platform } = {}) {
  const pathValues = protectedPaths.flatMap((protectedPath) => {
    if (!protectedPath) return [];
    return [String(protectedPath), path.basename(String(protectedPath))].filter(Boolean);
  });
  assertTextExcludesPathValues(raw, pathValues, label, { platform });
}

function assertG7PrivacySurfaces(
  testCase,
  { rawStatus, status, stdoutRaw, stderrRaw },
  protectedPaths,
  { platform = process.platform } = {},
) {
  for (const [label, raw] of [
    ["raw status.json", rawStatus],
    ["terminal message", status?.message],
    ["diagnostics", JSON.stringify(status?.diagnostics ?? null)],
    ["trim result", JSON.stringify(status?.trimResult ?? null)],
    ["G7 evidence", JSON.stringify(status?.g7Evidence ?? null)],
    ["stdout", stdoutRaw],
    ["stderr", stderrRaw],
  ]) {
    assertTextExcludesPaths(
      raw,
      protectedPaths,
      `Portable G7 smoke ${testCase.id} ${label}`,
      { platform },
    );
  }
}

async function verifySuccessfulCase({ testCase, status, outputPath, ffprobePath, platform }) {
  if (status.ok !== true) throw new Error(`Portable G7 smoke ${testCase.id} success status was not ok.`);
  const progress = assertG7Evidence(testCase, status.g7Evidence);
  assertOrderedStageSubsequence(status.stageHistory ?? [], requiredG7Stages(testCase), `Portable G7 smoke ${testCase.id}`);

  if (testCase.expectedCancellation) {
    if (await exists(outputPath)) {
      throw new Error(`Portable G7 smoke ${testCase.id} published an output after expected cancellation.`);
    }
    return Object.freeze({
      schemaVersion: G7_EVIDENCE_SCHEMA_VERSION,
      caseId: testCase.id,
      operation: testCase.operation,
      cancelled: true,
      progress,
      ui: Object.freeze({
        resetConfirmed: true,
        exportIdentityStable: true,
        exportGeometryStableWithinCssPx: G7_GEOMETRY_TOLERANCE_CSS_PX,
        queuedDropCount: 1,
        cancelInvokeCount: 1,
      }),
    });
  }

  if (!(await exists(outputPath))) {
    throw new Error(`Portable G7 smoke ${testCase.id} did not create its output.`);
  }
  if (status.outputPath !== null && status.outputPath !== undefined) {
    throw new Error(`Portable G7 smoke ${testCase.id} retained its private output path in status evidence.`);
  }
  if (status.diagnostics?.videoAction !== testCase.expectedOutput.videoAction) {
    throw new Error(`Portable G7 smoke ${testCase.id} used video action ${status.diagnostics?.videoAction ?? "none"}, expected ${testCase.expectedOutput.videoAction}.`);
  }
  const output = validateOutputProbe(testCase, await probeMedia(ffprobePath, outputPath));
  return Object.freeze({
    schemaVersion: G7_EVIDENCE_SCHEMA_VERSION,
    caseId: testCase.id,
    operation: testCase.operation,
    cancelled: false,
    progress,
    output,
    ui: Object.freeze({
      resetConfirmed: true,
      previewRotationDeg: status.g7Evidence.previewRotationDeg,
      postSpeedFrameRateFps: status.g7Evidence.postSpeedFrameRateFps,
      frameRateCapFps: status.g7Evidence.frameRateCapFps,
      mountedFrameRateCopyVerified: status.g7Evidence.frameRateMountedCopyVerified,
      exportIdentityStable: status.g7Evidence.exportControlPreservedIdentity,
      exportGeometryStableWithinCssPx: status.g7Evidence.exportControlPreservedGeometry
        ? G7_GEOMETRY_TOLERANCE_CSS_PX
        : null,
    }),
  });
}

async function runPortableG7OperationalUxSmoke({
  portableDir = process.env.VFL_PORTABLE_DIR || getPortableOutputDir(),
  platform = process.platform,
  caseIds,
  timeoutSeconds = 300,
  fixtureTimeoutSeconds = 180,
  keepSuccessArtifacts = process.env.VFL_G7_SMOKE_KEEP_ARTIFACTS === "1",
  verifyPreflight = true,
} = {}) {
  assertSupportedPlatform(platform);
  if (platform !== process.platform) {
    throw new Error(`Portable G7 smoke cannot execute ${platform} artifacts on ${process.platform}.`);
  }
  if (!Number.isFinite(timeoutSeconds) || timeoutSeconds <= 0 || timeoutSeconds > 600) {
    throw new Error("Portable G7 smoke timeoutSeconds must be between 1 and 600.");
  }
  if (!Number.isFinite(fixtureTimeoutSeconds) || fixtureTimeoutSeconds <= 0 || fixtureTimeoutSeconds > 300) {
    throw new Error("Portable G7 smoke fixtureTimeoutSeconds must be between 1 and 300.");
  }

  const selectedCases = resolveG7SmokeCases(caseIds);
  const { portableRoot, appPath, ffmpegPath, ffprobePath } = getPortableG5Paths(portableDir, { platform });
  for (const requiredPath of [appPath, ffmpegPath, ffprobePath]) {
    if (!(await exists(requiredPath))) {
      throw new Error(`Portable G7 smoke could not find required extracted payload file: ${requiredPath}`);
    }
  }
  const preflight = verifyPreflight
    ? await verifyPortablePreflight(portableRoot, { platform, ffmpegPath, appPath, ffprobePath })
    : null;

  const smokeRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-portable-g7-"));
  const fixtureRoot = path.resolve(smokeRoot, "fixtures");
  const resultRoot = path.resolve(smokeRoot, "results");
  await Promise.all([fs.mkdir(fixtureRoot, { recursive: true }), fs.mkdir(resultRoot, { recursive: true })]);
  let failed = false;
  const evidence = [];

  try {
    for (const fixtureId of fixtureBuildOrder(selectedCases)) {
      const command = buildG7FixtureCommand(fixtureId, fixtureRoot);
      await runBounded(ffmpegPath, command.args, {
        cwd: fixtureRoot,
        timeoutMs: fixtureTimeoutSeconds * 1000,
      });
      if (!(await exists(command.outputPath))) {
        throw new Error(`Portable G7 fixture command did not create ${command.outputPath}.`);
      }
      console.log(`Portable G7 fixture ready: ${fixtureId}`);
    }

    for (const [caseIndex, testCase] of selectedCases.entries()) {
      const caseRoot = path.resolve(resultRoot, `case-${String(caseIndex + 1).padStart(2, "0")}`);
      await fs.mkdir(caseRoot, { recursive: true });
      const inputPath = fixtureOutputPath(testCase.fixtureId, fixtureRoot);
      const dropPath = testCase.dropFixtureId ? fixtureOutputPath(testCase.dropFixtureId, fixtureRoot) : null;
      const outputPath = path.resolve(caseRoot, "case-output.mp4");
      const statusPath = path.resolve(caseRoot, "status.json");
      const stdoutPath = path.resolve(caseRoot, "app.stdout.log");
      const stderrPath = path.resolve(caseRoot, "app.stderr.log");
      const smokeEnv = buildG7SmokeEnvironment(testCase, {
        inputPath,
        outputPath,
        statusPath,
        dropPath,
        caseRoot,
        platform,
      });
      await Promise.all([
        ...(platform === "linux" ? [
          fs.mkdir(smokeEnv.XDG_DATA_HOME, { recursive: true }),
          fs.mkdir(smokeEnv.XDG_CONFIG_HOME, { recursive: true }),
          fs.mkdir(smokeEnv.XDG_CACHE_HOME, { recursive: true }),
        ] : []),
        ...(platform === "win32" ? [fs.mkdir(smokeEnv.WEBVIEW2_USER_DATA_FOLDER, { recursive: true })] : []),
      ]);

      const [stdoutFile, stderrFile] = await Promise.all([fs.open(stdoutPath, "w"), fs.open(stderrPath, "w")]);
      let child = null;
      let logsClosed = false;
      try {
        const launch = buildPortableG5Launch(appPath, { platform, env: smokeEnv });
        child = spawn(launch.command, launch.args, {
          cwd: portableRoot,
          detached: platform === "linux",
          env: smokeEnv,
          stdio: ["ignore", stdoutFile.fd, stderrFile.fd],
          windowsHide: true,
        });
        await waitForTerminalStatus(child, statusPath, testCase, timeoutSeconds);
        const privatePaths = [
          smokeRoot,
          fixtureRoot,
          resultRoot,
          caseRoot,
          inputPath,
          dropPath,
          outputPath,
          statusPath,
          stdoutPath,
          stderrPath,
          ".vfl-",
        ].filter(Boolean);
        try {
          await shutdownSmokeProcessAndLogs(child, platform, [stdoutFile, stderrFile]);
          child = null;
        } finally {
          logsClosed = true;
        }
        const [rawStatus, stdoutRaw, stderrRaw] = await Promise.all([
          fs.readFile(statusPath, "utf8"),
          fs.readFile(stdoutPath, "utf8"),
          fs.readFile(stderrPath, "utf8"),
        ]);
        const status = JSON.parse(rawStatus);
        assertG7PrivacySurfaces(
          testCase,
          { rawStatus, status, stdoutRaw, stderrRaw },
          privatePaths,
          { platform },
        );
        if (status.stage !== "success") {
          throw new Error(`Portable G7 smoke ${testCase.id} failed: ${status.message ?? "no message"}`);
        }
        const caseEvidence = await verifySuccessfulCase({
          testCase,
          status,
          outputPath,
          ffprobePath,
          platform,
        });
        const evidenceRaw = `${JSON.stringify({ ...caseEvidence, platform }, null, 2)}\n`;
        assertTextExcludesPaths(evidenceRaw, privatePaths, `Portable G7 smoke ${testCase.id} retained evidence`, { platform });
        await fs.writeFile(path.resolve(caseRoot, "evidence.json"), evidenceRaw, "utf8");
        const tempResidue = (await fs.readdir(caseRoot)).filter((name) => name.startsWith(".vfl-"));
        if (tempResidue.length > 0) {
          throw new Error(`Portable G7 smoke ${testCase.id} left unpublished temporary output(s): ${tempResidue.join(", ")}`);
        }
        evidence.push(caseEvidence);
        console.log(`Portable G7 smoke passed: ${testCase.id}`);
      } finally {
        if (!logsClosed) {
          await shutdownSmokeProcessAndLogs(child, platform, [stdoutFile, stderrFile], { strict: false });
        }
      }
    }

    console.log(`Portable G7 smoke passed ${selectedCases.length} cases on ${platform}.`);
    if (keepSuccessArtifacts) console.log(`Portable G7 smoke evidence retained at ${smokeRoot}`);
    return Object.freeze({
      smokeRoot: keepSuccessArtifacts ? smokeRoot : null,
      preflight,
      evidence: Object.freeze(evidence),
    });
  } catch (error) {
    failed = true;
    console.error(`Portable G7 smoke preserved failure evidence at ${smokeRoot}`);
    throw error;
  } finally {
    if (!failed && !keepSuccessArtifacts) {
      await fs.rm(smokeRoot, { recursive: true, force: true, maxRetries: 8, retryDelay: 250 });
    }
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === __filename) {
  const caseIds = String(process.env.VFL_G7_SMOKE_CASES ?? "")
    .split(",")
    .map((caseId) => caseId.trim())
    .filter(Boolean);
  await runPortableG7OperationalUxSmoke({ caseIds: caseIds.length > 0 ? caseIds : undefined });
}

export {
  G7_CANCEL_SPEED,
  G7_COPY_DURATION_S,
  G7_EVIDENCE_SCHEMA_VERSION,
  G7_FRAME_RATE_CAP_FPS,
  G7_GEOMETRY_TOLERANCE_CSS_PX,
  G7_TRANSFORM_SOURCE_DURATION_S,
  G7_TRANSFORM_SPEED,
  assertG7Evidence,
  assertG7PrivacySurfaces,
  assertMountedProgressHistory,
  assertOrderedStageSubsequence,
  assertProgressHistory,
  assertTextExcludesPaths,
  buildG7FixtureCommand,
  buildG7SmokeEnvironment,
  fixtureBuildOrder,
  fixtureOutputPath,
  g7SmokeCases,
  normalizeRational,
  requiredG7Stages,
  resolveG7SmokeCases,
  runPortableG7OperationalUxSmoke,
  validateOutputProbe,
};
