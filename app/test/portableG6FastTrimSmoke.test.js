import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  G6_EXPECTED_VIDEO_PACKET_COUNT,
  G6_AUDIO_TIMESTAMP_TOLERANCE_US,
  G6_PRIVATE_METADATA_SENTINELS,
  G6_PRIVATE_STREAM_METADATA_SENTINELS,
  G6_REPLACEMENT_TITLE,
  REQUIRED_FAST_TRIM_RESET_STAGES,
  WINDOWS_KEYBOARD_STAGES,
  assertBlockedInspection,
  assertCasePathPrivacy,
  assertMetadataPolicy,
  assertPreExecutionFailureDiagnostics,
  assertReadyInspection,
  assertStageHistory,
  assertTextExcludesSensitiveValues,
  assertTextExcludesPathValues,
  assertTrimResultContract,
  audibleAudioBounds,
  buildG6FixtureCommands,
  buildG6SmokeEnvironment,
  expectedClosedGopPacketSlice,
  findUniqueContiguousPacketSubsequence,
  fixtureBuildOrder,
  fastTrimResetEvidence,
  frameMeanAbsoluteError,
  framePresentationBounds,
  g6SmokeCases,
  hasLeadingPicturesAfterKey,
  normalizeRational,
  parseDecimalSecondsToUs,
  proveMatchedAudioSourceInterval,
  proveCopiedAudioPacketMapping,
  requiredErrorStageSequence,
  requiredSuccessStageSequence,
  resolveG6SmokeCases,
  splitRawRgbFrames,
  usToSecondsString,
} from "../scripts/run-portable-g6-fast-trim-smoke.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

function caseById(id) {
  const testCase = g6SmokeCases.find((candidate) => candidate.id === id);
  assert.ok(testCase, `missing G6 case ${id}`);
  return testCase;
}

function packet(dataHash, ptsUs, {
  dtsUs = ptsUs,
  durationUs = 33_333,
  flags = "__",
  sideDataList = [],
} = {}) {
  return { dataHash, ptsUs, dtsUs, durationUs, flags, sideDataList };
}

function readyInspection(testCase) {
  const requestedStartUs = Math.round(testCase.trimStartS * 1_000_000);
  const requestedEndUs = Math.round(testCase.trimEndS * 1_000_000);
  return {
    status: "ready",
    reasons: [],
    requestedStartUs,
    requestedEndUs,
    effectiveStartUs: testCase.effectiveStartUs,
    effectiveEndUs: testCase.effectiveEndUs,
    startExpansionUs: requestedStartUs - testCase.effectiveStartUs,
    endExpansionUs: testCase.effectiveEndUs - requestedEndUs,
    requiresAcceptance: testCase.requiresAcceptance,
    videoPacketCount: testCase.expectedVideoPacketCount,
    videoAction: "copy",
    audioAction: testCase.expectedAudioAction ?? "copy",
    consent: {
      planSchema: 1,
      confirmationToken: "path-free-token",
      requestedStartUs,
      requestedEndUs,
      effectiveStartUs: testCase.effectiveStartUs,
      effectiveEndUs: testCase.effectiveEndUs,
      videoPacketCount: testCase.expectedVideoPacketCount,
    },
  };
}

function successfulTrimPayload(testCase) {
  const inspection = readyInspection(testCase);
  const audioAction = testCase.expectedAudioAction ?? "copy";
  const trimResult = {
    mode: "fastCopy",
    requestedStartUs: inspection.requestedStartUs,
    requestedEndUs: inspection.requestedEndUs,
    effectiveStartUs: inspection.effectiveStartUs,
    effectiveEndUs: inspection.effectiveEndUs,
    actualStartUs: inspection.effectiveStartUs,
    actualEndUs: inspection.effectiveEndUs,
    videoPacketCount: inspection.videoPacketCount,
    videoAction: "copy",
    audioAction,
    ffmpegInvocations: 1,
    commandPreview: audioAction === "copy"
      ? `ffmpeg -seek_timestamp 1 -ss 2.000000 -i <input> -c:v copy -frames:v ${inspection.videoPacketCount} -c:a copy -shortest <output>`
      : `ffmpeg -seek_timestamp 1 -ss 2.000000 -i <input> -c:v copy -frames:v ${inspection.videoPacketCount} -an <output>`,
  };
  const diagnostics = {
    videoAction: "copy",
    audioAction,
    trimMode: "fastCopy",
    trimRequestedStartUs: inspection.requestedStartUs,
    trimRequestedEndUs: inspection.requestedEndUs,
    trimEffectiveStartUs: inspection.effectiveStartUs,
    trimEffectiveEndUs: inspection.effectiveEndUs,
    trimActualStartUs: inspection.effectiveStartUs,
    trimActualEndUs: inspection.effectiveEndUs,
    trimVideoPacketCount: inspection.videoPacketCount,
    trimFfmpegInvocations: 1,
    attempts: 1,
    copyFallbackReason: null,
  };
  return { inspection, trimResult, diagnostics };
}

test("G6 packaged corpus covers success, refusal, no-clobber, workflow, privacy, and literal Windows keys", () => {
  assert.deepEqual(g6SmokeCases.map((testCase) => testCase.id), [
    "h264-aligned-copy-workflow",
    "h264-between-keyframes",
    "vp9-opus-nonzero-start-between-keyframes",
    "h264-audio-drop-metadata-preserved",
    "incompatible-retained-audio-refused",
    "transform-refused",
    "open-gop-refused",
    "source-mutated-after-acceptance-refused",
    "preseeded-output-no-clobber",
  ]);
  const successes = g6SmokeCases.filter((testCase) => testCase.terminalStage === "success");
  assert.equal(successes.length, 4);
  assert.ok(successes.every((testCase) => Number.isSafeInteger(testCase.effectiveStartUs)));
  assert.ok(successes.every((testCase) => Number.isSafeInteger(testCase.effectiveEndUs)));
  assert.ok(successes.every((testCase) => testCase.expectedVideoPacketCount === 120));
  assert.equal(caseById("h264-aligned-copy-workflow").workflowQueueExport, true);
  assert.deepEqual(caseById("h264-aligned-copy-workflow").requiredWorkflowStages, [
    "workflow-recipe-ready",
    "workflow-recipe-saved",
    "workflow-queue-ready",
    "workflow-queue-complete",
    "workflow-ready",
  ]);
  assert.equal(caseById("h264-between-keyframes").windowsLiteralKeyboard, true);
  assert.deepEqual(caseById("incompatible-retained-audio-refused").expectedReasonCodes, ["audioCodecIncompatible"]);
  assert.deepEqual(caseById("transform-refused").expectedReasonCodes, ["resizeEnabled"]);
  assert.deepEqual(caseById("open-gop-refused").expectedReasonCodes, ["openGop"]);
  assert.equal(caseById("h264-audio-drop-metadata-preserved").expectedAudioAction, "drop");
  assert.equal(caseById("h264-audio-drop-metadata-preserved").stripMetadata, false);
  assert.equal(caseById("source-mutated-after-acceptance-refused").sourceMutation, true);
  assert.equal(caseById("preseeded-output-no-clobber").preseedOutput, true);
});

test("G6 case selection is bounded and rejects unknown or duplicate cases", () => {
  assert.equal(resolveG6SmokeCases().length, g6SmokeCases.length);
  assert.deepEqual(resolveG6SmokeCases(["h264-between-keyframes"]).map(({ id }) => id), ["h264-between-keyframes"]);
  assert.throws(() => resolveG6SmokeCases([]), /non-empty array/i);
  assert.throws(() => resolveG6SmokeCases(["unknown"]), /unknown portable G6 smoke case/i);
  assert.throws(
    () => resolveG6SmokeCases(["h264-between-keyframes", "h264-between-keyframes"]),
    /duplicated/i,
  );
});

test("G6 fixture commands pin two-second closed GOPs, B-frames, timestamp origin, codecs, and private metadata", () => {
  const root = path.resolve(os.tmpdir(), "vfl-g6-fixture-contract");
  const closed = buildG6FixtureCommands("h264-closed", root)[0].args.join(" ");
  assert.match(closed, /-g 60/);
  assert.match(closed, /-bf 2/);
  assert.match(closed, /open-gop=0/);
  assert.match(closed, /-force_key_frames expr:gte\(t,n_forced\*2\)/);
  assert.match(closed, /-c:v libx264/);
  assert.match(closed, /-c:a aac/);
  assert.match(closed, /-metadata:s:v:0 handler_name=PRIVATE-G6-VIDEO-HANDLER/);
  assert.match(closed, /-metadata:s:a:0 handler_name=PRIVATE-G6-AUDIO-HANDLER/);
  assert.match(closed, /-disposition:a:0 default\+comment\+forced\+hearing_impaired/);
  for (const sentinel of G6_PRIVATE_METADATA_SENTINELS) assert.match(closed, new RegExp(sentinel.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));

  const vp9 = buildG6FixtureCommands("vp9-opus-nonzero-start", root)[0].args.join(" ");
  assert.match(vp9, /-c:v libvpx-vp9/);
  assert.match(vp9, /-c:a libopus/);
  assert.match(vp9, /-output_ts_offset 5/);
  assert.match(vp9, /-disposition:a:0 default\+forced/);
  assert.doesNotMatch(vp9, /-disposition:a:0 [^\n]*comment/);

  const open = buildG6FixtureCommands("h264-open-gop", root)[0].args.join(" ");
  assert.match(open, /open-gop=1/);
  assert.match(open, /bframes=3/);
  assert.match(open, /b-pyramid=2/);

  const incompatible = buildG6FixtureCommands("h264-opus-incompatible", root)[0];
  assert.equal(incompatible.requiresFixtureId, "h264-closed");
  assert.match(incompatible.args.join(" "), /-c:v copy -c:a libopus/);
  assert.deepEqual(fixtureBuildOrder([caseById("incompatible-retained-audio-refused")]), [
    "h264-closed",
    "h264-opus-incompatible",
  ]);
});

test("G6 smoke environment strips caller overrides and activates literal keyboard only on Windows", () => {
  const root = path.resolve(os.tmpdir(), "vfl-g6-env");
  const between = caseById("h264-between-keyframes");
  const common = {
    inputPath: path.resolve(root, "input.mp4"),
    outputPath: path.resolve(root, "output.mp4"),
    statusPath: path.resolve(root, "status.json"),
    caseRoot: root,
    baseEnv: {
      PATH: "/bin",
      VFL_SMOKE_INPUT: "attacker-input",
      VFL_SMOKE_FAST_TRIM: "0",
      VFL_FFMPEG_PATH: "attacker-ffmpeg",
      VFL_FFPROBE_PATH: "attacker-ffprobe",
    },
  };
  const linux = buildG6SmokeEnvironment(between, { ...common, platform: "linux" });
  assert.equal(linux.PATH, "/bin");
  assert.equal(linux.VFL_SMOKE_FAST_TRIM, "1");
  assert.equal(linux.VFL_SMOKE_TRIM_START_S, "2.35");
  assert.equal(linux.VFL_SMOKE_TRIM_END_S, "5.65");
  assert.equal(linux.VFL_SMOKE_TITLE, G6_REPLACEMENT_TITLE);
  assert.equal(linux.VFL_SMOKE_AUDIO_ENABLED, "1");
  assert.equal(linux.VFL_SMOKE_STRIP_METADATA, "1");
  assert.equal(linux.VFL_SMOKE_SOURCE_MUTATION, "0");
  assert.equal(linux.VFL_SMOKE_SKIP_PREVIEW_INTERACTIONS, "1");
  assert.equal(linux.VFL_FFMPEG_PATH, undefined);
  assert.equal(linux.VFL_FFPROBE_PATH, undefined);
  assert.ok(linux.XDG_DATA_HOME.startsWith(root));

  const windows = buildG6SmokeEnvironment(between, { ...common, platform: "win32" });
  assert.equal(windows.VFL_SMOKE_SKIP_PREVIEW_INTERACTIONS, "0");
  assert.ok(windows.WEBVIEW2_USER_DATA_FOLDER.startsWith(root));
  assert.deepEqual(WINDOWS_KEYBOARD_STAGES["keyboard-fast-trim-ready"], ["RIGHT"]);
  assert.deepEqual(WINDOWS_KEYBOARD_STAGES["keyboard-fast-trim-accept-ready"], ["SPACE"]);

  const audioDrop = buildG6SmokeEnvironment(caseById("h264-audio-drop-metadata-preserved"), {
    ...common,
    platform: "linux",
  });
  assert.equal(audioDrop.VFL_SMOKE_AUDIO_ENABLED, "0");
  assert.equal(audioDrop.VFL_SMOKE_STRIP_METADATA, "0");
  const mutation = buildG6SmokeEnvironment(caseById("source-mutated-after-acceptance-refused"), {
    ...common,
    platform: "linux",
  });
  assert.equal(mutation.VFL_SMOKE_SOURCE_MUTATION, "1");
});

test("G6 decimal timestamps stay exact integer microseconds at packet boundaries", () => {
  assert.equal(parseDecimalSecondsToUs("2.35"), 2_350_000);
  assert.equal(parseDecimalSecondsToUs("5.650000"), 5_650_000);
  assert.equal(parseDecimalSecondsToUs("5.0000005"), 5_000_001);
  assert.equal(parseDecimalSecondsToUs("-0.0000005"), -1);
  assert.equal(usToSecondsString(2_350_000), "2.35");
  assert.equal(usToSecondsString(-1), "-0.000001");
  assert.throws(() => parseDecimalSecondsToUs("Infinity"), /not a decimal timestamp/i);
});

test("G6 packet evidence proves the exact closed-GOP prefix and excludes the end key", () => {
  const input = [
    packet("zero", 0, { flags: "K_" }),
    packet("start", 2_000_000, { dtsUs: 1_933_333, flags: "K_" }),
    packet("b1", 2_066_667, { dtsUs: 1_966_667 }),
    packet("b2", 2_033_333, { dtsUs: 2_000_000 }),
    packet("middle", 4_000_000, { flags: "K_" }),
    packet("middle-b", 4_033_333),
    packet("end-key", 6_000_000, { flags: "K_" }),
  ];
  const selected = expectedClosedGopPacketSlice(input, 2_000_000, 6_000_000);
  assert.deepEqual(selected.packets.map(({ dataHash }) => dataHash), ["start", "b1", "b2", "middle", "middle-b"]);
  assert.equal(selected.endKey.dataHash, "end-key");
  assert.deepEqual(findUniqueContiguousPacketSubsequence(input, selected.packets, "video"), {
    inputStartIndex: 1,
    packetCount: 5,
    sha256: findUniqueContiguousPacketSubsequence(input, selected.packets, "video").sha256,
  });
  assert.throws(
    () => findUniqueContiguousPacketSubsequence(
      [packet("repeat", 0), packet("repeat", 1)],
      [packet("repeat", 0)],
      "ambiguous",
    ),
    /matched 2 contiguous input locations/i,
  );
});

test("G6 open-GOP detector catches leading presentation pictures after a decode-order key", () => {
  const open = [
    packet("key", 2_000_000, { dtsUs: 1_866_667, flags: "K_" }),
    packet("leading-a", 1_933_333, { dtsUs: 1_900_000 }),
    packet("leading-b", 1_966_667, { dtsUs: 1_933_333 }),
    packet("next-key", 4_000_000, { flags: "K_" }),
  ];
  assert.equal(hasLeadingPicturesAfterKey(open, 2_000_000), true);
  assert.equal(hasLeadingPicturesAfterKey([
    packet("key", 2_000_000, { flags: "K_" }),
    packet("following", 2_033_333),
    packet("next-key", 4_000_000, { flags: "K_" }),
  ], 2_000_000), false);
});

test("G6 presentation helpers account for audio skip/discard and compare decoded edge frames", () => {
  const audio = [
    packet("a", -21_333, {
      durationUs: 21_333,
      sideDataList: [{ skip_samples: 1024 }],
    }),
    packet("b", 0, { durationUs: 21_333 }),
    packet("c", 21_333, {
      durationUs: 21_333,
      sideDataList: [{ discard_padding: 512 }],
    }),
  ];
  assert.deepEqual(audibleAudioBounds(audio, 48_000), { startUs: 0, endUs: 31_999 });
  assert.deepEqual(proveMatchedAudioSourceInterval(audio, {
    inputStartIndex: 1,
    packetCount: 2,
  }, 48_000, {
    expectedStartUs: 0,
    expectedEndUs: 42_666,
    maxEdgeSkewUs: 11_000,
    label: "unit audio",
  }), {
    startUs: 0,
    endUs: 31_999,
    expectedStartUs: 0,
    expectedEndUs: 42_666,
    startSkewUs: 0,
    endSkewUs: -10_667,
  });
  assert.throws(() => proveMatchedAudioSourceInterval(audio, {
    inputStartIndex: 1,
    packetCount: 2,
  }, 48_000, {
    expectedStartUs: 100_000,
    expectedEndUs: 200_000,
    maxEdgeSkewUs: 10_000,
    label: "wrong audio",
  }), /do not map to the effective interval/i);
  const copiedAudio = audio.slice(1).map((source) => ({ ...source }));
  assert.deepEqual(proveCopiedAudioPacketMapping(audio, copiedAudio, {
    inputStartIndex: 1,
    packetCount: 2,
  }, 0, { label: "unit copied audio" }), {
    timingToleranceUs: G6_AUDIO_TIMESTAMP_TOLERANCE_US,
    maxPtsDeltaUs: 0,
    maxDtsDeltaUs: 0,
    maxDurationDeltaUs: 0,
  });
  const shiftedAudio = copiedAudio.map((output, index) => (
    index === 0 ? { ...output, ptsUs: output.ptsUs + G6_AUDIO_TIMESTAMP_TOLERANCE_US + 1 } : output
  ));
  assert.throws(() => proveCopiedAudioPacketMapping(audio, shiftedAudio, {
    inputStartIndex: 1,
    packetCount: 2,
  }, 0, { label: "shifted audio" }), /source-minus-seek within 2000 us/i);
  assert.deepEqual(framePresentationBounds([
    { ptsUs: 0, durationUs: 33_333 },
    { ptsUs: 33_333, durationUs: 33_334 },
  ]), { startUs: 0, endUs: 66_667 });

  const comparison = frameMeanAbsoluteError(Buffer.from([0, 10, 20]), Buffer.from([0, 11, 18]));
  assert.equal(comparison.meanAbsoluteError, 1);
  assert.equal(comparison.maximumError, 2);
  const split = splitRawRgbFrames(Buffer.from([0, 1, 2, 3, 4, 5]), 1, 1);
  assert.equal(split.frameCount, 2);
  assert.deepEqual([...split.first], [0, 1, 2]);
  assert.deepEqual([...split.last], [3, 4, 5]);
  assert.equal(normalizeRational("2/2"), "1:1");
});

test("G6 ready inspection and result assertions enforce acceptance, one copy invocation, and diagnostics", () => {
  const testCase = caseById("h264-between-keyframes");
  const { inspection, trimResult, diagnostics } = successfulTrimPayload(testCase);
  assert.equal(assertReadyInspection(testCase, inspection), inspection);
  assert.equal(assertTrimResultContract(testCase, trimResult, diagnostics), trimResult);
  assert.throws(
    () => assertReadyInspection(testCase, { ...inspection, effectiveStartUs: 2_350_000 }),
    /effectiveStartUs mismatch/i,
  );
  assert.throws(
    () => assertTrimResultContract(testCase, { ...trimResult, ffmpegInvocations: 2 }, diagnostics),
    /ffmpegInvocations mismatch/i,
  );
  assert.throws(
    () => assertTrimResultContract(testCase, { ...trimResult, commandPreview: `${trimResult.commandPreview} -vf scale=160:90` }, diagnostics),
    /forbidden encode\/filter token/i,
  );

  const audioDropCase = caseById("h264-audio-drop-metadata-preserved");
  const audioDrop = successfulTrimPayload(audioDropCase);
  assert.equal(assertReadyInspection(audioDropCase, audioDrop.inspection), audioDrop.inspection);
  assert.equal(
    assertTrimResultContract(audioDropCase, audioDrop.trimResult, audioDrop.diagnostics),
    audioDrop.trimResult,
  );
  assert.match(audioDrop.trimResult.commandPreview, /-an/);
  assert.doesNotMatch(audioDrop.trimResult.commandPreview, /-c:a|-shortest/);
});

test("G6 refusal assertions reject executable authority and require stable reason codes", () => {
  const testCase = caseById("open-gop-refused");
  const blocked = {
    status: "blocked",
    reasons: [{ code: "openGop", message: "The containing interval crosses an open GOP." }],
    requestedStartUs: 2_350_000,
    requestedEndUs: 5_650_000,
    effectiveStartUs: null,
    effectiveEndUs: null,
    startExpansionUs: null,
    endExpansionUs: null,
    requiresAcceptance: false,
    videoPacketCount: null,
    videoAction: null,
    audioAction: null,
    consent: null,
  };
  assert.deepEqual(assertBlockedInspection(testCase, blocked), ["openGop"]);
  assert.throws(
    () => assertBlockedInspection(testCase, { ...blocked, consent: { confirmationToken: "stale" } }),
    /retained executable copy authority/i,
  );
  assert.throws(
    () => assertBlockedInspection(testCase, { ...blocked, reasons: [{ code: "edgeExpansionExceeded" }] }),
    /missed reason code openGop/i,
  );
});

test("G6 pre-execution diagnostics reject optional, attempted, or executable no-clobber evidence", () => {
  const testCase = caseById("preseeded-output-no-clobber");
  const diagnostics = {
    mode: "failed",
    videoAction: null,
    audioAction: null,
    attempts: 0,
    passes: 0,
    trimFfmpegInvocations: null,
    failureStage: "backend",
    failureReason: "Output file already exists. Choose a new filename.",
    commandPreview: "No FFmpeg command evidence was retained because the export failed before completion.",
  };
  assert.equal(assertPreExecutionFailureDiagnostics(testCase, diagnostics, {
    failureStage: "backend",
    reasonIncludes: "already exists",
  }), diagnostics);
  assert.throws(() => assertPreExecutionFailureDiagnostics(testCase, null, {
    failureStage: "backend",
  }), /omitted backend diagnostics/i);
  assert.throws(() => assertPreExecutionFailureDiagnostics(testCase, {
    ...diagnostics,
    attempts: 1,
  }, { failureStage: "backend" }), /zero execution attempts/i);
  assert.throws(() => assertPreExecutionFailureDiagnostics(testCase, {
    ...diagnostics,
    videoAction: "copy",
  }, { failureStage: "backend" }), /zero execution attempts/i);
  assert.throws(() => assertPreExecutionFailureDiagnostics(testCase, {
    ...diagnostics,
    commandPreview: "ffmpeg -i <input> -c:v copy <output>",
  }, { failureStage: "backend" }), /executable command evidence/i);
  assert.throws(() => assertPreExecutionFailureDiagnostics(testCase, {
    ...diagnostics,
    failureStage: "fast-trim-consent",
  }, { failureStage: "backend" }), /failureStage mismatch/i);
});

test("G6 status history enforces automatic inspection before workflow and terminal result", () => {
  const successCase = caseById("h264-aligned-copy-workflow");
  const successStages = [
    "detected",
    "input-applied",
    "probe-ready",
    "fast-trim-ready",
    "fast-trim-reset-trim-complete",
    "fast-trim-reset-all-complete",
    "workflow-recipe-ready",
    "workflow-recipe-saved",
    "workflow-queue-ready",
    "workflow-queue-complete",
    "workflow-ready",
    "interaction-ready",
    "encoding",
    "success",
  ];
  assert.deepEqual(requiredSuccessStageSequence(successCase), successStages);
  assert.doesNotThrow(() => assertStageHistory(successCase, { stageHistory: successStages }));
  assert.throws(() => assertStageHistory(successCase, {
    stageHistory: [
      "detected",
      "input-applied",
      "probe-ready",
      "workflow-ready",
      "fast-trim-ready",
      "fast-trim-reset-trim-complete",
      "fast-trim-reset-all-complete",
      "interaction-ready",
      "encoding",
      "success",
    ],
  }), /out of order/i);
  assert.throws(() => assertStageHistory(successCase, {
    stageHistory: [...successStages.slice(0, 4), "probe-ready", ...successStages.slice(4)],
  }), /duplicated/i);
  assert.deepEqual(REQUIRED_FAST_TRIM_RESET_STAGES, [
    "fast-trim-reset-trim-complete",
    "fast-trim-reset-all-complete",
  ]);
  assert.deepEqual(fastTrimResetEvidence({
    stageHistory: [
      "fast-trim-ready",
      "fast-trim-reset-trim-complete",
      "fast-trim-reset-all-complete",
      "workflow-ready",
    ],
  }), {
    resetTrimExactAndCleared: true,
    resetAllExactAndCleared: true,
  });
  assert.throws(() => fastTrimResetEvidence({
    stageHistory: ["fast-trim-reset-all-complete", "fast-trim-reset-trim-complete"],
  }), /ordered Reset Trim and Reset All/i);
  const refusal = caseById("transform-refused");
  assert.deepEqual(requiredErrorStageSequence(refusal), ["detected", "input-applied", "probe-ready", "error"]);
  assert.doesNotThrow(() => assertStageHistory(refusal, {
    stageHistory: ["detected", "input-applied", "probe-ready", "error"],
  }));
  assert.throws(() => assertStageHistory(refusal, {
    stageHistory: ["detected", "probe-ready", "input-applied", "error"],
  }), /out of order/i);
  assert.throws(() => assertStageHistory(refusal, {
    stageHistory: ["detected", "input-applied", "error", "probe-ready"],
  }), /out of order/i);
  assert.throws(() => assertStageHistory(refusal, {
    stageHistory: ["detected", "input-applied", "probe-ready", "success", "error"],
  }), /reached success/i);

  const noClobberCase = caseById("preseeded-output-no-clobber");
  const noClobberStages = [
    "detected",
    "input-applied",
    "probe-ready",
    "fast-trim-ready",
    "fast-trim-reset-trim-complete",
    "fast-trim-reset-all-complete",
    "workflow-ready",
    "interaction-ready",
    "encoding",
    "error",
  ];
  assert.deepEqual(requiredErrorStageSequence(noClobberCase), noClobberStages);
  assert.doesNotThrow(() => assertStageHistory(noClobberCase, { stageHistory: noClobberStages }));
  assert.throws(() => assertStageHistory(noClobberCase, {
    stageHistory: noClobberStages.filter((stage) => stage !== "fast-trim-reset-all-complete"),
  }), /missing/i);
  assert.throws(() => assertStageHistory(noClobberCase, {
    stageHistory: [
      ...noClobberStages.slice(0, 4),
      "fast-trim-reset-all-complete",
      "fast-trim-reset-trim-complete",
      ...noClobberStages.slice(6),
    ],
  }), /out of order/i);

  const windowsCase = caseById("h264-between-keyframes");
  const windowsStages = requiredSuccessStageSequence(windowsCase, { platform: "win32" });
  const workflowIndex = windowsStages.indexOf("workflow-ready");
  const previewIndex = windowsStages.indexOf("preview-ready");
  assert.ok(workflowIndex >= 0 && previewIndex === workflowIndex + 1);
  const crossedWindowsStages = [...windowsStages];
  [crossedWindowsStages[workflowIndex], crossedWindowsStages[previewIndex]] = [
    crossedWindowsStages[previewIndex],
    crossedWindowsStages[workflowIndex],
  ];
  assert.throws(() => assertStageHistory(windowsCase, {
    stageHistory: crossedWindowsStages,
  }, { platform: "win32" }), /out of order/i);

  const mutationCase = caseById("source-mutated-after-acceptance-refused");
  assert.doesNotThrow(() => assertStageHistory(mutationCase, {
    stageHistory: [
      "detected",
      "input-applied",
      "probe-ready",
      "fast-trim-ready",
      "fast-trim-reset-trim-complete",
      "fast-trim-reset-all-complete",
      "workflow-ready",
      "interaction-ready",
      "fast-trim-source-mutation-ready",
      "fast-trim-source-mutation-complete",
      "encoding",
      "error",
    ],
  }));
});

test("G6 metadata and retained evidence keep source-private values out while preserving explicit title", () => {
  const inputProbe = {
    format: {
      tags: {
        title: G6_PRIVATE_METADATA_SENTINELS[0],
        location: G6_PRIVATE_METADATA_SENTINELS[1],
        comment: G6_PRIVATE_METADATA_SENTINELS[2],
      },
    },
    streams: [
      { codec_type: "video", tags: { handler_name: G6_PRIVATE_STREAM_METADATA_SENTINELS[0] } },
      { codec_type: "audio", tags: { handler_name: G6_PRIVATE_STREAM_METADATA_SENTINELS[1] } },
    ],
  };
  const strippedOutputProbe = {
    format: { tags: { title: G6_REPLACEMENT_TITLE } },
    streams: [
      { codec_type: "video", tags: { handler_name: "VideoHandler" } },
      { codec_type: "audio", tags: { handler_name: "SoundHandler" } },
    ],
  };
  assert.deepEqual(assertMetadataPolicy(caseById("h264-between-keyframes"), inputProbe, strippedOutputProbe), {
    sourceSentinelsPresent: true,
    stripMetadata: true,
    sourceSentinelsStripped: true,
    preservedGlobalMetadata: false,
    preservedSelectedVideoMetadata: false,
    replacementTitlePresent: true,
  });
  const preservedOutputProbe = {
    format: {
      tags: {
        title: G6_REPLACEMENT_TITLE,
        artist: G6_PRIVATE_METADATA_SENTINELS[1],
        comment: G6_PRIVATE_METADATA_SENTINELS[2],
      },
    },
    streams: [
      { codec_type: "video", tags: { handler_name: G6_PRIVATE_STREAM_METADATA_SENTINELS[0] } },
    ],
  };
  assert.deepEqual(assertMetadataPolicy(
    caseById("h264-audio-drop-metadata-preserved"),
    inputProbe,
    preservedOutputProbe,
  ), {
    sourceSentinelsPresent: true,
    stripMetadata: false,
    sourceSentinelsStripped: false,
    preservedGlobalMetadata: true,
    preservedSelectedVideoMetadata: true,
    replacementTitlePresent: true,
  });
  assert.doesNotThrow(() => assertTextExcludesSensitiveValues("safe evidence", G6_PRIVATE_METADATA_SENTINELS, "evidence"));
  assert.throws(
    () => assertTextExcludesSensitiveValues(`leak ${G6_PRIVATE_METADATA_SENTINELS[0]}`, G6_PRIVATE_METADATA_SENTINELS, "evidence"),
    /leaked a private G6 source metadata value/i,
  );
  const protectedPaths = ["/tmp/private/input.mp4", "input.mp4", "case-01", ".vfl-"];
  assert.doesNotThrow(() => assertTextExcludesPathValues("safe command <input>", protectedPaths, "preview"));
  assert.throws(
    () => assertTextExcludesPathValues("opened input.mp4", protectedPaths, "preview"),
    /protected G6 filesystem path or basename/i,
  );
  assert.throws(
    () => assertTextExcludesPathValues("temporary .vfl-123-fast-trim.tmp.mp4", protectedPaths, "diagnostics"),
    /protected G6 filesystem path or basename/i,
  );
  assert.doesNotThrow(() => assertCasePathPrivacy(
    caseById("h264-between-keyframes"),
    {
      message: "Safe terminal message",
      outputPath: "/tmp/private/input.mp4",
      diagnostics: { commandPreview: "ffmpeg <input> <output>" },
      trimResult: null,
      fastTrimInspection: null,
    },
    "safe stdout",
    "safe stderr",
    protectedPaths,
  ));

  const windowsProtected = String.raw`C:\Users\VeryLongOperator\AppData\Local\Temp\vfl-portable-g6-AbCd12\results\case-01\case-output.mp4`;
  for (const leak of [
    "opened c:/users/verylongoperator/appdata/local/temp/VFL-PORTABLE-G6-ABCD12/results/case-01/CASE-OUTPUT.MP4",
    String.raw`opened \\?\C:\Users\VeryLongOperator\AppData\Local\Temp\vfl-portable-g6-AbCd12\results\case-01\case-output.mp4`,
    JSON.stringify({ output: String.raw`C:\Users\VeryLongOperator\AppData\Local\Temp\vfl-portable-g6-AbCd12\results\case-01\case-output.mp4` }),
    String.raw`opened C:\Users\VERYLO~1\AppData\Local\Temp\VFL-PO~1\results\case-01\case-output.mp4`,
    String.raw`opened C:\USERS~1\VERYLO~1\APPDAT~1\LOCAL~1\TEMP~1\VFL-PO~1\RESULT~1\CASE-0~1\CASE-O~1.MP4`,
    String.raw`error C:\USERS~1\VERYLO~1\APPDAT~1\LOCAL~1\TEMP~1\VFL-PO~1\RESULT~1\CASE-0~1\CASE-O~1.MP4: Access denied`,
  ]) {
    assert.throws(
      () => assertTextExcludesPathValues(leak, [windowsProtected], "windows evidence", { platform: "win32" }),
      /protected G6 filesystem path or basename/i,
    );
  }
  assert.throws(() => assertTextExcludesPathValues(
    String.raw`opened \\?\UNC\Server01\PrivateShare\results\case-01\status.json`,
    [String.raw`\\server01\privateshare\results\case-01\status.json`],
    "windows UNC evidence",
    { platform: "win32" },
  ), /protected G6 filesystem path or basename/i);
  assert.doesNotThrow(() => assertTextExcludesPathValues(
    String.raw`unrelated C:\Windows\Temp\browser-cache.log`,
    [windowsProtected],
    "unrelated windows path",
    { platform: "win32" },
  ));
  assert.doesNotThrow(() => assertTextExcludesPathValues(
    "opened /TMP/PRIVATE/INPUT.MP4",
    ["/tmp/private/input.mp4"],
    "linux evidence",
    { platform: "linux" },
  ));
});

test("G6 runner is wired after G5 in release verification with standalone preflight and bounded SendKeys", async () => {
  const [runner, release, packageRaw, powershell] = await Promise.all([
    fs.readFile(path.resolve(__dirname, "../scripts/run-portable-g6-fast-trim-smoke.mjs"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../scripts/run-release-portable.mjs"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../package.json"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../scripts/windows-g6-send-keys.ps1"), "utf8"),
  ]);
  const packageJson = JSON.parse(packageRaw);
  assert.equal(packageJson.scripts["smoke:portable:g6"], "node scripts/run-portable-g6-fast-trim-smoke.mjs");
  assert.match(release, /import \{ runPortableG6FastTrimSmoke \} from "\.\/run-portable-g6-fast-trim-smoke\.mjs"/);
  const g5Call = release.indexOf("await runPortableG5Smoke({ portableDir, platform });");
  const g6Call = release.indexOf("await runPortableG6FastTrimSmoke({ portableDir, platform, verifyPreflight: false });");
  assert.ok(g5Call >= 0 && g6Call > g5Call, "release verification must run G6 immediately after G5");
  assert.match(runner, /validatePayloadManifest/);
  assert.match(runner, /assertCapabilityContractCopy/);
  assert.match(runner, /verifyFfmpegCapabilityContract/);
  assert.match(runner, /Portable G6 smoke preserved failure evidence at/);
  assert.match(runner, /VFL_G6_SMOKE_KEEP_ARTIFACTS/);
  assert.match(runner, /VFL_SMOKE_AUDIO_ENABLED/);
  assert.match(runner, /VFL_SMOKE_STRIP_METADATA/);
  assert.match(runner, /VFL_SMOKE_SOURCE_MUTATION/);
  assert.match(runner, /fast-trim-reset-trim-complete/);
  assert.match(runner, /fast-trim-reset-all-complete/);
  assert.match(runner, /fast-trim-source-mutation-ready/);
  assert.match(runner, /fast-trim-source-mutation-complete/);
  assert.match(runner, /assertPreExecutionFailureDiagnostics/);
  assert.match(runner, /failureStage: "fast-trim-consent"/);
  assert.match(runner, /failureStage: "backend"/);
  assert.match(runner, /exact 8\.3 alias assigned by Windows depends on filesystem collision/);
  assert.match(runner, /unowned fixture-root artifact/);
  const finalShutdown = runner.lastIndexOf(
    "await shutdownSmokeProcessAndLogs(child, platform, [stdoutFile, stderrFile]);",
  );
  const completeLogRead = runner.indexOf('fs.readFile(stdoutPath, "utf8")', finalShutdown);
  const finalTempResidueCheck = runner.indexOf("const tempResidue", completeLogRead);
  assert.ok(finalShutdown >= 0, "G6 runner must explicitly terminate the full process tree and close logs");
  assert.ok(completeLogRead > finalShutdown, "complete logs must be read only after process-tree shutdown");
  assert.ok(finalTempResidueCheck > completeLogRead, "temp residue must be checked after complete log scanning");
  assert.match(runner, /taskkill completed but the packaged app process was not reaped/);
  assert.match(runner, /Linux process group remained after SIGKILL/);
  for (const protectedName of [
    "smokeRoot",
    "fixtureRoot",
    "resultRoot",
    "caseRoot",
    "statusPath",
    "stdoutPath",
    "stderrPath",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "WEBVIEW2_USER_DATA_FOLDER",
  ]) {
    assert.match(runner, new RegExp(protectedName));
  }
  assert.match(powershell, /SetForegroundWindow/);
  assert.match(powershell, /SendWait/);
  assert.match(powershell, /Unsupported G6 smoke key name/);
  assert.doesNotMatch(powershell, /Invoke-Expression|\biex\b/i);
});
