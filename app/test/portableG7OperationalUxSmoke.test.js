import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  G7_COPY_DURATION_S,
  G7_FRAME_RATE_CAP_FPS,
  G7_GEOMETRY_TOLERANCE_CSS_PX,
  G7_TRANSFORM_SOURCE_DURATION_S,
  G7_TRANSFORM_SPEED,
  assertG7Evidence,
  assertG7PrivacySurfaces,
  assertMountedProgressHistory,
  assertProgressHistory,
  assertTextExcludesPaths,
  buildG7FixtureCommand,
  buildG7SmokeEnvironment,
  fixtureBuildOrder,
  g7SmokeCases,
  normalizeRational,
  requiredG7Stages,
  resolveG7SmokeCases,
  validateOutputProbe,
} from "../scripts/run-portable-g7-operational-ux-smoke.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

function caseById(id) {
  const testCase = g7SmokeCases.find((candidate) => candidate.id === id);
  assert.ok(testCase, `missing G7 case ${id}`);
  return testCase;
}

function sample(phase, overallPct, {
  attemptId = 101,
  jobId = 202,
  stepIndex = 1,
  stepCount = 1,
  pass = 1,
  totalPasses = 1,
} = {}) {
  return {
    attemptId,
    jobId,
    phase,
    stepIndex,
    stepCount,
    pass,
    totalPasses,
    passPct: overallPct,
    overallPct,
  };
}

function mountedSample(phase, valueNow, overrides = {}) {
  const phaseLabel = phase === "copying"
    ? "Copying media"
    : phase === "finalizing"
      ? "Finalizing output"
      : "Encoding";
  return {
    attemptId: 101,
    jobId: 202,
    phase,
    sourceOverallPct: phase === "finalizing" ? 1 : valueNow / 100,
    isFinalizing: phase === "finalizing",
    role: "progressbar",
    ariaLabel: "Encoding progress",
    valueMin: 0,
    valueMax: 100,
    valueNow,
    valueText: `${phaseLabel}, ${valueNow} percent`,
    phaseLabel,
    visiblePercent: valueNow,
    fillWidth: `${valueNow}%`,
    ...overrides,
  };
}

function validEvidence(testCase) {
  const rotated = testCase.operation !== "copy-progress";
  const active = testCase.requireActiveControls;
  return {
    operation: testCase.operation,
    resetDialogRole: "alertdialog",
    resetCancelFocused: true,
    resetCancelPreservedSettings: true,
    resetCancelRestoredFocus: true,
    resetConfirmed: true,
    resetConfirmRestoredFocus: true,
    previewRotationDeg: rotated ? 90 : 0,
    previewTransform: rotated ? "translate(-50%, -50%) rotate(90deg)" : null,
    postSpeedFrameRateFps: rotated ? (testCase.operation === "cancel-drop" ? 30 : 120) : 30,
    frameRateCapFps: rotated ? 24 : null,
    frameRateCapApplies: rotated ? true : null,
    frameRateMountedCopyVerified: true,
    exportControlStable: true,
    exportControlInitiallyFocused: true,
    exportControlPreservedIdentity: active,
    exportControlPreservedGeometry: active,
    cancelControlSeparate: active,
    cancelInvokeCount: testCase.expectedCancellation ? 1 : 0,
    dropActionKind: testCase.expectedCancellation ? "queueInputs" : null,
    dropPreservedInput: testCase.expectedCancellation ? true : null,
    queuedDropCount: testCase.expectedCancellation ? 1 : null,
    progressHistory: testCase.operation === "copy-progress"
      ? [sample("copying", 0.1), sample("finalizing", 1)]
      : testCase.operation === "rotate-speed-cap"
        ? [sample("encoding", 0.1), sample("encoding", 0.7), sample("finalizing", 1)]
        : [sample("encoding", 0.05)],
    mountedProgressHistory: testCase.operation === "copy-progress"
      ? [mountedSample("copying", 10), mountedSample("finalizing", 99)]
      : testCase.operation === "rotate-speed-cap"
        ? [mountedSample("encoding", 10), mountedSample("encoding", 70), mountedSample("finalizing", 99)]
        : [mountedSample("encoding", 5)],
  };
}

test("G7 portable corpus is bounded to copy, rotate/speed/cap, and cancel/drop", () => {
  assert.deepEqual(g7SmokeCases.map(({ id }) => id), [
    "copy-progress",
    "rotate-speed-cap",
    "cancel-drop",
  ]);
  assert.deepEqual(caseById("copy-progress").expectedPhases, ["copying", "finalizing"]);
  assert.deepEqual(caseById("rotate-speed-cap").expectedPhases, ["encoding", "finalizing"]);
  assert.deepEqual(caseById("cancel-drop").expectedPhases, ["encoding"]);
  assert.equal(caseById("rotate-speed-cap").expectedOutput.width, 540);
  assert.equal(caseById("rotate-speed-cap").expectedOutput.height, 960);
  assert.equal(caseById("rotate-speed-cap").expectedOutput.frameRateFps, G7_FRAME_RATE_CAP_FPS);
  assert.equal(
    caseById("rotate-speed-cap").expectedOutput.durationS,
    G7_TRANSFORM_SOURCE_DURATION_S / G7_TRANSFORM_SPEED,
  );
  assert.equal(caseById("cancel-drop").expectedCancellation, true);
  assert.equal(G7_GEOMETRY_TOLERANCE_CSS_PX, 1);
});

test("G7 case selection rejects empty, duplicate, and unknown requests", () => {
  assert.equal(resolveG7SmokeCases().length, 3);
  assert.deepEqual(resolveG7SmokeCases(["cancel-drop"]).map(({ id }) => id), ["cancel-drop"]);
  assert.throws(() => resolveG7SmokeCases([]), /non-empty array/i);
  assert.throws(() => resolveG7SmokeCases(["copy-progress", "copy-progress"]), /duplicated/i);
  assert.throws(() => resolveG7SmokeCases(["unknown"]), /unknown portable G7 smoke case/i);
});

test("G7 fixture commands pin compatible copy and 60 fps high-motion source contracts", () => {
  const root = path.resolve(os.tmpdir(), "vfl-g7-fixtures");
  const copy = buildG7FixtureCommand("copy-compatible", root);
  const copyArgs = copy.args.join(" ");
  assert.match(copyArgs, /testsrc2=size=640x360:rate=30/);
  assert.match(copyArgs, new RegExp(`-t ${G7_COPY_DURATION_S}`));
  assert.match(copyArgs, /-c:v libx264/);
  assert.match(copyArgs, /-c:a aac/);
  assert.match(copyArgs, /-movflags \+faststart/);

  const high = buildG7FixtureCommand("high-motion", root);
  const highArgs = high.args.join(" ");
  assert.match(highArgs, /testsrc2=size=960x540:rate=60/);
  assert.match(highArgs, new RegExp(`-t ${G7_TRANSFORM_SOURCE_DURATION_S}`));
  assert.match(highArgs, /-preset ultrafast/);
  assert.match(highArgs, /-threads 1/);
  assert.deepEqual(fixtureBuildOrder(g7SmokeCases), ["copy-compatible", "high-motion"]);
});

test("G7 smoke environment strips caller overrides and isolates platform state", () => {
  const root = path.resolve(os.tmpdir(), "vfl-g7-env");
  const common = {
    inputPath: path.resolve(root, "input.mp4"),
    outputPath: path.resolve(root, "output.mp4"),
    statusPath: path.resolve(root, "status.json"),
    caseRoot: root,
    baseEnv: {
      PATH: "/bin",
      VFL_SMOKE_INPUT: "attacker-input",
      VFL_SMOKE_G7_OPERATION: "attacker-operation",
      VFL_FFMPEG_PATH: "attacker-ffmpeg",
    },
  };
  const linux = buildG7SmokeEnvironment(caseById("rotate-speed-cap"), {
    ...common,
    platform: "linux",
  });
  assert.equal(linux.PATH, "/bin");
  assert.equal(linux.VFL_SMOKE_G7_OPERATION, "rotate-speed-cap");
  assert.equal(linux.VFL_SMOKE_SKIP_PREVIEW_INTERACTIONS, "1");
  assert.equal(linux.VFL_FFMPEG_PATH, undefined);
  assert.ok(linux.XDG_DATA_HOME.startsWith(root));

  const cancel = buildG7SmokeEnvironment(caseById("cancel-drop"), {
    ...common,
    platform: "win32",
    dropPath: path.resolve(root, "queued.mp4"),
  });
  assert.equal(cancel.VFL_SMOKE_G7_DROP_PATH, path.resolve(root, "queued.mp4"));
  assert.ok(cancel.WEBVIEW2_USER_DATA_FOLDER.startsWith(root));
  assert.throws(
    () => buildG7SmokeEnvironment(caseById("cancel-drop"), { ...common, platform: "linux" }),
    /requires a dropPath/i,
  );
});

test("G7 progress history enforces one identity, monotonic overall progress, and phases", () => {
  const copyCase = caseById("copy-progress");
  assert.deepEqual(
    assertProgressHistory(copyCase, [sample("copying", 0.1), sample("copying", 0.5), sample("finalizing", 1)]),
    { sampleCount: 3, phases: ["copying", "finalizing"], identityCount: 1 },
  );
  assert.throws(
    () => assertProgressHistory(copyCase, [sample("copying", 0.7), sample("finalizing", 0.6)]),
    /regressed/i,
  );
  assert.throws(
    () => assertProgressHistory(copyCase, [sample("copying", 0.1), sample("finalizing", 1, { jobId: 303 })]),
    /mixed encode identities/i,
  );
  assert.throws(
    () => assertProgressHistory(copyCase, [sample("copying", 1)]),
    /never observed ordered progress phase finalizing/i,
  );
  assert.throws(
    () => assertProgressHistory(copyCase, [sample("finalizing", 0.2), sample("copying", 1)]),
    /never observed ordered progress phase finalizing/i,
  );
  assert.throws(
    () => assertProgressHistory(copyCase, [sample("copying", 0.2), sample("finalizing", 0.8), sample("copying", 1)]),
    /returned to copying after finalizing/i,
  );
  assert.throws(
    () => assertProgressHistory(copyCase, [sample("copying", 0.2), sample("finalizing", 0.99)]),
    /final retained progress sample was not completed finalization/i,
  );
});

test("G7 mounted progress history proves role, name, phase copy, values, and visible monotonicity", () => {
  const copyCase = caseById("copy-progress");
  const backendHistory = [
    sample("copying", 0.1),
    sample("copying", 0.7),
    sample("finalizing", 1),
  ];
  assert.deepEqual(
    assertMountedProgressHistory(copyCase, [
      mountedSample("copying", 10),
      mountedSample("copying", 70),
      mountedSample("finalizing", 99),
    ], backendHistory),
    { sampleCount: 3, phases: ["copying", "finalizing"], identityCount: 1 },
  );
  assert.throws(
    () => assertMountedProgressHistory(copyCase, [], backendHistory),
    /no mounted progressbar samples/i,
  );
  assert.throws(
    () => assertMountedProgressHistory(copyCase, [
      mountedSample("copying", 10, { role: null }),
      mountedSample("finalizing", 99),
    ], backendHistory),
    /role or accessible name/i,
  );
  assert.throws(
    () => assertMountedProgressHistory(copyCase, [
      mountedSample("copying", 10, { valueText: "10 percent" }),
      mountedSample("finalizing", 99),
    ], backendHistory),
    /value text/i,
  );
  assert.throws(
    () => assertMountedProgressHistory(copyCase, [
      mountedSample("copying", 20),
      mountedSample("copying", 19),
    ], [sample("copying", 0.2), sample("copying", 0.19)]),
    /visibly regressed/i,
  );
  assert.throws(
    () => assertMountedProgressHistory(
      copyCase,
      [mountedSample("encoding", 10)],
      [sample("encoding", 0.1)],
    ),
    /never mounted its copying phase copy/i,
  );
  assert.throws(
    () => assertMountedProgressHistory(
      copyCase,
      [mountedSample("copying", 10)],
      [sample("copying", 0.1), sample("finalizing", 1)],
    ),
    /never mounted its finalizing phase copy/i,
  );
  assert.throws(
    () => assertMountedProgressHistory(
      copyCase,
      [mountedSample("copying", 10), mountedSample("finalizing", 99)],
      [sample("copying", 0.2), sample("finalizing", 1)],
    ),
    /not linked to a real backend event/i,
  );
  const cancelCase = caseById("cancel-drop");
  assert.deepEqual(
    assertMountedProgressHistory(
      cancelCase,
      [mountedSample("encoding", 5)],
      [sample("encoding", 0.05)],
    ),
    { sampleCount: 1, phases: ["encoding"], identityCount: 1 },
  );
});

test("G7 evidence requires mounted reset, copy, stable geometry, drop, and cancellation proof", () => {
  for (const testCase of g7SmokeCases) {
    assert.doesNotThrow(() => assertG7Evidence(testCase, validEvidence(testCase)));
  }
  const transform = caseById("rotate-speed-cap");
  assert.throws(
    () => assertG7Evidence(transform, { ...validEvidence(transform), exportControlPreservedGeometry: false }),
    /exportControlPreservedGeometry/,
  );
  assert.throws(
    () => assertG7Evidence(transform, { ...validEvidence(transform), frameRateMountedCopyVerified: false }),
    /frameRateMountedCopyVerified/,
  );
  const cancel = caseById("cancel-drop");
  assert.throws(
    () => assertG7Evidence(cancel, { ...validEvidence(cancel), cancelInvokeCount: 2 }),
    /queued drop and one-shot cancellation/i,
  );
  assert.throws(
    () => assertG7Evidence(cancel, { ...validEvidence(cancel), progressHistory: [] }),
    /retained no encode progress samples/i,
  );
  assert.throws(
    () => assertG7Evidence(cancel, {
      ...validEvidence(cancel),
      progressHistory: [sample("copying", 0.05)],
    }),
    /never observed ordered progress phase encoding/i,
  );
  assert.throws(
    () => assertG7Evidence(cancel, { ...validEvidence(cancel), mountedProgressHistory: [] }),
    /no mounted progressbar samples/i,
  );
});

test("G7 required stage histories include active controls and cancel/drop only where applicable", () => {
  assert.deepEqual(requiredG7Stages(caseById("copy-progress")), [
    "detected", "input-applied", "probe-ready", "workflow-ready", "g7-ui-ready",
    "interaction-ready", "encoding", "success",
  ]);
  assert.deepEqual(requiredG7Stages(caseById("cancel-drop")).slice(-5), [
    "encoding", "g7-controls-ready", "g7-drop-queued", "g7-cancel-requested", "success",
  ]);
});

test("G7 output probe validation proves rotated dimensions, capped rate, and speed duration", () => {
  const transform = caseById("rotate-speed-cap");
  const validProbe = {
    streams: [{
      codec_type: "video",
      codec_name: "h264",
      width: 540,
      height: 960,
      avg_frame_rate: "24/1",
      r_frame_rate: "24/1",
    }],
    format: { duration: "10.016" },
  };
  assert.deepEqual(validateOutputProbe(transform, validProbe), {
    width: 540,
    height: 960,
    frameRateFps: 24,
    durationS: 10.016,
    codecName: "h264",
  });
  assert.throws(
    () => validateOutputProbe(transform, { ...validProbe, streams: [{ ...validProbe.streams[0], width: 960, height: 540 }] }),
    /expected 540x960/i,
  );
  assert.throws(
    () => validateOutputProbe(transform, { ...validProbe, streams: [{ ...validProbe.streams[0], avg_frame_rate: "60/1" }] }),
    /expected 24/i,
  );
  assert.equal(normalizeRational("60000/1001").toFixed(3), "59.940");
  assert.equal(normalizeRational("1/0"), null);
});

test("G7 retained evidence rejects protected paths and basenames", () => {
  const protectedPath = path.resolve(os.tmpdir(), "vfl-g7-private", "input-private.mp4");
  assert.doesNotThrow(() => assertTextExcludesPaths("path-free evidence", [protectedPath], "safe"));
  assert.throws(
    () => assertTextExcludesPaths(`failed at ${protectedPath}`, [protectedPath], "unsafe"),
    /exposed a protected (?:G6 )?filesystem path/i,
  );
  assert.throws(
    () => assertTextExcludesPaths("input-private.mp4", [protectedPath], "unsafe basename"),
    /exposed a protected (?:G6 )?filesystem path/i,
  );
});

test("G7 privacy proof scans raw status, diagnostics, trim result, stdout, and stderr", () => {
  const testCase = caseById("copy-progress");
  const protectedPath = path.resolve(os.tmpdir(), "vfl-g7-private", "input-private.mp4");
  const safeStatus = {
    stage: "success",
    ok: true,
    message: null,
    outputPath: null,
    diagnostics: { failureReason: null, commandPreview: "ffmpeg -i <input> <output>" },
    trimResult: { commandPreview: "ffmpeg -i <input> <output>" },
    g7Evidence: { operation: "copy-progress" },
  };
  assert.doesNotThrow(() => assertG7PrivacySurfaces(
    testCase,
    { rawStatus: JSON.stringify(safeStatus), status: safeStatus, stdoutRaw: "", stderrRaw: "" },
    [protectedPath, ".vfl-"],
  ));

  for (const [label, surfaces] of [
    ["raw status", { rawStatus: JSON.stringify({ ...safeStatus, futureField: { leaked: protectedPath } }) }],
    ["diagnostics", { status: { ...safeStatus, diagnostics: { failureReason: protectedPath } } }],
    ["trim result", { status: { ...safeStatus, trimResult: { commandPreview: protectedPath } } }],
    ["stdout", { stdoutRaw: protectedPath }],
    ["stderr", { stderrRaw: "input-private.mp4" }],
  ]) {
    assert.throws(
      () => assertG7PrivacySurfaces(
        testCase,
        {
          rawStatus: JSON.stringify(safeStatus),
          status: safeStatus,
          stdoutRaw: "",
          stderrRaw: "",
          ...surfaces,
        },
        [protectedPath, ".vfl-"],
      ),
      /exposed a protected G6 filesystem path or basename/i,
      label,
    );
  }
});

test("G7 privacy proof uses Windows-aware slash, case, and JSON matching", () => {
  const testCase = caseById("copy-progress");
  const protectedPath = "C:\\Users\\Alice\\Private\\input-private.mp4";
  const status = { stage: "success", ok: true, outputPath: null };
  assert.throws(
    () => assertG7PrivacySurfaces(
      testCase,
      {
        rawStatus: JSON.stringify({ ...status, nested: "c:/users/alice/private/INPUT-PRIVATE.MP4" }),
        status,
        stdoutRaw: "",
        stderrRaw: "",
      },
      [protectedPath],
      { platform: "win32" },
    ),
    /exposed a protected G6 filesystem path or basename/i,
  );
});

test("G7 smoke is wired into package scripts and extracted portable verification", async () => {
  const [packageJson, releaseRunner, app, css, rust] = await Promise.all([
    fs.readFile(path.resolve(__dirname, "../package.json"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../scripts/run-release-portable.mjs"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/App.css"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src-tauri/src/lib.rs"), "utf8"),
  ]);
  const parsedPackage = JSON.parse(packageJson);
  assert.equal(
    parsedPackage.scripts["smoke:portable:g7"],
    "node scripts/run-portable-g7-operational-ux-smoke.mjs",
  );
  assert.match(releaseRunner, /runPortableG7OperationalUxSmoke/);
  assert.match(releaseRunner, /await runPortableG7OperationalUxSmoke\(\{ portableDir, platform, verifyPreflight: false \}\)/);
  assert.match(rust, /settle_g7_active_controls/);
  assert.match(rust, /Duration::from_millis\(750\)/);
  assert.match(rust, /Duration::from_millis\(200\)/);
  assert.match(rust, /Duration::from_millis\(250\)/);
  assert.match(app, /cancellation never observed a real backend encode progress event/);
  assert.match(app, /if \(!cancellationRendered[\s\S]*?smokeG7ActiveChecksDoneRef\.current = true;/);
  assert.match(app, /data-smoke-id="encode-progress"/);
  assert.match(app, /mountedProgressHistory/);
  assert.match(app, /const smokeOutputPath = smokeConfigRef\.current\.g7Operation/);
  assert.match(app, /pathFreeSmokeMessage/);
  assert.match(app, /className=\{`vfl-progress \$\{jobId === null \? "is-placeholder" : ""\}`\}/);
  assert.match(css, /\.vfl-export-button\s*\{[\s\S]*?width: 112px;[\s\S]*?min-width: 112px;/);
  assert.match(css, /\.vfl-progress\.is-placeholder\s*\{[\s\S]*?visibility: hidden;/);
});
