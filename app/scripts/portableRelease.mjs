import crypto from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";

import { getPortableReleaseParentDir } from "./ffmpegBundle.mjs";

export function getPortableArchiveBaseName({ platform = process.platform, arch = process.arch } = {}) {
  if (platform !== "win32") {
    throw new Error(`Portable release artifacts are only supported on Windows hosts (got ${platform}).`);
  }
  return `Video_For_Lazies-win-${arch}`;
}

export function getPortableZipPath(options) {
  return path.resolve(getPortableReleaseParentDir(), `${getPortableArchiveBaseName(options)}.zip`);
}

export function getPortableSevenZipPath(options) {
  return path.resolve(getPortableReleaseParentDir(), `${getPortableArchiveBaseName(options)}.7z`);
}

export function getPortableChecksumPath() {
  return path.resolve(getPortableReleaseParentDir(), "SHA256SUMS.txt");
}

export async function sha256File(filePath) {
  const hash = crypto.createHash("sha256");
  hash.update(await fs.readFile(filePath));
  return hash.digest("hex");
}

export async function buildChecksumLines(filePaths) {
  const entries = [];
  for (const filePath of [...filePaths].sort((left, right) => left.localeCompare(right))) {
    entries.push(`${await sha256File(filePath)}  ${path.basename(filePath)}`);
  }
  return entries.join("\n") + (entries.length ? "\n" : "");
}
