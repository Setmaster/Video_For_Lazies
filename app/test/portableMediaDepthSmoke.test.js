import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";

import {
  TRANSFORM_BUFFER_HARD_LIMIT_BYTES,
  buildMediaDepthFixtureCommands,
  buildMediaDepthSmokeEnvironment,
  buildPortableMediaDepthLaunch,
  estimateTransformBufferBytes,
  fixtureFacts,
  getPortableMediaDepthPaths,
  mediaDepthFixtureOutputPath,
  mediaDepthSmokeCases,
  normalizeRational,
  parseSignalStats,
  resolveMediaDepthSmokeCases,
  selectPrimaryAudioStream,
  selectPrimaryVideoStream,
} from "../scripts/run-portable-media-depth-smoke.mjs";

test("packaged media-depth matrix covers policy, geometry, stream selection, and transform safety", () => {
  assert.deepEqual(
    mediaDepthSmokeCases.map((testCase) => testCase.id),
    [
      "sdr-baseline",
      "hdr10-explicit-policy-required",
      "hdr10-to-standard-sdr",
      "ten-bit-sdr-explicit-policy-required",
      "ten-bit-sdr-to-standard-sdr",
      "non-square-sar-normalized",
      "arbitrary-rotation-refusal",
      "attached-picture-primary-selection",
      "size-target-preserves-compatible-av-copy",
      "size-target-preserves-incompatible-audio-by-encode",
      "audio-packet-pathology-reverse",
      "reverse-loop-safe",
      "reverse-loop-hard-refusal",
    ],
  );

  const hdrCases = mediaDepthSmokeCases.filter((testCase) => testCase.fixtureId === "hdr10-pq");
  assert.deepEqual(hdrCases.map((testCase) => [testCase.colorPolicy, testCase.terminalStage]), [
    ["auto", "error"],
    ["standardSdr", "success"],
  ]);
  const convertedHdr = hdrCases.find((testCase) => testCase.terminalStage === "success");
  assert.deepEqual(convertedHdr.output.signalStats, {
    yMaxMinimum: 230,
    saturationMeanMinimum: 110,
  });
  const highBitSdrCases = mediaDepthSmokeCases.filter((testCase) => testCase.fixtureId === "ten-bit-sdr");
  assert.deepEqual(highBitSdrCases.map((testCase) => [testCase.colorPolicy, testCase.terminalStage]), [
    ["auto", "error"],
    ["standardSdr", "success"],
  ]);

  const hardCase = mediaDepthSmokeCases.find((testCase) => testCase.id === "reverse-loop-hard-refusal");
  assert.equal(hardCase.reverse, true);
  assert.equal(hardCase.loopVideo, true);
  assert.equal(hardCase.terminalStage, "error");
  assert.match(hardCase.errorIncludes, /2 GiB/);
});

test("media-depth case selection is bounded and rejects unknown or duplicate cases", () => {
  assert.deepEqual(
    resolveMediaDepthSmokeCases(["sdr-baseline", "reverse-loop-safe"]).map((testCase) => testCase.id),
    ["sdr-baseline", "reverse-loop-safe"],
  );
  assert.equal(resolveMediaDepthSmokeCases().length, mediaDepthSmokeCases.length);
  assert.throws(() => resolveMediaDepthSmokeCases([]), /non-empty array/i);
  assert.throws(() => resolveMediaDepthSmokeCases(["not-a-case"]), /unknown/i);
  assert.throws(() => resolveMediaDepthSmokeCases(["sdr-baseline", "sdr-baseline"]), /duplicated/i);
});

test("fixture commands carry complete deterministic color metadata and exact output paths", () => {
  const fixtureRoot = path.resolve("/tmp", "vfl fixture contract");
  const hdrCommands = buildMediaDepthFixtureCommands("hdr10-pq", fixtureRoot);
  assert.equal(hdrCommands.length, 1);
  assert.equal(hdrCommands[0].args.at(-1), path.resolve(fixtureRoot, "hdr10-pq.mp4"));
  assert.ok(hdrCommands[0].args.includes("libx265"));
  assert.match(hdrCommands[0].args[hdrCommands[0].args.indexOf("-x265-params") + 1], /colorprim=bt2020/);
  assert.match(hdrCommands[0].args[hdrCommands[0].args.indexOf("-x265-params") + 1], /transfer=smpte2084/);
  assert.match(hdrCommands[0].args[hdrCommands[0].args.indexOf("-x265-params") + 1], /colormatrix=bt2020nc/);
  assert.ok(hdrCommands[0].args.includes("yuv420p10le"));

  const tenBitCommands = buildMediaDepthFixtureCommands("ten-bit-sdr", fixtureRoot);
  const tenBitParams = tenBitCommands[0].args[tenBitCommands[0].args.indexOf("-x265-params") + 1];
  assert.match(tenBitParams, /colorprim=bt709/);
  assert.match(tenBitParams, /transfer=bt709/);
  assert.match(tenBitParams, /colormatrix=bt709/);

  const sdrCommands = buildMediaDepthFixtureCommands("sdr-baseline", fixtureRoot);
  const sdrParams = sdrCommands[0].args[sdrCommands[0].args.indexOf("-x264-params") + 1];
  assert.match(sdrParams, /colorprim=bt709/);
  assert.match(sdrParams, /transfer=bt709/);
  assert.equal(mediaDepthFixtureOutputPath("sdr-baseline", fixtureRoot), path.resolve(fixtureRoot, "sdr-baseline.mp4"));
});

test("attached-picture and transform fixtures are constructed without shell parsing", () => {
  const fixtureRoot = path.resolve("/tmp", "vfl fixture contract");
  const attached = buildMediaDepthFixtureCommands("attached-picture", fixtureRoot);
  assert.equal(attached.length, 3);
  const muxArgs = attached.at(-1).args;
  assert.deepEqual(
    muxArgs.slice(muxArgs.indexOf("-map"), muxArgs.indexOf("-c")),
    ["-map", "0:v:0", "-map", "1:v:0", "-map", "1:a:0"],
  );
  assert.equal(muxArgs[muxArgs.indexOf("-disposition:v:0") + 1], "attached_pic");
  assert.equal(muxArgs.at(-1), path.resolve(fixtureRoot, "attached-picture.mp4"));

  const rotated = buildMediaDepthFixtureCommands("arbitrary-rotation", fixtureRoot);
  assert.equal(rotated.length, 2);
  assert.equal(rotated.at(-1).args[rotated.at(-1).args.indexOf("-display_rotation:v:0") + 1], "45");
  assert.ok(rotated.at(-1).args.includes("copy"));
  assert.equal(rotated.at(-1).args.at(-1), path.resolve(fixtureRoot, "arbitrary-rotation.mp4"));

  const pathologicalAudioArgs = buildMediaDepthFixtureCommands("audio-packet-pathology", fixtureRoot)[0].args;
  assert.equal(pathologicalAudioArgs[pathologicalAudioArgs.indexOf("-t") + 1], "12.5");
  assert.equal(pathologicalAudioArgs[pathologicalAudioArgs.indexOf("-af") + 1], "asetnsamples=n=1:p=0");
  assert.ok(pathologicalAudioArgs.includes("pcm_s16le"));

  const tightSizeArgs = buildMediaDepthFixtureCommands("tight-size-incompatible-audio", fixtureRoot)[0].args;
  assert.ok(tightSizeArgs.includes("libx264"));
  assert.ok(tightSizeArgs.includes("libmp3lame"));
  assert.equal(tightSizeArgs[tightSizeArgs.indexOf("-t") + 1], "10");
  const compatibleSizeArgs = buildMediaDepthFixtureCommands("tight-size-compatible-av", fixtureRoot)[0].args;
  assert.ok(compatibleSizeArgs.includes("libx264"));
  assert.ok(compatibleSizeArgs.includes("aac"));
  assert.equal(compatibleSizeArgs[compatibleSizeArgs.indexOf("-b:a") + 1], "16k");

  const safeArgs = buildMediaDepthFixtureCommands("transform-safe", fixtureRoot)[0].args;
  assert.ok(safeArgs.some((arg) => arg.includes("geq=lum='16+200*N/7'")));
  assert.ok(safeArgs.includes("ffv1"));

  const hardArgs = buildMediaDepthFixtureCommands("transform-hard", fixtureRoot)[0].args;
  assert.ok(hardArgs.includes("color=c=black:size=3840x2160:rate=60"));
  assert.equal(hardArgs[hardArgs.indexOf("-t") + 1], "1.2");
  assert.equal(hardArgs.at(-1), path.resolve(fixtureRoot, "transform-hard.mp4"));
  assert.throws(() => buildMediaDepthFixtureCommands("missing", fixtureRoot), /unknown/i);
});

test("packaged smoke environment exposes strict color and Reverse hooks", () => {
  const testCase = mediaDepthSmokeCases.find((candidate) => candidate.id === "reverse-loop-safe");
  const env = buildMediaDepthSmokeEnvironment(testCase, {
    inputPath: "/tmp/input clip.mkv",
    outputPath: "/tmp/output clip.mp4",
    statusPath: "/tmp/status file.json",
    caseRoot: "/tmp/media-depth-case",
    platform: "win32",
    baseEnv: { KEEP_ME: "yes", WEBVIEW2_USER_DATA_FOLDER: "/tmp/shared-webview-profile" },
  });
  assert.equal(env.KEEP_ME, "yes");
  assert.equal(env.VFL_SMOKE_SKIP_PREVIEW_INTERACTIONS, "1");
  assert.equal(env.VFL_SMOKE_COLOR_POLICY, "auto");
  assert.equal(env.VFL_SMOKE_REVERSE, "1");
  assert.equal(env.VFL_SMOKE_LOOP, "1");
  assert.equal(env.VFL_SMOKE_FORMAT, "mp4");
  assert.equal(env.WEBVIEW2_USER_DATA_FOLDER, path.resolve("/tmp/media-depth-case/webview2-user-data"));
  assert.notEqual(env.WEBVIEW2_USER_DATA_FOLDER, "/tmp/shared-webview-profile");

  const convertedCase = mediaDepthSmokeCases.find((candidate) => candidate.id === "hdr10-to-standard-sdr");
  const convertedEnv = buildMediaDepthSmokeEnvironment(convertedCase, {
    inputPath: "/tmp/hdr.mp4",
    outputPath: "/tmp/sdr.mp4",
    statusPath: "/tmp/status.json",
    baseEnv: {},
  });
  assert.equal(convertedEnv.VFL_SMOKE_COLOR_POLICY, "standardSdr");
  assert.equal(convertedEnv.VFL_SMOKE_REVERSE, "0");
  assert.equal(convertedEnv.VFL_SMOKE_LOOP, "0");
  const tightSizeCase = mediaDepthSmokeCases.find((candidate) => candidate.id === "size-target-preserves-incompatible-audio-by-encode");
  const tightSizeEnv = buildMediaDepthSmokeEnvironment(tightSizeCase, {
    inputPath: "/tmp/tight.mp4",
    outputPath: "/tmp/tight-output.mp4",
    statusPath: "/tmp/tight-status.json",
    baseEnv: {},
  });
  assert.equal(tightSizeEnv.VFL_SMOKE_SIZE_LIMIT_MB, "0.1");
  assert.equal(tightSizeCase.output.audioCodecName, "aac");
  assert.equal(tightSizeCase.output.audioSampleRate, 48_000);
  assert.equal(tightSizeCase.output.audioChannels, 1);
  assert.equal(tightSizeCase.output.audioStreamCount, 1);
  assert.equal(tightSizeCase.output.outputSizeMaximum, 100_000);
  assert.equal(tightSizeCase.output.videoPacketPayloadsMatchInput, undefined);
  assert.deepEqual(tightSizeCase.status, {
    targetStatus: "met",
    targetBytes: 100_000,
    queueOutcomeKind: "done",
    videoAction: "encode",
    audioAction: "encode",
    audioCodec: "aac",
  });
  const compatibleSizeCase = mediaDepthSmokeCases.find((candidate) => candidate.id === "size-target-preserves-compatible-av-copy");
  assert.equal(compatibleSizeCase.sizeLimitMb, 0.1);
  assert.equal(compatibleSizeCase.output.audioStreamCount, 1);
  assert.equal(compatibleSizeCase.output.audioPacketPayloadsMatchInput, true);
  assert.throws(
    () => buildMediaDepthSmokeEnvironment({ ...convertedCase, colorPolicy: "guess" }, {
      inputPath: "/tmp/in",
      outputPath: "/tmp/out",
      statusPath: "/tmp/status",
    }),
    /canonical test case|unsupported/i,
  );
});

test("media-depth smoke isolates WebView2 and uses full-tree shutdown with bounded cleanup", async () => {
  const source = await fs.readFile(
    new URL("../scripts/run-portable-media-depth-smoke.mjs", import.meta.url),
    "utf8",
  );
  assert.match(source, /from "\.\/portable-smoke-support\.mjs"/);
  assert.match(source, /shutdownSmokeProcessAndLogs\(child, platform, \[\]\)/);
  assert.match(source, /WEBVIEW2_USER_DATA_FOLDER = path\.resolve\(caseRoot, "webview2-user-data"\)/);
  assert.match(source, /maxRetries: 5/);
  assert.match(source, /retryDelay: 200/);
});

test("portable media-depth paths and launch commands model exact Windows and Linux payloads", () => {
  const linux = getPortableMediaDepthPaths("/tmp/VFL", { platform: "linux" });
  assert.equal(linux.appPath, path.resolve("/tmp/VFL/video_for_lazies"));
  assert.equal(linux.ffmpegPath, path.resolve("/tmp/VFL/ffmpeg-sidecar/ffmpeg"));
  assert.equal(linux.ffprobePath, path.resolve("/tmp/VFL/ffmpeg-sidecar/ffprobe"));
  assert.deepEqual(buildPortableMediaDepthLaunch(linux.appPath, { platform: "linux", env: {} }), {
    command: "xvfb-run",
    args: ["-a", linux.appPath],
  });
  assert.deepEqual(buildPortableMediaDepthLaunch(linux.appPath, { platform: "linux", env: { DISPLAY: ":99" } }), {
    command: linux.appPath,
    args: [],
  });

  const windows = getPortableMediaDepthPaths("C:\\VFL", { platform: "win32" });
  assert.match(windows.appPath, /Video_For_Lazies\.exe$/);
  assert.match(windows.ffmpegPath, /ffmpeg\.exe$/);
  assert.match(windows.ffprobePath, /ffprobe\.exe$/);
  assert.deepEqual(buildPortableMediaDepthLaunch(windows.appPath, { platform: "win32", env: {} }), {
    command: windows.appPath,
    args: [],
  });
  assert.throws(() => getPortableMediaDepthPaths("/tmp/VFL", { platform: "darwin" }), /only Windows and Linux/i);
});

test("primary video selection excludes attached and still-image streams before default ordering", () => {
  const probe = {
    streams: [
      { index: 0, codec_type: "video", width: 64, height: 64, disposition: { attached_pic: 1, default: 1 } },
      { index: 1, codec_type: "video", width: 80, height: 80, disposition: { timed_thumbnails: 1, default: 1 } },
      { index: 4, codec_type: "video", width: 640, height: 360, disposition: { default: 0 } },
      { index: 3, codec_type: "video", width: 320, height: 180, disposition: { default: 1 } },
      { index: 5, codec_type: "audio", disposition: { default: 0 } },
      { index: 2, codec_type: "audio", sample_rate: "48000", disposition: { default: 1 } },
    ],
    format: { duration: "1.25" },
  };
  assert.equal(selectPrimaryVideoStream(probe).index, 3);
  const facts = fixtureFacts(probe);
  assert.equal(facts.video.index, 3);
  assert.equal(facts.audio.index, 2);
  assert.equal(selectPrimaryAudioStream(probe).index, 2);
  assert.equal(facts.attachedPictureCount, 1);
  assert.equal(facts.durationSeconds, 1.25);
});

test("hard fixture crosses 2 GiB only when both Reverse and Loop hooks apply", () => {
  const common = {
    width: 3840,
    height: 2160,
    frameRate: 60,
    durationSeconds: 1.2,
    bytesPerPixel: 1.5,
  };
  const loopOnly = estimateTransformBufferBytes({ ...common, loopVideo: true });
  const combined = estimateTransformBufferBytes({ ...common, reverse: true, loopVideo: true });
  assert.ok(loopOnly < TRANSFORM_BUFFER_HARD_LIMIT_BYTES);
  assert.ok(combined >= TRANSFORM_BUFFER_HARD_LIMIT_BYTES);
  assert.equal(combined, loopOnly * 2);

  const safeCombined = estimateTransformBufferBytes({
    width: 160,
    height: 90,
    frameRate: 12,
    durationSeconds: 2 / 3,
    bytesPerPixel: 1.5,
    reverse: true,
    loopVideo: true,
  });
  assert.ok(safeCombined < TRANSFORM_BUFFER_HARD_LIMIT_BYTES);
});

test("rational normalization accepts FFprobe colon and slash forms", () => {
  assert.equal(normalizeRational("4:3"), "4:3");
  assert.equal(normalizeRational("8/6"), "4:3");
  assert.equal(normalizeRational("1:1"), "1:1");
  assert.equal(normalizeRational(undefined), null);
});

test("signalstats parser captures the first finite value for deterministic HDR proof", () => {
  const parsed = parseSignalStats([
    "frame:0 pts:0",
    "lavfi.signalstats.YMAX=237",
    "lavfi.signalstats.SATAVG=118.351",
    "lavfi.signalstats.YMAX=240",
    "lavfi.signalstats.BAD=not-a-number",
  ].join("\n"));
  assert.deepEqual(parsed, { YMAX: 237, SATAVG: 118.351 });
});
