import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import url from "node:url";

import {
  EXTERNAL_SRT_FILTER,
  EXTERNAL_SRT_FORCE_STYLE,
  G5_EVIDENCE_SCHEMA_VERSION,
  G5_MAX_FIT_PLANS,
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
  sanitizeSubtitleFontPreflightLog,
  subtitleFixturePath,
  targetResultQueueOutcomeKind,
  validateTargetResultEvidence,
} from "../scripts/run-portable-g5-smoke.mjs";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

function fitPlan({
  planNumber,
  actualSizeBytes,
  targetBytes,
  selected = false,
  mutations = [],
  ffmpegInvocations = 2,
  audioAction = "encode",
  status,
}) {
  return {
    planNumber,
    label: planNumber === 1 ? "Requested settings" : `Correction ${planNumber}`,
    mutations,
    width: 640,
    height: 360,
    videoBitrateKbps: 64,
    audioAction,
    audioBitrateKbps: 32,
    actualSizeBytes,
    status: status ?? (actualSizeBytes <= targetBytes ? "met" : "missed"),
    ffmpegInvocations,
    selected,
  };
}

function targetStatus({
  targetBytes,
  actualBytes,
  strictFit,
  plans,
  queueOutcomeKind,
}) {
  return {
    queueOutcomeKind,
    targetResult: {
      status: actualBytes <= targetBytes ? "met" : "missed",
      targetBytes,
      actualBytes,
      overshootBytes: Math.max(0, actualBytes - targetBytes),
      strictFit,
      selectedPlanNumber: plans.find((plan) => plan.selected)?.planNumber ?? 0,
      plans,
    },
  };
}

test("G5 packaged corpus is bounded and covers exact targets plus external SRT failures", async () => {
  const g5SmokeSource = await fs.readFile(
    path.resolve(__dirname, "../scripts/run-portable-g5-smoke.mjs"),
    "utf8",
  );
  assert.equal(G5_EVIDENCE_SCHEMA_VERSION, 1);
  assert.equal(G5_MAX_FIT_PLANS, 4);
  assert.deepEqual(REQUIRED_G5_SMOKE_STAGES, [
    "detected",
    "input-applied",
    "probe-ready",
    "workflow-ready",
    "interaction-ready",
    "encoding",
  ]);
  assert.deepEqual(g5SmokeCases.map((testCase) => testCase.id), [
    "target-first-plan-met",
    "target-best-effort-missed",
    "target-strict-bounded-correction",
    "target-missed-queue-recovery",
    "mp3-impossible-target-missed",
    "external-srt-unicode-source-timing",
    "external-srt-malformed-rejected",
    "external-srt-missing-capability",
    "exact-trim-existing-output-refused",
  ]);

  const strictCase = g5SmokeCases.find((testCase) => testCase.id === "target-strict-bounded-correction");
  assert.equal(strictCase.strictFit, true);
  assert.equal(strictCase.expectedMinFitPlans, 2);
  assert.equal(strictCase.expectedAudioPresent, true);
  assert.equal(strictCase.expectedTargetStatus, "missed");
  const queueCase = g5SmokeCases.find((testCase) => testCase.id === "target-missed-queue-recovery");
  assert.equal(queueCase.workflowQueueExport, true);
  assert.equal(queueCase.g5QueueTargetMiss, true);
  assert.ok(queueCase.requiredStages.includes("workflow-queue-complete"));
  const mp3Case = g5SmokeCases.find((testCase) => testCase.id === "mp3-impossible-target-missed");
  assert.equal(mp3Case.outputFormat, "mp3");
  assert.equal(mp3Case.strictFit, false);
  assert.equal(mp3Case.expectedTargetStatus, "missed");

  const subtitleCase = g5SmokeCases.find((testCase) => testCase.id === "external-srt-unicode-source-timing");
  assert.equal(subtitleCase.trimStartS, 3);
  assert.equal(subtitleCase.trimEndS, 6);
  assert.equal(subtitleCase.expectedSubtitleCueCount, 1);
  assert.match(VALID_SUBTITLE_FILE_NAME, /字幕/);
  assert.match(VALID_SUBTITLE_FILE_NAME, /O'Brien,\[x\]; café\.srt$/);
  assert.match(VALID_SUBTITLE_TEXT, /00:00:04,000 --> 00:00:05,000/);
  assert.match(VALID_SUBTITLE_TEXT, /café Olá Καλημέρα Привет مرحبا/);
  assert.match(MALFORMED_SUBTITLE_FILE_NAME, /字幕/);
  assert.match(MALFORMED_SUBTITLE_TEXT, /not-a-time/);
  for (const caseId of ["external-srt-malformed-rejected", "external-srt-missing-capability"]) {
    const rejectionCase = g5SmokeCases.find((testCase) => testCase.id === caseId);
    assert.deepEqual(rejectionCase.requiredStages, ["detected", "error"]);
    assert.deepEqual(rejectionCase.forbiddenStages, ["workflow-ready", "interaction-ready", "encoding", "success"]);
  }
  const capabilityCase = g5SmokeCases.find((testCase) => testCase.id === "external-srt-missing-capability");
  assert.equal(capabilityCase.expectedErrorIncludes, "missing filter subtitles");
  const noClobberCase = g5SmokeCases.find((testCase) => testCase.id === "exact-trim-existing-output-refused");
  assert.equal(noClobberCase.preseedOutput, true);
  assert.equal(noClobberCase.trimStartS, 1);
  assert.equal(noClobberCase.trimEndS, 3);
  assert.equal(noClobberCase.expectedErrorIncludes, "already exists");
  assert.equal(noClobberCase.workflowQueueExport, undefined);
  assert.match(g5SmokeSource, /entry\.name\.startsWith\("\.vfl-"\) && entry\.name\.includes\("\.tmp\."\)/);
  assert.match(g5SmokeSource, /temporaryOutputCount: 0/);
});

test("existing-output refusal stays ahead of probing and FFmpeg execution", async () => {
  const backendSource = await fs.readFile(
    path.resolve(__dirname, "../src-tauri/src/video.rs"),
    "utf8",
  );
  const runEncodeJobStart = backendSource.indexOf("pub fn run_encode_job(");
  const outputValidation = backendSource.indexOf("let output_path = validate_output_path(", runEncodeJobStart);
  const sourceProbe = backendSource.indexOf("let probe = probe_video(", runEncodeJobStart);
  const firstFfmpegExecution = backendSource.indexOf("run_ffmpeg_with_progress(", runEncodeJobStart);

  assert.ok(runEncodeJobStart >= 0, "run_encode_job must remain present");
  assert.ok(outputValidation > runEncodeJobStart, "run_encode_job must validate the output destination");
  assert.ok(sourceProbe > outputValidation, "output validation must happen before source probing");
  assert.ok(firstFfmpegExecution > sourceProbe, "output validation must happen before FFmpeg execution");
});

test("G5 fixture commands and paths stay deterministic and use the bundled codec surface", () => {
  const root = path.resolve(os.tmpdir(), "vfl-g5-fixture-contract");
  const easy = buildG5FixtureCommand("target-easy", root);
  const hard = buildG5FixtureCommand("target-hard", root);
  const subtitle = buildG5FixtureCommand("subtitle-solid", root);
  const audioLong = buildG5FixtureCommand("audio-long", root);

  assert.equal(easy.outputPath, fixtureOutputPath("target-easy", root));
  assert.equal(hard.outputPath, fixtureOutputPath("target-hard", root));
  assert.equal(subtitle.outputPath, fixtureOutputPath("subtitle-solid", root));
  assert.equal(audioLong.outputPath, fixtureOutputPath("audio-long", root));
  assert.match(hard.args.join(" "), /testsrc2=size=640x360:rate=30/);
  assert.match(hard.args.join(" "), /-t 20/);
  assert.match(subtitle.args.join(" "), /color=c=0x606060:size=640x360:rate=30/);
  assert.match(subtitle.outputPath, /source-字幕-café\.mp4$/);
  assert.match(audioLong.args.join(" "), /-t 40/);
  assert.match(audioLong.args.join(" "), /-b:a 32k/);
  assert.equal(subtitleFixturePath("valid", root), path.resolve(root, VALID_SUBTITLE_FILE_NAME));
  assert.equal(subtitleFixturePath("malformed", root), path.resolve(root, MALFORMED_SUBTITLE_FILE_NAME));
  assert.throws(() => buildG5FixtureCommand("unknown", root), /unknown portable G5 fixture/i);
});

test("G5 smoke environment carries strict/subtitle controls without putting private data in evidence fields", () => {
  const testCase = g5SmokeCases.find((candidate) => candidate.id === "external-srt-missing-capability");
  const caseRoot = path.resolve(os.tmpdir(), "vfl-g5-env-contract");
  const subtitlePath = path.resolve(caseRoot, VALID_SUBTITLE_FILE_NAME);
  const env = buildG5SmokeEnvironment(testCase, {
    inputPath: path.resolve(caseRoot, "input.mp4"),
    outputPath: path.resolve(caseRoot, "output.mp4"),
    statusPath: path.resolve(caseRoot, "status.json"),
    subtitlePath,
    caseRoot,
    baseEnv: {
      PATH: "/fixture/bin",
      VFL_SMOKE_SUBTITLE_PATH: "/stale/private.srt",
      VFL_SMOKE_MISSING_CAPABILITY_FILTERS: "stale-filter",
    },
    platform: "linux",
  });

  assert.equal(env.VFL_SMOKE_STRICT_FIT, "0");
  assert.equal("VFL_SMOKE_STRICT_FIT_ALLOW_AUDIO_REMOVAL" in env, false);
  assert.equal(env.VFL_SMOKE_SUBTITLE_PATH, subtitlePath);
  assert.equal(env.VFL_SMOKE_MISSING_CAPABILITY_FILTERS, "subtitles");
  assert.equal(env.VFL_SMOKE_SKIP_PREVIEW_INTERACTIONS, "1");
  assert.equal(env.VFL_SMOKE_WORKFLOW_QUEUE, "0");
  assert.equal(env.VFL_SMOKE_G5_QUEUE_TARGET_MISS, "0");
  assert.equal(env.VFL_SMOKE_TRIM_START_S, "3");
  assert.equal(env.VFL_SMOKE_TRIM_END_S, "6");
  assert.equal(env.XDG_DATA_HOME, path.resolve(caseRoot, "xdg-data"));
  assert.equal(env.PATH, "/fixture/bin");
  const targetCase = g5SmokeCases.find((candidate) => candidate.id === "target-first-plan-met");
  const targetEnv = buildG5SmokeEnvironment(targetCase, {
    inputPath: path.resolve(caseRoot, "input.mp4"),
    outputPath: path.resolve(caseRoot, "output.mp4"),
    statusPath: path.resolve(caseRoot, "status.json"),
    caseRoot,
    baseEnv: {
      VFL_SMOKE_SUBTITLE_PATH: "/stale/private.srt",
      VFL_SMOKE_MISSING_CAPABILITY_FILTERS: "subtitles",
      VFL_FFMPEG_PATH: "/host/ffmpeg",
      VFL_FFPROBE_PATH: "/host/ffprobe",
    },
    platform: "linux",
  });
  assert.equal("VFL_SMOKE_SUBTITLE_PATH" in targetEnv, false);
  assert.equal("VFL_SMOKE_MISSING_CAPABILITY_FILTERS" in targetEnv, false);
  assert.equal("VFL_FFMPEG_PATH" in targetEnv, false);
  assert.equal("VFL_FFPROBE_PATH" in targetEnv, false);
  const queueCase = g5SmokeCases.find((candidate) => candidate.id === "target-missed-queue-recovery");
  const queueEnv = buildG5SmokeEnvironment(queueCase, {
    inputPath: path.resolve(caseRoot, "input.mp4"),
    outputPath: path.resolve(caseRoot, "output.mp4"),
    statusPath: path.resolve(caseRoot, "status.json"),
    caseRoot,
    baseEnv: {},
    platform: "linux",
  });
  assert.equal(queueEnv.VFL_SMOKE_WORKFLOW_QUEUE, "1");
  assert.equal(queueEnv.VFL_SMOKE_G5_QUEUE_TARGET_MISS, "1");
  const mp3Case = g5SmokeCases.find((candidate) => candidate.id === "mp3-impossible-target-missed");
  const mp3Env = buildG5SmokeEnvironment(mp3Case, {
    inputPath: path.resolve(caseRoot, "input.mp4"),
    outputPath: path.resolve(caseRoot, "output.mp3"),
    statusPath: path.resolve(caseRoot, "status.json"),
    caseRoot,
    baseEnv: {},
    platform: "linux",
  });
  assert.equal(mp3Env.VFL_SMOKE_FORMAT, "mp3");
  assert.throws(
    () => buildG5SmokeEnvironment(testCase, {
      inputPath: path.resolve(caseRoot, "input.mp4"),
      outputPath: path.resolve(caseRoot, "output.mp4"),
      statusPath: path.resolve(caseRoot, "status.json"),
      caseRoot,
    }),
    /requires a subtitlePath/i,
  );
});

test("missing capability mask is comma-separated, deduplicated, and allowlisted", () => {
  assert.equal(normalizeMissingCapabilityFilters(undefined), "");
  assert.equal(normalizeMissingCapabilityFilters(" subtitles,subtitles "), "subtitles");
  assert.equal(normalizeMissingCapabilityFilters(["subtitles"]), "subtitles");
  assert.throws(() => normalizeMissingCapabilityFilters("scale"), /non-allowlisted.*scale/i);
  assert.throws(() => normalizeMissingCapabilityFilters("subtitles,scale"), /non-allowlisted.*scale/i);
});

test("portable G5 path and launch helpers preserve native Windows/Linux runtime shape", () => {
  const linux = getPortableG5Paths("/tmp/VFL", { platform: "linux" });
  assert.equal(linux.appPath, path.resolve("/tmp/VFL/video_for_lazies"));
  assert.equal(linux.ffmpegPath, path.resolve("/tmp/VFL/ffmpeg-sidecar/ffmpeg"));
  assert.deepEqual(buildPortableG5Launch(linux.appPath, { platform: "linux", env: {} }), {
    command: "xvfb-run",
    args: ["-a", linux.appPath],
  });
  assert.deepEqual(buildPortableG5Launch(linux.appPath, { platform: "linux", env: { DISPLAY: ":99" } }), {
    command: linux.appPath,
    args: [],
  });

  const windows = getPortableG5Paths("C:\\VFL", { platform: "win32" });
  assert.match(windows.appPath, /Video_For_Lazies\.exe$/);
  assert.match(windows.ffmpegPath, /ffmpeg\.exe$/);
  assert.deepEqual(buildPortableG5Launch(windows.appPath, { platform: "win32" }), {
    command: windows.appPath,
    args: [],
  });
  assert.throws(() => getPortableG5Paths("/tmp/VFL", { platform: "darwin" }), /only Windows and Linux/i);
});

test("portable output identity accepts Windows extended paths and existing aliases", async () => {
  assert.equal(
    portablePathIdentity("\\\\?\\C:\\Users\\runneradmin\\AppData\\Local\\Temp\\clip.mp4", { platform: "win32" }),
    portablePathIdentity("C:\\Users\\runneradmin\\AppData\\Local\\Temp\\clip.mp4", { platform: "win32" }),
  );
  assert.equal(
    portablePathIdentity("\\\\?\\UNC\\server\\share\\clip.mp4", { platform: "win32" }),
    portablePathIdentity("\\\\server\\share\\clip.mp4", { platform: "win32" }),
  );

  const root = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-g5-path-alias-"));
  try {
    const original = path.resolve(root, "original.mp4");
    const alias = path.resolve(root, "alias.mp4");
    await fs.writeFile(original, "same file");
    await fs.link(original, alias);
    assert.equal(await portablePathsReferToSameFile(original, alias, { platform: process.platform }), true);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});

test("exact target evidence agrees with filesystem bytes, plan history, and queue outcome", () => {
  const metCase = g5SmokeCases.find((testCase) => testCase.id === "target-first-plan-met");
  const metPlan = fitPlan({ planNumber: 1, actualSizeBytes: 490_000, targetBytes: 500_000, selected: true });
  const metStatus = targetStatus({
    targetBytes: 500_000,
    actualBytes: 490_000,
    strictFit: false,
    plans: [metPlan],
    queueOutcomeKind: "done",
  });
  assert.deepEqual(validateTargetResultEvidence(metCase, metStatus, 490_000), {
    queueOutcomeKind: "done",
    targetResult: metStatus.targetResult,
  });
  assert.equal(targetResultQueueOutcomeKind(metStatus.targetResult), "done");

  const strictCase = g5SmokeCases.find((testCase) => testCase.id === "target-strict-bounded-correction");
  const missedPlans = [
    fitPlan({ planNumber: 1, actualSizeBytes: 160_000, targetBytes: 100_000 }),
    fitPlan({
      planNumber: 2,
      actualSizeBytes: 120_000,
      targetBytes: 100_000,
      selected: true,
      mutations: ["Video bitrate 64 to 32 kbps"],
    }),
  ];
  const missedStatus = targetStatus({
    targetBytes: 100_000,
    actualBytes: 120_000,
    strictFit: true,
    plans: missedPlans,
    queueOutcomeKind: "target-missed",
  });
  const missedEvidence = validateTargetResultEvidence(strictCase, missedStatus, 120_000);
  assert.equal(missedEvidence.queueOutcomeKind, "target-missed");
  assert.equal(targetResultQueueOutcomeKind(missedStatus.targetResult), "target-missed");

  assert.throws(
    () => validateTargetResultEvidence(strictCase, { ...missedStatus, queueOutcomeKind: "done" }, 120_000),
    /queue outcome contradicts targetResult/i,
  );
  assert.throws(
    () => validateTargetResultEvidence(strictCase, missedStatus, 120_001),
    /did not match filesystem bytes/i,
  );
  const fivePlans = Array.from({ length: 5 }, (_, index) => fitPlan({
    planNumber: index + 1,
    actualSizeBytes: 120_000 + index,
    targetBytes: 100_000,
    selected: index === 4,
    mutations: index === 0 ? [] : [`Mutation ${index}`],
  }));
  const overBudget = targetStatus({
    targetBytes: 100_000,
    actualBytes: fivePlans[4].actualSizeBytes,
    strictFit: true,
    plans: fivePlans,
    queueOutcomeKind: "target-missed",
  });
  assert.throws(() => validateTargetResultEvidence(strictCase, overBudget, fivePlans[4].actualSizeBytes), /2-4 fit plans/i);

  const multipleSelected = structuredClone(missedStatus);
  multipleSelected.targetResult.plans[0].selected = true;
  assert.throws(() => validateTargetResultEvidence(strictCase, multipleSelected, 120_000), /exactly one selected fit plan/i);
  const noSelected = structuredClone(missedStatus);
  noSelected.targetResult.plans[1].selected = false;
  assert.throws(() => validateTargetResultEvidence(strictCase, noSelected, 120_000), /exactly one selected fit plan/i);
  const wrongSelectedNumber = structuredClone(missedStatus);
  wrongSelectedNumber.targetResult.selectedPlanNumber = 1;
  assert.throws(() => validateTargetResultEvidence(strictCase, wrongSelectedNumber, 120_000), /exactly one selected fit plan/i);
  const wrongPlanStatus = structuredClone(missedStatus);
  wrongPlanStatus.targetResult.plans[0].status = "met";
  assert.throws(() => validateTargetResultEvidence(strictCase, wrongPlanStatus, 120_000), /status is not exact-byte derived/i);
  const selectedByteMismatch = structuredClone(missedStatus);
  selectedByteMismatch.targetResult.plans[1].actualSizeBytes = 119_999;
  assert.throws(() => validateTargetResultEvidence(strictCase, selectedByteMismatch, 120_000), /selected fit plan bytes did not match/i);
  const tooManyInvocations = structuredClone(missedStatus);
  tooManyInvocations.targetResult.plans[0].ffmpegInvocations = 3;
  assert.throws(() => validateTargetResultEvidence(strictCase, tooManyInvocations, 120_000), /exceeded two FFmpeg invocations/i);
  const noOpCorrection = structuredClone(missedStatus);
  noOpCorrection.targetResult.plans[1].mutations = [];
  assert.throws(
    () => validateTargetResultEvidence(strictCase, noOpCorrection, 120_000),
    /corrective fit plans without mutation|no-op corrective fit plan/i,
  );
  const unpermittedDrop = structuredClone(missedStatus);
  unpermittedDrop.targetResult.plans[1].audioAction = "drop";
  assert.throws(
    () => validateTargetResultEvidence(strictCase, unpermittedDrop, 120_000),
    /dropped audio while Include audio remained enabled/i,
  );
  const selectedLargerMiss = structuredClone(missedStatus);
  selectedLargerMiss.targetResult.plans[0].actualSizeBytes = 110_000;
  assert.throws(() => validateTargetResultEvidence(strictCase, selectedLargerMiss, 120_000), /smallest measured missed candidate/i);
  const continuedAfterFit = structuredClone(missedStatus);
  continuedAfterFit.targetResult.plans[0].actualSizeBytes = 90_000;
  continuedAfterFit.targetResult.plans[0].status = "met";
  assert.throws(() => validateTargetResultEvidence(strictCase, continuedAfterFit, 120_000), /reported a missed target despite a fitting plan/i);
  const metQueueContradiction = structuredClone(metStatus);
  metQueueContradiction.queueOutcomeKind = "target-missed";
  assert.throws(() => validateTargetResultEvidence(metCase, metQueueContradiction, 490_000), /queue outcome contradicts targetResult/i);

  const subtitleCase = g5SmokeCases.find((testCase) => testCase.id === "external-srt-unicode-source-timing");
  assert.deepEqual(validateTargetResultEvidence(subtitleCase, { targetResult: null, queueOutcomeKind: "done" }, 123_456), {
    queueOutcomeKind: "done",
    targetResult: null,
  });
  assert.throws(
    () => validateTargetResultEvidence(subtitleCase, { targetResult: null, queueOutcomeKind: "target-missed" }, 123_456),
    /queueOutcomeKind=done/i,
  );
});

test("status privacy rejects raw and JSON-escaped subtitle paths or text", () => {
  const subtitlePath = `C:\\Private\\字幕\\${VALID_SUBTITLE_FILE_NAME}`;
  const escapedSubtitlePath = JSON.stringify(subtitlePath).slice(1, -1);
  assert.doesNotThrow(() => assertPrivacySafeStatusRaw(
    JSON.stringify({ stage: "success", diagnostics: { subtitleBurnedIn: true, subtitleCueCount: 1 } }),
    [subtitlePath, VALID_SUBTITLE_FILE_NAME, "café Olá Καλημέρα Привет مرحبا"],
  ));
  assert.throws(
    () => assertPrivacySafeStatusRaw(JSON.stringify({ message: subtitlePath }), [subtitlePath]),
    /leaked private subtitle source material/i,
  );
  assert.throws(
    () => assertPrivacySafeStatusRaw(`{"message":"${escapedSubtitlePath}"}`, [subtitlePath]),
    /leaked private subtitle source material/i,
  );
  assert.throws(
    () => assertPrivacySafeStatusRaw(JSON.stringify({ message: "café Olá Καλημέρα Привет مرحبا" }), ["café Olá Καλημέρα Привет مرحبا"]),
    /leaked private subtitle source material/i,
  );
  assert.throws(
    () => assertPrivacySafeStatusRaw(JSON.stringify({ subtitlePath }), []),
    /forbidden subtitlePath field/i,
  );
});

test("subtitle timing evidence requires a high-contrast cue only in the source-timed window", () => {
  const before = grayFrameStats(Buffer.alloc(256, 96));
  const after = grayFrameStats(Buffer.alloc(256, 96));
  const visibleBytes = Buffer.alloc(256, 96);
  for (let index = 0; index < 128; index += 2) {
    visibleBytes[index] = 0;
    visibleBytes[index + 1] = 255;
  }
  const visible = grayFrameStats(visibleBytes);
  assert.doesNotThrow(() => assertSubtitleTimingEvidence({ before, visible, after }));
  assert.throws(
    () => assertSubtitleTimingEvidence({ before, visible: grayFrameStats(Buffer.alloc(256, 96)), after }),
    /lacks contrast/i,
  );
  assert.equal(before.byteCount, 256);
  assert.equal(before.range, 0);
  assert.equal(visible.range, 255);
  assert.match(visible.sha256, /^[a-f0-9]{64}$/);
});

test("Unicode subtitle font preflight uses the backend fixed style and rejects glyph/font warnings", async () => {
  const videoSource = await fs.readFile(path.resolve(__dirname, "../src-tauri/src/video.rs"), "utf8");
  const fontBytes = await fs.readFile(path.resolve(__dirname, "../src-tauri/assets/DejaVuSans.ttf"));
  assert.equal(STAGED_EXTERNAL_SRT_FILE_NAME, "vfl_external.srt");
  assert.equal(STAGED_EXTERNAL_SUBTITLE_FONT_DIR_NAME, "fonts");
  assert.equal(STAGED_EXTERNAL_SUBTITLE_FONT_FILE_NAME, "DejaVuSans.ttf");
  assert.equal(crypto.createHash("sha256").update(fontBytes).digest("hex"), SUBTITLE_FONT_SHA256);
  assert.equal(
    EXTERNAL_SRT_FILTER,
    `subtitles=filename=${STAGED_EXTERNAL_SRT_FILE_NAME}:fontsdir=${STAGED_EXTERNAL_SUBTITLE_FONT_DIR_NAME}:force_style='${EXTERNAL_SRT_FORCE_STYLE}'`,
  );
  assert.ok(
    videoSource.includes(`"${EXTERNAL_SRT_FILTER}"`),
    "packaged warning preflight must use the exact backend staged filename and fixed style",
  );
  assert.match(videoSource, /include_bytes!\("\.\.\/assets\/DejaVuSans\.ttf"\)/);
  assert.match(videoSource, /EXTERNAL_SUBTITLE_FONT_FILE_NAME/);
  assert.match(EXTERNAL_SRT_FORCE_STYLE, /FontName=DejaVu Sans/);
  assert.equal(countFontGlyphWarnings(""), 0);
  assert.equal(countFontGlyphWarnings("unrelated muxer warning\n"), 0);
  assert.equal(countFontGlyphWarnings([
    "Glyph 0x5B57 not found, selecting one more font for (DejaVu Sans, 400, 0)",
    "fontselect: failed to find any fallback with glyph 0x5B57",
    "Could not load font DejaVu Sans",
    "[Parsed_subtitles_0] Error opening memory font 'vfl_external.srt'",
  ].join("\n")), 4);
});

test("subtitle font preflight failures retain bounded path-free diagnostics", async () => {
  const root = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-g5-font-failure-"));
  try {
    const raw = [
      "café Olá Καλημέρα Привет مرحبا",
      "[Parsed_subtitles_0] Error opening memory font 'vfl_external.srt'",
    ].join("\n");
    assert.doesNotMatch(sanitizeSubtitleFontPreflightLog(raw), /Καλημέρα|Привет|مرحبا/);
    const sidecar = await retainSubtitleFontPreflightFailure(root, {
      stderr: Buffer.from(raw),
      exitCode: 1,
      signal: null,
    });
    const [stderrLog, jsonRaw] = await Promise.all([
      fs.readFile(path.resolve(root, "subtitle-font-preflight.stderr.log"), "utf8"),
      fs.readFile(path.resolve(root, "subtitle-font-preflight.failure.json"), "utf8"),
    ]);
    const json = JSON.parse(jsonRaw);
    assert.equal(sidecar.fontGlyphWarningCount, 1);
    assert.equal(json.fontGlyphWarningCount, 1);
    assert.equal(json.sha256, SUBTITLE_FONT_SHA256);
    assert.equal(json.exitCode, 1);
    assert.doesNotMatch(stderrLog, /Καλημέρα|Привет|مرحبا/);
    assert.match(stderrLog, /Error opening memory font/);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});

test("G5 corpus is wired into extracted portable verification", async () => {
  const runnerPath = path.resolve(__dirname, "../scripts/run-portable-g5-smoke.mjs");
  const releasePath = path.resolve(__dirname, "../scripts/run-release-portable.mjs");
  const [runner, release] = await Promise.all([
    fs.readFile(runnerPath, "utf8"),
    fs.readFile(releasePath, "utf8"),
  ]);

  assert.match(release, /import \{ runPortableG5Smoke \} from "\.\/run-portable-g5-smoke\.mjs"/);
  assert.match(release, /await runPortableG5Smoke\(\{ portableDir, platform \}\)/);
  assert.match(runner, /VFL_SMOKE_STRICT_FIT/);
  assert.doesNotMatch(runner, /VFL_SMOKE_STRICT_FIT_ALLOW_AUDIO_REMOVAL/);
  assert.match(runner, /VFL_SMOKE_SUBTITLE_PATH/);
  assert.match(runner, /VFL_SMOKE_MISSING_CAPABILITY_FILTERS/);
  assert.match(runner, /queueOutcomeKind/);
  assert.match(runner, /targetResult/);
  assert.match(runner, /subtitleBurnedIn/);
  assert.match(runner, /subtitleCueCount/);
  assert.match(runner, /subtitle-before\.png/);
  assert.match(runner, /subtitle-visible\.png/);
  assert.match(runner, /subtitle-after\.png/);
  assert.match(runner, /subtitleFontPreflight/);
  assert.match(runner, /fontGlyphWarningCount/);
  assert.match(runner, /Portable G5 smoke preserved failure evidence at/);
  assert.match(runner, /VFL_G5_SMOKE_KEEP_ARTIFACTS/);
});

test("G5 case selection rejects unknown and duplicate corpus entries", () => {
  assert.equal(resolveG5SmokeCases().length, g5SmokeCases.length);
  assert.deepEqual(resolveG5SmokeCases(["target-first-plan-met"]).map((testCase) => testCase.id), ["target-first-plan-met"]);
  assert.throws(() => resolveG5SmokeCases([]), /non-empty array/i);
  assert.throws(() => resolveG5SmokeCases(["missing"]), /unknown portable G5 smoke case/i);
  assert.throws(
    () => resolveG5SmokeCases(["target-first-plan-met", "target-first-plan-met"]),
    /duplicated/i,
  );
});

test("stage evidence distinguishes full success from intentional early rejection", () => {
  const successCase = g5SmokeCases.find((testCase) => testCase.id === "target-first-plan-met");
  assert.doesNotThrow(() => assertStageHistory(successCase, {
    stageHistory: [...REQUIRED_G5_SMOKE_STAGES, "success"],
  }));
  assert.throws(() => assertStageHistory(successCase, {
    stageHistory: ["detected", "input-applied", "probe-ready", "interaction-ready", "workflow-ready", "encoding", "success"],
  }), /out of order/i);

  const malformedCase = g5SmokeCases.find((testCase) => testCase.id === "external-srt-malformed-rejected");
  assert.doesNotThrow(() => assertStageHistory(malformedCase, {
    stageHistory: ["detected", "input-applied", "error"],
  }));
  assert.throws(() => assertStageHistory(malformedCase, {
    stageHistory: ["detected", "input-applied", "encoding", "error"],
  }), /forbidden post-rejection stages encoding/i);
});
