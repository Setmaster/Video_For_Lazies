import zlib from "node:zlib";

const END_OF_CENTRAL_DIRECTORY_SIGNATURE = 0x06054b50;
const CENTRAL_DIRECTORY_ENTRY_SIGNATURE = 0x02014b50;
const LOCAL_FILE_HEADER_SIGNATURE = 0x04034b50;
const END_OF_CENTRAL_DIRECTORY_SIZE = 22;
const MAX_ZIP_COMMENT_SIZE = 0xffff;
export const MAX_UPDATE_ARCHIVE_BYTES = 750 * 1024 * 1024;
export const MAX_UPDATE_ZIP_ENTRIES = 20_000;
const MAX_ZIP_ENTRY_UNCOMPRESSED_SIZE = 256 * 1024 * 1024;
const MAX_ZIP_TOTAL_UNCOMPRESSED_SIZE = 512 * 1024 * 1024;

function findEndOfCentralDirectory(archiveBytes, archiveLabel) {
  const firstCandidate = Math.max(
    0,
    archiveBytes.length - END_OF_CENTRAL_DIRECTORY_SIZE - MAX_ZIP_COMMENT_SIZE,
  );
  for (
    let offset = archiveBytes.length - END_OF_CENTRAL_DIRECTORY_SIZE;
    offset >= firstCandidate;
    offset -= 1
  ) {
    if (archiveBytes.readUInt32LE(offset) !== END_OF_CENTRAL_DIRECTORY_SIGNATURE) continue;
    const commentLength = archiveBytes.readUInt16LE(offset + 20);
    if (offset + END_OF_CENTRAL_DIRECTORY_SIZE + commentLength === archiveBytes.length) {
      return offset;
    }
  }
  throw new Error(`${archiveLabel} has no valid ZIP central directory.`);
}

function readCentralDirectoryEntries(archiveBytes, archiveLabel) {
  if (archiveBytes.length > MAX_UPDATE_ARCHIVE_BYTES) {
    throw new Error(`${archiveLabel} exceeds the supported update archive size.`);
  }
  if (archiveBytes.length < END_OF_CENTRAL_DIRECTORY_SIZE) {
    throw new Error(`${archiveLabel} is too small to be a ZIP archive.`);
  }

  const eocdOffset = findEndOfCentralDirectory(archiveBytes, archiveLabel);
  const diskNumber = archiveBytes.readUInt16LE(eocdOffset + 4);
  const centralDirectoryDisk = archiveBytes.readUInt16LE(eocdOffset + 6);
  const entryCountOnDisk = archiveBytes.readUInt16LE(eocdOffset + 8);
  const entryCount = archiveBytes.readUInt16LE(eocdOffset + 10);
  const centralDirectorySize = archiveBytes.readUInt32LE(eocdOffset + 12);
  const centralDirectoryOffset = archiveBytes.readUInt32LE(eocdOffset + 16);

  if (diskNumber !== 0 || centralDirectoryDisk !== 0 || entryCountOnDisk !== entryCount) {
    throw new Error(`${archiveLabel} uses an unsupported multi-disk ZIP layout.`);
  }
  if (
    entryCount === 0xffff
    || centralDirectorySize === 0xffffffff
    || centralDirectoryOffset === 0xffffffff
  ) {
    throw new Error(`${archiveLabel} uses unsupported ZIP64 metadata.`);
  }
  if (entryCount === 0 || entryCount > MAX_UPDATE_ZIP_ENTRIES) {
    throw new Error(`${archiveLabel} has a file count outside the supported range.`);
  }
  if (centralDirectoryOffset + centralDirectorySize > eocdOffset) {
    throw new Error(`${archiveLabel} has an invalid ZIP central directory range.`);
  }

  const entries = [];
  let offset = centralDirectoryOffset;
  for (let index = 0; index < entryCount; index += 1) {
    if (
      offset + 46 > archiveBytes.length
      || archiveBytes.readUInt32LE(offset) !== CENTRAL_DIRECTORY_ENTRY_SIGNATURE
    ) {
      throw new Error(`${archiveLabel} has an invalid ZIP central directory entry.`);
    }

    const flags = archiveBytes.readUInt16LE(offset + 8);
    const compressionMethod = archiveBytes.readUInt16LE(offset + 10);
    const crc32 = archiveBytes.readUInt32LE(offset + 16);
    const compressedSize = archiveBytes.readUInt32LE(offset + 20);
    const uncompressedSize = archiveBytes.readUInt32LE(offset + 24);
    const fileNameLength = archiveBytes.readUInt16LE(offset + 28);
    const extraLength = archiveBytes.readUInt16LE(offset + 30);
    const commentLength = archiveBytes.readUInt16LE(offset + 32);
    const versionMadeBy = archiveBytes.readUInt16LE(offset + 4);
    const externalAttributes = archiveBytes.readUInt32LE(offset + 38);
    const localHeaderOffset = archiveBytes.readUInt32LE(offset + 42);
    const entryEnd = offset + 46 + fileNameLength + extraLength + commentLength;
    if (entryEnd > archiveBytes.length) {
      throw new Error(`${archiveLabel} has a truncated ZIP central directory entry.`);
    }
    if (
      compressedSize === 0xffffffff
      || uncompressedSize === 0xffffffff
      || localHeaderOffset === 0xffffffff
    ) {
      throw new Error(`${archiveLabel} uses unsupported ZIP64 entry metadata.`);
    }

    const nameBytes = Buffer.from(archiveBytes.subarray(offset + 46, offset + 46 + fileNameLength));
    const name = nameBytes.toString("utf8");
    if (!Buffer.from(name, "utf8").equals(nameBytes) || name.includes("\0")) {
      throw new Error(`${archiveLabel} contains an invalid ZIP entry name.`);
    }
    entries.push({
      name,
      nameBytes,
      normalizedName: name.replaceAll("\\", "/"),
      flags,
      compressionMethod,
      crc32,
      compressedSize,
      uncompressedSize,
      localHeaderOffset,
      versionMadeBy,
      externalAttributes,
      centralDirectoryOffset,
    });
    offset = entryEnd;
  }

  if (offset !== centralDirectoryOffset + centralDirectorySize) {
    throw new Error(`${archiveLabel} has inconsistent ZIP central directory metadata.`);
  }
  return entries;
}

function inflateEntry(archiveBytes, entry, archiveLabel) {
  const { localHeaderOffset } = entry;
  if (
    localHeaderOffset + 30 > archiveBytes.length
    || archiveBytes.readUInt32LE(localHeaderOffset) !== LOCAL_FILE_HEADER_SIGNATURE
  ) {
    throw new Error(`${archiveLabel} has an invalid local header for ${entry.name}.`);
  }
  const localFlags = archiveBytes.readUInt16LE(localHeaderOffset + 6);
  const localCompressionMethod = archiveBytes.readUInt16LE(localHeaderOffset + 8);
  if ((entry.flags & 0x1) !== 0 || (localFlags & 0x1) !== 0) {
    throw new Error(`${archiveLabel} contains an encrypted ZIP entry: ${entry.name}.`);
  }
  if (localFlags !== entry.flags || localCompressionMethod !== entry.compressionMethod) {
    throw new Error(`${archiveLabel} has inconsistent local ZIP metadata for ${entry.name}.`);
  }

  const fileNameLength = archiveBytes.readUInt16LE(localHeaderOffset + 26);
  const extraLength = archiveBytes.readUInt16LE(localHeaderOffset + 28);
  const localNameBytes = archiveBytes.subarray(localHeaderOffset + 30, localHeaderOffset + 30 + fileNameLength);
  if (!Buffer.from(localNameBytes).equals(entry.nameBytes)) {
    throw new Error(`${archiveLabel} has mismatched local and central names for ${entry.name}.`);
  }
  if ((entry.flags & 0x08) === 0) {
    if (
      archiveBytes.readUInt32LE(localHeaderOffset + 14) !== entry.crc32
      || archiveBytes.readUInt32LE(localHeaderOffset + 18) !== entry.compressedSize
      || archiveBytes.readUInt32LE(localHeaderOffset + 22) !== entry.uncompressedSize
    ) {
      throw new Error(`${archiveLabel} has inconsistent local sizes or CRC for ${entry.name}.`);
    }
  }
  const dataOffset = localHeaderOffset + 30 + fileNameLength + extraLength;
  const dataEnd = dataOffset + entry.compressedSize;
  if (dataEnd > entry.centralDirectoryOffset) {
    throw new Error(`${archiveLabel} has truncated ZIP data for ${entry.name}.`);
  }

  const compressedBytes = archiveBytes.subarray(dataOffset, dataEnd);
  let result;
  if (entry.compressionMethod === 0) {
    if (entry.compressedSize !== entry.uncompressedSize) {
      throw new Error(`${archiveLabel} has invalid stored sizes for ${entry.name}.`);
    }
    result = Buffer.from(compressedBytes);
  } else if (entry.compressionMethod === 8) {
    try {
      result = zlib.inflateRawSync(compressedBytes, {
        maxOutputLength: Math.max(1, entry.uncompressedSize),
      });
    } catch (error) {
      throw new Error(`${archiveLabel} could not inflate ${entry.name}: ${error.message}`);
    }
  } else {
    throw new Error(
      `${archiveLabel} uses unsupported ZIP compression method ${entry.compressionMethod} for ${entry.name}.`,
    );
  }

  if (result.length !== entry.uncompressedSize) {
    throw new Error(`${archiveLabel} has an invalid uncompressed size for ${entry.name}.`);
  }
  if ((zlib.crc32(result) >>> 0) !== entry.crc32) {
    throw new Error(`${archiveLabel} has a CRC mismatch for ${entry.name}.`);
  }
  return result;
}

function zipEntryKind(entry, archiveLabel) {
  const creatorSystem = entry.versionMadeBy >>> 8;
  const unixMode = creatorSystem === 3 ? (entry.externalAttributes >>> 16) & 0xffff : null;
  const unixFileType = unixMode === null ? null : unixMode & 0o170000;
  if (unixFileType === 0o120000) {
    throw new Error(`${archiveLabel} contains a symbolic-link ZIP entry: ${entry.name}.`);
  }
  if (
    unixFileType !== null
    && unixFileType !== 0
    && unixFileType !== 0o040000
    && unixFileType !== 0o100000
  ) {
    throw new Error(`${archiveLabel} contains a non-regular ZIP entry: ${entry.name}.`);
  }
  const nameDirectory = entry.normalizedName.endsWith("/");
  const modeDirectory = unixFileType === 0o040000;
  if (nameDirectory !== modeDirectory && unixFileType !== null && unixFileType !== 0) {
    throw new Error(`${archiveLabel} has conflicting directory metadata for ${entry.name}.`);
  }
  return {
    isDirectory: nameDirectory || modeDirectory,
    unixMode,
  };
}

export function readVerifiedZipEntriesFromBuffer(archiveBytes, { archiveLabel = "ZIP archive" } = {}) {
  if (!Buffer.isBuffer(archiveBytes)) {
    throw new Error("archiveBytes must be a Buffer.");
  }
  const entries = readCentralDirectoryEntries(archiveBytes, archiveLabel);
  const normalizedNames = new Set();
  const localOffsets = new Set();
  let totalUncompressedSize = 0;
  for (const entry of entries) {
    if (normalizedNames.has(entry.normalizedName)) {
      throw new Error(`${archiveLabel} contains duplicate ZIP entries for: ${entry.normalizedName}`);
    }
    if (entry.name !== entry.normalizedName) {
      throw new Error(`${archiveLabel} contains a non-canonical ZIP path separator: ${entry.name}`);
    }
    if (localOffsets.has(entry.localHeaderOffset)) {
      throw new Error(`${archiveLabel} contains overlapping ZIP entry metadata.`);
    }
    normalizedNames.add(entry.normalizedName);
    localOffsets.add(entry.localHeaderOffset);
    if (entry.uncompressedSize > MAX_ZIP_ENTRY_UNCOMPRESSED_SIZE) {
      throw new Error(`${archiveLabel} contains an oversized uncompressed ZIP entry: ${entry.name}.`);
    }
    totalUncompressedSize += entry.uncompressedSize;
    if (
      !Number.isSafeInteger(totalUncompressedSize)
      || totalUncompressedSize > MAX_ZIP_TOTAL_UNCOMPRESSED_SIZE
    ) {
      throw new Error(`${archiveLabel} exceeds the supported uncompressed ZIP size.`);
    }
  }

  return entries.map((entry) => {
    const kind = zipEntryKind(entry, archiveLabel);
    const bytes = inflateEntry(archiveBytes, entry, archiveLabel);
    if (kind.isDirectory && bytes.length !== 0) {
      throw new Error(`${archiveLabel} contains a non-empty directory ZIP entry: ${entry.name}.`);
    }
    return Object.freeze({
      name: entry.normalizedName,
      bytes,
      isDirectory: kind.isDirectory,
      unixMode: kind.unixMode,
    });
  });
}

export function readUniqueZipEntryFromBuffer(archiveBytes, entryPath, { archiveLabel = "ZIP archive" } = {}) {
  if (!Buffer.isBuffer(archiveBytes)) {
    throw new Error("archiveBytes must be a Buffer.");
  }
  if (typeof entryPath !== "string" || entryPath.length === 0) {
    throw new Error("entryPath must be a non-empty string.");
  }

  const normalizedEntryPath = entryPath.replaceAll("\\", "/");
  const matches = readCentralDirectoryEntries(archiveBytes, archiveLabel)
    .filter((entry) => entry.normalizedName === normalizedEntryPath);
  if (matches.length === 0) {
    throw new Error(`${archiveLabel} is missing required ZIP entry: ${normalizedEntryPath}`);
  }
  if (matches.length !== 1) {
    throw new Error(`${archiveLabel} contains duplicate ZIP entries for: ${normalizedEntryPath}`);
  }
  return inflateEntry(archiveBytes, matches[0], archiveLabel);
}
