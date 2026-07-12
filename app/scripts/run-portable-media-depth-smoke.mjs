import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import url from "node:url";
import { spawn } from "node:child_process";

import { ffmpegSidecarResourceTarget, getPortableOutputDir } from "./ffmpegBundle.mjs";
import { getPortableExecutableName } from "./portableRelease.mjs";
import { shutdownSmokeProcessAndLogs } from "./portable-smoke-support.mjs";

const __filename = url.fileURLToPath(import.meta.url);
const TRANSFORM_BUFFER_HARD_LIMIT_BYTES = 2 * 1024 * 1024 * 1024;
const TRANSFORM_BUFFER_SAFETY_FACTOR = 1.5;
const TRANSFORM_VIDEO_FRAME_OVERHEAD_BYTES = 8 * 1024;
const PATHOLOGICAL_AUDIO_PACKET_COUNT = 100_000;
const SUPPORTED_PLATFORMS = new Set(["linux", "win32"]);
const REQUIRED_SMOKE_STAGES = Object.freeze([
  "detected",
  "input-applied",
  "probe-ready",
  "interaction-ready",
  "encoding",
]);

const mediaDepthSmokeCases = Object.freeze([
  Object.freeze({
    id: "sdr-baseline",
    fixtureId: "sdr-baseline",
    terminalStage: "success",
    colorPolicy: "auto",
    reverse: false,
    loopVideo: false,
    output: Object.freeze({
      width: 320,
      height: 180,
      sampleAspectRatio: "1:1",
      durationMultiplier: 1,
      pixelFormat: "yuv420p",
      bitDepth: 8,
    }),
  }),
  Object.freeze({
    id: "hdr10-explicit-policy-required",
    fixtureId: "hdr10-pq",
    terminalStage: "error",
    colorPolicy: "auto",
    reverse: false,
    loopVideo: false,
    errorIncludes: "requires the explicit Standard SDR color policy",
  }),
  Object.freeze({
    id: "hdr10-to-standard-sdr",
    fixtureId: "hdr10-pq",
    terminalStage: "success",
    colorPolicy: "standardSdr",
    reverse: false,
    loopVideo: false,
    output: Object.freeze({
      width: 320,
      height: 180,
      sampleAspectRatio: "1:1",
      durationMultiplier: 1,
      codecName: "h264",
      pixelFormat: "yuv420p",
      bitDepth: 8,
      colorRange: "tv",
      colorSpace: "bt709",
      colorTransfer: "bt709",
      colorPrimaries: "bt709",
      signalStats: Object.freeze({
        yMaxMinimum: 230,
        saturationMeanMinimum: 110,
      }),
    }),
  }),
  Object.freeze({
    id: "ten-bit-sdr-explicit-policy-required",
    fixtureId: "ten-bit-sdr",
    terminalStage: "error",
    colorPolicy: "auto",
    reverse: false,
    loopVideo: false,
    errorIncludes: "requires the explicit Standard SDR color policy",
  }),
  Object.freeze({
    id: "ten-bit-sdr-to-standard-sdr",
    fixtureId: "ten-bit-sdr",
    terminalStage: "success",
    colorPolicy: "standardSdr",
    reverse: false,
    loopVideo: false,
    output: Object.freeze({
      width: 320,
      height: 180,
      sampleAspectRatio: "1:1",
      durationMultiplier: 1,
      codecName: "h264",
      pixelFormat: "yuv420p",
      bitDepth: 8,
      colorRange: "tv",
      colorSpace: "bt709",
      colorTransfer: "bt709",
      colorPrimaries: "bt709",
    }),
  }),
  Object.freeze({
    id: "non-square-sar-normalized",
    fixtureId: "non-square-sar",
    terminalStage: "success",
    colorPolicy: "auto",
    reverse: false,
    loopVideo: false,
    output: Object.freeze({
      width: 426,
      height: 180,
      sampleAspectRatio: "1:1",
      durationMultiplier: 1,
      codecName: "h264",
      pixelFormat: "yuv420p",
      bitDepth: 8,
    }),
  }),
  Object.freeze({
    id: "arbitrary-rotation-refusal",
    fixtureId: "arbitrary-rotation",
    terminalStage: "error",
    colorPolicy: "auto",
    reverse: false,
    loopVideo: false,
    errorIncludes: "display rotation",
  }),
  Object.freeze({
    id: "attached-picture-primary-selection",
    fixtureId: "attached-picture",
    terminalStage: "success",
    colorPolicy: "auto",
    reverse: false,
    loopVideo: false,
    output: Object.freeze({
      width: 320,
      height: 180,
      sampleAspectRatio: "1:1",
      durationMultiplier: 1,
      codecName: "h264",
      attachedPictureCount: 0,
    }),
  }),
  Object.freeze({
    id: "size-target-preserves-compatible-av-copy",
    fixtureId: "tight-size-compatible-av",
    terminalStage: "success",
    colorPolicy: "auto",
    reverse: false,
    loopVideo: false,
    sizeLimitMb: 0.1,
    output: Object.freeze({
      width: 16,
      height: 16,
      sampleAspectRatio: "1:1",
      durationMultiplier: 1,
      codecName: "h264",
      audioCodecName: "aac",
      audioSampleRate: 8_000,
      audioChannels: 1,
      audioStreamCount: 1,
      outputSizeMaximum: 100_000,
      videoPacketPayloadsMatchInput: true,
      audioPacketPayloadsMatchInput: true,
    }),
  }),
  Object.freeze({
    id: "size-target-preserves-incompatible-audio-by-encode",
    fixtureId: "tight-size-incompatible-audio",
    terminalStage: "success",
    colorPolicy: "auto",
    reverse: false,
    loopVideo: false,
    sizeLimitMb: 0.1,
    output: Object.freeze({
      width: 160,
      height: 90,
      sampleAspectRatio: "1:1",
      durationMultiplier: 1,
      codecName: "h264",
      audioCodecName: "aac",
      audioSampleRate: 48_000,
      audioChannels: 1,
      audioStreamCount: 1,
      outputSizeMaximum: 100_000,
    }),
    status: Object.freeze({
      targetStatus: "met",
      targetBytes: 100_000,
      queueOutcomeKind: "done",
      videoAction: "encode",
      audioAction: "encode",
      audioCodec: "aac",
    }),
  }),
  Object.freeze({
    id: "audio-packet-pathology-reverse",
    fixtureId: "audio-packet-pathology",
    terminalStage: "success",
    colorPolicy: "auto",
    reverse: true,
    loopVideo: false,
    output: Object.freeze({
      width: 16,
      height: 16,
      sampleAspectRatio: "1:1",
      durationMultiplier: 1,
      codecName: "h264",
      audioCodecName: "aac",
      audioSampleRate: 8_000,
      audioChannels: 1,
    }),
  }),
  Object.freeze({
    id: "reverse-loop-safe",
    fixtureId: "transform-safe",
    terminalStage: "success",
    colorPolicy: "auto",
    reverse: true,
    loopVideo: true,
    output: Object.freeze({
      width: 160,
      height: 90,
      sampleAspectRatio: "1:1",
      durationMultiplier: 2,
      codecName: "h264",
      firstFrameMeanMinimum: 150,
    }),
  }),
  Object.freeze({
    id: "reverse-loop-hard-refusal",
    fixtureId: "transform-hard",
    terminalStage: "error",
    colorPolicy: "auto",
    reverse: true,
    loopVideo: true,
    errorIncludes: "above the 2 GiB safety limit",
  }),
]);

const mediaDepthSmokeCaseById = new Map(mediaDepthSmokeCases.map((testCase) => [testCase.id, testCase]));

function assertSupportedPlatform(platform) {
  if (!SUPPORTED_PLATFORMS.has(platform)) {
    throw new Error(`Portable media-depth smoke supports only Windows and Linux, not ${platform}.`);
  }
}

function resolveMediaDepthSmokeCases(caseIds) {
  if (caseIds === undefined || caseIds === null) {
    return [...mediaDepthSmokeCases];
  }
  if (!Array.isArray(caseIds) || caseIds.length === 0) {
    throw new Error("Portable media-depth smoke caseIds must be a non-empty array when provided.");
  }

  const seen = new Set();
  return caseIds.map((caseId) => {
    const normalizedId = String(caseId).trim();
    if (seen.has(normalizedId)) {
      throw new Error(`Portable media-depth smoke case is duplicated: ${normalizedId}`);
    }
    seen.add(normalizedId);
    const testCase = mediaDepthSmokeCaseById.get(normalizedId);
    if (!testCase) {
      throw new Error(`Unknown portable media-depth smoke case: ${normalizedId}`);
    }
    return testCase;
  });
}

function getPortableMediaDepthPaths(portableDir, { platform = process.platform } = {}) {
  assertSupportedPlatform(platform);
  const portableRoot = path.resolve(portableDir);
  const suffix = platform === "win32" ? ".exe" : "";
  return {
    portableRoot,
    appPath: path.resolve(portableRoot, getPortableExecutableName({ platform })),
    ffmpegPath: path.resolve(portableRoot, ffmpegSidecarResourceTarget, `ffmpeg${suffix}`),
    ffprobePath: path.resolve(portableRoot, ffmpegSidecarResourceTarget, `ffprobe${suffix}`),
  };
}

function buildPortableMediaDepthLaunch(appPath, {
  platform = process.platform,
  env = process.env,
} = {}) {
  assertSupportedPlatform(platform);
  if (platform === "linux" && !env.DISPLAY) {
    return { command: "xvfb-run", args: ["-a", appPath] };
  }
  return { command: appPath, args: [] };
}

function baseFixtureArgs() {
  return ["-y", "-hide_banner", "-loglevel", "error"];
}

function h264SdrParams() {
  return "colorprim=bt709:transfer=bt709:colormatrix=bt709:range=tv";
}

function h265Params({ colorPrimaries, colorTransfer, colorMatrix }) {
  return `pools=none:frame-threads=1:log-level=error:colorprim=${colorPrimaries}:transfer=${colorTransfer}:colormatrix=${colorMatrix}:range=limited`;
}

function buildMediaDepthFixtureCommands(fixtureId, fixtureRoot) {
  const root = path.resolve(fixtureRoot);
  const command = (args, outputPath) => Object.freeze({
    args: Object.freeze([...baseFixtureArgs(), ...args, outputPath]),
    outputPath,
  });

  switch (fixtureId) {
    case "sdr-baseline": {
      const outputPath = path.resolve(root, "sdr-baseline.mp4");
      return [command([
        "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=24",
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
        "-t", "0.75",
        "-c:v", "libx264", "-preset", "ultrafast", "-threads", "1",
        "-x264-params", h264SdrParams(), "-pix_fmt", "yuv420p",
        "-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart",
      ], outputPath)];
    }
    case "hdr10-pq": {
      const outputPath = path.resolve(root, "hdr10-pq.mp4");
      return [command([
        "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=24,format=yuv420p10le",
        "-f", "lavfi", "-i", "sine=frequency=550:sample_rate=48000",
        "-t", "0.75",
        "-c:v", "libx265", "-preset", "ultrafast", "-threads", "1",
        "-x265-params", h265Params({
          colorPrimaries: "bt2020",
          colorTransfer: "smpte2084",
          colorMatrix: "bt2020nc",
        }),
        "-pix_fmt", "yuv420p10le", "-tag:v", "hvc1",
        "-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart",
      ], outputPath)];
    }
    case "ten-bit-sdr": {
      const outputPath = path.resolve(root, "ten-bit-sdr.mp4");
      return [command([
        "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=24,format=yuv420p10le",
        "-f", "lavfi", "-i", "sine=frequency=660:sample_rate=48000",
        "-t", "0.75",
        "-c:v", "libx265", "-preset", "ultrafast", "-threads", "1",
        "-x265-params", h265Params({
          colorPrimaries: "bt709",
          colorTransfer: "bt709",
          colorMatrix: "bt709",
        }),
        "-pix_fmt", "yuv420p10le", "-tag:v", "hvc1",
        "-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart",
      ], outputPath)];
    }
    case "non-square-sar": {
      const outputPath = path.resolve(root, "non-square-sar.mp4");
      return [command([
        "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=24",
        "-f", "lavfi", "-i", "sine=frequency=770:sample_rate=48000",
        "-t", "0.75", "-vf", "setsar=4/3",
        "-c:v", "libx264", "-preset", "ultrafast", "-threads", "1",
        "-x264-params", h264SdrParams(), "-pix_fmt", "yuv420p",
        "-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart",
      ], outputPath)];
    }
    case "arbitrary-rotation": {
      const basePath = path.resolve(root, "arbitrary-rotation-base.mp4");
      const outputPath = path.resolve(root, "arbitrary-rotation.mp4");
      return [
        command([
          "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=24",
          "-f", "lavfi", "-i", "sine=frequency=825:sample_rate=48000",
          "-t", "0.75",
          "-c:v", "libx264", "-preset", "ultrafast", "-threads", "1",
          "-x264-params", h264SdrParams(), "-pix_fmt", "yuv420p",
          "-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart",
        ], basePath),
        command([
          "-display_rotation:v:0", "45", "-i", basePath,
          "-map", "0", "-c", "copy", "-movflags", "+faststart",
        ], outputPath),
      ];
    }
    case "attached-picture": {
      const primaryPath = path.resolve(root, "attached-primary-video.mp4");
      const coverPath = path.resolve(root, "attached-cover.png");
      const outputPath = path.resolve(root, "attached-picture.mp4");
      return [
        command([
          "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=24",
          "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000",
          "-t", "0.75",
          "-c:v", "libx264", "-preset", "ultrafast", "-threads", "1",
          "-x264-params", h264SdrParams(), "-pix_fmt", "yuv420p",
          "-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart",
        ], primaryPath),
        command([
          "-f", "lavfi", "-i", "color=c=red:size=64x64",
          "-frames:v", "1",
        ], coverPath),
        command([
          "-i", coverPath, "-i", primaryPath,
          "-map", "0:v:0", "-map", "1:v:0", "-map", "1:a:0",
          "-c", "copy", "-disposition:v:0", "attached_pic", "-disposition:v:1", "default",
          "-map_metadata", "-1", "-movflags", "+faststart",
        ], outputPath),
      ];
    }
    case "tight-size-incompatible-audio": {
      const outputPath = path.resolve(root, "tight-size-incompatible-audio.mp4");
      return [command([
        "-f", "lavfi", "-i", "color=c=black:size=160x90:rate=24",
        "-f", "lavfi", "-i", "sine=frequency=660:sample_rate=48000",
        "-t", "10",
        "-c:v", "libx264", "-preset", "ultrafast", "-threads", "1",
        "-b:v", "24k", "-maxrate", "24k", "-bufsize", "48k", "-pix_fmt", "yuv420p",
        "-c:a", "libmp3lame", "-b:a", "96k", "-movflags", "+faststart",
      ], outputPath)];
    }
    case "tight-size-compatible-av": {
      const outputPath = path.resolve(root, "tight-size-compatible-av.mp4");
      return [command([
        "-f", "lavfi", "-i", "color=c=black:size=16x16:rate=4",
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=8000",
        "-t", "10",
        "-c:v", "libx264", "-preset", "ultrafast", "-threads", "1",
        "-crf", "51", "-pix_fmt", "yuv420p",
        "-c:a", "aac", "-b:a", "16k", "-movflags", "+faststart",
      ], outputPath)];
    }
    case "transform-safe": {
      const outputPath = path.resolve(root, "transform-safe.mkv");
      return [command([
        "-f", "lavfi", "-i", "nullsrc=size=160x90:rate=12,geq=lum='16+200*N/7':cb=128:cr=128,format=yuv420p",
        "-f", "lavfi", "-i", "sine=frequency=990:sample_rate=48000",
        "-t", "0.666667",
        "-c:v", "ffv1", "-level", "3", "-pix_fmt", "yuv420p",
        "-c:a", "flac",
      ], outputPath)];
    }
    case "audio-packet-pathology": {
      const outputPath = path.resolve(root, "audio-packet-pathology.mkv");
      return [command([
        "-f", "lavfi", "-i", "color=c=black:size=16x16:rate=4",
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=8000",
        "-t", "12.5", "-af", "asetnsamples=n=1:p=0",
        "-c:v", "ffv1", "-level", "3", "-pix_fmt", "yuv420p",
        "-c:a", "pcm_s16le",
      ], outputPath)];
    }
    case "transform-hard": {
      const outputPath = path.resolve(root, "transform-hard.mp4");
      return [command([
        "-f", "lavfi", "-i", "color=c=black:size=3840x2160:rate=60",
        "-t", "1.2",
        "-c:v", "libx264", "-preset", "ultrafast", "-tune", "stillimage", "-crf", "51", "-threads", "1",
        "-x264-params", h264SdrParams(), "-pix_fmt", "yuv420p", "-an", "-movflags", "+faststart",
      ], outputPath)];
    }
    default:
      throw new Error(`Unknown portable media-depth fixture: ${fixtureId}`);
  }
}

function mediaDepthFixtureOutputPath(fixtureId, fixtureRoot) {
  const commands = buildMediaDepthFixtureCommands(fixtureId, fixtureRoot);
  return commands.at(-1).outputPath;
}

function buildMediaDepthSmokeEnvironment(testCase, {
  inputPath,
  outputPath,
  statusPath,
  caseRoot = statusPath ? path.dirname(statusPath) : null,
  platform = process.platform,
  baseEnv = process.env,
} = {}) {
  if (!testCase || !mediaDepthSmokeCaseById.has(testCase.id)) {
    throw new Error("Portable media-depth smoke requires a canonical test case.");
  }
  if (testCase.colorPolicy !== "auto" && testCase.colorPolicy !== "standardSdr") {
    throw new Error(`Unsupported media-depth smoke color policy: ${testCase.colorPolicy}`);
  }
  for (const [label, value] of [["inputPath", inputPath], ["outputPath", outputPath], ["statusPath", statusPath]]) {
    if (!value || !String(value).trim()) {
      throw new Error(`Portable media-depth smoke ${label} is required.`);
    }
  }

  const env = {
    ...baseEnv,
    VFL_SMOKE_INPUT: path.resolve(inputPath),
    VFL_SMOKE_OUTPUT: path.resolve(outputPath),
    VFL_SMOKE_STATUS: path.resolve(statusPath),
    VFL_SMOKE_FORMAT: "mp4",
    VFL_SMOKE_SIZE_LIMIT_MB: String(testCase.sizeLimitMb ?? 0),
    VFL_SMOKE_TRIM_START_S: "0",
    VFL_SMOKE_SKIP_PREVIEW_INTERACTIONS: "1",
    VFL_SMOKE_COLOR_POLICY: testCase.colorPolicy,
    VFL_SMOKE_REVERSE: testCase.reverse ? "1" : "0",
    VFL_SMOKE_LOOP: testCase.loopVideo ? "1" : "0",
  };
  if (platform === "win32") {
    env.WEBVIEW2_USER_DATA_FOLDER = path.resolve(caseRoot, "webview2-user-data");
  }
  return env;
}

function parseRate(rawValue) {
  const value = String(rawValue ?? "").trim();
  if (!value) return null;
  const match = value.match(/^(-?\d+(?:\.\d+)?)(?:[/:](-?\d+(?:\.\d+)?))?$/);
  if (!match) return null;
  const numerator = Number(match[1]);
  const denominator = match[2] === undefined ? 1 : Number(match[2]);
  if (!Number.isFinite(numerator) || !Number.isFinite(denominator) || denominator === 0) return null;
  const result = numerator / denominator;
  return Number.isFinite(result) ? result : null;
}

function normalizeRational(rawValue) {
  const value = String(rawValue ?? "").trim();
  const match = value.match(/^(\d+)[/:](\d+)$/);
  if (!match) return value || null;
  let numerator = Number(match[1]);
  let denominator = Number(match[2]);
  if (numerator <= 0 || denominator <= 0) return value;
  while (denominator !== 0) {
    const remainder = numerator % denominator;
    numerator = denominator;
    denominator = remainder;
  }
  const gcd = numerator;
  return `${Number(match[1]) / gcd}:${Number(match[2]) / gcd}`;
}

function streamIsAttachedOrStill(stream) {
  const disposition = stream?.disposition ?? {};
  return disposition.attached_pic === 1 || disposition.timed_thumbnails === 1 || disposition.still_image === 1;
}

function selectPrimaryVideoStream(probe) {
  const candidates = Array.isArray(probe?.streams)
    ? probe.streams.filter((stream) => stream?.codec_type === "video" && !streamIsAttachedOrStill(stream))
    : [];
  candidates.sort((left, right) => {
    const defaultDelta = Number(right?.disposition?.default === 1) - Number(left?.disposition?.default === 1);
    if (defaultDelta !== 0) return defaultDelta;
    return Number(left?.index ?? Number.MAX_SAFE_INTEGER) - Number(right?.index ?? Number.MAX_SAFE_INTEGER);
  });
  return candidates[0] ?? null;
}

function selectPrimaryAudioStream(probe) {
  const candidates = Array.isArray(probe?.streams)
    ? probe.streams.filter((stream) => stream?.codec_type === "audio")
    : [];
  candidates.sort((left, right) => {
    const defaultDelta = Number(right?.disposition?.default === 1) - Number(left?.disposition?.default === 1);
    if (defaultDelta !== 0) return defaultDelta;
    return Number(left?.index ?? Number.MAX_SAFE_INTEGER) - Number(right?.index ?? Number.MAX_SAFE_INTEGER);
  });
  return candidates[0] ?? null;
}

function streamBitDepth(stream) {
  const explicit = Number(stream?.bits_per_raw_sample || stream?.bits_per_sample);
  if (Number.isFinite(explicit) && explicit > 0) return explicit;
  const pixelFormat = String(stream?.pix_fmt ?? "").toLowerCase();
  const match = pixelFormat.match(/(?:^|[^0-9])(9|10|12|14|16)(?:le|be|$)/);
  return match ? Number(match[1]) : 8;
}

function estimateTransformBufferBytes({
  width,
  height,
  frameRate,
  durationSeconds,
  bytesPerPixel = 1.5,
  reverse = false,
  loopVideo = false,
  safetyFactor = TRANSFORM_BUFFER_SAFETY_FACTOR,
}) {
  const frameCount = Math.ceil(Number(frameRate) * Number(durationSeconds));
  const stages = Number(Boolean(reverse)) + Number(Boolean(loopVideo));
  const frameBytes = Number(width) * Number(height) * Number(bytesPerPixel) + TRANSFORM_VIDEO_FRAME_OVERHEAD_BYTES;
  return Math.ceil(frameBytes * frameCount * stages * Number(safetyFactor));
}

function fixtureFacts(probe) {
  const video = selectPrimaryVideoStream(probe);
  const audio = selectPrimaryAudioStream(probe);
  const attachedPictureCount = Array.isArray(probe?.streams)
    ? probe.streams.filter((stream) => stream?.codec_type === "video" && stream?.disposition?.attached_pic === 1).length
    : 0;
  return {
    video,
    audio,
    attachedPictureCount,
    durationSeconds: Number(probe?.format?.duration ?? video?.duration),
    frameRate: Math.max(
      parseRate(video?.avg_frame_rate) ?? 0,
      parseRate(video?.r_frame_rate) ?? 0,
    ),
    sampleAspectRatio: normalizeRational(video?.sample_aspect_ratio),
    bitDepth: streamBitDepth(video),
  };
}

function assertEqualFact(actual, expected, label) {
  if (actual !== expected) {
    throw new Error(`Portable media-depth smoke ${label} mismatch. expected=${expected} actual=${actual ?? "none"}`);
  }
}

function assertMediaDepthFixture(fixtureId, probe) {
  const facts = fixtureFacts(probe);
  if (!facts.video) {
    throw new Error(`Portable media-depth fixture ${fixtureId} has no usable primary video stream.`);
  }
  if (!Number.isFinite(facts.durationSeconds) || facts.durationSeconds <= 0) {
    throw new Error(`Portable media-depth fixture ${fixtureId} has an invalid duration.`);
  }

  switch (fixtureId) {
    case "sdr-baseline":
      assertEqualFact(facts.video.pix_fmt, "yuv420p", "SDR fixture pixel format");
      assertEqualFact(facts.bitDepth, 8, "SDR fixture bit depth");
      assertEqualFact(facts.video.color_range, "tv", "SDR fixture color range");
      assertEqualFact(facts.video.color_space, "bt709", "SDR fixture color space");
      assertEqualFact(facts.video.color_transfer, "bt709", "SDR fixture color transfer");
      assertEqualFact(facts.video.color_primaries, "bt709", "SDR fixture color primaries");
      break;
    case "hdr10-pq":
      if (facts.bitDepth < 10) throw new Error("HDR10 fixture did not retain at least 10-bit video.");
      assertEqualFact(facts.video.color_range, "tv", "HDR10 fixture color range");
      assertEqualFact(facts.video.color_space, "bt2020nc", "HDR10 fixture color space");
      assertEqualFact(facts.video.color_transfer, "smpte2084", "HDR10 fixture color transfer");
      assertEqualFact(facts.video.color_primaries, "bt2020", "HDR10 fixture color primaries");
      break;
    case "ten-bit-sdr":
      if (facts.bitDepth < 10) throw new Error("10-bit SDR fixture did not retain at least 10-bit video.");
      assertEqualFact(facts.video.color_range, "tv", "10-bit SDR fixture color range");
      assertEqualFact(facts.video.color_space, "bt709", "10-bit SDR fixture color space");
      assertEqualFact(facts.video.color_transfer, "bt709", "10-bit SDR fixture color transfer");
      assertEqualFact(facts.video.color_primaries, "bt709", "10-bit SDR fixture color primaries");
      break;
    case "non-square-sar":
      assertEqualFact(facts.sampleAspectRatio, "4:3", "SAR fixture sample aspect ratio");
      break;
    case "arbitrary-rotation": {
      const matrix = facts.video.side_data_list?.find((entry) => entry?.side_data_type === "Display Matrix");
      if (!String(matrix?.displaymatrix ?? "").trim()) {
        throw new Error("Arbitrary-rotation fixture has no authoritative display matrix.");
      }
      assertEqualFact(Number(matrix.rotation), 45, "arbitrary-rotation fixture matrix angle");
      break;
    }
    case "attached-picture":
      if (facts.attachedPictureCount < 1) throw new Error("Attached-picture fixture has no attached picture stream.");
      assertEqualFact(facts.video.width, 320, "attached-picture primary width");
      assertEqualFact(facts.video.height, 180, "attached-picture primary height");
      break;
    case "tight-size-incompatible-audio":
      if (!facts.audio) throw new Error("Tight-size fixture has no primary audio stream.");
      assertEqualFact(facts.video.codec_name, "h264", "tight-size fixture video codec");
      assertEqualFact(facts.audio.codec_name, "mp3", "tight-size fixture incompatible audio codec");
      if (Number(probe?.format?.size) <= 100_000) {
        throw new Error("Tight-size fixture does not exceed its 100000-byte target before export.");
      }
      break;
    case "tight-size-compatible-av":
      if (!facts.audio) throw new Error("Compatible tight-size fixture has no primary audio stream.");
      assertEqualFact(facts.video.codec_name, "h264", "compatible tight-size fixture video codec");
      assertEqualFact(facts.audio.codec_name, "aac", "compatible tight-size fixture audio codec");
      if (Number(probe?.format?.size) > 100_000) {
        throw new Error("Compatible tight-size fixture does not fit its 100000-byte target before export.");
      }
      break;
    case "transform-safe": {
      const bytes = estimateTransformBufferBytes({
        width: facts.video.width,
        height: facts.video.height,
        frameRate: facts.frameRate,
        durationSeconds: facts.durationSeconds,
        reverse: true,
        loopVideo: true,
      });
      if (!Number.isFinite(bytes) || bytes >= TRANSFORM_BUFFER_HARD_LIMIT_BYTES) {
        throw new Error(`Safe Reverse/Loop fixture unexpectedly estimates ${bytes} decoded bytes.`);
      }
      break;
    }
    case "audio-packet-pathology":
      if (!facts.audio) throw new Error("Pathological-audio fixture has no primary audio stream.");
      assertEqualFact(facts.audio.codec_name, "pcm_s16le", "pathological-audio fixture codec");
      assertEqualFact(Number(facts.audio.sample_rate), 8_000, "pathological-audio fixture sample rate");
      assertEqualFact(facts.audio.channels, 1, "pathological-audio fixture channels");
      assertEqualFact(Number(facts.audio.nb_read_packets), PATHOLOGICAL_AUDIO_PACKET_COUNT, "pathological-audio fixture packet count");
      break;
    case "transform-hard": {
      const loopOnlyBytes = estimateTransformBufferBytes({
        width: facts.video.width,
        height: facts.video.height,
        frameRate: facts.frameRate,
        durationSeconds: facts.durationSeconds,
        loopVideo: true,
      });
      const combinedBytes = estimateTransformBufferBytes({
        width: facts.video.width,
        height: facts.video.height,
        frameRate: facts.frameRate,
        durationSeconds: facts.durationSeconds,
        reverse: true,
        loopVideo: true,
      });
      if (loopOnlyBytes >= TRANSFORM_BUFFER_HARD_LIMIT_BYTES || combinedBytes < TRANSFORM_BUFFER_HARD_LIMIT_BYTES) {
        throw new Error(`Hard Reverse/Loop fixture does not straddle the guard as intended. loop=${loopOnlyBytes} combined=${combinedBytes}`);
      }
      break;
    }
    default:
      throw new Error(`Unknown portable media-depth fixture: ${fixtureId}`);
  }
  return facts;
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
      reject(new Error(
        timedOut
          ? `${command} timed out after ${timeoutMs} ms.`
          : `${command} exited with code ${code} signal ${signal ?? "none"}${detail ? `\n${detail}` : ""}`,
      ));
    }));
  });
}

async function probeMedia(ffprobePath, mediaPath) {
  const result = await runBounded(ffprobePath, [
    "-v", "quiet",
    "-print_format", "json",
    "-show_format",
    "-show_streams",
    "-count_packets",
    mediaPath,
  ], { capture: true, timeoutMs: 30_000 });
  return JSON.parse(result.stdout.toString("utf8"));
}

async function streamPacketPayloadHashes(ffprobePath, mediaPath, streamSelector) {
  const result = await runBounded(ffprobePath, [
    "-v", "error",
    "-select_streams", streamSelector,
    "-show_packets",
    "-show_entries", "packet=data_hash",
    "-show_data_hash", "sha256",
    "-of", "json",
    mediaPath,
  ], { capture: true, timeoutMs: 30_000 });
  const parsed = JSON.parse(result.stdout.toString("utf8"));
  return (Array.isArray(parsed?.packets) ? parsed.packets : [])
    .map((packet) => String(packet?.data_hash ?? "").trim())
    .filter(Boolean);
}

async function waitForTerminalSmokeStatus(child, statusPath, testCase, timeoutSeconds) {
  const deadline = Date.now() + timeoutSeconds * 1000;
  let lastStatus = null;
  let lastStage = null;
  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 250));
    const status = await readJsonFileOrNull(statusPath);
    if (status) {
      lastStatus = status;
      if (status.stage !== lastStage) {
        console.log(`Portable media-depth smoke ${testCase.id} stage: ${status.stage}`);
        lastStage = status.stage;
      }
      if (status.stage === "success" || status.stage === "error") {
        if (status.stage !== testCase.terminalStage) {
          throw new Error(`Portable media-depth smoke ${testCase.id} expected ${testCase.terminalStage} but reached ${status.stage}: ${status.message ?? "no message"}`);
        }
        return status;
      }
    }
    if ((child.exitCode !== null || child.signalCode !== null) && !lastStatus) {
      throw new Error(`Portable media-depth smoke ${testCase.id} app exited before writing status.`);
    }
    if ((child.exitCode !== null || child.signalCode !== null) && lastStatus?.stage !== testCase.terminalStage) {
      throw new Error(`Portable media-depth smoke ${testCase.id} app exited at stage ${lastStatus?.stage ?? "none"}.`);
    }
  }
  throw new Error(`Portable media-depth smoke ${testCase.id} timed out. Last stage: ${lastStatus?.stage ?? "none"}`);
}

function assertSmokeStageHistory(testCase, status) {
  const history = Array.isArray(status?.stageHistory) ? status.stageHistory : [];
  const missing = REQUIRED_SMOKE_STAGES.filter((stage) => !history.includes(stage));
  if (missing.length > 0) {
    throw new Error(`Portable media-depth smoke ${testCase.id} missed stages ${missing.join(", ")}. Saw: ${history.join(" -> ")}`);
  }
  if (!history.includes(testCase.terminalStage)) {
    throw new Error(`Portable media-depth smoke ${testCase.id} history did not include ${testCase.terminalStage}.`);
  }
}

async function firstFrameGrayMean(ffmpegPath, outputPath) {
  const result = await runBounded(ffmpegPath, [
    "-hide_banner", "-loglevel", "error",
    "-i", outputPath,
    "-map", "0:v:0",
    "-vf", "format=gray",
    "-frames:v", "1",
    "-f", "rawvideo",
    "pipe:1",
  ], { capture: true, timeoutMs: 30_000 });
  if (result.stdout.length === 0) {
    throw new Error("Portable media-depth smoke could not decode the first output frame.");
  }
  let sum = 0;
  for (const value of result.stdout) sum += value;
  return sum / result.stdout.length;
}

function parseSignalStats(rawOutput) {
  const stats = {};
  const pattern = /^lavfi\.signalstats\.([A-Z]+)=([^\r\n]+)$/gm;
  for (const match of String(rawOutput ?? "").matchAll(pattern)) {
    const value = Number(match[2]);
    if (Number.isFinite(value) && stats[match[1]] === undefined) {
      stats[match[1]] = value;
    }
  }
  return stats;
}

async function firstFrameSignalStats(ffmpegPath, outputPath) {
  const result = await runBounded(ffmpegPath, [
    "-hide_banner", "-loglevel", "error",
    "-i", outputPath,
    "-map", "0:v:0",
    "-vf", "select=eq(n\\,0),signalstats,metadata=print:file=-",
    "-frames:v", "1",
    "-f", "null",
    "-",
  ], { capture: true, timeoutMs: 30_000 });
  return parseSignalStats(result.stdout.toString("utf8"));
}

async function assertMediaDepthOutput({ testCase, inputProbe, outputProbe, ffmpegPath, ffprobePath, inputPath, outputPath }) {
  const expected = testCase.output;
  if (!expected) throw new Error(`Portable media-depth smoke ${testCase.id} has no output contract.`);
  const inputFacts = fixtureFacts(inputProbe);
  const outputFacts = fixtureFacts(outputProbe);
  if (!outputFacts.video) throw new Error(`Portable media-depth smoke ${testCase.id} output has no video stream.`);

  for (const [property, expectedValue] of [["width", expected.width], ["height", expected.height]]) {
    if (expectedValue !== undefined) {
      assertEqualFact(outputFacts.video[property], expectedValue, `${testCase.id} output ${property}`);
    }
  }
  if (expected.codecName !== undefined) assertEqualFact(outputFacts.video.codec_name, expected.codecName, `${testCase.id} output codec`);
  if (expected.pixelFormat !== undefined) assertEqualFact(outputFacts.video.pix_fmt, expected.pixelFormat, `${testCase.id} output pixel format`);
  if (expected.bitDepth !== undefined) assertEqualFact(outputFacts.bitDepth, expected.bitDepth, `${testCase.id} output bit depth`);
  if (expected.sampleAspectRatio !== undefined) assertEqualFact(outputFacts.sampleAspectRatio, expected.sampleAspectRatio, `${testCase.id} output sample aspect ratio`);
  if (expected.colorRange !== undefined) assertEqualFact(outputFacts.video.color_range, expected.colorRange, `${testCase.id} output color range`);
  if (expected.colorSpace !== undefined) assertEqualFact(outputFacts.video.color_space, expected.colorSpace, `${testCase.id} output color space`);
  if (expected.colorTransfer !== undefined) assertEqualFact(outputFacts.video.color_transfer, expected.colorTransfer, `${testCase.id} output color transfer`);
  if (expected.colorPrimaries !== undefined) assertEqualFact(outputFacts.video.color_primaries, expected.colorPrimaries, `${testCase.id} output color primaries`);
  if (expected.attachedPictureCount !== undefined) assertEqualFact(outputFacts.attachedPictureCount, expected.attachedPictureCount, `${testCase.id} output attached-picture count`);
  if (expected.audioCodecName !== undefined) assertEqualFact(outputFacts.audio?.codec_name, expected.audioCodecName, `${testCase.id} output audio codec`);
  if (expected.audioSampleRate !== undefined) assertEqualFact(Number(outputFacts.audio?.sample_rate), expected.audioSampleRate, `${testCase.id} output audio sample rate`);
  if (expected.audioChannels !== undefined) assertEqualFact(outputFacts.audio?.channels, expected.audioChannels, `${testCase.id} output audio channels`);
  if (expected.audioStreamCount !== undefined) {
    const audioStreamCount = Array.isArray(outputProbe?.streams)
      ? outputProbe.streams.filter((stream) => stream?.codec_type === "audio").length
      : 0;
    assertEqualFact(audioStreamCount, expected.audioStreamCount, `${testCase.id} output audio-stream count`);
  }

  if (expected.outputSizeMaximum !== undefined) {
    const outputSize = (await fs.stat(outputPath)).size;
    if (outputSize > expected.outputSizeMaximum) {
      throw new Error(`Portable media-depth smoke ${testCase.id} output ${outputSize} bytes exceeds ${expected.outputSizeMaximum}.`);
    }
  }

  if (expected.videoPacketPayloadsMatchInput) {
    const [inputHashes, outputHashes] = await Promise.all([
      streamPacketPayloadHashes(ffprobePath, inputPath, "v:0"),
      streamPacketPayloadHashes(ffprobePath, outputPath, "v:0"),
    ]);
    if (inputHashes.length === 0 || JSON.stringify(inputHashes) !== JSON.stringify(outputHashes)) {
      throw new Error(`Portable media-depth smoke ${testCase.id} did not preserve exact video packet payloads through stream copy.`);
    }
  }

  if (expected.audioPacketPayloadsMatchInput) {
    const [inputHashes, outputHashes] = await Promise.all([
      streamPacketPayloadHashes(ffprobePath, inputPath, "a:0"),
      streamPacketPayloadHashes(ffprobePath, outputPath, "a:0"),
    ]);
    if (inputHashes.length === 0 || JSON.stringify(inputHashes) !== JSON.stringify(outputHashes)) {
      throw new Error(`Portable media-depth smoke ${testCase.id} did not preserve exact audio packet payloads through stream copy.`);
    }
  }

  const expectedDuration = inputFacts.durationSeconds * expected.durationMultiplier;
  if (!Number.isFinite(outputFacts.durationSeconds) || Math.abs(outputFacts.durationSeconds - expectedDuration) > 0.18) {
    throw new Error(`Portable media-depth smoke ${testCase.id} duration mismatch. expected=${expectedDuration.toFixed(3)} actual=${outputFacts.durationSeconds}`);
  }

  if (expected.firstFrameMeanMinimum !== undefined) {
    const mean = await firstFrameGrayMean(ffmpegPath, outputPath);
    if (mean < expected.firstFrameMeanMinimum) {
      throw new Error(`Portable media-depth smoke ${testCase.id} first frame mean ${mean.toFixed(2)} did not prove Reverse ran before Loop.`);
    }
  }

  if (expected.signalStats !== undefined) {
    const stats = await firstFrameSignalStats(ffmpegPath, outputPath);
    if (!Number.isFinite(stats.YMAX) || stats.YMAX < expected.signalStats.yMaxMinimum) {
      throw new Error(`Portable media-depth smoke ${testCase.id} first-frame YMAX ${stats.YMAX ?? "missing"} did not prove HDR tone mapping.`);
    }
    if (!Number.isFinite(stats.SATAVG) || stats.SATAVG < expected.signalStats.saturationMeanMinimum) {
      throw new Error(`Portable media-depth smoke ${testCase.id} first-frame SATAVG ${stats.SATAVG ?? "missing"} did not prove HDR tone mapping.`);
    }
  }
}

function assertMediaDepthStatus(testCase, status, outputSizeBytes) {
  const expected = testCase.status;
  if (!expected) return;
  assertEqualFact(status?.outputSizeBytes, outputSizeBytes, `${testCase.id} status output bytes`);
  assertEqualFact(status?.targetResult?.status, expected.targetStatus, `${testCase.id} target status`);
  assertEqualFact(status?.targetResult?.targetBytes, expected.targetBytes, `${testCase.id} target bytes`);
  assertEqualFact(status?.targetResult?.actualBytes, outputSizeBytes, `${testCase.id} target actual bytes`);
  assertEqualFact(status?.queueOutcomeKind, expected.queueOutcomeKind, `${testCase.id} queue outcome`);
  assertEqualFact(status?.diagnostics?.actualSizeBytes, outputSizeBytes, `${testCase.id} diagnostic actual bytes`);
  assertEqualFact(status?.diagnostics?.videoAction, expected.videoAction, `${testCase.id} diagnostic video action`);
  assertEqualFact(status?.diagnostics?.audioAction, expected.audioAction, `${testCase.id} diagnostic audio action`);
  assertEqualFact(status?.diagnostics?.audioCodec, expected.audioCodec, `${testCase.id} diagnostic audio codec`);
}

async function runPortableMediaDepthSmoke({
  portableDir = process.env.VFL_PORTABLE_DIR || getPortableOutputDir(),
  platform = process.platform,
  caseIds,
  timeoutSeconds = 90,
  fixtureTimeoutSeconds = 90,
  keepFailureArtifacts = true,
} = {}) {
  assertSupportedPlatform(platform);
  if (platform !== process.platform) {
    throw new Error(`Portable media-depth smoke cannot execute ${platform} artifacts on ${process.platform}.`);
  }
  if (!Number.isFinite(timeoutSeconds) || timeoutSeconds <= 0 || timeoutSeconds > 300) {
    throw new Error("Portable media-depth smoke timeoutSeconds must be between 1 and 300.");
  }
  if (!Number.isFinite(fixtureTimeoutSeconds) || fixtureTimeoutSeconds <= 0 || fixtureTimeoutSeconds > 300) {
    throw new Error("Portable media-depth smoke fixtureTimeoutSeconds must be between 1 and 300.");
  }

  const selectedCases = resolveMediaDepthSmokeCases(caseIds);
  const { portableRoot, appPath, ffmpegPath, ffprobePath } = getPortableMediaDepthPaths(portableDir, { platform });
  for (const requiredPath of [appPath, ffmpegPath, ffprobePath]) {
    if (!(await exists(requiredPath))) {
      throw new Error(`Portable media-depth smoke could not find required extracted payload file: ${requiredPath}`);
    }
  }

  const smokeRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-portable-media-depth-"));
  const fixtureRoot = path.resolve(smokeRoot, "fixtures");
  const resultRoot = path.resolve(smokeRoot, "results");
  await fs.mkdir(fixtureRoot, { recursive: true });
  await fs.mkdir(resultRoot, { recursive: true });
  let failed = false;

  try {
    const fixtureIds = [...new Set(selectedCases.map((testCase) => testCase.fixtureId))];
    const fixtureProbes = new Map();
    for (const fixtureId of fixtureIds) {
      for (const fixtureCommand of buildMediaDepthFixtureCommands(fixtureId, fixtureRoot)) {
        await runBounded(ffmpegPath, fixtureCommand.args, {
          cwd: fixtureRoot,
          timeoutMs: fixtureTimeoutSeconds * 1000,
        });
        if (!(await exists(fixtureCommand.outputPath))) {
          throw new Error(`Portable media-depth fixture command did not create ${fixtureCommand.outputPath}`);
        }
      }
      const inputPath = mediaDepthFixtureOutputPath(fixtureId, fixtureRoot);
      const probe = await probeMedia(ffprobePath, inputPath);
      assertMediaDepthFixture(fixtureId, probe);
      fixtureProbes.set(fixtureId, probe);
      console.log(`Portable media-depth fixture ready: ${fixtureId}`);
    }

    for (const testCase of selectedCases) {
      const caseRoot = path.resolve(resultRoot, testCase.id);
      await fs.mkdir(caseRoot, { recursive: true });
      const inputPath = mediaDepthFixtureOutputPath(testCase.fixtureId, fixtureRoot);
      const outputPath = path.resolve(caseRoot, `${testCase.id}.mp4`);
      const statusPath = path.resolve(caseRoot, "status.json");
      const smokeEnv = buildMediaDepthSmokeEnvironment(testCase, {
        inputPath,
        outputPath,
        statusPath,
        caseRoot,
        platform,
      });
      if (platform === "win32") {
        await fs.mkdir(smokeEnv.WEBVIEW2_USER_DATA_FOLDER, { recursive: true });
      }
      const launch = buildPortableMediaDepthLaunch(appPath, { platform, env: smokeEnv });
      let child = null;
      try {
        child = spawn(launch.command, launch.args, {
          cwd: portableRoot,
          detached: platform === "linux",
          env: smokeEnv,
          stdio: "ignore",
        });
        const status = await waitForTerminalSmokeStatus(child, statusPath, testCase, timeoutSeconds);
        assertSmokeStageHistory(testCase, status);

        if (testCase.terminalStage === "error") {
          if (!String(status.message ?? "").includes(testCase.errorIncludes)) {
            throw new Error(`Portable media-depth smoke ${testCase.id} error mismatch. expected substring=${testCase.errorIncludes} actual=${status.message ?? "none"}`);
          }
          if (await exists(outputPath)) {
            throw new Error(`Portable media-depth smoke ${testCase.id} wrote an output despite a required pre-spawn refusal.`);
          }
        } else {
          if (!(await exists(outputPath))) {
            throw new Error(`Portable media-depth smoke ${testCase.id} reported success without an output.`);
          }
          const stats = await fs.stat(outputPath);
          if (stats.size <= 0) throw new Error(`Portable media-depth smoke ${testCase.id} wrote an empty output.`);
          assertMediaDepthStatus(testCase, status, stats.size);
          const outputProbe = await probeMedia(ffprobePath, outputPath);
          await assertMediaDepthOutput({
            testCase,
            inputProbe: fixtureProbes.get(testCase.fixtureId),
            outputProbe,
            ffmpegPath,
            ffprobePath,
            inputPath,
            outputPath,
          });
        }
        console.log(`Portable media-depth smoke passed: ${testCase.id}`);
      } finally {
        await shutdownSmokeProcessAndLogs(child, platform, []);
      }
    }
    console.log(`Portable media-depth smoke passed ${selectedCases.length} cases on ${platform}.`);
  } catch (error) {
    failed = true;
    if (keepFailureArtifacts) {
      console.error(`Portable media-depth smoke preserved failure evidence at ${smokeRoot}`);
    }
    throw error;
  } finally {
    if (!failed || !keepFailureArtifacts) {
      await fs.rm(smokeRoot, {
        recursive: true,
        force: true,
        maxRetries: 5,
        retryDelay: 200,
      });
    }
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === __filename) {
  await runPortableMediaDepthSmoke();
}

export {
  REQUIRED_SMOKE_STAGES,
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
  runPortableMediaDepthSmoke,
  selectPrimaryVideoStream,
  selectPrimaryAudioStream,
};
