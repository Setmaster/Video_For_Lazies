import fs from "node:fs/promises";
import path from "node:path";
import zlib from "node:zlib";

const UTF8_FLAG = 0x0800;
const UNIX_CREATOR_VERSION = (3 << 8) | 20;
const VERSION_NEEDED = 20;

async function collectEntries(rootDir, currentDir = rootDir) {
  const relativeDirectory = path.relative(path.dirname(rootDir), currentDir)
    .split(path.sep)
    .join("/");
  const directoryStat = await fs.lstat(currentDir);
  if (!directoryStat.isDirectory() || directoryStat.isSymbolicLink()) {
    throw new Error(`Release ZIP fixture root must be a regular directory: ${currentDir}`);
  }

  const entries = [{
    name: `${relativeDirectory}/`,
    bytes: Buffer.alloc(0),
    mode: 0o040000 | (directoryStat.mode & 0o777),
  }];
  const children = await fs.readdir(currentDir, { withFileTypes: true });
  children.sort((left, right) => left.name.localeCompare(right.name));

  for (const child of children) {
    const absolutePath = path.resolve(currentDir, child.name);
    const stat = await fs.lstat(absolutePath);
    if (stat.isSymbolicLink()) {
      throw new Error(`Release ZIP fixtures must not contain symlinks: ${absolutePath}`);
    }
    if (stat.isDirectory()) {
      entries.push(...await collectEntries(rootDir, absolutePath));
      continue;
    }
    if (!stat.isFile()) {
      throw new Error(`Release ZIP fixtures must contain regular files only: ${absolutePath}`);
    }
    entries.push({
      name: path.relative(path.dirname(rootDir), absolutePath).split(path.sep).join("/"),
      bytes: await fs.readFile(absolutePath),
      mode: 0o100000 | (stat.mode & 0o777),
    });
  }
  return entries;
}

function buildStoredZip(entries) {
  if (entries.length > 0xffff) {
    throw new Error("Release ZIP fixture contains too many entries for ZIP32.");
  }

  const localParts = [];
  const centralParts = [];
  let localOffset = 0;

  for (const entry of entries) {
    const nameBytes = Buffer.from(entry.name, "utf8");
    const size = entry.bytes.length;
    if (nameBytes.length > 0xffff || size > 0xffffffff || localOffset > 0xffffffff) {
      throw new Error(`Release ZIP fixture entry exceeds ZIP32 limits: ${entry.name}`);
    }
    const crc32 = zlib.crc32(entry.bytes) >>> 0;

    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(VERSION_NEEDED, 4);
    local.writeUInt16LE(UTF8_FLAG, 6);
    local.writeUInt32LE(crc32, 14);
    local.writeUInt32LE(size, 18);
    local.writeUInt32LE(size, 22);
    local.writeUInt16LE(nameBytes.length, 26);
    localParts.push(local, nameBytes, entry.bytes);

    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(UNIX_CREATOR_VERSION, 4);
    central.writeUInt16LE(VERSION_NEEDED, 6);
    central.writeUInt16LE(UTF8_FLAG, 8);
    central.writeUInt32LE(crc32, 16);
    central.writeUInt32LE(size, 20);
    central.writeUInt32LE(size, 24);
    central.writeUInt16LE(nameBytes.length, 28);
    central.writeUInt32LE((entry.mode << 16) >>> 0, 38);
    central.writeUInt32LE(localOffset, 42);
    centralParts.push(central, nameBytes);

    localOffset += local.length + nameBytes.length + size;
  }

  const centralDirectory = Buffer.concat(centralParts);
  if (localOffset + centralDirectory.length > 0xffffffff) {
    throw new Error("Release ZIP fixture exceeds ZIP32 limits.");
  }
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(centralDirectory.length, 12);
  end.writeUInt32LE(localOffset, 16);
  return Buffer.concat([...localParts, centralDirectory, end]);
}

export async function createReleaseZipFixture(portableDir, zipPath) {
  await fs.mkdir(path.dirname(zipPath), { recursive: true });
  const entries = await collectEntries(path.resolve(portableDir));
  await fs.writeFile(zipPath, buildStoredZip(entries));
}
