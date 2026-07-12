import crypto from "node:crypto";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import url from "node:url";
import { spawn } from "node:child_process";

import {
  FFMPEG_CAPABILITY_CONTRACT_FILE_NAME,
  assertCapabilityContractCopy,
  verifyFfmpegCapabilityContract,
} from "./ffmpegCapabilities.mjs";
import { ffmpegSidecarResourceTarget, getPortableOutputDir } from "./ffmpegBundle.mjs";
import {
  buildPortableG5Launch,
  getPortableG5Paths,
  portablePathsReferToSameFile,
} from "./run-portable-g5-smoke.mjs";
import { validatePayloadManifest } from "./updateManifests.mjs";
import { getProjectVersion } from "./versioning.mjs";

const __filename = url.fileURLToPath(import.meta.url);

const G6_EVIDENCE_SCHEMA_VERSION = 1;
const G6_FIXTURE_DURATION_S = 8;
const G6_FRAME_RATE = 30;
const G6_GOP_SECONDS = 2;
const G6_EXPECTED_VIDEO_PACKET_COUNT = G6_FRAME_RATE * 4;
const G6_REPLACEMENT_TITLE = "G6 packaged fast trim proof";
const G6_PRIVATE_METADATA_SENTINELS = Object.freeze([
  "PRIVATE-G6-SOURCE-TITLE",
  "PRIVATE-G6-LOCATION-43.6532--79.3832",
  "PRIVATE-G6-COMMENT-DO-NOT-RETAIN",
]);
const G6_PRIVATE_STREAM_METADATA_SENTINELS = Object.freeze([
  "PRIVATE-G6-VIDEO-HANDLER",
  "PRIVATE-G6-AUDIO-HANDLER",
]);
const G6_MAX_AV_EDGE_SKEW_US = 100_000;
const G6_AUDIO_TIMESTAMP_TOLERANCE_US = 2_000;
const G6_MAX_FRAME_MAE = 1;
const SUPPORTED_PLATFORMS = new Set(["linux", "win32"]);
const REQUIRED_FAST_TRIM_RESET_STAGES = Object.freeze([
  "fast-trim-reset-trim-complete",
  "fast-trim-reset-all-complete",
]);

const g6SmokeCases = Object.freeze([
  Object.freeze({
    id: "h264-aligned-copy-workflow",
    fixtureId: "h264-closed",
    outputFormat: "mp4",
    terminalStage: "success",
    trimStartS: 2,
    trimEndS: 6,
    effectiveStartUs: 2_000_000,
    effectiveEndUs: 6_000_000,
    sourceTimestampOffsetUs: 0,
    expectedVideoCodec: "h264",
    expectedAudioCodec: "aac",
    expectedVideoPacketCount: G6_EXPECTED_VIDEO_PACKET_COUNT,
    requiresAcceptance: false,
    workflowQueueExport: true,
    requiredWorkflowStages: Object.freeze([
      "workflow-recipe-ready",
      "workflow-recipe-saved",
      "workflow-queue-ready",
      "workflow-queue-complete",
      "workflow-ready",
    ]),
    skipPreviewInteractions: true,
  }),
  Object.freeze({
    id: "h264-between-keyframes",
    fixtureId: "h264-closed",
    outputFormat: "mp4",
    terminalStage: "success",
    trimStartS: 2.35,
    trimEndS: 5.65,
    effectiveStartUs: 2_000_000,
    effectiveEndUs: 6_000_000,
    sourceTimestampOffsetUs: 0,
    expectedVideoCodec: "h264",
    expectedAudioCodec: "aac",
    expectedVideoPacketCount: G6_EXPECTED_VIDEO_PACKET_COUNT,
    requiresAcceptance: true,
    windowsLiteralKeyboard: true,
    windowsRequiredKeyboardStages: Object.freeze([
      "preview-ready",
      "keyboard-trim-ready",
      "keyboard-trim-incremented",
      "keyboard-trim-complete",
      "keyboard-fast-trim-ready",
      "keyboard-fast-trim-accept-ready",
      "keyboard-fast-trim-complete",
      "keyboard-crop-ready",
      "keyboard-crop-complete",
      "keyboard-modal-ready",
      "keyboard-modal-open",
      "keyboard-complete",
      "accessibility-ready",
    ]),
    workflowQueueExport: false,
    skipPreviewInteractions: true,
  }),
  Object.freeze({
    id: "vp9-opus-nonzero-start-between-keyframes",
    fixtureId: "vp9-opus-nonzero-start",
    outputFormat: "webm",
    terminalStage: "success",
    trimStartS: 2.35,
    trimEndS: 5.65,
    effectiveStartUs: 2_000_000,
    effectiveEndUs: 6_000_000,
    sourceTimestampOffsetUs: 5_000_000,
    expectedVideoCodec: "vp9",
    expectedAudioCodec: "opus",
    expectedVideoPacketCount: G6_EXPECTED_VIDEO_PACKET_COUNT,
    requiresAcceptance: true,
    workflowQueueExport: false,
    skipPreviewInteractions: true,
  }),
  Object.freeze({
    id: "h264-audio-drop-metadata-preserved",
    fixtureId: "h264-closed",
    outputFormat: "mp4",
    terminalStage: "success",
    trimStartS: 2.35,
    trimEndS: 5.65,
    effectiveStartUs: 2_000_000,
    effectiveEndUs: 6_000_000,
    sourceTimestampOffsetUs: 0,
    expectedVideoCodec: "h264",
    expectedAudioCodec: null,
    expectedAudioAction: "drop",
    expectedVideoPacketCount: G6_EXPECTED_VIDEO_PACKET_COUNT,
    requiresAcceptance: true,
    audioEnabled: false,
    stripMetadata: false,
    workflowQueueExport: false,
    skipPreviewInteractions: true,
  }),
  Object.freeze({
    id: "incompatible-retained-audio-refused",
    fixtureId: "h264-opus-incompatible",
    outputFormat: "mp4",
    terminalStage: "error",
    trimStartS: 2,
    trimEndS: 6,
    expectedReasonCodes: Object.freeze(["audioCodecIncompatible"]),
    workflowQueueExport: false,
    skipPreviewInteractions: true,
  }),
  Object.freeze({
    id: "transform-refused",
    fixtureId: "h264-closed",
    outputFormat: "mp4",
    terminalStage: "error",
    trimStartS: 2,
    trimEndS: 6,
    resizeMode: "custom",
    resizeWidthPx: 160,
    resizeHeightPx: 90,
    expectedReasonCodes: Object.freeze(["resizeEnabled"]),
    workflowQueueExport: false,
    skipPreviewInteractions: true,
  }),
  Object.freeze({
    id: "open-gop-refused",
    fixtureId: "h264-open-gop",
    outputFormat: "mp4",
    terminalStage: "error",
    trimStartS: 2.35,
    trimEndS: 5.65,
    expectedReasonCodes: Object.freeze(["openGop"]),
    workflowQueueExport: false,
    skipPreviewInteractions: true,
  }),
  Object.freeze({
    id: "source-mutated-after-acceptance-refused",
    fixtureId: "h264-closed",
    outputFormat: "mp4",
    terminalStage: "error",
    trimStartS: 2.35,
    trimEndS: 5.65,
    effectiveStartUs: 2_000_000,
    effectiveEndUs: 6_000_000,
    sourceTimestampOffsetUs: 0,
    expectedVideoCodec: "h264",
    expectedAudioCodec: "aac",
    expectedVideoPacketCount: G6_EXPECTED_VIDEO_PACKET_COUNT,
    requiresAcceptance: true,
    perCaseFixtureCopy: true,
    sourceMutation: true,
    expectedErrorIncludes: "stale",
    workflowQueueExport: false,
    skipPreviewInteractions: true,
  }),
  Object.freeze({
    id: "preseeded-output-no-clobber",
    fixtureId: "h264-closed",
    outputFormat: "mp4",
    terminalStage: "error",
    trimStartS: 2,
    trimEndS: 6,
    effectiveStartUs: 2_000_000,
    effectiveEndUs: 6_000_000,
    sourceTimestampOffsetUs: 0,
    expectedVideoCodec: "h264",
    expectedAudioCodec: "aac",
    expectedVideoPacketCount: G6_EXPECTED_VIDEO_PACKET_COUNT,
    requiresAcceptance: false,
    preseedOutput: true,
    expectedErrorIncludes: "already exists",
    workflowQueueExport: false,
    skipPreviewInteractions: true,
  }),
]);

const g6SmokeCaseById = new Map(g6SmokeCases.map((testCase) => [testCase.id, testCase]));

function assertSupportedPlatform(platform) {
  if (!SUPPORTED_PLATFORMS.has(platform)) {
    throw new Error(`Portable G6 smoke supports only Windows and Linux, not ${platform}.`);
  }
}

function resolveG6SmokeCases(caseIds) {
  if (caseIds === undefined || caseIds === null) return [...g6SmokeCases];
  if (!Array.isArray(caseIds) || caseIds.length === 0) {
    throw new Error("Portable G6 smoke caseIds must be a non-empty array when provided.");
  }
  const seen = new Set();
  return caseIds.map((rawCaseId) => {
    const caseId = String(rawCaseId).trim();
    if (seen.has(caseId)) throw new Error(`Portable G6 smoke case is duplicated: ${caseId}`);
    seen.add(caseId);
    const testCase = g6SmokeCaseById.get(caseId);
    if (!testCase) throw new Error(`Unknown portable G6 smoke case: ${caseId}`);
    return testCase;
  });
}

function fixtureOutputPath(fixtureId, fixtureRoot) {
  const fileNames = {
    "h264-closed": "g6-closed-h264-aac.mp4",
    "vp9-opus-nonzero-start": "g6-nonzero-vp9-opus.mkv",
    "h264-opus-incompatible": "g6-h264-opus-incompatible.mkv",
    "h264-open-gop": "g6-open-gop-h264-aac.mp4",
  };
  const fileName = fileNames[fixtureId];
  if (!fileName) throw new Error(`Unknown portable G6 fixture: ${fixtureId}`);
  return path.resolve(fixtureRoot, fileName);
}

function fixtureMetadataArgs() {
  return [
    "-metadata", `title=${G6_PRIVATE_METADATA_SENTINELS[0]}`,
    "-metadata", `artist=${G6_PRIVATE_METADATA_SENTINELS[1]}`,
    "-metadata", `comment=${G6_PRIVATE_METADATA_SENTINELS[2]}`,
  ];
}

function h264ColorArgs() {
  return [
    "-color_range", "tv",
    "-color_primaries", "bt709",
    "-color_trc", "bt709",
    "-colorspace", "bt709",
  ];
}

function fixtureStreamMetadataArgs(container) {
  const audioDispositions = container === "mp4"
    ? "default+comment+forced+hearing_impaired"
    : "default+forced";
  return [
    "-metadata:s:v:0", `handler_name=${G6_PRIVATE_STREAM_METADATA_SENTINELS[0]}`,
    "-metadata:s:a:0", `handler_name=${G6_PRIVATE_STREAM_METADATA_SENTINELS[1]}`,
    "-disposition:v:0", "default",
    "-disposition:a:0", audioDispositions,
  ];
}

function buildG6FixtureCommands(fixtureId, fixtureRoot) {
  const outputPath = fixtureOutputPath(fixtureId, fixtureRoot);
  const common = ["-y", "-hide_banner", "-loglevel", "error"];
  const videoSource = `testsrc2=size=320x180:rate=${G6_FRAME_RATE}`;
  const audioSource = "anoisesrc=sample_rate=48000:amplitude=0.12:seed=424242:nb_samples=1024";

  switch (fixtureId) {
    case "h264-closed":
      return [Object.freeze({
        outputPath,
        args: Object.freeze([
          ...common,
          "-f", "lavfi", "-i", videoSource,
          "-f", "lavfi", "-i", audioSource,
          "-t", String(G6_FIXTURE_DURATION_S),
          "-map", "0:v:0", "-map", "1:a:0",
          "-c:v", "libx264", "-preset", "ultrafast", "-threads", "1", "-pix_fmt", "yuv420p",
          "-g", "60", "-keyint_min", "60", "-sc_threshold", "0", "-bf", "2",
          "-x264-params", "open-gop=0:keyint=60:min-keyint=60:scenecut=0:bframes=2",
          "-force_key_frames", "expr:gte(t,n_forced*2)",
          ...h264ColorArgs(),
          "-c:a", "aac", "-b:a", "96k",
          ...fixtureMetadataArgs(),
          ...fixtureStreamMetadataArgs("mp4"),
          "-movflags", "+faststart",
          outputPath,
        ]),
      })];
    case "vp9-opus-nonzero-start":
      return [Object.freeze({
        outputPath,
        args: Object.freeze([
          ...common,
          "-f", "lavfi", "-i", videoSource,
          "-f", "lavfi", "-i", audioSource,
          "-t", String(G6_FIXTURE_DURATION_S),
          "-map", "0:v:0", "-map", "1:a:0",
          "-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-threads", "1",
          "-row-mt", "0", "-lag-in-frames", "0", "-auto-alt-ref", "0",
          "-g", "60", "-keyint_min", "60", "-force_key_frames", "expr:gte(t,n_forced*2)",
          "-b:v", "500k", "-pix_fmt", "yuv420p",
          ...h264ColorArgs(),
          "-c:a", "libopus", "-b:a", "96k",
          ...fixtureMetadataArgs(),
          ...fixtureStreamMetadataArgs("webm"),
          "-output_ts_offset", "5",
          outputPath,
        ]),
      })];
    case "h264-opus-incompatible": {
      const sourcePath = fixtureOutputPath("h264-closed", fixtureRoot);
      return [Object.freeze({
        outputPath,
        requiresFixtureId: "h264-closed",
        args: Object.freeze([
          ...common,
          "-i", sourcePath,
          "-f", "lavfi", "-i", "anoisesrc=sample_rate=48000:amplitude=0.12:seed=515151:nb_samples=1024",
          "-t", String(G6_FIXTURE_DURATION_S),
          "-map", "0:v:0", "-map", "1:a:0",
          "-c:v", "copy", "-c:a", "libopus", "-b:a", "96k",
          ...fixtureMetadataArgs(),
          ...fixtureStreamMetadataArgs("webm"),
          outputPath,
        ]),
      })];
    }
    case "h264-open-gop":
      return [Object.freeze({
        outputPath,
        args: Object.freeze([
          ...common,
          "-f", "lavfi", "-i", videoSource,
          "-f", "lavfi", "-i", "anoisesrc=sample_rate=48000:amplitude=0.12:seed=616161:nb_samples=1024",
          "-t", String(G6_FIXTURE_DURATION_S),
          "-map", "0:v:0", "-map", "1:a:0",
          "-c:v", "libx264", "-preset", "ultrafast", "-threads", "1", "-pix_fmt", "yuv420p",
          "-g", "60", "-keyint_min", "60", "-sc_threshold", "0", "-bf", "3",
          "-x264-params", "open-gop=1:keyint=60:min-keyint=60:scenecut=0:bframes=3:b-pyramid=2",
          ...h264ColorArgs(),
          "-c:a", "aac", "-b:a", "96k",
          ...fixtureMetadataArgs(),
          ...fixtureStreamMetadataArgs("mp4"),
          "-movflags", "+faststart",
          outputPath,
        ]),
      })];
    default:
      throw new Error(`Unknown portable G6 fixture: ${fixtureId}`);
  }
}

function parseDecimalSecondsToUs(value, label = "timestamp") {
  const raw = String(value ?? "").trim();
  const match = raw.match(/^([+-]?)(\d+)(?:\.(\d+))?$/);
  if (!match) throw new Error(`Portable G6 smoke ${label} is not a decimal timestamp: ${raw || "empty"}`);
  const sign = match[1] === "-" ? -1n : 1n;
  const whole = BigInt(match[2]);
  const fraction = match[3] ?? "";
  const microsDigits = fraction.slice(0, 6).padEnd(6, "0");
  let micros = whole * 1_000_000n + BigInt(microsDigits || "0");
  if (fraction.length > 6 && Number(fraction[6]) >= 5) micros += 1n;
  const signed = sign * micros;
  const parsed = Number(signed);
  if (!Number.isSafeInteger(parsed)) {
    throw new Error(`Portable G6 smoke ${label} exceeds safe integer microseconds.`);
  }
  return parsed;
}

function usToSecondsString(value) {
  if (!Number.isSafeInteger(value)) throw new Error(`Microseconds must be a safe integer, got ${String(value)}.`);
  const sign = value < 0 ? "-" : "";
  const absolute = Math.abs(value);
  const whole = Math.floor(absolute / 1_000_000);
  const fraction = String(absolute % 1_000_000).padStart(6, "0").replace(/0+$/, "");
  return `${sign}${whole}${fraction ? `.${fraction}` : ""}`;
}

function safeInteger(value, label) {
  if (!Number.isSafeInteger(value)) {
    throw new Error(`Portable G6 smoke ${label} must be a safe integer, got ${String(value)}.`);
  }
  return value;
}

function normalizeRational(value) {
  const raw = String(value ?? "").trim();
  const match = raw.match(/^(\d+)[/:](\d+)$/);
  if (!match) return raw || null;
  let left = Number(match[1]);
  let right = Number(match[2]);
  if (!Number.isSafeInteger(left) || !Number.isSafeInteger(right) || left <= 0 || right <= 0) return raw;
  const originalLeft = left;
  const originalRight = right;
  while (right !== 0) {
    const remainder = left % right;
    left = right;
    right = remainder;
  }
  return `${originalLeft / left}:${originalRight / left}`;
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
  maxCaptureBytes = 64 * 1024 * 1024,
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
    let stdoutBytes = 0;
    let stderrBytes = 0;
    let captureOverflow = false;
    const retain = (target, chunk, currentBytes) => {
      const bytes = Buffer.from(chunk);
      if (currentBytes + bytes.length > maxCaptureBytes) {
        captureOverflow = true;
        child.kill("SIGKILL");
        return currentBytes;
      }
      target.push(bytes);
      return currentBytes + bytes.length;
    };
    if (capture) {
      child.stdout.on("data", (chunk) => {
        stdoutBytes = retain(stdout, chunk, stdoutBytes);
      });
      child.stderr.on("data", (chunk) => {
        stderrBytes = retain(stderr, chunk, stderrBytes);
      });
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
      if (code === 0 && !timedOut && !captureOverflow) {
        resolve({ stdout: stdoutBuffer, stderr: stderrBuffer });
        return;
      }
      const detail = stderrBuffer.toString("utf8").trim();
      const message = captureOverflow
        ? `${path.basename(command)} exceeded the ${maxCaptureBytes} byte capture limit.`
        : timedOut
          ? `${path.basename(command)} timed out after ${timeoutMs} ms.`
          : `${path.basename(command)} exited with code ${code} signal ${signal ?? "none"}${detail ? `\n${detail}` : ""}`;
      const error = new Error(message);
      error.stdout = stdoutBuffer;
      error.stderr = stderrBuffer;
      error.timedOut = timedOut;
      error.captureOverflow = captureOverflow;
      error.exitCode = code;
      error.signal = signal;
      reject(error);
    }));
  });
}

async function probeMedia(ffprobePath, mediaPath) {
  const result = await runBounded(ffprobePath, [
    "-v", "error",
    "-print_format", "json",
    "-show_format",
    "-show_streams",
    "-count_packets",
    mediaPath,
  ], { capture: true, timeoutMs: 30_000, maxCaptureBytes: 4 * 1024 * 1024 });
  return JSON.parse(result.stdout.toString("utf8"));
}

async function probePackets(ffprobePath, mediaPath, streamSelector) {
  const result = await runBounded(ffprobePath, [
    "-v", "error",
    "-select_streams", streamSelector,
    "-show_packets",
    "-show_entries",
    "packet=stream_index,pts,dts,duration,pts_time,dts_time,duration_time,flags,data_hash:packet_side_data=side_data_type,skip_samples,discard_padding",
    "-show_data_hash", "sha256",
    "-of", "json",
    mediaPath,
  ], { capture: true, timeoutMs: 30_000, maxCaptureBytes: 32 * 1024 * 1024 });
  const payload = JSON.parse(result.stdout.toString("utf8"));
  return (Array.isArray(payload?.packets) ? payload.packets : []).map((packet, index) => {
    const ptsUs = parseDecimalSecondsToUs(packet.pts_time, `${streamSelector} packet ${index} PTS`);
    const rawDts = String(packet.dts_time ?? "").trim();
    const dtsUs = rawDts && rawDts.toUpperCase() !== "N/A"
      ? parseDecimalSecondsToUs(rawDts, `${streamSelector} packet ${index} DTS`)
      : null;
    const durationUs = parseDecimalSecondsToUs(packet.duration_time, `${streamSelector} packet ${index} duration`);
    const dataHash = String(packet.data_hash ?? "").trim();
    if (!dataHash) throw new Error(`Portable G6 smoke ${streamSelector} packet ${index} omitted data_hash.`);
    return Object.freeze({
      index,
      ptsUs,
      dtsUs,
      durationUs,
      flags: String(packet.flags ?? ""),
      dataHash,
      sideDataList: Object.freeze(Array.isArray(packet.side_data_list) ? packet.side_data_list : []),
    });
  });
}

async function probeVideoFrames(ffprobePath, mediaPath) {
  const result = await runBounded(ffprobePath, [
    "-v", "error",
    "-select_streams", "v:0",
    "-show_frames",
    "-show_entries", "frame=best_effort_timestamp_time,duration_time,key_frame,pict_type",
    "-of", "json",
    mediaPath,
  ], { capture: true, timeoutMs: 30_000, maxCaptureBytes: 16 * 1024 * 1024 });
  const payload = JSON.parse(result.stdout.toString("utf8"));
  return (Array.isArray(payload?.frames) ? payload.frames : []).map((frame, index) => Object.freeze({
    index,
    ptsUs: parseDecimalSecondsToUs(frame.best_effort_timestamp_time, `video frame ${index} PTS`),
    durationUs: parseDecimalSecondsToUs(frame.duration_time, `video frame ${index} duration`),
    keyFrame: Number(frame.key_frame) === 1,
    pictType: String(frame.pict_type ?? ""),
  }));
}

function packetHashDigest(packets) {
  const digest = crypto.createHash("sha256");
  for (const packet of packets) digest.update(`${packet.dataHash}\n`);
  return digest.digest("hex");
}

function findUniqueContiguousPacketSubsequence(inputPackets, outputPackets, label = "stream") {
  if (!Array.isArray(inputPackets) || !Array.isArray(outputPackets) || outputPackets.length === 0) {
    throw new Error(`Portable G6 smoke ${label} packet sequences must be nonempty arrays.`);
  }
  if (outputPackets.length > inputPackets.length) {
    throw new Error(`Portable G6 smoke ${label} output has more packets than the input.`);
  }
  const starts = [];
  for (let start = 0; start + outputPackets.length <= inputPackets.length; start += 1) {
    let matches = true;
    for (let offset = 0; offset < outputPackets.length; offset += 1) {
      if (inputPackets[start + offset].dataHash !== outputPackets[offset].dataHash) {
        matches = false;
        break;
      }
    }
    if (matches) starts.push(start);
  }
  if (starts.length !== 1) {
    throw new Error(`Portable G6 smoke ${label} output packet hashes matched ${starts.length} contiguous input locations; expected exactly one.`);
  }
  return Object.freeze({
    inputStartIndex: starts[0],
    packetCount: outputPackets.length,
    sha256: packetHashDigest(outputPackets),
  });
}

function expectedClosedGopPacketSlice(inputVideoPackets, absoluteStartUs, absoluteEndUs) {
  const startIndex = inputVideoPackets.findIndex(
    (packet) => packet.ptsUs === absoluteStartUs && packet.flags.includes("K"),
  );
  const endIndex = inputVideoPackets.findIndex(
    (packet, index) => index > startIndex && packet.ptsUs === absoluteEndUs && packet.flags.includes("K"),
  );
  if (startIndex < 0 || endIndex <= startIndex) {
    throw new Error(`Portable G6 fixture did not expose closed-GOP keys at ${absoluteStartUs}-${absoluteEndUs} us.`);
  }
  const packets = inputVideoPackets.slice(startIndex, endIndex);
  if (packets.length === 0) throw new Error("Portable G6 fixture produced an empty closed-GOP packet prefix.");
  return Object.freeze({ startIndex, endIndex, packets: Object.freeze(packets), endKey: inputVideoPackets[endIndex] });
}

function hasLeadingPicturesAfterKey(inputVideoPackets, keyPtsUs) {
  const keyIndex = inputVideoPackets.findIndex(
    (packet) => packet.ptsUs === keyPtsUs && packet.flags.includes("K"),
  );
  if (keyIndex < 0) return false;
  for (let index = keyIndex + 1; index < inputVideoPackets.length; index += 1) {
    const packet = inputVideoPackets[index];
    if (packet.flags.includes("K")) break;
    if (packet.ptsUs < keyPtsUs) return true;
    if (Number.isSafeInteger(packet.dtsUs) && packet.dtsUs >= keyPtsUs + 500_000) break;
  }
  return false;
}

function streamByType(probe, codecType) {
  return Array.isArray(probe?.streams)
    ? probe.streams.find((stream) => stream?.codec_type === codecType) ?? null
    : null;
}

function audibleAudioBounds(packets, sampleRate) {
  if (!Array.isArray(packets) || packets.length === 0) return null;
  if (!Number.isSafeInteger(sampleRate) || sampleRate <= 0) {
    throw new Error(`Portable G6 smoke audio sample rate is invalid: ${String(sampleRate)}.`);
  }
  let startUs = Number.POSITIVE_INFINITY;
  let endUs = Number.NEGATIVE_INFINITY;
  for (const packet of packets) {
    const skipSamples = packet.sideDataList.reduce(
      (total, entry) => total + (Number.isSafeInteger(Number(entry?.skip_samples)) ? Number(entry.skip_samples) : 0),
      0,
    );
    const discardPadding = packet.sideDataList.reduce(
      (total, entry) => total + (Number.isSafeInteger(Number(entry?.discard_padding)) ? Number(entry.discard_padding) : 0),
      0,
    );
    const packetStartUs = packet.ptsUs + Math.round(skipSamples * 1_000_000 / sampleRate);
    const packetEndUs = packet.ptsUs + packet.durationUs - Math.round(discardPadding * 1_000_000 / sampleRate);
    startUs = Math.min(startUs, packetStartUs);
    endUs = Math.max(endUs, packetEndUs);
  }
  return Object.freeze({ startUs, endUs });
}

function proveMatchedAudioSourceInterval(inputPackets, packetEvidence, sampleRate, {
  expectedStartUs,
  expectedEndUs,
  maxEdgeSkewUs = G6_MAX_AV_EDGE_SKEW_US,
  label = "audio",
} = {}) {
  if (
    !packetEvidence ||
    !Number.isSafeInteger(packetEvidence.inputStartIndex) ||
    packetEvidence.inputStartIndex < 0 ||
    !Number.isSafeInteger(packetEvidence.packetCount) ||
    packetEvidence.packetCount <= 0
  ) {
    throw new Error(`Portable G6 smoke ${label} source interval has invalid packet evidence.`);
  }
  if (
    !Number.isSafeInteger(expectedStartUs) ||
    !Number.isSafeInteger(expectedEndUs) ||
    expectedEndUs <= expectedStartUs ||
    !Number.isSafeInteger(maxEdgeSkewUs) ||
    maxEdgeSkewUs < 0
  ) {
    throw new Error(`Portable G6 smoke ${label} source interval has invalid expected bounds.`);
  }
  const sourcePackets = inputPackets.slice(
    packetEvidence.inputStartIndex,
    packetEvidence.inputStartIndex + packetEvidence.packetCount,
  );
  if (sourcePackets.length !== packetEvidence.packetCount) {
    throw new Error(`Portable G6 smoke ${label} source interval exceeds the input packet list.`);
  }
  const bounds = audibleAudioBounds(sourcePackets, sampleRate);
  if (!bounds) throw new Error(`Portable G6 smoke ${label} source interval has no audible bounds.`);
  const startSkewUs = bounds.startUs - expectedStartUs;
  const endSkewUs = bounds.endUs - expectedEndUs;
  if (Math.abs(startSkewUs) > maxEdgeSkewUs || Math.abs(endSkewUs) > maxEdgeSkewUs) {
    throw new Error(
      `Portable G6 smoke ${label} matched source packets do not map to the effective interval. start=${startSkewUs} end=${endSkewUs}`,
    );
  }
  return Object.freeze({
    startUs: bounds.startUs,
    endUs: bounds.endUs,
    expectedStartUs,
    expectedEndUs,
    startSkewUs,
    endSkewUs,
  });
}

function proveCopiedAudioPacketMapping(inputPackets, outputPackets, packetEvidence, absoluteSeekUs, {
  toleranceUs = G6_AUDIO_TIMESTAMP_TOLERANCE_US,
  label = "audio",
} = {}) {
  if (
    !Number.isSafeInteger(absoluteSeekUs) ||
    !Number.isSafeInteger(toleranceUs) ||
    toleranceUs < 0 ||
    !packetEvidence ||
    !Number.isSafeInteger(packetEvidence.inputStartIndex) ||
    !Number.isSafeInteger(packetEvidence.packetCount) ||
    packetEvidence.packetCount !== outputPackets.length
  ) {
    throw new Error(`Portable G6 smoke ${label} has invalid packet timing evidence.`);
  }
  let maxPtsDeltaUs = 0;
  let maxDtsDeltaUs = 0;
  let maxDurationDeltaUs = 0;
  for (let index = 0; index < outputPackets.length; index += 1) {
    const source = inputPackets[packetEvidence.inputStartIndex + index];
    const output = outputPackets[index];
    if (
      !source ||
      source.dataHash !== output.dataHash ||
      !Number.isSafeInteger(source.ptsUs) ||
      !Number.isSafeInteger(source.dtsUs) ||
      !Number.isSafeInteger(output.ptsUs) ||
      !Number.isSafeInteger(output.dtsUs) ||
      !Number.isSafeInteger(source.durationUs) ||
      !Number.isSafeInteger(output.durationUs)
    ) {
      throw new Error(`Portable G6 smoke ${label} packet ${index} lacks exact copy/timing evidence.`);
    }
    const ptsDeltaUs = Math.abs((source.ptsUs - absoluteSeekUs) - output.ptsUs);
    const dtsDeltaUs = Math.abs((source.dtsUs - absoluteSeekUs) - output.dtsUs);
    const durationDeltaUs = Math.abs(source.durationUs - output.durationUs);
    maxPtsDeltaUs = Math.max(maxPtsDeltaUs, ptsDeltaUs);
    maxDtsDeltaUs = Math.max(maxDtsDeltaUs, dtsDeltaUs);
    maxDurationDeltaUs = Math.max(maxDurationDeltaUs, durationDeltaUs);
    if (ptsDeltaUs > toleranceUs || dtsDeltaUs > toleranceUs || durationDeltaUs > toleranceUs) {
      throw new Error(
        `Portable G6 smoke ${label} packet ${index} does not map from source-minus-seek within ${toleranceUs} us.`,
      );
    }
  }
  return Object.freeze({
    timingToleranceUs: toleranceUs,
    maxPtsDeltaUs,
    maxDtsDeltaUs,
    maxDurationDeltaUs,
  });
}

function framePresentationBounds(frames) {
  if (!Array.isArray(frames) || frames.length === 0) return null;
  return Object.freeze({
    startUs: Math.min(...frames.map((frame) => frame.ptsUs)),
    endUs: Math.max(...frames.map((frame) => frame.ptsUs + frame.durationUs)),
  });
}

function frameMeanAbsoluteError(first, second) {
  const left = Buffer.from(first ?? []);
  const right = Buffer.from(second ?? []);
  if (left.length === 0 || left.length !== right.length) {
    throw new Error(`Portable G6 frame buffers differ in length (${left.length} vs ${right.length}).`);
  }
  let absoluteError = 0;
  let maximumError = 0;
  for (let index = 0; index < left.length; index += 1) {
    const delta = Math.abs(left[index] - right[index]);
    absoluteError += delta;
    maximumError = Math.max(maximumError, delta);
  }
  return Object.freeze({
    byteCount: left.length,
    meanAbsoluteError: absoluteError / left.length,
    maximumError,
    leftSha256: crypto.createHash("sha256").update(left).digest("hex"),
    rightSha256: crypto.createHash("sha256").update(right).digest("hex"),
  });
}

async function decodeRgbFrames(ffmpegPath, mediaPath, {
  startUs = null,
  endUs = null,
} = {}) {
  const filters = [];
  if (startUs !== null || endUs !== null) {
    if (!Number.isSafeInteger(startUs) || !Number.isSafeInteger(endUs) || endUs <= startUs) {
      throw new Error("Portable G6 smoke decode bounds must be increasing safe integer microseconds.");
    }
    filters.push(`trim=start=${usToSecondsString(startUs)}:end=${usToSecondsString(endUs)}`);
    filters.push("setpts=PTS-STARTPTS");
  }
  filters.push("format=rgb24");
  const result = await runBounded(ffmpegPath, [
    "-hide_banner", "-loglevel", "error",
    "-i", mediaPath,
    "-map", "0:v:0",
    "-vf", filters.join(","),
    "-an", "-sn", "-dn",
    "-fps_mode", "passthrough",
    "-f", "rawvideo",
    "-pix_fmt", "rgb24",
    "pipe:1",
  ], {
    capture: true,
    timeoutMs: 60_000,
    maxCaptureBytes: 96 * 1024 * 1024,
  });
  return result.stdout;
}

function splitRawRgbFrames(rawVideo, width, height) {
  const frameBytes = Number(width) * Number(height) * 3;
  const raw = Buffer.from(rawVideo ?? []);
  if (!Number.isSafeInteger(frameBytes) || frameBytes <= 0 || raw.length === 0 || raw.length % frameBytes !== 0) {
    throw new Error(`Portable G6 smoke raw RGB evidence has ${raw.length} bytes for ${width}x${height} frames.`);
  }
  const frameCount = raw.length / frameBytes;
  return Object.freeze({
    frameBytes,
    frameCount,
    first: raw.subarray(0, frameBytes),
    last: raw.subarray(raw.length - frameBytes),
  });
}

function normalizedTags(probe) {
  const entries = [];
  const collect = (tags) => {
    if (!tags || typeof tags !== "object" || Array.isArray(tags)) return;
    for (const [key, value] of Object.entries(tags)) {
      entries.push([String(key).toLowerCase(), String(value)]);
    }
  };
  collect(probe?.format?.tags);
  for (const stream of probe?.streams ?? []) collect(stream?.tags);
  return entries;
}

function tagValues(tags) {
  return tags && typeof tags === "object" && !Array.isArray(tags)
    ? Object.values(tags).map((value) => String(value))
    : [];
}

function streamTagValues(probe, codecType) {
  return tagValues(streamByType(probe, codecType)?.tags);
}

function assertMetadataPolicy(testCase, inputProbe, outputProbe) {
  const inputGlobalValues = tagValues(inputProbe?.format?.tags);
  const inputVideoValues = streamTagValues(inputProbe, "video");
  const inputAudioValues = streamTagValues(inputProbe, "audio");
  for (const sentinel of G6_PRIVATE_METADATA_SENTINELS) {
    if (!inputGlobalValues.some((value) => value.includes(sentinel))) {
      throw new Error(`Portable G6 fixture omitted private metadata sentinel: ${sentinel}`);
    }
  }
  if (!inputVideoValues.some((value) => value.includes(G6_PRIVATE_STREAM_METADATA_SENTINELS[0]))) {
    throw new Error("Portable G6 fixture omitted its selected-video private metadata sentinel.");
  }
  if (!inputAudioValues.some((value) => value.includes(G6_PRIVATE_STREAM_METADATA_SENTINELS[1]))) {
    throw new Error("Portable G6 fixture omitted its selected-audio private metadata sentinel.");
  }
  const outputTags = normalizedTags(outputProbe);
  const outputValues = outputTags.map(([, value]) => value);
  const outputGlobalValues = tagValues(outputProbe?.format?.tags);
  const outputVideoValues = streamTagValues(outputProbe, "video");
  const allSentinels = [...G6_PRIVATE_METADATA_SENTINELS, ...G6_PRIVATE_STREAM_METADATA_SENTINELS];
  if (testCase.stripMetadata !== false) {
    for (const sentinel of allSentinels) {
      if (outputValues.some((value) => value.includes(sentinel))) {
        throw new Error("Portable G6 fast trim retained private source metadata despite stripMetadata=true.");
      }
    }
  } else {
    for (const sentinel of G6_PRIVATE_METADATA_SENTINELS.slice(1)) {
      if (!outputGlobalValues.some((value) => value.includes(sentinel))) {
        throw new Error(`Portable G6 fast trim omitted preserved global metadata: ${sentinel}`);
      }
    }
    if (!outputVideoValues.some((value) => value.includes(G6_PRIVATE_STREAM_METADATA_SENTINELS[0]))) {
      throw new Error("Portable G6 fast trim omitted preserved selected-video metadata.");
    }
    for (const overriddenOrDropped of [G6_PRIVATE_METADATA_SENTINELS[0], G6_PRIVATE_STREAM_METADATA_SENTINELS[1]]) {
      if (outputValues.some((value) => value.includes(overriddenOrDropped))) {
        throw new Error(`Portable G6 fast trim unexpectedly retained overridden/dropped metadata: ${overriddenOrDropped}`);
      }
    }
  }
  const outputTitles = outputTags
    .filter(([key]) => key === "title")
    .map(([, value]) => value);
  if (!outputTitles.includes(G6_REPLACEMENT_TITLE)) {
    throw new Error("Portable G6 fast trim stripped or changed the explicit replacement title.");
  }
  return Object.freeze({
    sourceSentinelsPresent: true,
    stripMetadata: testCase.stripMetadata !== false,
    sourceSentinelsStripped: testCase.stripMetadata !== false,
    preservedGlobalMetadata: testCase.stripMetadata === false,
    preservedSelectedVideoMetadata: testCase.stripMetadata === false,
    replacementTitlePresent: true,
  });
}

function assertTextExcludesSensitiveValues(rawValue, sensitiveValues, label) {
  const raw = String(rawValue ?? "");
  for (const value of sensitiveValues) {
    const token = String(value ?? "");
    if (!token) continue;
    const escaped = JSON.stringify(token).slice(1, -1);
    if (raw.includes(token) || (escaped && raw.includes(escaped))) {
      throw new Error(`${label} leaked a private G6 source metadata value.`);
    }
  }
}

function normalizeWindowsPathSearchText(value) {
  let normalized = String(value ?? "").toLowerCase();
  // JSON-serialized paths double their backslashes. Collapse only slash escapes
  // before comparing so ordinary text and serialized evidence share one shape.
  while (normalized.includes("\\\\")) normalized = normalized.replaceAll("\\\\", "\\");
  normalized = normalized.replaceAll("/", "\\");
  normalized = normalized.replaceAll("\\?\\unc\\", "\\");
  normalized = normalized.replaceAll("\\?\\", "");
  return normalized;
}

function windowsPathComponents(value) {
  const normalized = normalizeWindowsPathSearchText(value);
  return normalized
    .split("\\")
    .map((component) => component.trim())
    .filter((component) => component && !/^[a-z]:$/i.test(component));
}

function escapeRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function windowsShortAliasPattern(component) {
  const lastDot = component.lastIndexOf(".");
  const hasExtension = lastDot > 0 && lastDot < component.length - 1;
  const stem = hasExtension ? component.slice(0, lastDot) : component;
  const extension = hasExtension ? component.slice(lastDot + 1) : "";
  const shortStem = stem
    .replace(/[ .]/g, "")
    .replace(/["*/:<>?\\|+,;=\[\]]/g, "")
    .slice(0, 6);
  if (!shortStem) return null;
  const shortExtension = extension
    .replace(/[ .]/g, "")
    .replace(/["*/:<>?\\|+,;=\[\]]/g, "")
    .slice(0, 3);
  return `${escapeRegExp(shortStem)}~\\d+${shortExtension ? `\\.${escapeRegExp(shortExtension)}` : ""}`;
}

function windowsProtectedComponentPattern(component) {
  const exact = escapeRegExp(component);
  const alias = windowsShortAliasPattern(component);
  return alias ? `(?:${exact}|${alias})` : exact;
}

function containsProtectedWindowsPath(rawValue, pathValue) {
  const raw = normalizeWindowsPathSearchText(rawValue);
  const token = normalizeWindowsPathSearchText(pathValue);
  if (!token) return false;
  if (raw.includes(token)) return true;

  // The exact 8.3 alias assigned by Windows depends on filesystem collision
  // state. Match the standard prefix~number form component-by-component, but
  // require a complete protected path or a three-component protected suffix.
  // A single shared component such as Temp or AppData is not private evidence.
  const components = windowsPathComponents(pathValue);
  if (components.length < 3) return false;
  for (let start = 0; start <= components.length - 3; start += 1) {
    const sequence = components.slice(start);
    const pattern = sequence.map(windowsProtectedComponentPattern).join("\\\\");
    // A match may end at a separator or a character Windows forbids inside a
    // path component. This covers ordinary error text such as `: Access denied`
    // without accepting a protected prefix of a different valid filename.
    if (new RegExp(`(?:^|\\\\)${pattern}(?=$|\\\\|[\\x00-\\x20<>:"|?*])`, "i").test(raw)) {
      return true;
    }
  }
  return false;
}

function assertTextExcludesPathValues(rawValue, pathValues, label, { platform = process.platform } = {}) {
  const raw = String(rawValue ?? "");
  for (const value of pathValues) {
    const token = String(value ?? "");
    if (!token) continue;
    if (platform === "win32") {
      if (containsProtectedWindowsPath(raw, token)) {
        throw new Error(`${label} exposed a protected G6 filesystem path or basename.`);
      }
      continue;
    }
    const escaped = JSON.stringify(token).slice(1, -1);
    if (raw.includes(token) || (escaped && raw.includes(escaped))) {
      throw new Error(`${label} exposed a protected G6 filesystem path or basename.`);
    }
  }
}

function assertCasePathPrivacy(testCase, status, stdoutRaw, stderrRaw, pathValues, { platform = process.platform } = {}) {
  for (const [label, value] of [
    ["terminal message", status?.message],
    ["diagnostics", JSON.stringify(status?.diagnostics ?? null)],
    ["trim result", JSON.stringify(status?.trimResult ?? null)],
    ["inspection", JSON.stringify(status?.fastTrimInspection ?? null)],
    ["stdout", stdoutRaw],
    ["stderr", stderrRaw],
  ]) {
    assertTextExcludesPathValues(
      value,
      pathValues,
      `Portable G6 smoke ${testCase.id} ${label}`,
      { platform },
    );
  }
}

function buildG6SmokeEnvironment(testCase, {
  inputPath,
  outputPath,
  statusPath,
  caseRoot,
  platform = process.platform,
  baseEnv = process.env,
} = {}) {
  if (!testCase || !g6SmokeCaseById.has(testCase.id)) {
    throw new Error("Portable G6 smoke requires a canonical test case.");
  }
  assertSupportedPlatform(platform);
  for (const [label, value] of [["inputPath", inputPath], ["outputPath", outputPath], ["statusPath", statusPath], ["caseRoot", caseRoot]]) {
    if (!value || !String(value).trim()) throw new Error(`Portable G6 smoke ${label} is required.`);
  }
  const cleanBaseEnv = Object.fromEntries(
    Object.entries(baseEnv).filter(([name]) => (
      !name.startsWith("VFL_SMOKE_") && name !== "VFL_FFMPEG_PATH" && name !== "VFL_FFPROBE_PATH"
    )),
  );
  const skipPreviewInteractions = testCase.windowsLiteralKeyboard && platform === "win32"
    ? false
    : testCase.skipPreviewInteractions;
  const env = {
    ...cleanBaseEnv,
    VFL_SMOKE_INPUT: path.resolve(inputPath),
    VFL_SMOKE_OUTPUT: path.resolve(outputPath),
    VFL_SMOKE_STATUS: path.resolve(statusPath),
    VFL_SMOKE_FORMAT: testCase.outputFormat,
    VFL_SMOKE_SIZE_LIMIT_MB: "0",
    VFL_SMOKE_TRIM_START_S: String(testCase.trimStartS),
    VFL_SMOKE_TRIM_END_S: String(testCase.trimEndS),
    VFL_SMOKE_FAST_TRIM: "1",
    VFL_SMOKE_TITLE: G6_REPLACEMENT_TITLE,
    VFL_SMOKE_AUDIO_ENABLED: testCase.audioEnabled === false ? "0" : "1",
    VFL_SMOKE_STRIP_METADATA: testCase.stripMetadata === false ? "0" : "1",
    VFL_SMOKE_SOURCE_MUTATION: testCase.sourceMutation ? "1" : "0",
    VFL_SMOKE_SKIP_PREVIEW_INTERACTIONS: skipPreviewInteractions ? "1" : "0",
    VFL_SMOKE_WORKFLOW_QUEUE: testCase.workflowQueueExport ? "1" : "0",
    VFL_SMOKE_G5_QUEUE_TARGET_MISS: "0",
    VFL_SMOKE_STRICT_FIT: "0",
    VFL_SMOKE_STRICT_FIT_ALLOW_AUDIO_REMOVAL: "0",
    ...(testCase.resizeMode ? { VFL_SMOKE_RESIZE_MODE: testCase.resizeMode } : {}),
    ...(testCase.resizeWidthPx ? { VFL_SMOKE_RESIZE_WIDTH_PX: String(testCase.resizeWidthPx) } : {}),
    ...(testCase.resizeHeightPx ? { VFL_SMOKE_RESIZE_HEIGHT_PX: String(testCase.resizeHeightPx) } : {}),
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
  if (!child) return;
  const childIsRunning = () => child.exitCode === null && child.signalCode === null;
  if (platform === "win32" && child.pid) {
    let taskkillError = null;
    try {
      await runBounded("taskkill.exe", ["/PID", String(child.pid), "/T", "/F"], {
        capture: true,
        timeoutMs: 10_000,
        maxCaptureBytes: 512 * 1024,
      });
      if (!(await waitForChildExit(child, 5_000))) {
        throw new Error("Portable G6 smoke taskkill completed but the packaged app process was not reaped.");
      }
      return;
    } catch (error) {
      taskkillError = error;
    }
    if (!childIsRunning()) return;
    try {
      child.kill("SIGKILL");
    } catch {
      // Preserve the taskkill failure below.
    }
    await waitForChildExit(child, 2_500);
    throw new Error(`Portable G6 smoke could not terminate the Windows app process tree: ${taskkillError.message}`);
  }
  if (platform === "linux" && child.pid) {
    const groupExists = () => {
      try {
        process.kill(-child.pid, 0);
        return true;
      } catch (error) {
        return error?.code === "EPERM";
      }
    };
    const waitForGroupExit = async (timeoutMs) => {
      const deadline = Date.now() + timeoutMs;
      while (Date.now() < deadline) {
        if (!groupExists()) return true;
        await new Promise((resolve) => setTimeout(resolve, 100));
      }
      return !groupExists();
    };
    if (groupExists()) {
      try {
        process.kill(-child.pid, "SIGTERM");
      } catch {
        // The group may have exited between the existence check and signal.
      }
      if (await waitForGroupExit(2_500)) {
        if (!(await waitForChildExit(child, 2_500))) {
          throw new Error("Portable G6 smoke Linux process group exited but its launcher child was not reaped.");
        }
        return;
      }
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch {
        // The group may have exited between the wait and signal.
      }
      if (!(await waitForGroupExit(2_500))) {
        throw new Error("Portable G6 smoke Linux process group remained after SIGKILL.");
      }
      if (!(await waitForChildExit(child, 2_500))) {
        throw new Error("Portable G6 smoke Linux launcher child remained after process-group SIGKILL.");
      }
      return;
    }
    if (!childIsRunning()) return;
  }
  const signal = (kind) => {
    try {
      child.kill(kind);
      return true;
    } catch {
      return false;
    }
  };
  signal("SIGTERM");
  if (await waitForChildExit(child, 2_500)) return;
  signal("SIGKILL");
  if (!(await waitForChildExit(child, 2_500))) {
    throw new Error("Portable G6 smoke child remained after SIGKILL.");
  }
}

async function shutdownSmokeProcessAndLogs(child, platform, logHandles, { strict = true } = {}) {
  const failures = [];
  try {
    await terminateSmokeChild(child, platform);
  } catch (error) {
    failures.push(error);
  }
  const syncResults = await Promise.allSettled(logHandles.map((handle) => handle.sync()));
  failures.push(...syncResults.filter((result) => result.status === "rejected").map((result) => result.reason));
  const closeResults = await Promise.allSettled(logHandles.map((handle) => handle.close()));
  failures.push(...closeResults.filter((result) => result.status === "rejected").map((result) => result.reason));
  if (strict && failures.length > 0) {
    throw failures[0];
  }
}

const WINDOWS_KEYBOARD_STAGES = Object.freeze({
  "workflow-recipe-ready": Object.freeze(["ENTER"]),
  "workflow-queue-ready": Object.freeze(["ENTER"]),
  "keyboard-fast-trim-ready": Object.freeze(["RIGHT"]),
  "keyboard-fast-trim-accept-ready": Object.freeze(["SPACE"]),
  "keyboard-trim-ready": Object.freeze(["RIGHT"]),
  "keyboard-trim-incremented": Object.freeze(["LEFT", "TAB"]),
  "keyboard-crop-ready": Object.freeze(["UP"]),
  "keyboard-modal-ready": Object.freeze(["ENTER"]),
  "keyboard-modal-open": Object.freeze(["ESC"]),
});

async function sendWindowsSmokeKeys(processId, keys) {
  const helperPath = path.resolve(path.dirname(__filename), "windows-g6-send-keys.ps1");
  await runBounded("powershell.exe", [
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy", "Bypass",
    "-File", helperPath,
    "-ProcessId", String(processId),
    "-Sequence", keys.join(","),
  ], { capture: true, timeoutMs: 15_000, maxCaptureBytes: 512 * 1024 });
}

async function waitForTerminalStatus(child, statusPath, testCase, platform, timeoutSeconds, {
  inputPath,
  runtimeEvidence,
} = {}) {
  const deadline = Date.now() + timeoutSeconds * 1000;
  let lastStatus = null;
  let lastStage = null;
  const sentKeyboardStages = new Set();
  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 250));
    const status = await readJsonFileOrNull(statusPath);
    if (status) {
      lastStatus = status;
      if (status.stage !== lastStage) {
        console.log(`Portable G6 smoke ${testCase.id} stage: ${status.stage}`);
        lastStage = status.stage;
      }
      if (
        platform === "win32" &&
        testCase.windowsLiteralKeyboard === true &&
        child.pid &&
        WINDOWS_KEYBOARD_STAGES[status.stage] &&
        !sentKeyboardStages.has(status.stage)
      ) {
        sentKeyboardStages.add(status.stage);
        await sendWindowsSmokeKeys(child.pid, WINDOWS_KEYBOARD_STAGES[status.stage]);
      }
      if (
        testCase.sourceMutation === true &&
        status.stage === "fast-trim-source-mutation-ready" &&
        !runtimeEvidence?.sourceMutation
      ) {
        if (!inputPath || !runtimeEvidence) {
          throw new Error(`Portable G6 smoke ${testCase.id} source mutation hook is not configured.`);
        }
        const sentinel = Buffer.from("\nVFL G6 SOURCE MUTATION SENTINEL\n", "utf8");
        const before = await fs.stat(inputPath);
        await fs.appendFile(inputPath, sentinel);
        const after = await fs.stat(inputPath);
        if (after.size !== before.size + sentinel.length) {
          throw new Error(`Portable G6 smoke ${testCase.id} source mutation size did not change exactly once.`);
        }
        runtimeEvidence.sourceMutation = Object.freeze({
          appended: true,
          appendedBytes: sentinel.length,
          beforeSizeBytes: before.size,
          afterSizeBytes: after.size,
        });
      }
      if (status.stage === "success" || status.stage === "error") {
        if (status.stage !== testCase.terminalStage) {
          throw new Error(`Portable G6 smoke ${testCase.id} expected ${testCase.terminalStage} but reached ${status.stage}: ${status.message ?? "no message"}`);
        }
        return status;
      }
    }
    if ((child.exitCode !== null || child.signalCode !== null) && !lastStatus) {
      throw new Error(`Portable G6 smoke ${testCase.id} app exited before writing status.`);
    }
    if ((child.exitCode !== null || child.signalCode !== null) && lastStatus?.stage !== testCase.terminalStage) {
      throw new Error(`Portable G6 smoke ${testCase.id} app exited at stage ${lastStatus?.stage ?? "none"}.`);
    }
  }
  throw new Error(`Portable G6 smoke ${testCase.id} timed out. Last stage: ${lastStatus?.stage ?? "none"}`);
}

function requiredSuccessStageSequence(testCase, { platform = "linux" } = {}) {
  const workflowStages = testCase.requiredWorkflowStages ?? ["workflow-ready"];
  return Object.freeze([
    "detected",
    "input-applied",
    "probe-ready",
    "fast-trim-ready",
    ...REQUIRED_FAST_TRIM_RESET_STAGES,
    ...workflowStages,
    ...(platform === "win32" ? testCase.windowsRequiredKeyboardStages ?? [] : []),
    "interaction-ready",
    "encoding",
    "success",
  ]);
}

function requiredErrorStageSequence(testCase) {
  const prefix = ["detected", "input-applied", "probe-ready"];
  if (testCase.sourceMutation) {
    return Object.freeze([
      ...prefix,
      "fast-trim-ready",
      ...REQUIRED_FAST_TRIM_RESET_STAGES,
      "workflow-ready",
      "interaction-ready",
      "fast-trim-source-mutation-ready",
      "fast-trim-source-mutation-complete",
      "encoding",
      "error",
    ]);
  }
  if (testCase.preseedOutput) {
    return Object.freeze([
      ...prefix,
      "fast-trim-ready",
      ...REQUIRED_FAST_TRIM_RESET_STAGES,
      "workflow-ready",
      "interaction-ready",
      "encoding",
      "error",
    ]);
  }
  return Object.freeze([...prefix, "error"]);
}

function assertExactOrderedStageSubsequence(testCase, history, requiredStages) {
  const requiredSet = new Set(requiredStages);
  const observed = history.filter((stage) => requiredSet.has(stage));
  if (JSON.stringify(observed) !== JSON.stringify(requiredStages)) {
    const missing = requiredStages.filter((stage) => !history.includes(stage));
    const detail = missing.length > 0 ? ` Missing: ${missing.join(", ")}.` : "";
    throw new Error(
      `Portable G6 smoke ${testCase.id} required stages were missing, duplicated, or out of order.${detail} Saw: ${history.join(" -> ")}`,
    );
  }
}

function assertStageHistory(testCase, status, { platform = "linux" } = {}) {
  const history = Array.isArray(status?.stageHistory) ? status.stageHistory : [];
  if (history.length === 0) throw new Error(`Portable G6 smoke ${testCase.id} omitted stageHistory.`);
  const requiredStages = testCase.terminalStage === "success"
    ? requiredSuccessStageSequence(testCase, { platform })
    : requiredErrorStageSequence(testCase);
  assertExactOrderedStageSubsequence(testCase, history, requiredStages);
  if (testCase.terminalStage !== "success" && history.includes("success")) {
    throw new Error(`Portable G6 smoke ${testCase.id} reached success on a refusal path.`);
  }
}

function fastTrimResetEvidence(status) {
  const history = Array.isArray(status?.stageHistory) ? status.stageHistory : [];
  const resetTrimIndex = history.indexOf(REQUIRED_FAST_TRIM_RESET_STAGES[0]);
  const resetAllIndex = history.indexOf(REQUIRED_FAST_TRIM_RESET_STAGES[1]);
  if (resetTrimIndex < 0 || resetAllIndex <= resetTrimIndex) {
    throw new Error("Portable G6 smoke did not prove ordered Reset Trim and Reset All Fast authority clearing.");
  }
  return Object.freeze({
    resetTrimExactAndCleared: true,
    resetAllExactAndCleared: true,
  });
}

function expectedRequestedBounds(testCase) {
  return Object.freeze({
    startUs: Math.round(testCase.trimStartS * 1_000_000),
    endUs: Math.round(testCase.trimEndS * 1_000_000),
  });
}

function expectedAudioAction(testCase) {
  return testCase.expectedAudioAction ?? "copy";
}

function assertReadyInspection(testCase, inspection) {
  if (!inspection || inspection.status !== "ready") {
    throw new Error(`Portable G6 smoke ${testCase.id} omitted a ready fastTrimInspection.`);
  }
  const requested = expectedRequestedBounds(testCase);
  const expected = {
    requestedStartUs: requested.startUs,
    requestedEndUs: requested.endUs,
    effectiveStartUs: testCase.effectiveStartUs,
    effectiveEndUs: testCase.effectiveEndUs,
    startExpansionUs: requested.startUs - testCase.effectiveStartUs,
    endExpansionUs: testCase.effectiveEndUs - requested.endUs,
    requiresAcceptance: testCase.requiresAcceptance,
    videoPacketCount: testCase.expectedVideoPacketCount,
    videoAction: "copy",
    audioAction: expectedAudioAction(testCase),
  };
  for (const [field, expectedValue] of Object.entries(expected)) {
    if (inspection[field] !== expectedValue) {
      throw new Error(`Portable G6 smoke ${testCase.id} inspection ${field} mismatch. expected=${expectedValue} actual=${String(inspection[field])}`);
    }
  }
  if (!Array.isArray(inspection.reasons) || inspection.reasons.length !== 0) {
    throw new Error(`Portable G6 smoke ${testCase.id} ready inspection reported blocking reasons.`);
  }
  if (!inspection.consent || typeof inspection.consent !== "object" || Array.isArray(inspection.consent)) {
    throw new Error(`Portable G6 smoke ${testCase.id} ready inspection omitted bounded consent.`);
  }
  for (const field of [
    "requestedStartUs",
    "requestedEndUs",
    "effectiveStartUs",
    "effectiveEndUs",
    "videoPacketCount",
  ]) {
    if (inspection.consent[field] !== inspection[field]) {
      throw new Error(`Portable G6 smoke ${testCase.id} consent ${field} does not bind the inspection.`);
    }
  }
  if (!Number.isSafeInteger(inspection.consent.planSchema) || inspection.consent.planSchema <= 0) {
    throw new Error(`Portable G6 smoke ${testCase.id} consent omitted a positive planSchema.`);
  }
  if (typeof inspection.consent.confirmationToken !== "string" || !inspection.consent.confirmationToken.trim()) {
    throw new Error(`Portable G6 smoke ${testCase.id} consent omitted its path-free confirmation token.`);
  }
  return inspection;
}

function assertTrimResultContract(testCase, trimResult, diagnostics) {
  if (!trimResult || typeof trimResult !== "object" || Array.isArray(trimResult)) {
    throw new Error(`Portable G6 smoke ${testCase.id} omitted trimResult.`);
  }
  const requested = expectedRequestedBounds(testCase);
  const expected = {
    mode: "fastCopy",
    requestedStartUs: requested.startUs,
    requestedEndUs: requested.endUs,
    effectiveStartUs: testCase.effectiveStartUs,
    effectiveEndUs: testCase.effectiveEndUs,
    actualStartUs: testCase.effectiveStartUs,
    actualEndUs: testCase.effectiveEndUs,
    videoPacketCount: testCase.expectedVideoPacketCount,
    videoAction: "copy",
    audioAction: expectedAudioAction(testCase),
    ffmpegInvocations: 1,
  };
  for (const [field, expectedValue] of Object.entries(expected)) {
    if (trimResult[field] !== expectedValue) {
      throw new Error(`Portable G6 smoke ${testCase.id} trimResult ${field} mismatch. expected=${expectedValue} actual=${String(trimResult[field])}`);
    }
  }
  const preview = String(trimResult.commandPreview ?? "");
  for (const token of ["-seek_timestamp 1", "-ss", `-frames:v ${testCase.expectedVideoPacketCount}`, "-c:v copy"]) {
    if (!preview.includes(token)) {
      throw new Error(`Portable G6 smoke ${testCase.id} command preview omitted ${token}.`);
    }
  }
  if (expectedAudioAction(testCase) === "copy") {
    if (!preview.includes("-c:a copy") || !preview.includes("-shortest") || preview.includes("-an")) {
      throw new Error(`Portable G6 smoke ${testCase.id} retained audio without the bounded copy/-shortest contract.`);
    }
  } else if (!preview.includes("-an") || preview.includes("-c:a") || preview.includes("-shortest")) {
    throw new Error(`Portable G6 smoke ${testCase.id} audio-drop command did not use only -an.`);
  }
  for (const forbidden of ["-vf", "-af", "libx264", "libvpx", "libopus", " aac "]) {
    if (preview.includes(forbidden)) {
      throw new Error(`Portable G6 smoke ${testCase.id} command preview exposed a forbidden encode/filter token: ${forbidden}`);
    }
  }
  assertTextExcludesSensitiveValues(
    preview,
    [...G6_PRIVATE_METADATA_SENTINELS, ...G6_PRIVATE_STREAM_METADATA_SENTINELS, G6_REPLACEMENT_TITLE],
    `${testCase.id} command preview`,
  );

  if (!diagnostics || typeof diagnostics !== "object" || Array.isArray(diagnostics)) {
    throw new Error(`Portable G6 smoke ${testCase.id} omitted diagnostics.`);
  }
  const diagnosticExpected = {
    videoAction: "copy",
    audioAction: expectedAudioAction(testCase),
    trimMode: "fastCopy",
    trimRequestedStartUs: requested.startUs,
    trimRequestedEndUs: requested.endUs,
    trimEffectiveStartUs: testCase.effectiveStartUs,
    trimEffectiveEndUs: testCase.effectiveEndUs,
    trimActualStartUs: testCase.effectiveStartUs,
    trimActualEndUs: testCase.effectiveEndUs,
    trimVideoPacketCount: testCase.expectedVideoPacketCount,
    trimFfmpegInvocations: 1,
    attempts: 1,
  };
  for (const [field, expectedValue] of Object.entries(diagnosticExpected)) {
    if (diagnostics[field] !== expectedValue) {
      throw new Error(`Portable G6 smoke ${testCase.id} diagnostics ${field} mismatch. expected=${expectedValue} actual=${String(diagnostics[field])}`);
    }
  }
  if (diagnostics.copyFallbackReason !== undefined && diagnostics.copyFallbackReason !== null) {
    throw new Error(`Portable G6 smoke ${testCase.id} reported a forbidden copy fallback.`);
  }
  return trimResult;
}

function assertBlockedInspection(testCase, inspection) {
  if (!inspection || inspection.status !== "blocked") {
    throw new Error(`Portable G6 smoke ${testCase.id} refusal omitted a blocked fastTrimInspection.`);
  }
  const reasonCodes = Array.isArray(inspection.reasons)
    ? inspection.reasons.map((reason) => String(reason?.code ?? ""))
    : [];
  for (const expectedCode of testCase.expectedReasonCodes ?? []) {
    if (!reasonCodes.includes(expectedCode)) {
      throw new Error(`Portable G6 smoke ${testCase.id} missed reason code ${expectedCode}. Saw: ${reasonCodes.join(", ")}`);
    }
  }
  if (
    inspection.effectiveStartUs !== null ||
    inspection.effectiveEndUs !== null ||
    inspection.videoPacketCount !== null ||
    inspection.videoAction !== null ||
    inspection.audioAction !== null ||
    inspection.consent !== null
  ) {
    throw new Error(`Portable G6 smoke ${testCase.id} blocked inspection retained executable copy authority.`);
  }
  return reasonCodes;
}

function assertStreamProperties(testCase, inputProbe, outputProbe) {
  const inputVideo = streamByType(inputProbe, "video");
  const inputAudio = streamByType(inputProbe, "audio");
  const outputVideo = streamByType(outputProbe, "video");
  const outputAudio = streamByType(outputProbe, "audio");
  const retainsAudio = expectedAudioAction(testCase) === "copy";
  if (!inputVideo || !inputAudio || !outputVideo || (retainsAudio && !outputAudio)) {
    throw new Error(`Portable G6 smoke ${testCase.id} omitted a required input/output stream.`);
  }
  if ((outputProbe.streams ?? []).filter((stream) => stream?.codec_type === "video").length !== 1) {
    throw new Error(`Portable G6 smoke ${testCase.id} output video stream selection was not exact.`);
  }
  const outputAudioCount = (outputProbe.streams ?? []).filter((stream) => stream?.codec_type === "audio").length;
  if (outputAudioCount !== (retainsAudio ? 1 : 0)) {
    throw new Error(`Portable G6 smoke ${testCase.id} output audio stream selection was not exact.`);
  }
  if (
    outputVideo.codec_name !== testCase.expectedVideoCodec ||
    (retainsAudio && outputAudio?.codec_name !== testCase.expectedAudioCodec)
  ) {
    throw new Error(`Portable G6 smoke ${testCase.id} codec mismatch. expected=${testCase.expectedVideoCodec}/${testCase.expectedAudioCodec} actual=${outputVideo.codec_name}/${outputAudio?.codec_name ?? "none"}`);
  }
  for (const field of ["color_range", "color_space", "color_transfer", "color_primaries"]) {
    if ((outputVideo[field] ?? null) !== (inputVideo[field] ?? null)) {
      throw new Error(`Portable G6 smoke ${testCase.id} changed video ${field}.`);
    }
  }
  if (normalizeRational(outputVideo.sample_aspect_ratio) !== normalizeRational(inputVideo.sample_aspect_ratio)) {
    throw new Error(`Portable G6 smoke ${testCase.id} changed sample aspect ratio.`);
  }
  if (normalizeRational(outputVideo.sample_aspect_ratio) !== "1:1") {
    throw new Error(`Portable G6 smoke ${testCase.id} fixture/output is not square-pixel.`);
  }
  const retainedStreamPairs = [["video", inputVideo, outputVideo]];
  if (retainsAudio) retainedStreamPairs.push(["audio", inputAudio, outputAudio]);
  for (const [label, inputStream, outputStream] of retainedStreamPairs) {
    const dispositionFields = new Set([
      ...Object.keys(inputStream?.disposition ?? {}),
      ...Object.keys(outputStream?.disposition ?? {}),
    ]);
    for (const field of dispositionFields) {
      if (Number(outputStream?.disposition?.[field] ?? 0) !== Number(inputStream?.disposition?.[field] ?? 0)) {
        throw new Error(`Portable G6 smoke ${testCase.id} changed selected ${label} ${field} disposition.`);
      }
    }
  }
  return Object.freeze({ inputVideo, inputAudio, outputVideo, outputAudio });
}

function assertExactPacketList(expectedPackets, outputPackets, label) {
  if (expectedPackets.length !== outputPackets.length) {
    throw new Error(`Portable G6 smoke ${label} packet count mismatch. expected=${expectedPackets.length} actual=${outputPackets.length}`);
  }
  for (let index = 0; index < expectedPackets.length; index += 1) {
    if (expectedPackets[index].dataHash !== outputPackets[index].dataHash) {
      throw new Error(`Portable G6 smoke ${label} packet ${index} payload hash changed.`);
    }
  }
}

async function verifySuccessfulCase({
  testCase,
  status,
  inputPath,
  outputPath,
  ffmpegPath,
  ffprobePath,
  platform,
}) {
  if (!(await exists(outputPath))) {
    throw new Error(`Portable G6 smoke ${testCase.id} reported success without an output.`);
  }
  const outputStats = await fs.stat(outputPath);
  if (!Number.isSafeInteger(outputStats.size) || outputStats.size <= 0) {
    throw new Error(`Portable G6 smoke ${testCase.id} wrote an empty or unreportable output.`);
  }
  if (
    typeof status.outputPath !== "string" ||
    !(await portablePathsReferToSameFile(status.outputPath, outputPath, { platform }))
  ) {
    throw new Error(`Portable G6 smoke ${testCase.id} status outputPath did not identify the published artifact.`);
  }
  if (status.ok !== true || status.outputSizeBytes !== outputStats.size) {
    throw new Error(`Portable G6 smoke ${testCase.id} status bytes did not match the ${outputStats.size} byte artifact.`);
  }
  const inspection = assertReadyInspection(testCase, status.fastTrimInspection);
  const trimResult = assertTrimResultContract(testCase, status.trimResult, status.diagnostics);
  const resetEvidence = fastTrimResetEvidence(status);

  const [inputProbe, outputProbe, inputVideoPackets, outputVideoPackets, inputAudioPackets, outputAudioPackets, outputFrames] = await Promise.all([
    probeMedia(ffprobePath, inputPath),
    probeMedia(ffprobePath, outputPath),
    probePackets(ffprobePath, inputPath, "v:0"),
    probePackets(ffprobePath, outputPath, "v:0"),
    probePackets(ffprobePath, inputPath, "a:0"),
    probePackets(ffprobePath, outputPath, "a:0"),
    probeVideoFrames(ffprobePath, outputPath),
  ]);
  const streams = assertStreamProperties(testCase, inputProbe, outputProbe);
  const absoluteStartUs = testCase.sourceTimestampOffsetUs + testCase.effectiveStartUs;
  const absoluteEndUs = testCase.sourceTimestampOffsetUs + testCase.effectiveEndUs;
  const expectedVideo = expectedClosedGopPacketSlice(inputVideoPackets, absoluteStartUs, absoluteEndUs);
  if (expectedVideo.packets.length !== testCase.expectedVideoPacketCount) {
    throw new Error(`Portable G6 smoke ${testCase.id} fixture expected ${testCase.expectedVideoPacketCount} packets but selected ${expectedVideo.packets.length}.`);
  }
  assertExactPacketList(expectedVideo.packets, outputVideoPackets, `${testCase.id} video`);
  if (!outputVideoPackets[0]?.flags.includes("K")) {
    throw new Error(`Portable G6 smoke ${testCase.id} output did not begin with a key packet.`);
  }
  if (outputVideoPackets.some((packet) => packet.dataHash === expectedVideo.endKey.dataHash)) {
    throw new Error(`Portable G6 smoke ${testCase.id} copied the excluded end keyframe.`);
  }
  const videoPacketEvidence = findUniqueContiguousPacketSubsequence(inputVideoPackets, outputVideoPackets, `${testCase.id} video`);
  if (videoPacketEvidence.inputStartIndex !== expectedVideo.startIndex) {
    throw new Error(`Portable G6 smoke ${testCase.id} video subsequence started at the wrong source packet.`);
  }
  const retainsAudio = expectedAudioAction(testCase) === "copy";
  const audioPacketEvidence = retainsAudio
    ? findUniqueContiguousPacketSubsequence(inputAudioPackets, outputAudioPackets, `${testCase.id} audio`)
    : null;
  const sourceAudioInterval = audioPacketEvidence
    ? proveMatchedAudioSourceInterval(
      inputAudioPackets,
      audioPacketEvidence,
      Number(streams.inputAudio.sample_rate),
      {
        expectedStartUs: absoluteStartUs,
        expectedEndUs: absoluteEndUs,
        label: `${testCase.id} audio`,
      },
    )
    : null;
  const audioPacketMapping = audioPacketEvidence
    ? proveCopiedAudioPacketMapping(
      inputAudioPackets,
      outputAudioPackets,
      audioPacketEvidence,
      absoluteStartUs,
      { label: `${testCase.id} audio` },
    )
    : null;
  if (!retainsAudio && outputAudioPackets.length !== 0) {
    throw new Error(`Portable G6 smoke ${testCase.id} audio-drop output retained audio packets.`);
  }

  const videoBounds = framePresentationBounds(outputFrames);
  const audioBounds = retainsAudio
    ? audibleAudioBounds(outputAudioPackets, Number(streams.outputAudio.sample_rate))
    : null;
  if (!videoBounds || (retainsAudio && !audioBounds)) {
    throw new Error(`Portable G6 smoke ${testCase.id} omitted output presentation bounds.`);
  }
  const expectedDurationUs = testCase.effectiveEndUs - testCase.effectiveStartUs;
  const videoFrameDurationUs = Math.max(...outputFrames.map((frame) => frame.durationUs));
  if (Math.abs((videoBounds.endUs - videoBounds.startUs) - expectedDurationUs) > videoFrameDurationUs + 2_000) {
    throw new Error(`Portable G6 smoke ${testCase.id} video duration missed the effective interval.`);
  }
  const formatDurationUs = parseDecimalSecondsToUs(outputProbe?.format?.duration, `${testCase.id} format duration`);
  if (Math.abs(formatDurationUs - expectedDurationUs) > G6_MAX_AV_EDGE_SKEW_US) {
    throw new Error(`Portable G6 smoke ${testCase.id} container duration differs from the effective interval by more than 100 ms.`);
  }
  const startSkewUs = audioBounds ? audioBounds.startUs - videoBounds.startUs : null;
  const endSkewUs = audioBounds ? audioBounds.endUs - videoBounds.endUs : null;
  if (
    startSkewUs !== null && endSkewUs !== null &&
    (Math.abs(startSkewUs) > G6_MAX_AV_EDGE_SKEW_US || Math.abs(endSkewUs) > G6_MAX_AV_EDGE_SKEW_US)
  ) {
    throw new Error(`Portable G6 smoke ${testCase.id} A/V edge skew exceeded 100 ms. start=${startSkewUs} end=${endSkewUs}`);
  }

  const [inputRgbRaw, outputRgbRaw] = await Promise.all([
    // FFmpeg normalizes decoded filter timestamps to the stream origin even
    // when packet evidence carries a nonzero absolute container timestamp.
    decodeRgbFrames(ffmpegPath, inputPath, {
      startUs: testCase.effectiveStartUs,
      endUs: testCase.effectiveEndUs,
    }),
    decodeRgbFrames(ffmpegPath, outputPath),
  ]);
  const inputRgb = splitRawRgbFrames(inputRgbRaw, streams.inputVideo.width, streams.inputVideo.height);
  const outputRgb = splitRawRgbFrames(outputRgbRaw, streams.outputVideo.width, streams.outputVideo.height);
  if (inputRgb.frameCount !== testCase.expectedVideoPacketCount || outputRgb.frameCount !== testCase.expectedVideoPacketCount) {
    throw new Error(`Portable G6 smoke ${testCase.id} decoded frame count mismatch. input=${inputRgb.frameCount} output=${outputRgb.frameCount}`);
  }
  const allFrames = frameMeanAbsoluteError(inputRgbRaw, outputRgbRaw);
  const firstFrame = frameMeanAbsoluteError(inputRgb.first, outputRgb.first);
  const lastFrame = frameMeanAbsoluteError(inputRgb.last, outputRgb.last);
  if (
    allFrames.meanAbsoluteError > G6_MAX_FRAME_MAE ||
    firstFrame.meanAbsoluteError > G6_MAX_FRAME_MAE ||
    lastFrame.meanAbsoluteError > G6_MAX_FRAME_MAE
  ) {
    throw new Error(`Portable G6 smoke ${testCase.id} decoded frame MAE exceeded ${G6_MAX_FRAME_MAE}.`);
  }

  const metadata = assertMetadataPolicy(testCase, inputProbe, outputProbe);
  if (platform === "linux") {
    const inputMode = (await fs.stat(inputPath)).mode & 0o777;
    const outputMode = outputStats.mode & 0o777;
    if (inputMode !== outputMode) {
      throw new Error(`Portable G6 smoke ${testCase.id} output permissions mismatch. expected=${inputMode.toString(8)} actual=${outputMode.toString(8)}`);
    }
  }

  return Object.freeze({
    schemaVersion: G6_EVIDENCE_SCHEMA_VERSION,
    caseId: testCase.id,
    terminalStage: status.stage,
    stageHistory: Object.freeze([...status.stageHistory]),
    inspection: Object.freeze({
      status: inspection.status,
      requestedStartUs: inspection.requestedStartUs,
      requestedEndUs: inspection.requestedEndUs,
      effectiveStartUs: inspection.effectiveStartUs,
      effectiveEndUs: inspection.effectiveEndUs,
      startExpansionUs: inspection.startExpansionUs,
      endExpansionUs: inspection.endExpansionUs,
      requiresAcceptance: inspection.requiresAcceptance,
      videoPacketCount: inspection.videoPacketCount,
    }),
    trimResult: Object.freeze({
      mode: trimResult.mode,
      requestedStartUs: trimResult.requestedStartUs,
      requestedEndUs: trimResult.requestedEndUs,
      effectiveStartUs: trimResult.effectiveStartUs,
      effectiveEndUs: trimResult.effectiveEndUs,
      actualStartUs: trimResult.actualStartUs,
      actualEndUs: trimResult.actualEndUs,
      videoPacketCount: trimResult.videoPacketCount,
      videoAction: trimResult.videoAction,
      audioAction: trimResult.audioAction,
      ffmpegInvocations: trimResult.ffmpegInvocations,
    }),
    resetEvidence,
    output: Object.freeze({
      sizeBytes: outputStats.size,
      formatDurationUs,
      videoCodec: streams.outputVideo.codec_name,
      audioCodec: streams.outputAudio?.codec_name ?? null,
      videoPacketCount: outputVideoPackets.length,
      audioPacketCount: outputAudioPackets.length,
    }),
    packetEvidence: Object.freeze({
      video: videoPacketEvidence,
      audio: audioPacketEvidence
        ? Object.freeze({ ...audioPacketEvidence, ...audioPacketMapping })
        : null,
    }),
    frameEvidence: Object.freeze({
      frameCount: outputRgb.frameCount,
      allMeanAbsoluteError: allFrames.meanAbsoluteError,
      allMaximumError: allFrames.maximumError,
      firstMeanAbsoluteError: firstFrame.meanAbsoluteError,
      firstMaximumError: firstFrame.maximumError,
      lastMeanAbsoluteError: lastFrame.meanAbsoluteError,
      lastMaximumError: lastFrame.maximumError,
    }),
    avEvidence: Object.freeze({
      videoStartUs: videoBounds.startUs,
      videoEndUs: videoBounds.endUs,
      audioStartUs: audioBounds?.startUs ?? null,
      audioEndUs: audioBounds?.endUs ?? null,
      startSkewUs,
      endSkewUs,
      sourceAudioStartUs: sourceAudioInterval?.startUs ?? null,
      sourceAudioEndUs: sourceAudioInterval?.endUs ?? null,
      sourceExpectedStartUs: sourceAudioInterval?.expectedStartUs ?? null,
      sourceExpectedEndUs: sourceAudioInterval?.expectedEndUs ?? null,
      sourceStartSkewUs: sourceAudioInterval?.startSkewUs ?? null,
      sourceEndSkewUs: sourceAudioInterval?.endSkewUs ?? null,
    }),
    metadata,
  });
}

function assertPreExecutionFailureDiagnostics(testCase, diagnostics, {
  failureStage,
  reasonIncludes,
} = {}) {
  if (!diagnostics || typeof diagnostics !== "object" || Array.isArray(diagnostics)) {
    throw new Error(`Portable G6 smoke ${testCase.id} pre-execution failure omitted backend diagnostics.`);
  }
  if (failureStage && diagnostics.failureStage !== failureStage) {
    throw new Error(
      `Portable G6 smoke ${testCase.id} failureStage mismatch. expected=${failureStage} actual=${String(diagnostics.failureStage)}`,
    );
  }
  if (
    reasonIncludes &&
    !String(diagnostics.failureReason ?? "").toLowerCase().includes(String(reasonIncludes).toLowerCase())
  ) {
    throw new Error(`Portable G6 smoke ${testCase.id} backend failureReason omitted ${reasonIncludes}.`);
  }
  if (
    diagnostics.videoAction !== null ||
    diagnostics.audioAction !== null ||
    diagnostics.attempts !== 0 ||
    diagnostics.passes !== 0 ||
    diagnostics.trimFfmpegInvocations !== null
  ) {
    throw new Error(`Portable G6 smoke ${testCase.id} backend diagnostics did not prove zero execution attempts.`);
  }
  const preview = String(diagnostics.commandPreview ?? "");
  if (
    !preview.includes("No FFmpeg command") ||
    /(?:^|\s)-(?:c:[av]|i|ss|vf|af|frames:v|map)(?:\s|$)|\b(?:libx264|libvpx|libopus|aac)\b/i.test(preview)
  ) {
    throw new Error(`Portable G6 smoke ${testCase.id} backend diagnostics retained executable command evidence.`);
  }
  return diagnostics;
}

async function verifyErrorCase(testCase, status, outputPath, preseed, runtimeEvidence) {
  if (status.ok !== false) throw new Error(`Portable G6 smoke ${testCase.id} refusal did not report ok=false.`);
  if (status.trimResult !== undefined && status.trimResult !== null) {
    throw new Error(`Portable G6 smoke ${testCase.id} refusal retained trimResult.`);
  }
  let reasonCodes = [];
  if (testCase.expectedReasonCodes) {
    reasonCodes = assertBlockedInspection(testCase, status.fastTrimInspection);
  }
  if (testCase.expectedErrorIncludes && !String(status.message ?? "").toLowerCase().includes(testCase.expectedErrorIncludes.toLowerCase())) {
    throw new Error(`Portable G6 smoke ${testCase.id} error mismatch. expected=${testCase.expectedErrorIncludes} actual=${status.message ?? "none"}`);
  }
  if (testCase.preseedOutput) {
    assertReadyInspection(testCase, status.fastTrimInspection);
    if (!preseed || !(await exists(outputPath))) {
      throw new Error(`Portable G6 smoke ${testCase.id} lost the pre-seeded destination.`);
    }
    const bytes = await fs.readFile(outputPath);
    const sha256 = crypto.createHash("sha256").update(bytes).digest("hex");
    if (bytes.length !== preseed.sizeBytes || sha256 !== preseed.sha256) {
      throw new Error(`Portable G6 smoke ${testCase.id} changed the pre-seeded destination.`);
    }
  } else if (await exists(outputPath)) {
    throw new Error(`Portable G6 smoke ${testCase.id} published output despite a required Fast refusal.`);
  }
  const diagnostics = status.diagnostics;
  if (testCase.sourceMutation) {
    assertReadyInspection(testCase, status.fastTrimInspection);
    if (!runtimeEvidence?.sourceMutation?.appended) {
      throw new Error(`Portable G6 smoke ${testCase.id} did not append its source mutation sentinel.`);
    }
    assertPreExecutionFailureDiagnostics(testCase, diagnostics, {
      failureStage: "fast-trim-consent",
      reasonIncludes: "stale",
    });
  } else if (testCase.preseedOutput) {
    assertPreExecutionFailureDiagnostics(testCase, diagnostics, {
      failureStage: "backend",
      reasonIncludes: testCase.expectedErrorIncludes,
    });
  } else if (diagnostics) {
    if (diagnostics.videoAction || diagnostics.audioAction || Number(diagnostics.attempts ?? 0) !== 0) {
      throw new Error(`Portable G6 smoke ${testCase.id} refusal reported an encode/copy attempt.`);
    }
    const preview = String(diagnostics.commandPreview ?? "");
    for (const forbidden of ["libx264", "libvpx", "libopus", "-c:v copy", "-c:a copy"]) {
      if (preview.includes(forbidden)) throw new Error(`Portable G6 smoke ${testCase.id} refusal retained a command attempt.`);
    }
  }
  return Object.freeze({
    schemaVersion: G6_EVIDENCE_SCHEMA_VERSION,
    caseId: testCase.id,
    terminalStage: status.stage,
    stageHistory: Object.freeze([...status.stageHistory]),
    reasonCodes: Object.freeze(reasonCodes),
    output: null,
    noClobber: testCase.preseedOutput ? Object.freeze({ preserved: true, sha256: preseed.sha256, sizeBytes: preseed.sizeBytes }) : null,
    sourceMutation: testCase.sourceMutation ? runtimeEvidence.sourceMutation : null,
  });
}

function assertFixtureMetadata(probe, fixtureId) {
  const values = tagValues(probe?.format?.tags);
  for (const sentinel of G6_PRIVATE_METADATA_SENTINELS) {
    if (!values.some((value) => value.includes(sentinel))) {
      throw new Error(`Portable G6 fixture ${fixtureId} omitted metadata sentinel ${sentinel}.`);
    }
  }
  if (!streamTagValues(probe, "video").some((value) => value.includes(G6_PRIVATE_STREAM_METADATA_SENTINELS[0]))) {
    throw new Error(`Portable G6 fixture ${fixtureId} omitted selected-video metadata.`);
  }
  if (!streamTagValues(probe, "audio").some((value) => value.includes(G6_PRIVATE_STREAM_METADATA_SENTINELS[1]))) {
    throw new Error(`Portable G6 fixture ${fixtureId} omitted selected-audio metadata.`);
  }
}

async function assertFixtureContract(fixtureId, ffprobePath, inputPath) {
  const [probe, videoPackets, audioPackets] = await Promise.all([
    probeMedia(ffprobePath, inputPath),
    probePackets(ffprobePath, inputPath, "v:0"),
    probePackets(ffprobePath, inputPath, "a:0"),
  ]);
  const video = streamByType(probe, "video");
  const audio = streamByType(probe, "audio");
  if (!video || !audio) throw new Error(`Portable G6 fixture ${fixtureId} omitted video or audio.`);
  assertFixtureMetadata(probe, fixtureId);
  const nonDefaultAudioDispositions = ["h264-closed", "h264-open-gop"].includes(fixtureId)
    ? ["comment", "forced", "hearing_impaired"]
    : ["forced"];
  if (!nonDefaultAudioDispositions.every((field) => Number(audio?.disposition?.[field] ?? 0) === 1)) {
    throw new Error(`Portable G6 fixture ${fixtureId} omitted required non-default selected-audio dispositions.`);
  }
  if (
    !["h264-closed", "h264-open-gop"].includes(fixtureId) &&
    ["comment", "hearing_impaired"].some((field) => Number(audio?.disposition?.[field] ?? 0) !== 0)
  ) {
    throw new Error(`Portable G6 fixture ${fixtureId} claimed unsupported Matroska/WebM audio dispositions.`);
  }
  if (normalizeRational(video.sample_aspect_ratio) !== "1:1") {
    throw new Error(`Portable G6 fixture ${fixtureId} is not square-pixel.`);
  }

  switch (fixtureId) {
    case "h264-closed": {
      if (video.codec_name !== "h264" || audio.codec_name !== "aac") {
        throw new Error("Portable G6 closed fixture codec mismatch.");
      }
      if (Number(video.has_b_frames ?? 0) <= 0) {
        throw new Error("Portable G6 closed fixture omitted required H.264 B-frames.");
      }
      if (videoPackets.some((packet) => !Number.isSafeInteger(packet.dtsUs))) {
        throw new Error("Portable G6 closed fixture omitted decode-order timestamps.");
      }
      for (const keyUs of [2_000_000, 4_000_000, 6_000_000]) {
        if (hasLeadingPicturesAfterKey(videoPackets, keyUs)) {
          throw new Error(`Portable G6 closed fixture unexpectedly has leading pictures at ${keyUs} us.`);
        }
      }
      const slice = expectedClosedGopPacketSlice(videoPackets, 2_000_000, 6_000_000);
      if (slice.packets.length !== G6_EXPECTED_VIDEO_PACKET_COUNT) {
        throw new Error(`Portable G6 closed fixture selected ${slice.packets.length} packets, expected ${G6_EXPECTED_VIDEO_PACKET_COUNT}.`);
      }
      break;
    }
    case "vp9-opus-nonzero-start": {
      if (video.codec_name !== "vp9" || audio.codec_name !== "opus") {
        throw new Error("Portable G6 nonzero-start fixture codec mismatch.");
      }
      if (parseDecimalSecondsToUs(video.start_time, "nonzero fixture video start") !== 5_000_000) {
        throw new Error("Portable G6 VP9 fixture did not retain the +5 second timestamp origin.");
      }
      if (videoPackets.some((packet) => !Number.isSafeInteger(packet.dtsUs))) {
        throw new Error("Portable G6 VP9 fixture omitted decode-order timestamps.");
      }
      const audioPacketStartUs = Math.min(...audioPackets.map((packet) => packet.ptsUs));
      if (audioPacketStartUs >= 5_000_000 || audioPacketStartUs < 4_900_000) {
        throw new Error(`Portable G6 VP9/Opus fixture did not retain bounded negative packet priming relative to video: ${audioPacketStartUs} us.`);
      }
      const audibleBounds = audibleAudioBounds(audioPackets, Number(audio.sample_rate));
      if (!audibleBounds || Math.abs(audibleBounds.startUs - 5_000_000) > 2_000) {
        throw new Error(`Portable G6 VP9/Opus fixture has unbounded audible priming: ${audibleBounds?.startUs ?? "missing"} us.`);
      }
      const slice = expectedClosedGopPacketSlice(videoPackets, 7_000_000, 11_000_000);
      if (slice.packets.length !== G6_EXPECTED_VIDEO_PACKET_COUNT) {
        throw new Error(`Portable G6 VP9 fixture selected ${slice.packets.length} packets, expected ${G6_EXPECTED_VIDEO_PACKET_COUNT}.`);
      }
      break;
    }
    case "h264-opus-incompatible":
      if (video.codec_name !== "h264" || audio.codec_name !== "opus") {
        throw new Error("Portable G6 incompatible-audio fixture codec mismatch.");
      }
      break;
    case "h264-open-gop":
      if (video.codec_name !== "h264" || audio.codec_name !== "aac") {
        throw new Error("Portable G6 open-GOP fixture codec mismatch.");
      }
      if (Number(video.has_b_frames ?? 0) <= 0) {
        throw new Error("Portable G6 open-GOP fixture omitted required H.264 B-frames.");
      }
      if (videoPackets.some((packet) => !Number.isSafeInteger(packet.dtsUs))) {
        throw new Error("Portable G6 open-GOP fixture omitted decode-order timestamps.");
      }
      if (!hasLeadingPicturesAfterKey(videoPackets, 2_000_000)) {
        throw new Error("Portable G6 open-GOP fixture did not expose deterministic leading pictures after the 2 second key.");
      }
      break;
    default:
      throw new Error(`Unknown portable G6 fixture: ${fixtureId}`);
  }
  return Object.freeze({ probe, videoPackets });
}

function fixtureBuildOrder(selectedCases) {
  const selected = new Set(selectedCases.map((testCase) => testCase.fixtureId));
  if (selected.has("h264-opus-incompatible")) selected.add("h264-closed");
  return ["h264-closed", "vp9-opus-nonzero-start", "h264-opus-incompatible", "h264-open-gop"]
    .filter((fixtureId) => selected.has(fixtureId));
}

async function assertFixtureRootOwnership(fixtureRoot, fixtureIds) {
  const expectedNames = new Set(
    fixtureIds.map((fixtureId) => path.basename(fixtureOutputPath(fixtureId, fixtureRoot))),
  );
  const entries = await fs.readdir(fixtureRoot, { withFileTypes: true });
  const unexpected = entries
    .filter((entry) => !entry.isFile() || !expectedNames.has(entry.name))
    .map((entry) => entry.name);
  if (unexpected.length > 0) {
    throw new Error(`Portable G6 smoke found unowned fixture-root artifact(s): ${unexpected.join(", ")}`);
  }
}

async function consumeWorkflowRecoveryArtifact(testCase, fixtureRoot, fixtureIds) {
  if (!testCase.workflowQueueExport) return null;
  const ownedNames = new Set(
    fixtureIds.map((fixtureId) => path.basename(fixtureOutputPath(fixtureId, fixtureRoot))),
  );
  const entries = await fs.readdir(fixtureRoot, { withFileTypes: true });
  const unownedFiles = entries.filter((entry) => entry.isFile() && !ownedNames.has(entry.name));
  const source = path.parse(fixtureOutputPath(testCase.fixtureId, fixtureRoot));
  const expectedName = `${source.name}-2${source.ext}`;
  if (unownedFiles.length !== 1 || unownedFiles[0].name !== expectedName) {
    throw new Error("Portable G6 workflow did not produce exactly its owned queued-recovery artifact.");
  }
  const artifactPath = path.resolve(fixtureRoot, expectedName);
  const bytes = await fs.readFile(artifactPath);
  if (bytes.length === 0) throw new Error("Portable G6 workflow produced an empty queued-recovery artifact.");
  const evidence = Object.freeze({
    produced: true,
    sizeBytes: bytes.length,
    sha256: crypto.createHash("sha256").update(bytes).digest("hex"),
    deleted: true,
  });
  await fs.rm(artifactPath, { force: true });
  if (await exists(artifactPath)) {
    throw new Error("Portable G6 workflow queued-recovery artifact remained after runner cleanup.");
  }
  return evidence;
}

async function verifyPortablePreflight(portableDir, {
  platform,
  ffmpegPath,
  appPath,
  ffprobePath,
} = {}) {
  const version = await getProjectVersion();
  const manifest = await validatePayloadManifest({ portableDir, version, platform });
  const capabilityPath = path.resolve(
    portableDir,
    ffmpegSidecarResourceTarget,
    FFMPEG_CAPABILITY_CONTRACT_FILE_NAME,
  );
  const contract = await assertCapabilityContractCopy(capabilityPath);
  await verifyFfmpegCapabilityContract({
    ffmpegPath,
    contract,
    label: `Portable G6 ${platform} FFmpeg`,
  });
  const expectedPaths = [appPath, ffmpegPath, ffprobePath, capabilityPath]
    .map((absolutePath) => path.relative(portableDir, absolutePath).split(path.sep).join("/"));
  const manifestPaths = new Set(manifest.files.map((entry) => entry.path));
  const missing = expectedPaths.filter((relativePath) => !manifestPaths.has(relativePath));
  if (missing.length > 0) {
    throw new Error(`Portable G6 payload manifest omitted runtime file(s): ${missing.join(", ")}`);
  }
  return Object.freeze({
    version,
    capabilitySchemaVersion: contract.schemaVersion,
    payloadFileCount: manifest.files.length,
  });
}

async function runPortableG6FastTrimSmoke({
  portableDir = process.env.VFL_PORTABLE_DIR || getPortableOutputDir(),
  platform = process.platform,
  caseIds,
  timeoutSeconds = 300,
  fixtureTimeoutSeconds = 120,
  keepSuccessArtifacts = process.env.VFL_G6_SMOKE_KEEP_ARTIFACTS === "1",
  verifyPreflight = true,
} = {}) {
  assertSupportedPlatform(platform);
  if (platform !== process.platform) {
    throw new Error(`Portable G6 smoke cannot execute ${platform} artifacts on ${process.platform}.`);
  }
  if (!Number.isFinite(timeoutSeconds) || timeoutSeconds <= 0 || timeoutSeconds > 600) {
    throw new Error("Portable G6 smoke timeoutSeconds must be between 1 and 600.");
  }
  if (!Number.isFinite(fixtureTimeoutSeconds) || fixtureTimeoutSeconds <= 0 || fixtureTimeoutSeconds > 300) {
    throw new Error("Portable G6 smoke fixtureTimeoutSeconds must be between 1 and 300.");
  }

  const selectedCases = resolveG6SmokeCases(caseIds);
  const { portableRoot, appPath, ffmpegPath, ffprobePath } = getPortableG5Paths(portableDir, { platform });
  for (const requiredPath of [appPath, ffmpegPath, ffprobePath]) {
    if (!(await exists(requiredPath))) {
      throw new Error(`Portable G6 smoke could not find required extracted payload file: ${requiredPath}`);
    }
  }
  const preflight = verifyPreflight
    ? await verifyPortablePreflight(portableRoot, { platform, ffmpegPath, appPath, ffprobePath })
    : null;

  const smokeRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-portable-g6-"));
  const fixtureRoot = path.resolve(smokeRoot, "fixtures");
  const resultRoot = path.resolve(smokeRoot, "results");
  await Promise.all([
    fs.mkdir(fixtureRoot, { recursive: true }),
    fs.mkdir(resultRoot, { recursive: true }),
  ]);
  let failed = false;
  const evidence = [];

  try {
    const fixtureIds = fixtureBuildOrder(selectedCases);
    for (const fixtureId of fixtureIds) {
      for (const fixtureCommand of buildG6FixtureCommands(fixtureId, fixtureRoot)) {
        if (fixtureCommand.requiresFixtureId) {
          const dependencyPath = fixtureOutputPath(fixtureCommand.requiresFixtureId, fixtureRoot);
          if (!(await exists(dependencyPath))) {
            throw new Error(`Portable G6 fixture ${fixtureId} requires missing ${fixtureCommand.requiresFixtureId}.`);
          }
        }
        await runBounded(ffmpegPath, fixtureCommand.args, {
          cwd: fixtureRoot,
          timeoutMs: fixtureTimeoutSeconds * 1000,
        });
        if (!(await exists(fixtureCommand.outputPath))) {
          throw new Error(`Portable G6 fixture command did not create ${fixtureCommand.outputPath}`);
        }
      }
      await assertFixtureContract(fixtureId, ffprobePath, fixtureOutputPath(fixtureId, fixtureRoot));
      console.log(`Portable G6 fixture ready: ${fixtureId}`);
    }
    await assertFixtureRootOwnership(fixtureRoot, fixtureIds);

    for (const [caseIndex, testCase] of selectedCases.entries()) {
      const caseRoot = path.resolve(resultRoot, `case-${String(caseIndex + 1).padStart(2, "0")}`);
      await fs.mkdir(caseRoot, { recursive: true });
      const fixturePath = fixtureOutputPath(testCase.fixtureId, fixtureRoot);
      const inputPath = testCase.perCaseFixtureCopy
        ? path.resolve(caseRoot, `case-source${path.extname(fixturePath)}`)
        : fixturePath;
      if (testCase.perCaseFixtureCopy) await fs.copyFile(fixturePath, inputPath);
      const outputPath = path.resolve(caseRoot, `case-output.${testCase.outputFormat}`);
      const statusPath = path.resolve(caseRoot, "status.json");
      const stdoutPath = path.resolve(caseRoot, "app.stdout.log");
      const stderrPath = path.resolve(caseRoot, "app.stderr.log");
      let preseed = null;
      if (testCase.preseedOutput) {
        const sentinel = Buffer.from("VFL G6 NO-CLOBBER SENTINEL\n", "utf8");
        await fs.writeFile(outputPath, sentinel);
        preseed = Object.freeze({
          sizeBytes: sentinel.length,
          sha256: crypto.createHash("sha256").update(sentinel).digest("hex"),
        });
      }
      const smokeEnv = buildG6SmokeEnvironment(testCase, {
        inputPath,
        outputPath,
        statusPath,
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
      let logsClosed = false;
      const runtimeEvidence = {};
      try {
        const launch = buildPortableG5Launch(appPath, { platform, env: smokeEnv });
        child = spawn(launch.command, launch.args, {
          cwd: portableRoot,
          detached: platform === "linux",
          env: smokeEnv,
          stdio: ["ignore", stdoutFile.fd, stderrFile.fd],
          windowsHide: true,
        });
        const status = await waitForTerminalStatus(child, statusPath, testCase, platform, timeoutSeconds, {
          inputPath,
          runtimeEvidence,
        });
        assertStageHistory(testCase, status, { platform });
        let caseEvidence = testCase.terminalStage === "success"
          ? await verifySuccessfulCase({
            testCase,
            status,
            inputPath,
            outputPath,
            ffmpegPath,
            ffprobePath,
            platform,
          })
          : await verifyErrorCase(testCase, status, outputPath, preseed, runtimeEvidence);
        const workflowArtifact = await consumeWorkflowRecoveryArtifact(testCase, fixtureRoot, fixtureIds);
        await assertFixtureRootOwnership(fixtureRoot, fixtureIds);
        caseEvidence = Object.freeze({ ...caseEvidence, workflowArtifact });

        try {
          await shutdownSmokeProcessAndLogs(child, platform, [stdoutFile, stderrFile]);
        } finally {
          logsClosed = true;
        }
        const [rawStatus, stdoutRaw, stderrRaw] = await Promise.all([
          fs.readFile(statusPath, "utf8"),
          fs.readFile(stdoutPath, "utf8"),
          fs.readFile(stderrPath, "utf8"),
        ]);
        for (const [label, raw] of [["status", rawStatus], ["stdout", stdoutRaw], ["stderr", stderrRaw]]) {
          assertTextExcludesSensitiveValues(
            raw,
            [...G6_PRIVATE_METADATA_SENTINELS, ...G6_PRIVATE_STREAM_METADATA_SENTINELS],
            `Portable G6 smoke ${testCase.id} ${label}`,
          );
        }
        const pathPrivacyValues = [
          smokeRoot,
          fixtureRoot,
          resultRoot,
          caseRoot,
          inputPath,
          outputPath,
          statusPath,
          stdoutPath,
          stderrPath,
          smokeEnv.XDG_DATA_HOME,
          smokeEnv.XDG_CONFIG_HOME,
          smokeEnv.XDG_CACHE_HOME,
          smokeEnv.WEBVIEW2_USER_DATA_FOLDER,
          ...[
            smokeRoot,
            fixtureRoot,
            resultRoot,
            caseRoot,
            inputPath,
            outputPath,
            statusPath,
            stdoutPath,
            stderrPath,
            smokeEnv.XDG_DATA_HOME,
            smokeEnv.XDG_CONFIG_HOME,
            smokeEnv.XDG_CACHE_HOME,
            smokeEnv.WEBVIEW2_USER_DATA_FOLDER,
          ].filter(Boolean).map((protectedPath) => path.basename(protectedPath)),
          ".vfl-",
        ].filter(Boolean);
        assertCasePathPrivacy(testCase, status, stdoutRaw, stderrRaw, pathPrivacyValues, { platform });
        await assertFixtureRootOwnership(fixtureRoot, fixtureIds);
        const evidenceRaw = `${JSON.stringify({ ...caseEvidence, platform }, null, 2)}\n`;
        assertTextExcludesSensitiveValues(
          evidenceRaw,
          [
            ...G6_PRIVATE_METADATA_SENTINELS,
            ...G6_PRIVATE_STREAM_METADATA_SENTINELS,
            "confirmationToken",
            "fastCopyConsent",
            "sourceIdentity",
          ],
          `Portable G6 smoke ${testCase.id} retained evidence`,
        );
        assertTextExcludesPathValues(
          evidenceRaw,
          pathPrivacyValues,
          `Portable G6 smoke ${testCase.id} retained evidence`,
          { platform },
        );
        await fs.writeFile(path.resolve(caseRoot, "evidence.json"), evidenceRaw, "utf8");
        const tempResidue = (await fs.readdir(caseRoot)).filter((name) => name.startsWith(".vfl-"));
        if (tempResidue.length > 0) {
          throw new Error(`Portable G6 smoke ${testCase.id} left unpublished temporary output(s): ${tempResidue.join(", ")}`);
        }
        evidence.push(caseEvidence);
        console.log(`Portable G6 smoke passed: ${testCase.id}`);
      } finally {
        if (!logsClosed) {
          await shutdownSmokeProcessAndLogs(child, platform, [stdoutFile, stderrFile], { strict: false });
          logsClosed = true;
        }
      }
    }
    console.log(`Portable G6 smoke passed ${selectedCases.length} cases on ${platform}.`);
    if (keepSuccessArtifacts) console.log(`Portable G6 smoke evidence retained at ${smokeRoot}`);
    return Object.freeze({
      smokeRoot: keepSuccessArtifacts ? smokeRoot : null,
      preflight,
      evidence: Object.freeze(evidence),
    });
  } catch (error) {
    failed = true;
    console.error(`Portable G6 smoke preserved failure evidence at ${smokeRoot}`);
    throw error;
  } finally {
    if (!failed && !keepSuccessArtifacts) {
      await fs.rm(smokeRoot, { recursive: true, force: true, maxRetries: 8, retryDelay: 250 });
    }
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === __filename) {
  const caseIds = String(process.env.VFL_G6_SMOKE_CASES ?? "")
    .split(",")
    .map((caseId) => caseId.trim())
    .filter(Boolean);
  await runPortableG6FastTrimSmoke({ caseIds: caseIds.length > 0 ? caseIds : undefined });
}

export {
  G6_EVIDENCE_SCHEMA_VERSION,
  G6_AUDIO_TIMESTAMP_TOLERANCE_US,
  G6_EXPECTED_VIDEO_PACKET_COUNT,
  G6_FIXTURE_DURATION_S,
  G6_FRAME_RATE,
  G6_GOP_SECONDS,
  G6_MAX_AV_EDGE_SKEW_US,
  G6_MAX_FRAME_MAE,
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
  fixtureOutputPath,
  fastTrimResetEvidence,
  frameMeanAbsoluteError,
  framePresentationBounds,
  g6SmokeCases,
  hasLeadingPicturesAfterKey,
  normalizeRational,
  packetHashDigest,
  parseDecimalSecondsToUs,
  proveMatchedAudioSourceInterval,
  proveCopiedAudioPacketMapping,
  requiredErrorStageSequence,
  requiredSuccessStageSequence,
  resolveG6SmokeCases,
  runPortableG6FastTrimSmoke,
  safeInteger,
  shutdownSmokeProcessAndLogs,
  splitRawRgbFrames,
  usToSecondsString,
  verifyPortablePreflight,
};
