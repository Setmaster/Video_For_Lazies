import crypto from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";

import { getPortableReleaseParentDir } from "./ffmpegBundle.mjs";
import { normalizeVersionInput } from "./versioning.mjs";

export function getPortablePlatformLabel(platform = process.platform) {
  if (platform === "win32") {
    return "win";
  }
  if (platform === "linux") {
    return "linux";
  }
  throw new Error(`Portable release artifacts are only supported on Windows and Linux hosts (got ${platform}).`);
}

export function getPortableArchLabel(arch = process.arch) {
  if (arch !== "x64") {
    throw new Error(`Portable release artifacts are only supported for x64 hosts (got ${arch}).`);
  }
  return "x64";
}

export function getPortableTargetLabel({ platform = process.platform, arch = process.arch } = {}) {
  return `${getPortablePlatformLabel(platform)}-${getPortableArchLabel(arch)}`;
}

export function getPortableExecutableName({ platform = process.platform } = {}) {
  return getPortablePlatformLabel(platform) === "win" ? "Video_For_Lazies.exe" : "video_for_lazies";
}

export function getPortableArchiveBaseName({
  platform = process.platform,
  arch = process.arch,
  version,
} = {}) {
  return `Video_For_Lazies-v${normalizeVersionInput(version)}-${getPortableTargetLabel({ platform, arch })}`;
}

export function getPortableZipPath(options) {
  return path.resolve(getPortableReleaseParentDir(), `${getPortableArchiveBaseName(options)}.zip`);
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
