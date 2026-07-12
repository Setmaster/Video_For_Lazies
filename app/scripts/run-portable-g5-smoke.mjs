import crypto from "node:crypto";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import url from "node:url";
import { spawn } from "node:child_process";

import { ffmpegSidecarResourceTarget, getPortableOutputDir } from "./ffmpegBundle.mjs";
import { getPortableExecutableName } from "./portableRelease.mjs";

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const G5_EVIDENCE_SCHEMA_VERSION = 1;
const G5_MAX_FIT_PLANS = 4;
const G5_NO_CLOBBER_SENTINEL = "VFL G5 EXACT TRIM NO-CLOBBER SENTINEL\n";
const SUPPORTED_PLATFORMS = new Set(["linux", "win32"]);
const MASKABLE_CAPABILITY_FILTERS = new Set(["subtitles"]);
const REQUIRED_G5_SMOKE_STAGES = Object.freeze([
  "detected",
  "input-applied",
  "probe-ready",
  "workflow-ready",
  "interaction-ready",
  "encoding",
]);
const VALID_SUBTITLE_FILE_NAME = "字幕 O'Brien,[x]; café.srt";
const MALFORMED_SUBTITLE_FILE_NAME = "malformed 字幕 O'Brien,[x]; café.srt";
const VALID_SUBTITLE_TEXT = [
  "\uFEFF1",
  "00:00:04,000 --> 00:00:05,000",
  "café Olá Καλημέρα Привет مرحبا",
  "",
].join("\n");
const STAGED_EXTERNAL_SRT_FILE_NAME = "vfl_external.srt";
const STAGED_EXTERNAL_SUBTITLE_FONT_DIR_NAME = "fonts";
const STAGED_EXTERNAL_SUBTITLE_FONT_FILE_NAME = "DejaVuSans.ttf";
const SUBTITLE_FONT_SOURCE_PATH = path.resolve(__dirname, "../src-tauri/assets/DejaVuSans.ttf");
const SUBTITLE_FONT_SHA256 = "7da195a74c55bef988d0d48f9508bd5d849425c1770dba5d7bfc6ce9ed848954";
const EXTERNAL_SRT_FORCE_STYLE = "FontName=DejaVu Sans,FontSize=24,PrimaryColour=&H00FFFFFF,OutlineColour=&H00000000,BorderStyle=1,Outline=2,Shadow=0,Alignment=2,MarginL=24,MarginR=24,MarginV=24";
const EXTERNAL_SRT_FILTER = `subtitles=filename=${STAGED_EXTERNAL_SRT_FILE_NAME}:fontsdir=${STAGED_EXTERNAL_SUBTITLE_FONT_DIR_NAME}:force_style='${EXTERNAL_SRT_FORCE_STYLE}'`;
const FONT_GLYPH_WARNING_PATTERNS = Object.freeze([
  /\bglyph\b.*\b(?:not found|missing|unavailable)\b/i,
  /\bmissing\b.*\bglyph\b/i,
  /\bfontselect\b.*\b(?:failed|unable|not found)\b/i,
  /\b(?:could not|cannot|failed to|unable to)\b.*\b(?:load|find|open)\b.*\bfont\b/i,
  /\berror\b.*\bopen(?:ing)?\b.*\bfont\b/i,
  /\bno usable fonts?\b/i,
  /\bfontconfig\b.*\b(?:error|failed|failure)\b/i,
]);
const MALFORMED_SUBTITLE_TEXT = [
  "1",
  "not-a-time --> 00:00:05,000",
  "private malformed subtitle sentinel",
  "",
].join("\n");

const g5SmokeCases = Object.freeze([
  Object.freeze({
    id: "target-first-plan-met",
    fixtureId: "target-easy",
    terminalStage: "success",
    sizeLimitMb: 0.5,
    strictFit: false,
    strictFitAllowAudioRemoval: false,
    expectedTargetStatus: "met",
    expectedMinFitPlans: 1,
    expectedAudioPresent: true,
  }),
  Object.freeze({
    id: "target-best-effort-missed",
    fixtureId: "target-hard",
    terminalStage: "success",
    sizeLimitMb: 0.1,
    strictFit: false,
    strictFitAllowAudioRemoval: false,
    expectedTargetStatus: "missed",
    expectedMinFitPlans: 1,
    expectedAudioPresent: true,
  }),
  Object.freeze({
    id: "target-strict-bounded-correction",
    fixtureId: "target-hard",
    terminalStage: "success",
    sizeLimitMb: 0.1,
    strictFit: true,
    strictFitAllowAudioRemoval: false,
    expectedTargetStatus: "missed",
    expectedMinFitPlans: 2,
    expectedAudioPresent: true,
  }),
  Object.freeze({
    id: "target-strict-audio-removal-permitted",
    fixtureId: "target-hard",
    terminalStage: "success",
    sizeLimitMb: 0.1,
    strictFit: true,
    strictFitAllowAudioRemoval: true,
    expectedTargetStatus: "missed",
    expectedMinFitPlans: 3,
    expectedAudioPresent: false,
    expectedAudioRemoved: true,
  }),
  Object.freeze({
    id: "target-missed-queue-recovery",
    fixtureId: "target-hard",
    terminalStage: "success",
    sizeLimitMb: 0.1,
    strictFit: false,
    strictFitAllowAudioRemoval: false,
    workflowQueueExport: true,
    g5QueueTargetMiss: true,
    expectedTargetStatus: "missed",
    expectedMinFitPlans: 1,
    expectedAudioPresent: true,
    requiredStages: Object.freeze([
      "detected",
      "input-applied",
      "probe-ready",
      "workflow-recipe-ready",
      "workflow-recipe-saved",
      "workflow-queue-ready",
      "workflow-queue-complete",
      "workflow-ready",
      "interaction-ready",
      "encoding",
      "success",
    ]),
  }),
  Object.freeze({
    id: "mp3-impossible-target-missed",
    fixtureId: "audio-long",
    outputFormat: "mp3",
    terminalStage: "success",
    sizeLimitMb: 0.1,
    strictFit: false,
    strictFitAllowAudioRemoval: false,
    expectedTargetStatus: "missed",
    expectedMinFitPlans: 1,
    expectedAudioPresent: true,
  }),
  Object.freeze({
    id: "external-srt-unicode-source-timing",
    fixtureId: "subtitle-solid",
    terminalStage: "success",
    sizeLimitMb: 0,
    strictFit: false,
    strictFitAllowAudioRemoval: false,
    trimStartS: 3,
    trimEndS: 6,
    subtitleFixture: "valid",
    expectedSubtitleBurnedIn: true,
    expectedSubtitleCueCount: 1,
    expectedAudioPresent: true,
  }),
  Object.freeze({
    id: "external-srt-malformed-rejected",
    fixtureId: "subtitle-solid",
    terminalStage: "error",
    sizeLimitMb: 0,
    strictFit: false,
    strictFitAllowAudioRemoval: false,
    trimStartS: 3,
    trimEndS: 6,
    subtitleFixture: "malformed",
    expectedErrorIncludes: "invalid start time",
    requiredStages: Object.freeze(["detected", "error"]),
    forbiddenStages: Object.freeze(["workflow-ready", "interaction-ready", "encoding", "success"]),
  }),
  Object.freeze({
    id: "external-srt-missing-capability",
    fixtureId: "subtitle-solid",
    terminalStage: "error",
    sizeLimitMb: 0,
    strictFit: false,
    strictFitAllowAudioRemoval: false,
    trimStartS: 3,
    trimEndS: 6,
    subtitleFixture: "valid",
    missingCapabilityFilters: Object.freeze(["subtitles"]),
    expectedErrorIncludes: "missing filter subtitles",
    requiredStages: Object.freeze(["detected", "error"]),
    forbiddenStages: Object.freeze(["workflow-ready", "interaction-ready", "encoding", "success"]),
  }),
  Object.freeze({
    id: "exact-trim-existing-output-refused",
    fixtureId: "target-easy",
    terminalStage: "error",
    sizeLimitMb: 0,
    strictFit: false,
    strictFitAllowAudioRemoval: false,
    trimStartS: 1,
    trimEndS: 3,
    preseedOutput: true,
    expectedErrorIncludes: "already exists",
  }),
]);

const g5SmokeCaseById = new Map(g5SmokeCases.map((testCase) => [testCase.id, testCase]));

function assertSupportedPlatform(platform) {
  if (!SUPPORTED_PLATFORMS.has(platform)) {
    throw new Error(`Portable G5 smoke supports only Windows and Linux, not ${platform}.`);
  }
}

function resolveG5SmokeCases(caseIds) {
  if (caseIds === undefined || caseIds === null) return [...g5SmokeCases];
  if (!Array.isArray(caseIds) || caseIds.length === 0) {
    throw new Error("Portable G5 smoke caseIds must be a non-empty array when provided.");
  }
  const seen = new Set();
  return caseIds.map((rawCaseId) => {
    const caseId = String(rawCaseId).trim();
    if (seen.has(caseId)) throw new Error(`Portable G5 smoke case is duplicated: ${caseId}`);
    seen.add(caseId);
    const testCase = g5SmokeCaseById.get(caseId);
    if (!testCase) throw new Error(`Unknown portable G5 smoke case: ${caseId}`);
    return testCase;
  });
}

function normalizeMissingCapabilityFilters(value) {
  if (value === undefined || value === null || value === "") return "";
  const rawNames = Array.isArray(value) ? value : String(value).split(",");
  const names = rawNames.map((name) => String(name).trim()).filter(Boolean);
  if (names.length === 0) return "";
  const unique = [];
  for (const name of names) {
    if (!MASKABLE_CAPABILITY_FILTERS.has(name)) {
      throw new Error(`Portable G5 smoke cannot mask non-allowlisted FFmpeg filter: ${name}`);
    }
    if (!unique.includes(name)) unique.push(name);
  }
  return unique.join(",");
}

function getPortableG5Paths(portableDir, { platform = process.platform } = {}) {
  assertSupportedPlatform(platform);
  const portableRoot = path.resolve(portableDir);
  const suffix = platform === "win32" ? ".exe" : "";
  return Object.freeze({
    portableRoot,
    appPath: path.resolve(portableRoot, getPortableExecutableName({ platform })),
    ffmpegPath: path.resolve(portableRoot, ffmpegSidecarResourceTarget, `ffmpeg${suffix}`),
    ffprobePath: path.resolve(portableRoot, ffmpegSidecarResourceTarget, `ffprobe${suffix}`),
  });
}

function buildPortableG5Launch(appPath, { platform = process.platform, env = process.env } = {}) {
  assertSupportedPlatform(platform);
  if (platform === "linux" && !env.DISPLAY) {
    return Object.freeze({ command: "xvfb-run", args: Object.freeze(["-a", appPath]) });
  }
  return Object.freeze({ command: appPath, args: Object.freeze([]) });
}

function fixtureOutputPath(fixtureId, fixtureRoot) {
  const fileNames = {
    "target-easy": "target-easy.mp4",
    "target-hard": "target-hard.mp4",
    "audio-long": "audio-long.mp4",
    "subtitle-solid": "source-字幕-café.mp4",
  };
  const fileName = fileNames[fixtureId];
  if (!fileName) throw new Error(`Unknown portable G5 fixture: ${fixtureId}`);
  return path.resolve(fixtureRoot, fileName);
}

function buildG5FixtureCommand(fixtureId, fixtureRoot) {
  const outputPath = fixtureOutputPath(fixtureId, fixtureRoot);
  const common = ["-y", "-hide_banner", "-loglevel", "error"];
  switch (fixtureId) {
    case "target-easy":
      return Object.freeze({
        outputPath,
        args: Object.freeze([
          ...common,
          "-f", "lavfi", "-i", "color=c=gray:size=160x90:rate=12",
          "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
          "-t", "2",
          "-c:v", "libx264", "-preset", "ultrafast", "-threads", "1", "-b:v", "40k", "-pix_fmt", "yuv420p",
          "-c:a", "aac", "-b:a", "32k", "-movflags", "+faststart",
          outputPath,
        ]),
      });
    case "target-hard":
      return Object.freeze({
        outputPath,
        args: Object.freeze([
          ...common,
          "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30",
          "-f", "lavfi", "-i", "sine=frequency=660:sample_rate=48000",
          "-t", "20",
          "-c:v", "libx264", "-preset", "ultrafast", "-threads", "1", "-b:v", "900k", "-pix_fmt", "yuv420p",
          "-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart",
          outputPath,
        ]),
      });
    case "audio-long":
      return Object.freeze({
        outputPath,
        args: Object.freeze([
          ...common,
          "-f", "lavfi", "-i", "color=c=black:size=16x16:rate=4",
          "-f", "lavfi", "-i", "sine=frequency=770:sample_rate=48000",
          "-t", "40",
          "-c:v", "libx264", "-preset", "ultrafast", "-threads", "1", "-crf", "51", "-pix_fmt", "yuv420p",
          "-c:a", "aac", "-b:a", "32k", "-movflags", "+faststart",
          outputPath,
        ]),
      });
    case "subtitle-solid":
      return Object.freeze({
        outputPath,
        args: Object.freeze([
          ...common,
          "-f", "lavfi", "-i", "color=c=0x606060:size=640x360:rate=30",
          "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000",
          "-t", "6",
          "-c:v", "libx264", "-preset", "ultrafast", "-threads", "1", "-b:v", "300k", "-pix_fmt", "yuv420p",
          "-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart",
          outputPath,
        ]),
      });
    default:
      throw new Error(`Unknown portable G5 fixture: ${fixtureId}`);
  }
}

function subtitleFixturePath(kind, fixtureRoot) {
  if (kind === "valid") return path.resolve(fixtureRoot, VALID_SUBTITLE_FILE_NAME);
  if (kind === "malformed") return path.resolve(fixtureRoot, MALFORMED_SUBTITLE_FILE_NAME);
  if (kind === undefined || kind === null) return null;
  throw new Error(`Unknown portable G5 subtitle fixture: ${kind}`);
}

function buildG5SmokeEnvironment(testCase, {
  inputPath,
  outputPath,
  statusPath,
  subtitlePath = null,
  caseRoot,
  baseEnv = process.env,
  platform = process.platform,
} = {}) {
  if (!testCase || !g5SmokeCaseById.has(testCase.id)) {
    throw new Error("Portable G5 smoke requires a canonical test case.");
  }
  for (const [label, value] of [["inputPath", inputPath], ["outputPath", outputPath], ["statusPath", statusPath], ["caseRoot", caseRoot]]) {
    if (!value || !String(value).trim()) throw new Error(`Portable G5 smoke ${label} is required.`);
  }
  if (testCase.subtitleFixture && !subtitlePath) {
    throw new Error(`Portable G5 smoke ${testCase.id} requires a subtitlePath.`);
  }
  const missingCapabilityFilters = normalizeMissingCapabilityFilters(testCase.missingCapabilityFilters);
  const trimStartS = testCase.trimStartS ?? 0;
  const trimEndS = testCase.trimEndS;
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
    VFL_SMOKE_FORMAT: testCase.outputFormat ?? "mp4",
    VFL_SMOKE_SIZE_LIMIT_MB: String(testCase.sizeLimitMb),
    VFL_SMOKE_TRIM_START_S: String(trimStartS),
    ...(trimEndS === undefined || trimEndS === null ? {} : { VFL_SMOKE_TRIM_END_S: String(trimEndS) }),
    VFL_SMOKE_SKIP_PREVIEW_INTERACTIONS: "1",
    VFL_SMOKE_WORKFLOW_QUEUE: testCase.workflowQueueExport ? "1" : "0",
    VFL_SMOKE_G5_QUEUE_TARGET_MISS: testCase.g5QueueTargetMiss ? "1" : "0",
    VFL_SMOKE_STRICT_FIT: testCase.strictFit ? "1" : "0",
    VFL_SMOKE_STRICT_FIT_ALLOW_AUDIO_REMOVAL: testCase.strictFitAllowAudioRemoval ? "1" : "0",
    ...(subtitlePath ? { VFL_SMOKE_SUBTITLE_PATH: path.resolve(subtitlePath) } : {}),
    ...(missingCapabilityFilters ? { VFL_SMOKE_MISSING_CAPABILITY_FILTERS: missingCapabilityFilters } : {}),
  };
  if (platform === "linux") {
    env.XDG_DATA_HOME = path.resolve(caseRoot, "xdg-data");
    env.XDG_CONFIG_HOME = path.resolve(caseRoot, "xdg-config");
    env.XDG_CACHE_HOME = path.resolve(caseRoot, "xdg-cache");
  } else if (platform === "win32") {
    env.WEBVIEW2_USER_DATA_FOLDER = path.resolve(caseRoot, "webview2-user-data");
  }
  return env;
}

function targetResultQueueOutcomeKind(targetResult) {
  return targetResult?.status === "missed" ? "target-missed" : "done";
}

function assertPositiveSafeInteger(value, label) {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`${label} must be a positive safe integer, got ${String(value)}.`);
  }
}

function portablePathIdentity(value, { platform = process.platform } = {}) {
  const raw = String(value ?? "");
  if (platform !== "win32") return path.resolve(raw);
  let normalized = path.win32.normalize(raw);
  if (normalized.toLowerCase().startsWith("\\\\?\\unc\\")) {
    normalized = `\\\\${normalized.slice(8)}`;
  } else if (normalized.startsWith("\\\\?\\")) {
    normalized = normalized.slice(4);
  }
  return path.win32.normalize(normalized).toLowerCase();
}

async function portablePathsReferToSameFile(firstPath, secondPath, { platform = process.platform } = {}) {
  if (portablePathIdentity(firstPath, { platform }) === portablePathIdentity(secondPath, { platform })) {
    return true;
  }
  try {
    const [firstRealPath, secondRealPath] = await Promise.all([
      fs.realpath(firstPath),
      fs.realpath(secondPath),
    ]);
    if (portablePathIdentity(firstRealPath, { platform }) === portablePathIdentity(secondRealPath, { platform })) {
      return true;
    }
    const [firstStats, secondStats] = await Promise.all([
      fs.stat(firstRealPath),
      fs.stat(secondRealPath),
    ]);
    return firstStats.dev === secondStats.dev && firstStats.ino !== 0 && firstStats.ino === secondStats.ino;
  } catch {
    return false;
  }
}

function validateTargetResultEvidence(testCase, status, outputSizeBytes) {
  if (!(testCase.sizeLimitMb > 0)) {
    if (status?.targetResult !== undefined && status.targetResult !== null) {
      throw new Error(`Portable G5 smoke ${testCase.id} reported targetResult without a target.`);
    }
    if (status?.queueOutcomeKind !== "done") {
      throw new Error(`Portable G5 smoke ${testCase.id} expected queueOutcomeKind=done without a missed target.`);
    }
    return Object.freeze({ queueOutcomeKind: "done", targetResult: null });
  }

  const targetResult = status?.targetResult;
  if (!targetResult || typeof targetResult !== "object" || Array.isArray(targetResult)) {
    throw new Error(`Portable G5 smoke ${testCase.id} omitted authoritative targetResult evidence.`);
  }
  const expectedTargetBytes = Math.trunc(testCase.sizeLimitMb * 1_000_000);
  assertPositiveSafeInteger(targetResult.targetBytes, `${testCase.id} targetBytes`);
  assertPositiveSafeInteger(targetResult.actualBytes, `${testCase.id} actualBytes`);
  if (targetResult.targetBytes !== expectedTargetBytes) {
    throw new Error(`Portable G5 smoke ${testCase.id} target bytes mismatch. expected=${expectedTargetBytes} actual=${targetResult.targetBytes}`);
  }
  if (targetResult.actualBytes !== outputSizeBytes) {
    throw new Error(`Portable G5 smoke ${testCase.id} target actual bytes ${targetResult.actualBytes} did not match filesystem bytes ${outputSizeBytes}.`);
  }
  const derivedStatus = outputSizeBytes <= expectedTargetBytes ? "met" : "missed";
  const derivedOvershoot = Math.max(0, outputSizeBytes - expectedTargetBytes);
  if (targetResult.status !== derivedStatus || targetResult.status !== testCase.expectedTargetStatus) {
    throw new Error(`Portable G5 smoke ${testCase.id} target status mismatch. expected=${testCase.expectedTargetStatus} derived=${derivedStatus} actual=${targetResult.status}`);
  }
  if (targetResult.overshootBytes !== derivedOvershoot) {
    throw new Error(`Portable G5 smoke ${testCase.id} overshoot mismatch. expected=${derivedOvershoot} actual=${targetResult.overshootBytes}`);
  }
  if (targetResult.strictFit !== testCase.strictFit) {
    throw new Error(`Portable G5 smoke ${testCase.id} strictFit evidence mismatch.`);
  }
  const queueOutcomeKind = targetResultQueueOutcomeKind(targetResult);
  if (status.queueOutcomeKind !== queueOutcomeKind) {
    throw new Error(`Portable G5 smoke ${testCase.id} queue outcome contradicts targetResult. expected=${queueOutcomeKind} actual=${status.queueOutcomeKind}`);
  }
  if (!Array.isArray(targetResult.plans)) {
    throw new Error(`Portable G5 smoke ${testCase.id} targetResult plans must be an array.`);
  }
  const minimumPlans = testCase.expectedMinFitPlans ?? 1;
  if (targetResult.plans.length < minimumPlans || targetResult.plans.length > G5_MAX_FIT_PLANS) {
    throw new Error(`Portable G5 smoke ${testCase.id} expected ${minimumPlans}-${G5_MAX_FIT_PLANS} fit plans, got ${targetResult.plans.length}.`);
  }
  const selectedPlans = [];
  let sawMutation = false;
  let totalFfmpegInvocations = 0;
  let firstMetPlanIndex = -1;
  for (const [index, plan] of targetResult.plans.entries()) {
    const expectedPlanNumber = index + 1;
    if (plan?.planNumber !== expectedPlanNumber) {
      throw new Error(`Portable G5 smoke ${testCase.id} fit plan numbering is not contiguous at ${expectedPlanNumber}.`);
    }
    if (typeof plan.label !== "string" || !plan.label.trim()) {
      throw new Error(`Portable G5 smoke ${testCase.id} fit plan ${expectedPlanNumber} has no label.`);
    }
    if (!Array.isArray(plan.mutations) || plan.mutations.some((mutation) => typeof mutation !== "string" || !mutation.trim())) {
      throw new Error(`Portable G5 smoke ${testCase.id} fit plan ${expectedPlanNumber} has invalid mutation evidence.`);
    }
    sawMutation ||= plan.mutations.length > 0;
    assertPositiveSafeInteger(plan.actualSizeBytes, `${testCase.id} plan ${expectedPlanNumber} actualSizeBytes`);
    assertPositiveSafeInteger(plan.ffmpegInvocations, `${testCase.id} plan ${expectedPlanNumber} ffmpegInvocations`);
    if (plan.ffmpegInvocations > 2) {
      throw new Error(`Portable G5 smoke ${testCase.id} fit plan ${expectedPlanNumber} exceeded two FFmpeg invocations.`);
    }
    totalFfmpegInvocations += plan.ffmpegInvocations;
    const planDerivedStatus = plan.actualSizeBytes <= expectedTargetBytes ? "met" : "missed";
    if (plan.status !== planDerivedStatus) {
      throw new Error(`Portable G5 smoke ${testCase.id} fit plan ${expectedPlanNumber} status is not exact-byte derived.`);
    }
    if (plan.status === "met" && firstMetPlanIndex < 0) firstMetPlanIndex = index;
    if (!testCase.strictFitAllowAudioRemoval && plan.audioAction === "drop") {
      throw new Error(`Portable G5 smoke ${testCase.id} dropped audio without explicit Strict Fit permission.`);
    }
    if (plan.selected) selectedPlans.push(plan);
  }
  if (totalFfmpegInvocations > G5_MAX_FIT_PLANS * 2) {
    throw new Error(`Portable G5 smoke ${testCase.id} exceeded the global eight-invocation work budget.`);
  }
  if (testCase.strictFit && targetResult.plans.length > 1 && !sawMutation) {
    throw new Error(`Portable G5 smoke ${testCase.id} ran corrective fit plans without mutation evidence.`);
  }
  if (testCase.strictFit && targetResult.plans.slice(1).some((plan) => plan.mutations.length === 0)) {
    throw new Error(`Portable G5 smoke ${testCase.id} retained a no-op corrective fit plan.`);
  }
  const audioDropPlans = targetResult.plans.filter((plan) => plan.audioAction === "drop");
  if (audioDropPlans.length > 0) {
    const finalPlan = targetResult.plans.at(-1);
    if (
      !testCase.strictFitAllowAudioRemoval ||
      audioDropPlans.length !== 1 ||
      audioDropPlans[0] !== finalPlan ||
      finalPlan.selected !== true
    ) {
      throw new Error(`Portable G5 smoke ${testCase.id} may drop audio only on the final selected plan with explicit Strict Fit permission.`);
    }
  }
  if (selectedPlans.length !== 1 || selectedPlans[0].planNumber !== targetResult.selectedPlanNumber) {
    throw new Error(`Portable G5 smoke ${testCase.id} must identify exactly one selected fit plan.`);
  }
  if (selectedPlans[0].actualSizeBytes !== outputSizeBytes) {
    throw new Error(`Portable G5 smoke ${testCase.id} selected fit plan bytes did not match the published artifact.`);
  }
  if (derivedStatus === "met") {
    if (firstMetPlanIndex < 0 || firstMetPlanIndex !== targetResult.plans.length - 1 || !targetResult.plans[firstMetPlanIndex].selected) {
      throw new Error(`Portable G5 smoke ${testCase.id} did not stop on and select the first fitting plan.`);
    }
  } else {
    if (firstMetPlanIndex >= 0) {
      throw new Error(`Portable G5 smoke ${testCase.id} reported a missed target despite a fitting plan.`);
    }
    const smallestMeasuredBytes = Math.min(...targetResult.plans.map((plan) => plan.actualSizeBytes));
    if (selectedPlans[0].actualSizeBytes !== smallestMeasuredBytes) {
      throw new Error(`Portable G5 smoke ${testCase.id} did not publish the smallest measured missed candidate.`);
    }
  }
  return Object.freeze({
    queueOutcomeKind,
    targetResult,
  });
}

function assertPrivacySafeStatusRaw(rawStatus, sensitiveValues, label = "Portable G5 smoke status") {
  const raw = String(rawStatus ?? "");
  if (/"subtitlePath"\s*:/.test(raw)) {
    throw new Error(`${label} exposed the forbidden subtitlePath field.`);
  }
  for (const value of sensitiveValues) {
    const token = String(value ?? "");
    if (!token) continue;
    const escaped = JSON.stringify(token).slice(1, -1);
    if (raw.includes(token) || (escaped && raw.includes(escaped))) {
      throw new Error(`${label} leaked private subtitle source material.`);
    }
  }
}

function grayFrameStats(rawFrame) {
  const bytes = Buffer.from(rawFrame ?? []);
  if (bytes.length === 0) throw new Error("Portable G5 smoke decoded an empty subtitle evidence frame.");
  let min = 255;
  let max = 0;
  let sum = 0;
  let squaredSum = 0;
  for (const value of bytes) {
    min = Math.min(min, value);
    max = Math.max(max, value);
    sum += value;
    squaredSum += value * value;
  }
  const mean = sum / bytes.length;
  const variance = Math.max(0, squaredSum / bytes.length - mean * mean);
  return Object.freeze({
    byteCount: bytes.length,
    min,
    max,
    range: max - min,
    mean,
    variance,
    sha256: crypto.createHash("sha256").update(bytes).digest("hex"),
  });
}

function assertSubtitleTimingEvidence(evidence) {
  const { before, visible, after } = evidence;
  const absentVariance = Math.max(before.variance, after.variance);
  const absentRange = Math.max(before.range, after.range);
  if (visible.range < Math.max(60, absentRange + 40)) {
    throw new Error(`Portable G5 smoke visible subtitle frame lacks contrast. visible=${visible.range} absent=${absentRange}`);
  }
  if (visible.variance < absentVariance + 20) {
    throw new Error(`Portable G5 smoke visible subtitle frame lacks glyph variance. visible=${visible.variance.toFixed(2)} absent=${absentVariance.toFixed(2)}`);
  }
  if (visible.sha256 === before.sha256 || visible.sha256 === after.sha256) {
    throw new Error("Portable G5 smoke subtitle-visible frame matched an outside-cue frame.");
  }
}

async function exists(filePath) {
  try {
    await fs.access(filePath);
    return true;
  } catch {
    return false;
  }
}

async function readJsonFileOrNull(filePath) {
  try {
    return JSON.parse(await fs.readFile(filePath, "utf8"));
  } catch {
    return null;
  }
}

async function runBounded(command, args, {
  cwd,
  env,
  timeoutMs = 60_000,
  capture = false,
} = {}) {
  return await new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      env,
      stdio: capture ? ["ignore", "pipe", "pipe"] : "inherit",
      windowsHide: true,
    });
    const stdout = [];
    const stderr = [];
    if (capture) {
      child.stdout.on("data", (chunk) => stdout.push(Buffer.from(chunk)));
      child.stderr.on("data", (chunk) => stderr.push(Buffer.from(chunk)));
    }
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGKILL");
    }, timeoutMs);
    let settled = false;
    const finish = (action) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      action();
    };
    child.on("error", (error) => finish(() => reject(error)));
    child.on("close", (code, signal) => finish(() => {
      const stdoutBuffer = Buffer.concat(stdout);
      const stderrBuffer = Buffer.concat(stderr);
      if (code === 0 && !timedOut) {
        resolve({ stdout: stdoutBuffer, stderr: stderrBuffer });
        return;
      }
      const detail = stderrBuffer.toString("utf8").trim();
      const error = new Error(
        timedOut
          ? `${command} timed out after ${timeoutMs} ms.`
          : `${command} exited with code ${code} signal ${signal ?? "none"}${detail ? `\n${detail}` : ""}`,
      );
      error.stdout = stdoutBuffer;
      error.stderr = stderrBuffer;
      error.timedOut = timedOut;
      error.exitCode = code;
      error.signal = signal;
      reject(error);
    }));
  });
}

function countFontGlyphWarnings(rawStderr) {
  return String(rawStderr ?? "")
    .split(/\r?\n/)
    .filter((line) => line.trim() && FONT_GLYPH_WARNING_PATTERNS.some((pattern) => pattern.test(line)))
    .length;
}

function sanitizeSubtitleFontPreflightLog(rawStderr) {
  let sanitized = String(rawStderr ?? "");
  for (const sensitive of [
    VALID_SUBTITLE_TEXT.trim(),
    MALFORMED_SUBTITLE_TEXT.trim(),
    "café Olá Καλημέρα Привет مرحبا",
    "private malformed subtitle sentinel",
  ]) {
    sanitized = sanitized.split(sensitive).join("<subtitle text>");
  }
  return sanitized.slice(0, 50_000);
}

async function retainSubtitleFontPreflightFailure(caseRoot, {
  stderr,
  timedOut = false,
  exitCode = null,
  signal = null,
} = {}) {
  const sanitizedStderr = sanitizeSubtitleFontPreflightLog(stderr);
  assertPrivacySafeStatusRaw(
    sanitizedStderr,
    [VALID_SUBTITLE_TEXT.trim(), MALFORMED_SUBTITLE_TEXT.trim(), "café Olá Καλημέρα Привет مرحبا"],
    "Portable G5 subtitle font preflight diagnostics",
  );
  const matchingLines = sanitizedStderr
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line && FONT_GLYPH_WARNING_PATTERNS.some((pattern) => pattern.test(line)));
  const sidecar = Object.freeze({
    schemaVersion: 1,
    family: "DejaVu Sans",
    fileName: STAGED_EXTERNAL_SUBTITLE_FONT_FILE_NAME,
    sha256: SUBTITLE_FONT_SHA256,
    timedOut: Boolean(timedOut),
    exitCode: Number.isInteger(exitCode) ? exitCode : null,
    signal: typeof signal === "string" ? signal : null,
    fontGlyphWarningCount: matchingLines.length,
    uniqueMatchedWarningLines: Object.freeze([...new Set(matchingLines)].slice(0, 50)),
  });
  await Promise.all([
    fs.writeFile(
      path.resolve(caseRoot, "subtitle-font-preflight.stderr.log"),
      sanitizedStderr || "No FFmpeg stderr was captured.\n",
      "utf8",
    ),
    fs.writeFile(
      path.resolve(caseRoot, "subtitle-font-preflight.failure.json"),
      `${JSON.stringify(sidecar, null, 2)}\n`,
      "utf8",
    ),
  ]);
  return sidecar;
}

async function runSubtitleFontGlyphPreflight(ffmpegPath, inputPath, subtitlePath, caseRoot) {
  const preflightRoot = path.resolve(caseRoot, "subtitle-font-preflight");
  const stagedSubtitlePath = path.resolve(preflightRoot, STAGED_EXTERNAL_SRT_FILE_NAME);
  const stagedFontDir = path.resolve(preflightRoot, STAGED_EXTERNAL_SUBTITLE_FONT_DIR_NAME);
  const stagedFontPath = path.resolve(stagedFontDir, STAGED_EXTERNAL_SUBTITLE_FONT_FILE_NAME);
  await Promise.all([
    fs.mkdir(preflightRoot, { recursive: true }),
    fs.mkdir(stagedFontDir, { recursive: true }),
  ]);
  try {
    await Promise.all([
      fs.copyFile(subtitlePath, stagedSubtitlePath),
      fs.copyFile(SUBTITLE_FONT_SOURCE_PATH, stagedFontPath),
    ]);
    let result;
    try {
      result = await runBounded(ffmpegPath, [
        "-nostdin", "-hide_banner", "-loglevel", "warning",
        "-i", inputPath,
        "-map", "0:v:0",
        "-vf", EXTERNAL_SRT_FILTER,
        "-an", "-t", "6",
        "-f", "null", "-",
      ], {
        cwd: preflightRoot,
        capture: true,
        timeoutMs: 60_000,
      });
    } catch (error) {
      await retainSubtitleFontPreflightFailure(caseRoot, {
        stderr: error?.stderr,
        timedOut: error?.timedOut,
        exitCode: error?.exitCode,
        signal: error?.signal,
      });
      throw error;
    }
    const stderr = result.stderr.toString("utf8");
    const warningLineCount = stderr.split(/\r?\n/).filter((line) => line.trim()).length;
    const fontGlyphWarningCount = countFontGlyphWarnings(stderr);
    if (fontGlyphWarningCount > 0) {
      await retainSubtitleFontPreflightFailure(caseRoot, { stderr, exitCode: 0 });
      throw new Error(`Portable G5 smoke subtitle font preflight reported ${fontGlyphWarningCount} font/glyph warnings.`);
    }
    return Object.freeze({
      passed: true,
      family: "DejaVu Sans",
      fileName: STAGED_EXTERNAL_SUBTITLE_FONT_FILE_NAME,
      sha256: SUBTITLE_FONT_SHA256,
      warningLineCount,
      fontGlyphWarningCount,
    });
  } finally {
    await fs.rm(preflightRoot, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
}

async function verifyEmbeddedSubtitleFont(appPath) {
  const [appBytes, fontBytes] = await Promise.all([
    fs.readFile(appPath),
    fs.readFile(SUBTITLE_FONT_SOURCE_PATH),
  ]);
  const actualSha256 = crypto.createHash("sha256").update(fontBytes).digest("hex");
  if (actualSha256 !== SUBTITLE_FONT_SHA256) {
    throw new Error(`Portable G5 subtitle font source SHA256 mismatch. expected=${SUBTITLE_FONT_SHA256} actual=${actualSha256}`);
  }
  if (appBytes.indexOf(fontBytes) < 0) {
    throw new Error("Portable G5 app binary does not contain the fixed DejaVu Sans subtitle font bytes.");
  }
  return Object.freeze({
    family: "DejaVu Sans",
    fileName: STAGED_EXTERNAL_SUBTITLE_FONT_FILE_NAME,
    sha256: actualSha256,
    embeddedInApp: true,
  });
}

async function probeMedia(ffprobePath, mediaPath) {
  const result = await runBounded(ffprobePath, [
    "-v", "quiet",
    "-print_format", "json",
    "-show_format",
    "-show_streams",
    mediaPath,
  ], { capture: true, timeoutMs: 30_000 });
  return JSON.parse(result.stdout.toString("utf8"));
}

async function streamPacketHashes(ffprobePath, mediaPath, selector) {
  const result = await runBounded(ffprobePath, [
    "-v", "error",
    "-select_streams", selector,
    "-show_packets",
    "-show_entries", "packet=data_hash",
    "-show_data_hash", "sha256",
    "-of", "json",
    mediaPath,
  ], { capture: true, timeoutMs: 30_000 });
  const payload = JSON.parse(result.stdout.toString("utf8"));
  return (Array.isArray(payload?.packets) ? payload.packets : [])
    .map((packet) => String(packet?.data_hash ?? "").trim())
    .filter(Boolean);
}

async function decodeBottomHalfGrayFrame(ffmpegPath, mediaPath, timeS, pngPath) {
  await runBounded(ffmpegPath, [
    "-y", "-hide_banner", "-loglevel", "error",
    "-i", mediaPath,
    "-ss", String(timeS),
    "-frames:v", "1",
    pngPath,
  ], { timeoutMs: 30_000 });
  const result = await runBounded(ffmpegPath, [
    "-hide_banner", "-loglevel", "error",
    "-i", mediaPath,
    "-ss", String(timeS),
    "-map", "0:v:0",
    "-vf", "crop=iw:ih/2:0:ih/2,format=gray",
    "-frames:v", "1",
    "-f", "rawvideo",
    "pipe:1",
  ], { capture: true, timeoutMs: 30_000 });
  return grayFrameStats(result.stdout);
}

async function collectSubtitleTimingEvidence(ffmpegPath, outputPath, caseRoot) {
  const before = await decodeBottomHalfGrayFrame(ffmpegPath, outputPath, 0.5, path.resolve(caseRoot, "subtitle-before.png"));
  const visible = await decodeBottomHalfGrayFrame(ffmpegPath, outputPath, 1.5, path.resolve(caseRoot, "subtitle-visible.png"));
  const after = await decodeBottomHalfGrayFrame(ffmpegPath, outputPath, 2.5, path.resolve(caseRoot, "subtitle-after.png"));
  const evidence = Object.freeze({ before, visible, after });
  assertSubtitleTimingEvidence(evidence);
  return evidence;
}

function waitForChildExit(child, timeoutMs) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return Promise.resolve(true);
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      child.off("exit", onExit);
      resolve(false);
    }, timeoutMs);
    const onExit = () => {
      clearTimeout(timer);
      resolve(true);
    };
    child.once("exit", onExit);
  });
}

async function terminateSmokeChild(child, platform) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  if (platform === "win32" && child.pid) {
    try {
      await runBounded("taskkill.exe", ["/PID", String(child.pid), "/T", "/F"], {
        capture: true,
        timeoutMs: 10_000,
      });
      return;
    } catch {
      // Fall through to the direct process termination path.
    }
  }
  const signal = (kind) => {
    try {
      if (platform === "linux" && child.pid) process.kill(-child.pid, kind);
      else child.kill(kind);
      return true;
    } catch {
      try {
        child.kill(kind);
        return true;
      } catch {
        return false;
      }
    }
  };
  signal("SIGTERM");
  if (await waitForChildExit(child, 2_500)) return;
  signal("SIGKILL");
  await waitForChildExit(child, 2_500);
}

async function waitForTerminalStatus(child, statusPath, testCase, timeoutSeconds) {
  const deadline = Date.now() + timeoutSeconds * 1000;
  let lastStatus = null;
  let lastStage = null;
  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 250));
    const status = await readJsonFileOrNull(statusPath);
    if (status) {
      lastStatus = status;
      if (status.stage !== lastStage) {
        console.log(`Portable G5 smoke ${testCase.id} stage: ${status.stage}`);
        lastStage = status.stage;
      }
      if (status.stage === "success" || status.stage === "error") {
        if (status.stage !== testCase.terminalStage) {
          throw new Error(`Portable G5 smoke ${testCase.id} expected ${testCase.terminalStage} but reached ${status.stage}: ${status.message ?? "no message"}`);
        }
        return status;
      }
    }
    if ((child.exitCode !== null || child.signalCode !== null) && !lastStatus) {
      throw new Error(`Portable G5 smoke ${testCase.id} app exited before writing status.`);
    }
    if ((child.exitCode !== null || child.signalCode !== null) && lastStatus?.stage !== testCase.terminalStage) {
      throw new Error(`Portable G5 smoke ${testCase.id} app exited at stage ${lastStatus?.stage ?? "none"}.`);
    }
  }
  throw new Error(`Portable G5 smoke ${testCase.id} timed out. Last stage: ${lastStatus?.stage ?? "none"}`);
}

function assertStageHistory(testCase, status) {
  const history = Array.isArray(status?.stageHistory) ? status.stageHistory : [];
  const required = testCase.requiredStages ?? [...REQUIRED_G5_SMOKE_STAGES, testCase.terminalStage];
  const missing = required.filter((stage) => !history.includes(stage));
  if (missing.length > 0) {
    throw new Error(`Portable G5 smoke ${testCase.id} missed stages ${missing.join(", ")}. Saw: ${history.join(" -> ")}`);
  }
  const observed = history.filter((stage) => required.includes(stage));
  if (JSON.stringify(observed) !== JSON.stringify(required)) {
    throw new Error(`Portable G5 smoke ${testCase.id} required stages were out of order. Saw: ${history.join(" -> ")}`);
  }
  const forbidden = testCase.forbiddenStages ?? [];
  const unexpectedlyObserved = forbidden.filter((stage) => history.includes(stage));
  if (unexpectedlyObserved.length > 0) {
    throw new Error(`Portable G5 smoke ${testCase.id} reached forbidden post-rejection stages ${unexpectedlyObserved.join(", ")}.`);
  }
}

function countStreams(probe, codecType) {
  return Array.isArray(probe?.streams)
    ? probe.streams.filter((stream) => stream?.codec_type === codecType).length
    : 0;
}

async function verifySuccessfulCase({
  testCase,
  status,
  inputPath,
  outputPath,
  subtitlePath,
  ffmpegPath,
  ffprobePath,
  caseRoot,
  embeddedSubtitleFont,
}) {
  if (!(await exists(outputPath))) {
    throw new Error(`Portable G5 smoke ${testCase.id} reported success without an output.`);
  }
  const outputStats = await fs.stat(outputPath);
  assertPositiveSafeInteger(outputStats.size, `${testCase.id} filesystem output size`);
  if (
    typeof status.outputPath !== "string" ||
    !(await portablePathsReferToSameFile(status.outputPath, outputPath))
  ) {
    throw new Error(`Portable G5 smoke ${testCase.id} status outputPath did not identify the published artifact.`);
  }
  if (status.ok !== true || status.outputSizeBytes !== outputStats.size) {
    throw new Error(`Portable G5 smoke ${testCase.id} status bytes did not match the ${outputStats.size} byte artifact.`);
  }
  const targetEvidence = validateTargetResultEvidence(testCase, status, outputStats.size);
  const diagnostics = status.diagnostics;
  if (!diagnostics || typeof diagnostics !== "object" || Array.isArray(diagnostics)) {
    throw new Error(`Portable G5 smoke ${testCase.id} omitted privacy-safe diagnostics.`);
  }
  if (diagnostics.actualSizeBytes !== outputStats.size) {
    throw new Error(`Portable G5 smoke ${testCase.id} diagnostic actual bytes did not match the artifact.`);
  }
  const expectedTargetBytes = testCase.sizeLimitMb > 0 ? Math.trunc(testCase.sizeLimitMb * 1_000_000) : null;
  if ((diagnostics.requestedSizeBytes ?? null) !== expectedTargetBytes) {
    throw new Error(`Portable G5 smoke ${testCase.id} diagnostic target bytes mismatch.`);
  }

  const [inputProbe, outputProbe] = await Promise.all([
    probeMedia(ffprobePath, inputPath),
    probeMedia(ffprobePath, outputPath),
  ]);
  const audioStreamCount = countStreams(outputProbe, "audio");
  const subtitleStreamCount = countStreams(outputProbe, "subtitle");
  if (testCase.expectedAudioPresent && audioStreamCount < 1) {
    throw new Error(`Portable G5 smoke ${testCase.id} unexpectedly removed requested audio.`);
  }
  if (testCase.expectedAudioPresent && diagnostics.audioAction === "drop") {
    throw new Error(`Portable G5 smoke ${testCase.id} reported dropped audio despite the preservation contract.`);
  }
  if (testCase.expectedAudioRemoved) {
    const selectedPlan = targetEvidence.targetResult?.plans.find((plan) => plan.selected);
    if (
      audioStreamCount !== 0 ||
      diagnostics.audioAction !== "drop" ||
      selectedPlan?.audioAction !== "drop" ||
      !selectedPlan.mutations.some((mutation) => /audio removed.*explicit/i.test(mutation))
    ) {
      throw new Error(`Portable G5 smoke ${testCase.id} did not prove explicitly permitted audio removal.`);
    }
  }

  let subtitleTiming = null;
  let subtitleFontPreflight = null;
  if (testCase.expectedSubtitleBurnedIn) {
    if (diagnostics.subtitleBurnedIn !== true || diagnostics.subtitleCueCount !== testCase.expectedSubtitleCueCount) {
      throw new Error(`Portable G5 smoke ${testCase.id} omitted subtitle burn-in/cue diagnostics.`);
    }
    if (diagnostics.videoAction !== "encode") {
      throw new Error(`Portable G5 smoke ${testCase.id} did not report forced video re-encode.`);
    }
    const normalizedCommandPreview = String(diagnostics.commandPreview ?? "").replaceAll("'\\''", "'");
    if (!normalizedCommandPreview.includes(EXTERNAL_SRT_FILTER)) {
      throw new Error(`Portable G5 smoke ${testCase.id} did not execute the exact fixed subtitle font/filter contract.`);
    }
    if (subtitleStreamCount !== 0) {
      throw new Error(`Portable G5 smoke ${testCase.id} muxed a subtitle stream instead of burning pixels.`);
    }
    const [inputHashes, outputHashes] = await Promise.all([
      streamPacketHashes(ffprobePath, inputPath, "v:0"),
      streamPacketHashes(ffprobePath, outputPath, "v:0"),
    ]);
    if (inputHashes.length === 0 || outputHashes.length === 0 || JSON.stringify(inputHashes) === JSON.stringify(outputHashes)) {
      throw new Error(`Portable G5 smoke ${testCase.id} did not prove video packet re-encoding.`);
    }
    subtitleTiming = await collectSubtitleTimingEvidence(ffmpegPath, outputPath, caseRoot);
    subtitleFontPreflight = await runSubtitleFontGlyphPreflight(
      ffmpegPath,
      inputPath,
      subtitlePath,
      caseRoot,
    );
    if (
      embeddedSubtitleFont?.embeddedInApp !== true ||
      embeddedSubtitleFont.sha256 !== subtitleFontPreflight.sha256
    ) {
      throw new Error(`Portable G5 smoke ${testCase.id} did not prove the preflight font is embedded in the packaged app.`);
    }
  } else if (diagnostics.subtitleBurnedIn !== false) {
    throw new Error(`Portable G5 smoke ${testCase.id} reported an unexpected subtitle burn-in.`);
  }

  const outputDurationS = Number(outputProbe?.format?.duration);
  const expectedDurationS = testCase.trimEndS === undefined
    ? Number(inputProbe?.format?.duration)
    : testCase.trimEndS - (testCase.trimStartS ?? 0);
  if (!Number.isFinite(outputDurationS) || !Number.isFinite(expectedDurationS) || Math.abs(outputDurationS - expectedDurationS) > 0.2) {
    throw new Error(`Portable G5 smoke ${testCase.id} duration mismatch. expected=${expectedDurationS} actual=${outputDurationS}`);
  }

  return Object.freeze({
    schemaVersion: G5_EVIDENCE_SCHEMA_VERSION,
    caseId: testCase.id,
    terminalStage: status.stage,
    stageHistory: Object.freeze([...status.stageHistory]),
    queueOutcomeKind: targetEvidence.queueOutcomeKind,
    targetResult: targetEvidence.targetResult,
    output: Object.freeze({
      sizeBytes: outputStats.size,
      audioStreamCount,
      subtitleStreamCount,
      durationS: outputDurationS,
    }),
    diagnostics: Object.freeze({
      requestedSizeBytes: diagnostics.requestedSizeBytes ?? null,
      actualSizeBytes: diagnostics.actualSizeBytes,
      videoAction: diagnostics.videoAction ?? null,
      audioAction: diagnostics.audioAction ?? null,
      subtitleBurnedIn: diagnostics.subtitleBurnedIn,
      subtitleCueCount: diagnostics.subtitleCueCount ?? null,
    }),
    subtitleFontPreflight,
    subtitleTiming,
  });
}

async function verifyErrorCase(testCase, status, outputPath, caseRoot, preseed) {
  if (status.ok !== false) {
    throw new Error(`Portable G5 smoke ${testCase.id} error status did not report ok=false.`);
  }
  if (!String(status.message ?? "").includes(testCase.expectedErrorIncludes)) {
    throw new Error(`Portable G5 smoke ${testCase.id} error mismatch. expected=${testCase.expectedErrorIncludes} actual=${status.message ?? "none"}`);
  }
  let noClobber = null;
  if (testCase.preseedOutput) {
    if (!preseed || !(await exists(outputPath))) {
      throw new Error(`Portable G5 smoke ${testCase.id} lost the pre-seeded destination.`);
    }
    const bytes = await fs.readFile(outputPath);
    const sha256 = crypto.createHash("sha256").update(bytes).digest("hex");
    if (bytes.length !== preseed.sizeBytes || sha256 !== preseed.sha256) {
      throw new Error(`Portable G5 smoke ${testCase.id} changed the pre-seeded destination.`);
    }
    const temporaryOutputs = (await fs.readdir(caseRoot, { withFileTypes: true }))
      .filter((entry) => entry.isFile() && entry.name.startsWith(".vfl-") && entry.name.includes(".tmp."))
      .map((entry) => entry.name);
    if (temporaryOutputs.length > 0) {
      throw new Error(`Portable G5 smoke ${testCase.id} left temporary output files behind.`);
    }
    const diagnostics = status.diagnostics;
    if (
      !diagnostics ||
      diagnostics.failureStage !== "backend" ||
      !String(diagnostics.failureReason ?? "").includes(testCase.expectedErrorIncludes) ||
      diagnostics.videoAction !== null ||
      diagnostics.audioAction !== null ||
      diagnostics.attempts !== 0 ||
      diagnostics.passes !== 0 ||
      !String(diagnostics.commandPreview ?? "").includes("No FFmpeg command")
    ) {
      throw new Error(`Portable G5 smoke ${testCase.id} did not report the expected early no-clobber refusal evidence.`);
    }
    noClobber = Object.freeze({
      preserved: true,
      sizeBytes: preseed.sizeBytes,
      sha256: preseed.sha256,
      temporaryOutputCount: 0,
    });
  } else if (await exists(outputPath)) {
    throw new Error(`Portable G5 smoke ${testCase.id} published output despite a required rejection.`);
  }
  return Object.freeze({
    schemaVersion: G5_EVIDENCE_SCHEMA_VERSION,
    caseId: testCase.id,
    terminalStage: status.stage,
    stageHistory: Object.freeze([...status.stageHistory]),
    queueOutcomeKind: null,
    targetResult: null,
    output: null,
    diagnostics: testCase.preseedOutput ? Object.freeze({
      failureStage: status.diagnostics.failureStage,
      attempts: status.diagnostics.attempts,
      passes: status.diagnostics.passes,
    }) : null,
    subtitleFontPreflight: null,
    subtitleTiming: null,
    errorCategory: testCase.preseedOutput
      ? "no-clobber"
      : testCase.missingCapabilityFilters
        ? "missing-capability"
        : "malformed-srt",
    noClobber,
  });
}

async function runPortableG5Smoke({
  portableDir = process.env.VFL_PORTABLE_DIR || getPortableOutputDir(),
  platform = process.platform,
  caseIds,
  timeoutSeconds = 300,
  fixtureTimeoutSeconds = 120,
  keepSuccessArtifacts = process.env.VFL_G5_SMOKE_KEEP_ARTIFACTS === "1",
} = {}) {
  assertSupportedPlatform(platform);
  if (platform !== process.platform) {
    throw new Error(`Portable G5 smoke cannot execute ${platform} artifacts on ${process.platform}.`);
  }
  if (!Number.isFinite(timeoutSeconds) || timeoutSeconds <= 0 || timeoutSeconds > 600) {
    throw new Error("Portable G5 smoke timeoutSeconds must be between 1 and 600.");
  }
  if (!Number.isFinite(fixtureTimeoutSeconds) || fixtureTimeoutSeconds <= 0 || fixtureTimeoutSeconds > 300) {
    throw new Error("Portable G5 smoke fixtureTimeoutSeconds must be between 1 and 300.");
  }

  const selectedCases = resolveG5SmokeCases(caseIds);
  const { portableRoot, appPath, ffmpegPath, ffprobePath } = getPortableG5Paths(portableDir, { platform });
  for (const requiredPath of [appPath, ffmpegPath, ffprobePath]) {
    if (!(await exists(requiredPath))) {
      throw new Error(`Portable G5 smoke could not find required extracted payload file: ${requiredPath}`);
    }
  }
  const embeddedSubtitleFont = await verifyEmbeddedSubtitleFont(appPath);

  const smokeRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-portable-g5-"));
  const fixtureRoot = path.resolve(smokeRoot, "fixtures");
  const resultRoot = path.resolve(smokeRoot, "results");
  await Promise.all([
    fs.mkdir(fixtureRoot, { recursive: true }),
    fs.mkdir(resultRoot, { recursive: true }),
  ]);
  let failed = false;
  const evidence = [];

  try {
    const fixtureIds = [...new Set(selectedCases.map((testCase) => testCase.fixtureId))];
    for (const fixtureId of fixtureIds) {
      const fixture = buildG5FixtureCommand(fixtureId, fixtureRoot);
      await runBounded(ffmpegPath, fixture.args, {
        cwd: fixtureRoot,
        timeoutMs: fixtureTimeoutSeconds * 1000,
      });
      if (!(await exists(fixture.outputPath))) {
        throw new Error(`Portable G5 fixture command did not create ${fixture.outputPath}`);
      }
      console.log(`Portable G5 fixture ready: ${fixtureId}`);
    }
    if (selectedCases.some((testCase) => testCase.subtitleFixture === "valid")) {
      await fs.writeFile(subtitleFixturePath("valid", fixtureRoot), VALID_SUBTITLE_TEXT, "utf8");
    }
    if (selectedCases.some((testCase) => testCase.subtitleFixture === "malformed")) {
      await fs.writeFile(subtitleFixturePath("malformed", fixtureRoot), MALFORMED_SUBTITLE_TEXT, "utf8");
    }

    for (const testCase of selectedCases) {
      const caseRoot = path.resolve(resultRoot, testCase.id);
      await fs.mkdir(caseRoot, { recursive: true });
      const inputPath = fixtureOutputPath(testCase.fixtureId, fixtureRoot);
      const outputPath = path.resolve(caseRoot, `${testCase.id}.${testCase.outputFormat ?? "mp4"}`);
      const statusPath = path.resolve(caseRoot, "status.json");
      const stdoutPath = path.resolve(caseRoot, "app.stdout.log");
      const stderrPath = path.resolve(caseRoot, "app.stderr.log");
      const subtitlePath = subtitleFixturePath(testCase.subtitleFixture, fixtureRoot);
      let preseed = null;
      if (testCase.preseedOutput) {
        const bytes = Buffer.from(G5_NO_CLOBBER_SENTINEL, "utf8");
        await fs.writeFile(outputPath, bytes);
        preseed = Object.freeze({
          sizeBytes: bytes.length,
          sha256: crypto.createHash("sha256").update(bytes).digest("hex"),
        });
      }
      const smokeEnv = buildG5SmokeEnvironment(testCase, {
        inputPath,
        outputPath,
        statusPath,
        subtitlePath,
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

      const [stdoutFile, stderrFile] = await Promise.all([
        fs.open(stdoutPath, "w"),
        fs.open(stderrPath, "w"),
      ]);
      let child = null;
      try {
        const launch = buildPortableG5Launch(appPath, { platform, env: smokeEnv });
        child = spawn(launch.command, launch.args, {
          cwd: portableRoot,
          detached: platform === "linux",
          env: smokeEnv,
          stdio: ["ignore", stdoutFile.fd, stderrFile.fd],
          windowsHide: true,
        });
        const status = await waitForTerminalStatus(child, statusPath, testCase, timeoutSeconds);
        assertStageHistory(testCase, status);
        const rawStatus = await fs.readFile(statusPath, "utf8");
        const subtitleSensitiveValues = subtitlePath
          ? [subtitlePath, path.basename(subtitlePath), VALID_SUBTITLE_TEXT.trim(), MALFORMED_SUBTITLE_TEXT.trim(), "café Olá Καλημέρα Привет مرحبا", "private malformed subtitle sentinel"]
          : [];
        assertPrivacySafeStatusRaw(rawStatus, subtitleSensitiveValues, `Portable G5 smoke ${testCase.id} status`);
        await Promise.all([stdoutFile.sync(), stderrFile.sync()]);
        const [stdoutRaw, stderrRaw] = await Promise.all([
          fs.readFile(stdoutPath, "utf8"),
          fs.readFile(stderrPath, "utf8"),
        ]);
        assertPrivacySafeStatusRaw(stdoutRaw, subtitleSensitiveValues, `Portable G5 smoke ${testCase.id} stdout`);
        assertPrivacySafeStatusRaw(stderrRaw, subtitleSensitiveValues, `Portable G5 smoke ${testCase.id} stderr`);
        const caseEvidence = testCase.terminalStage === "success"
          ? await verifySuccessfulCase({
            testCase,
            status,
            inputPath,
            outputPath,
            subtitlePath,
            ffmpegPath,
            ffprobePath,
            caseRoot,
            embeddedSubtitleFont,
          })
          : await verifyErrorCase(testCase, status, outputPath, caseRoot, preseed);
        evidence.push(caseEvidence);
        const evidenceRaw = `${JSON.stringify({ ...caseEvidence, platform }, null, 2)}\n`;
        assertPrivacySafeStatusRaw(
          evidenceRaw,
          subtitleSensitiveValues,
          `Portable G5 smoke ${testCase.id} retained evidence`,
        );
        await fs.writeFile(
          path.resolve(caseRoot, "evidence.json"),
          evidenceRaw,
          "utf8",
        );
        console.log(`Portable G5 smoke passed: ${testCase.id}`);
      } finally {
        await terminateSmokeChild(child, platform);
        await Promise.allSettled([stdoutFile.close(), stderrFile.close()]);
      }
    }
    console.log(`Portable G5 smoke passed ${selectedCases.length} cases on ${platform}.`);
    if (keepSuccessArtifacts) {
      console.log(`Portable G5 smoke evidence retained at ${smokeRoot}`);
    }
    return Object.freeze({ smokeRoot: keepSuccessArtifacts ? smokeRoot : null, evidence: Object.freeze(evidence) });
  } catch (error) {
    failed = true;
    console.error(`Portable G5 smoke preserved failure evidence at ${smokeRoot}`);
    throw error;
  } finally {
    if (!failed && !keepSuccessArtifacts) {
      await fs.rm(smokeRoot, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
    }
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === __filename) {
  const caseIds = String(process.env.VFL_G5_SMOKE_CASES ?? "")
    .split(",")
    .map((caseId) => caseId.trim())
    .filter(Boolean);
  await runPortableG5Smoke({ caseIds: caseIds.length > 0 ? caseIds : undefined });
}

export {
  G5_EVIDENCE_SCHEMA_VERSION,
  G5_MAX_FIT_PLANS,
  EXTERNAL_SRT_FILTER,
  EXTERNAL_SRT_FORCE_STYLE,
  MALFORMED_SUBTITLE_FILE_NAME,
  MALFORMED_SUBTITLE_TEXT,
  REQUIRED_G5_SMOKE_STAGES,
  STAGED_EXTERNAL_SRT_FILE_NAME,
  STAGED_EXTERNAL_SUBTITLE_FONT_DIR_NAME,
  STAGED_EXTERNAL_SUBTITLE_FONT_FILE_NAME,
  SUBTITLE_FONT_SHA256,
  VALID_SUBTITLE_FILE_NAME,
  VALID_SUBTITLE_TEXT,
  assertPrivacySafeStatusRaw,
  assertStageHistory,
  assertSubtitleTimingEvidence,
  buildG5FixtureCommand,
  buildG5SmokeEnvironment,
  buildPortableG5Launch,
  countFontGlyphWarnings,
  fixtureOutputPath,
  g5SmokeCases,
  getPortableG5Paths,
  grayFrameStats,
  normalizeMissingCapabilityFilters,
  portablePathIdentity,
  portablePathsReferToSameFile,
  resolveG5SmokeCases,
  retainSubtitleFontPreflightFailure,
  runPortableG5Smoke,
  sanitizeSubtitleFontPreflightLog,
  subtitleFixturePath,
  targetResultQueueOutcomeKind,
  validateTargetResultEvidence,
  verifyEmbeddedSubtitleFont,
};
