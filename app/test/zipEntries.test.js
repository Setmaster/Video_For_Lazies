import assert from "node:assert/strict";
import test from "node:test";
import zlib from "node:zlib";

import {
  MAX_UPDATE_ARCHIVE_BYTES,
  MAX_UPDATE_ZIP_ENTRIES,
  readVerifiedZipEntriesFromBuffer,
} from "../scripts/zipEntries.mjs";

function storedZip(entries) {
  const localParts = [];
  const centralParts = [];
  let localOffset = 0;

  for (const entry of entries) {
    const nameBytes = Buffer.from(entry.name, "utf8");
    const bytes = Buffer.from(entry.bytes ?? "");
    const compressionMethod = entry.compressionMethod ?? 0;
    const compressedBytes = compressionMethod === 8 ? zlib.deflateRawSync(bytes) : bytes;
    const crc32 = zlib.crc32(bytes) >>> 0;
    const declaredSize = entry.declaredSize ?? bytes.length;
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0, 6);
    local.writeUInt16LE(compressionMethod, 8);
    local.writeUInt32LE(crc32, 14);
    local.writeUInt32LE(compressedBytes.length, 18);
    local.writeUInt32LE(declaredSize, 22);
    local.writeUInt16LE(nameBytes.length, 26);
    localParts.push(local, nameBytes, compressedBytes);

    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE((3 << 8) | 20, 4);
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE(0, 8);
    central.writeUInt16LE(compressionMethod, 10);
    central.writeUInt32LE(crc32, 16);
    central.writeUInt32LE(compressedBytes.length, 20);
    central.writeUInt32LE(declaredSize, 24);
    central.writeUInt16LE(nameBytes.length, 28);
    central.writeUInt32LE(((entry.mode ?? 0o100644) << 16) >>> 0, 38);
    central.writeUInt32LE(localOffset, 42);
    centralParts.push(central, nameBytes);
    localOffset += local.length + nameBytes.length + compressedBytes.length;
  }

  const centralDirectory = Buffer.concat(centralParts);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(centralDirectory.length, 12);
  end.writeUInt32LE(localOffset, 16);
  return Buffer.concat([...localParts, centralDirectory, end]);
}

test("verified ZIP reader checks every regular file and preserves Unix modes", () => {
  const archive = storedZip([
    { name: "Video_For_Lazies/", mode: 0o040755 },
    { name: "Video_For_Lazies/payload.bin", bytes: "payload", mode: 0o100640 },
  ]);
  const entries = readVerifiedZipEntriesFromBuffer(archive, { archiveLabel: "fixture.zip" });
  assert.equal(entries.length, 2);
  assert.equal(entries[0].isDirectory, true);
  assert.equal(entries[1].isDirectory, false);
  assert.equal(entries[1].bytes.toString("utf8"), "payload");
  assert.equal(entries[1].unixMode & 0o777, 0o640);
});

test("verified ZIP reader rejects corrupt payload bytes by CRC", () => {
  const archive = storedZip([
    { name: "Video_For_Lazies/payload.bin", bytes: "payload", mode: 0o100644 },
  ]);
  const corrupted = Buffer.from(archive);
  const payloadOffset = 30 + Buffer.byteLength("Video_For_Lazies/payload.bin");
  corrupted[payloadOffset] ^= 0xff;
  assert.throws(
    () => readVerifiedZipEntriesFromBuffer(corrupted, { archiveLabel: "corrupt.zip" }),
    /CRC mismatch/i,
  );
});

test("verified ZIP reader rejects symlinks and normalized duplicate names", () => {
  assert.throws(
    () => readVerifiedZipEntriesFromBuffer(storedZip([
      { name: "Video_For_Lazies/link", bytes: "target", mode: 0o120777 },
    ])),
    /symbolic-link/i,
  );
  assert.throws(
    () => readVerifiedZipEntriesFromBuffer(storedZip([
      { name: "Video_For_Lazies/file", bytes: "one" },
      { name: "Video_For_Lazies\\file", bytes: "two" },
    ])),
    /duplicate ZIP entries/i,
  );
});

test("verified ZIP reader rejects a lone raw backslash entry name", () => {
  assert.throws(
    () => readVerifiedZipEntriesFromBuffer(storedZip([
      { name: "Video_For_Lazies\\payload.bin", bytes: "payload" },
    ])),
    /non-canonical ZIP path separator/i,
  );
});

test("verified ZIP reader rejects oversized declared entries before inflation", () => {
  assert.throws(
    () => readVerifiedZipEntriesFromBuffer(storedZip([
      {
        name: "Video_For_Lazies/oversized.bin",
        declaredSize: 256 * 1024 * 1024 + 1,
      },
    ])),
    /oversized uncompressed ZIP entry/i,
  );
});

test("verified ZIP reader bounds actual DEFLATE output by the declared size", () => {
  assert.throws(
    () => readVerifiedZipEntriesFromBuffer(storedZip([
      {
        name: "Video_For_Lazies/expands.bin",
        bytes: Buffer.alloc(1024 * 1024, 0x61),
        compressionMethod: 8,
        declaredSize: 1,
      },
    ])),
    /could not inflate|invalid uncompressed size/i,
  );
});

test("verified ZIP reader rejects inconsistent stored sizes before copying", () => {
  assert.throws(
    () => readVerifiedZipEntriesFromBuffer(storedZip([
      {
        name: "Video_For_Lazies/stored.bin",
        bytes: "payload",
        declaredSize: 1,
      },
    ])),
    /invalid stored sizes/i,
  );
});

test("verified ZIP reader mirrors updater archive and entry-count limits", () => {
  assert.equal(MAX_UPDATE_ARCHIVE_BYTES, 750 * 1024 * 1024);
  assert.equal(MAX_UPDATE_ZIP_ENTRIES, 20_000);
  const entries = Array.from({ length: MAX_UPDATE_ZIP_ENTRIES + 1 }, (_, index) => ({
    name: `Video_For_Lazies/entry-${index}`,
  }));
  assert.throws(
    () => readVerifiedZipEntriesFromBuffer(storedZip(entries)),
    /file count outside the supported range/i,
  );
});
