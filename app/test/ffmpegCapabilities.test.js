import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import {
  FFMPEG_CAPABILITY_CONTRACT_FILE_NAME,
  assertCapabilityContractCopy,
  assertReleaseRequiredFfmpegCapabilities,
  evaluateFfmpegCapabilityContract,
  ffmpegCapabilityContractPath,
  formatFfmpegCapabilitySummary,
  parseFfmpegEncoderNames,
  parseFfmpegFilterNames,
  parseFfmpegVersion,
  probeFfmpegCapabilities,
  readFfmpegCapabilityContract,
  validateFfmpegCapabilityContract,
} from "../scripts/ffmpegCapabilities.mjs";

const minimalContract = {
  schemaVersion: 1,
  features: {
    coreExport: {
      releaseRequired: true,
      encoders: ["libx264"],
      filters: ["scale"],
    },
    optionalFeature: {
      releaseRequired: false,
      encoders: [],
      filters: ["optional_filter"],
    },
  },
};

test("shipped FFmpeg contract has the stable Goal 3 feature schema", async () => {
  const contract = await readFfmpegCapabilityContract();
  assert.deepEqual(JSON.parse(JSON.stringify(contract)), {
    schemaVersion: 1,
    features: {
      coreExport: {
        releaseRequired: true,
        encoders: ["aac", "libmp3lame", "libopus", "libvorbis", "libvpx", "libvpx-vp9", "libx264", "mpeg4", "png"],
        filters: ["aresample", "asetpts", "atempo", "atrim", "crop", "cropdetect", "eq", "fps", "loudnorm", "noise", "scale", "setpts", "transpose", "trim"],
      },
      hdrToSdr: {
        releaseRequired: true,
        encoders: ["libx264"],
        filters: ["format", "tonemap", "zscale"],
      },
      sarNormalize: {
        releaseRequired: true,
        encoders: [],
        filters: ["scale", "setsar"],
      },
      reverseLoop: {
        releaseRequired: true,
        encoders: [],
        filters: ["areverse", "asetnsamples", "asplit", "concat", "reverse", "split"],
      },
    },
  });
  assert.equal(FFMPEG_CAPABILITY_CONTRACT_FILE_NAME, "FFMPEG_CAPABILITIES.json");
});

test("contract schema rejects unknown fields, invalid versions, and duplicate names", () => {
  assert.throws(
    () => validateFfmpegCapabilityContract({ ...minimalContract, unexpected: true }),
    /unsupported field/i,
  );
  assert.throws(
    () => validateFfmpegCapabilityContract({ ...minimalContract, schemaVersion: 2 }),
    /unsupported.*schemaVersion/i,
  );
  assert.throws(
    () => validateFfmpegCapabilityContract({
      ...minimalContract,
      features: {
        coreExport: {
          releaseRequired: true,
          encoders: ["libx264", "libx264"],
          filters: [],
        },
      },
    }),
    /duplicate.*libx264/i,
  );
  assert.throws(
    () => validateFfmpegCapabilityContract({
      ...minimalContract,
      features: {
        coreExport: {
          releaseRequired: "yes",
          encoders: [],
          filters: [],
        },
      },
    }),
    /releaseRequired must be a boolean/i,
  );
});

test("FFmpeg text inventories parse only capability rows", () => {
  const encoders = parseFfmpegEncoderNames(`
Encoders:
 V..... = Video
 V....D libx264              libx264 H.264
 A....D aac                  AAC
 bad header
`);
  assert.deepEqual([...encoders], ["libx264", "aac"]);

  const filters = parseFfmpegFilterNames(`
Filters:
 T.. = Timeline support
 .. scale             V->V       Scale video
 .S tonemap           V->V       Tone map
 garbage
`);
  assert.deepEqual([...filters], ["scale", "tonemap"]);
  assert.equal(parseFfmpegVersion("ffmpeg version n8.1.2-test Copyright"), "n8.1.2-test");
  assert.throws(() => parseFfmpegVersion("not ffmpeg"), /unrecognized version/i);
});

test("feature evaluation reports exact missing encoders and filters", () => {
  const result = evaluateFfmpegCapabilityContract(minimalContract, {
    ffmpegVersion: "test-build",
    encoders: new Set(["libx264"]),
    filters: new Set(["other_filter"]),
  });
  assert.deepEqual(result.features.coreExport.missingEncoders, []);
  assert.deepEqual(result.features.coreExport.missingFilters, ["scale"]);
  assert.equal(result.features.coreExport.available, false);
  assert.equal(result.features.optionalFeature.available, false);
  assert.throws(
    () => assertReleaseRequiredFfmpegCapabilities(result, "Fixture FFmpeg"),
    /coreExport: filters scale/,
  );
  assert.match(formatFfmpegCapabilitySummary(result), /coreExport=missing/);
});

test("optional missing capabilities do not fail the release-required gate", () => {
  const result = evaluateFfmpegCapabilityContract(minimalContract, {
    ffmpegVersion: "test-build",
    encoders: new Set(["libx264"]),
    filters: new Set(["scale"]),
  });
  assert.equal(assertReleaseRequiredFfmpegCapabilities(result), result);
  assert.equal(result.features.optionalFeature.available, false);
});

test("capability probing fails closed when the target is not FFmpeg", async () => {
  await assert.rejects(
    probeFfmpegCapabilities(process.execPath, { timeoutMs: 5_000 }),
    /exited with code|empty encoder|unrecognized version/i,
  );
});

test("packaged contract must be a byte-for-byte source copy", async () => {
  const tempRoot = await fs.mkdtemp(path.join(os.tmpdir(), "vfl-capability-contract-test-"));
  try {
    const copiedPath = path.join(tempRoot, FFMPEG_CAPABILITY_CONTRACT_FILE_NAME);
    await fs.copyFile(ffmpegCapabilityContractPath, copiedPath);
    const contract = await assertCapabilityContractCopy(copiedPath);
    assert.equal(contract.schemaVersion, 1);

    await fs.appendFile(copiedPath, "\n");
    await assert.rejects(assertCapabilityContractCopy(copiedPath), /not a verbatim copy/i);
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});
