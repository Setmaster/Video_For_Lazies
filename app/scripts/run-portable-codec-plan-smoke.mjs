import path from "node:path";
import url from "node:url";

import { getPortableOutputDir } from "./ffmpegBundle.mjs";
import { runLinuxPortableExportSmoke } from "./run-portable-export-smoke.mjs";

const __filename = url.fileURLToPath(import.meta.url);

const codecPlanCases = Object.freeze([
  Object.freeze({
    label: "MP4 compatible remux",
    codecFixture: "h264-aac-mkv",
    outputFormat: "mp4",
    expectedVideoCodec: "h264",
    expectedAudioCodec: "aac",
    expectedVideoAction: "copy",
    expectedAudioAction: "copy",
  }),
  Object.freeze({
    label: "MP4 video copy and audio re-encode",
    codecFixture: "h264-opus-mkv",
    outputFormat: "mp4",
    expectedVideoCodec: "h264",
    expectedAudioCodec: "aac",
    expectedVideoAction: "copy",
    expectedAudioAction: "encode",
  }),
  Object.freeze({
    label: "MP4 video re-encode and audio copy",
    codecFixture: "vp9-aac-mkv",
    outputFormat: "mp4",
    expectedVideoCodec: "h264",
    expectedAudioCodec: "aac",
    expectedVideoAction: "encode",
    expectedAudioAction: "copy",
  }),
  Object.freeze({
    label: "MP4 incompatible full re-encode",
    codecFixture: "vp8-vorbis-webm",
    outputFormat: "mp4",
    expectedVideoCodec: "h264",
    expectedAudioCodec: "aac",
    expectedVideoAction: "encode",
    expectedAudioAction: "encode",
  }),
  Object.freeze({
    label: "WebM compatible remux",
    codecFixture: "vp9-opus-mkv",
    outputFormat: "webm",
    expectedVideoCodec: "vp9",
    expectedAudioCodec: "opus",
    expectedVideoAction: "copy",
    expectedAudioAction: "copy",
  }),
  Object.freeze({
    label: "WebM incompatible full re-encode",
    codecFixture: "h264-aac-mp4",
    outputFormat: "webm",
    expectedVideoCodec: "vp9",
    expectedAudioCodec: "opus",
    expectedVideoAction: "encode",
    expectedAudioAction: "encode",
  }),
]);

async function runPortableCodecPlanSmoke({
  portableDir = process.env.VFL_PORTABLE_DIR || getPortableOutputDir(),
  timeoutSeconds = 90,
} = {}) {
  if (process.platform !== "linux") {
    console.log("Skipping the Linux-only portable codec-plan matrix on this host.");
    return;
  }

  for (const testCase of codecPlanCases) {
    console.log(`Portable codec-plan smoke: ${testCase.label}`);
    await runLinuxPortableExportSmoke({
      portableDir,
      timeoutSeconds,
      useFullDuration: true,
      ...testCase,
    });
  }

  console.log(`Portable codec-plan smoke passed ${codecPlanCases.length} cases.`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === __filename) {
  await runPortableCodecPlanSmoke();
}

export { codecPlanCases, runPortableCodecPlanSmoke };
