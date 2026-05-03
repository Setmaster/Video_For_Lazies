import crypto from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";

import {
  ffmpegSidecarResourceTarget,
  portableLinuxDesktopFileName,
  portableLinuxIconFileName,
} from "./ffmpegBundle.mjs";
import {
  getPortableArchiveBaseName,
  getPortableExecutableName,
  getPortableTargetLabel,
  sha256File,
} from "./portableRelease.mjs";
import { normalizeVersionInput } from "./versioning.mjs";

export const APP_ID = "com.setmaster.video-for-lazies";
export const UPDATE_CHANNEL = "stable";
export const PAYLOAD_MANIFEST_SCHEMA = "com.setmaster.video-for-lazies.payload-manifest.v1";
export const UPDATE_MANIFEST_SCHEMA = "com.setmaster.video-for-lazies.update-manifest.v1";
export const PAYLOAD_MANIFEST_FILE_NAME = "VFL_PAYLOAD_MANIFEST.json";
export const UPDATE_MANIFEST_FILE_NAME = "vfl-update-manifest-v1.json";
export const UPDATE_MANIFEST_SIGNATURE_FILE_NAME = `${UPDATE_MANIFEST_FILE_NAME}.sig`;
export const PORTABLE_ROOT_DIR_NAME = "Video_For_Lazies";
export const UPDATE_RELEASE_BASE_URL = "https://github.com/Setmaster/Video_For_Lazies/releases";

const POSIX_SEP_PATTERN = /\\/g;
const LOWER_HEX_SHA256_PATTERN = /^[a-f0-9]{64}$/;
const RELEASE_ZIP_PATTERN = /^Video_For_Lazies-v(.+)-(linux|win)-x64\.zip$/;
const PAYLOAD_SIDECAR_PATTERN = /^Video_For_Lazies-v(.+)-(linux|win)-x64\.payload-manifest\.json$/;

export function getUpdateTargetLabel({ platform = process.platform, arch = process.arch } = {}) {
  const portableTarget = getPortableTargetLabel({ platform, arch });
  if (portableTarget === "win-x64") return "windows-x64";
  if (portableTarget === "linux-x64") return "linux-x64";
  throw new Error(`Unsupported updater target: ${portableTarget}`);
}

function updateTargetFromPortableTarget(portableTarget) {
  if (portableTarget === "win-x64") return "windows-x64";
  if (portableTarget === "linux-x64") return "linux-x64";
  throw new Error(`Unsupported portable target: ${portableTarget}`);
}

function portableTargetFromUpdateTarget(updateTarget) {
  if (updateTarget === "windows-x64") return "win-x64";
  if (updateTarget === "linux-x64") return "linux-x64";
  throw new Error(`Unsupported update target: ${updateTarget}`);
}

export function getPayloadManifestPath(portableDir) {
  return path.resolve(portableDir, PAYLOAD_MANIFEST_FILE_NAME);
}

export function getPayloadManifestSidecarPath({
  releaseDir,
  platform = process.platform,
  arch = process.arch,
  version,
} = {}) {
  return path.resolve(
    releaseDir,
    `${getPortableArchiveBaseName({ platform, arch, version })}.payload-manifest.json`,
  );
}

function normalizePortableRelativePath(relativePath) {
  return relativePath.split(path.sep).join("/").replace(POSIX_SEP_PATTERN, "/");
}

export function assertSafePortableRelativePath(relativePath) {
  if (typeof relativePath !== "string" || relativePath.trim() === "") {
    throw new Error("Portable payload path must be a non-empty string.");
  }
  if (relativePath.includes("\0")) {
    throw new Error(`Portable payload path contains a null byte: ${relativePath}`);
  }
  if (relativePath.startsWith("/") || relativePath.startsWith("\\")) {
    throw new Error(`Portable payload path must be relative: ${relativePath}`);
  }
  if (/^[A-Za-z]:/.test(relativePath) || relativePath.startsWith("//") || relativePath.startsWith("\\\\")) {
    throw new Error(`Portable payload path must not include a drive or UNC prefix: ${relativePath}`);
  }

  const normalized = path.posix.normalize(relativePath.replace(POSIX_SEP_PATTERN, "/"));
  if (normalized === "." || normalized.startsWith("../") || normalized === "..") {
    throw new Error(`Portable payload path escapes the portable root: ${relativePath}`);
  }
  return normalized;
}

async function walkPortableFiles(rootDir, currentDir = rootDir) {
  const entries = await fs.readdir(currentDir, { withFileTypes: true });
  const files = [];

  for (const entry of entries) {
    const entryPath = path.resolve(currentDir, entry.name);
    const stat = await fs.lstat(entryPath);
    const relativePath = normalizePortableRelativePath(path.relative(rootDir, entryPath));

    if (stat.isSymbolicLink()) {
      throw new Error(`Portable payload must not contain symlinks: ${relativePath}`);
    }
    if (entry.isDirectory()) {
      files.push(...await walkPortableFiles(rootDir, entryPath));
      continue;
    }
    if (!entry.isFile()) {
      throw new Error(`Portable payload must contain regular files only: ${relativePath}`);
    }
    if (relativePath === PAYLOAD_MANIFEST_FILE_NAME) {
      continue;
    }
    files.push({ absolutePath: entryPath, relativePath, stat });
  }

  return files;
}

async function buildPayloadFileEntry({ absolutePath, relativePath, stat, platform }) {
  const safePath = assertSafePortableRelativePath(relativePath);
  return {
    path: safePath,
    sha256: await sha256File(absolutePath),
    sizeBytes: stat.size,
    mode: platform === "linux" ? stat.mode & 0o777 : null,
    kind: "file",
  };
}

export async function generatePayloadManifest({
  portableDir,
  version,
  platform = process.platform,
  arch = process.arch,
} = {}) {
  if (!portableDir) {
    throw new Error("portableDir is required.");
  }

  const normalizedVersion = normalizeVersionInput(version);
  const files = (await walkPortableFiles(portableDir))
    .sort((left, right) => left.relativePath.localeCompare(right.relativePath));
  const manifest = {
    schema: PAYLOAD_MANIFEST_SCHEMA,
    appId: APP_ID,
    version: normalizedVersion,
    target: getUpdateTargetLabel({ platform, arch }),
    rootDir: PORTABLE_ROOT_DIR_NAME,
    files: [],
  };

  for (const file of files) {
    manifest.files.push(await buildPayloadFileEntry({ ...file, platform }));
  }

  const manifestPath = getPayloadManifestPath(portableDir);
  await fs.writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  return { manifest, manifestPath };
}

function assertManifestHeader(manifest, { schema, version, target } = {}) {
  if (!manifest || typeof manifest !== "object") {
    throw new Error("Manifest must be a JSON object.");
  }
  if (manifest.schema !== schema) {
    throw new Error(`Unexpected manifest schema: ${manifest.schema}`);
  }
  if (manifest.appId !== APP_ID) {
    throw new Error(`Unexpected app id in manifest: ${manifest.appId}`);
  }
  if (version !== undefined && manifest.version !== normalizeVersionInput(version)) {
    throw new Error(`Unexpected manifest version: ${manifest.version}`);
  }
  if (target !== undefined && manifest.target !== target) {
    throw new Error(`Unexpected manifest target: ${manifest.target}`);
  }
}

function assertLowerSha256(value, label) {
  if (!LOWER_HEX_SHA256_PATTERN.test(String(value ?? ""))) {
    throw new Error(`${label} must be a lowercase SHA256 hex digest.`);
  }
}

function expectedRequiredFiles({ platform }) {
  const required = [
    getPortableExecutableName({ platform }),
    platform === "win32" ? "vfl-update-helper.exe" : "vfl-update-helper",
    "README.md",
    "LICENSE.txt",
    "THIRD_PARTY_NOTICES.md",
    "SOURCE.md",
    "FFMPEG_BUNDLING.md",
    `${ffmpegSidecarResourceTarget}/LICENSE.txt`,
    `${ffmpegSidecarResourceTarget}/FFMPEG_BUNDLE_NOTICES.txt`,
  ];

  if (platform === "linux") {
    required.push(portableLinuxIconFileName, portableLinuxDesktopFileName);
  }

  return required;
}

export async function readPayloadManifest(portableDir) {
  return JSON.parse(await fs.readFile(getPayloadManifestPath(portableDir), "utf8"));
}

export async function validatePayloadManifest({
  portableDir,
  version,
  platform = process.platform,
  arch = process.arch,
} = {}) {
  const target = getUpdateTargetLabel({ platform, arch });
  const manifest = await readPayloadManifest(portableDir);
  assertManifestHeader(manifest, { schema: PAYLOAD_MANIFEST_SCHEMA, version, target });

  if (manifest.rootDir !== PORTABLE_ROOT_DIR_NAME) {
    throw new Error(`Unexpected portable root in payload manifest: ${manifest.rootDir}`);
  }
  if (!Array.isArray(manifest.files) || manifest.files.length === 0) {
    throw new Error("Payload manifest must list at least one file.");
  }

  const seenPaths = new Set();
  for (const entry of manifest.files) {
    const safePath = assertSafePortableRelativePath(entry.path);
    if (safePath !== entry.path) {
      throw new Error(`Payload manifest path is not normalized: ${entry.path}`);
    }
    if (seenPaths.has(safePath)) {
      throw new Error(`Duplicate payload manifest path: ${safePath}`);
    }
    seenPaths.add(safePath);

    if (entry.kind !== "file") {
      throw new Error(`Unsupported payload entry kind for ${safePath}: ${entry.kind}`);
    }
    assertLowerSha256(entry.sha256, `Payload hash for ${safePath}`);
    if (!Number.isSafeInteger(entry.sizeBytes) || entry.sizeBytes < 0) {
      throw new Error(`Payload size for ${safePath} must be a safe non-negative integer.`);
    }
    if (platform === "linux" && (!Number.isInteger(entry.mode) || entry.mode < 0 || entry.mode > 0o777)) {
      throw new Error(`Payload mode for ${safePath} must be a Unix mode.`);
    }
    if (platform !== "linux" && entry.mode !== null) {
      throw new Error(`Payload mode for ${safePath} must be null on non-Linux targets.`);
    }

    const absolutePath = path.resolve(portableDir, safePath);
    const stat = await fs.lstat(absolutePath);
    if (stat.isSymbolicLink() || !stat.isFile()) {
      throw new Error(`Payload entry is not a regular file: ${safePath}`);
    }
    if (stat.size !== entry.sizeBytes) {
      throw new Error(`Payload size mismatch for ${safePath}.`);
    }
    if ((await sha256File(absolutePath)) !== entry.sha256) {
      throw new Error(`Payload hash mismatch for ${safePath}.`);
    }
    if (platform === "linux" && (stat.mode & 0o777) !== entry.mode) {
      throw new Error(`Payload mode mismatch for ${safePath}.`);
    }
  }

  const actualFiles = (await walkPortableFiles(portableDir)).map((file) => file.relativePath).sort();
  const manifestFiles = [...seenPaths].sort();
  if (JSON.stringify(actualFiles) !== JSON.stringify(manifestFiles)) {
    throw new Error("Payload manifest does not exactly match the portable file list.");
  }

  for (const requiredFile of expectedRequiredFiles({ platform })) {
    if (!seenPaths.has(requiredFile)) {
      throw new Error(`Payload manifest is missing required file: ${requiredFile}`);
    }
  }

  return manifest;
}

export async function copyPayloadManifestSidecar({
  portableDir,
  releaseDir,
  version,
  platform = process.platform,
  arch = process.arch,
} = {}) {
  const outputPath = getPayloadManifestSidecarPath({ releaseDir, version, platform, arch });
  await fs.copyFile(getPayloadManifestPath(portableDir), outputPath);
  return outputPath;
}

async function walkFiles(root) {
  const entries = await fs.readdir(root, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const entryPath = path.resolve(root, entry.name);
    if (entry.isDirectory()) {
      files.push(...await walkFiles(entryPath));
    } else if (entry.isFile()) {
      files.push(entryPath);
    }
  }
  return files;
}

function parseReleaseFile(fileName, pattern) {
  const match = fileName.match(pattern);
  if (!match) return null;
  const [, version, platformLabel] = match;
  return {
    version,
    portableTarget: `${platformLabel}-x64`,
    updateTarget: updateTargetFromPortableTarget(`${platformLabel}-x64`),
  };
}

export async function buildUpdateManifest({
  releaseAssetDir,
  version,
  publishedAt = new Date().toISOString(),
} = {}) {
  if (!releaseAssetDir) {
    throw new Error("releaseAssetDir is required.");
  }

  const normalizedVersion = normalizeVersionInput(version);
  const files = await walkFiles(releaseAssetDir);
  const zipFiles = new Map();
  const payloadSidecars = new Map();

  for (const filePath of files) {
    const fileName = path.basename(filePath);
    const zipInfo = parseReleaseFile(fileName, RELEASE_ZIP_PATTERN);
    if (zipInfo) {
      if (normalizeVersionInput(zipInfo.version) !== normalizedVersion) {
        throw new Error(`Release zip version does not match requested version: ${fileName}`);
      }
      zipFiles.set(zipInfo.updateTarget, filePath);
      continue;
    }

    const payloadInfo = parseReleaseFile(fileName, PAYLOAD_SIDECAR_PATTERN);
    if (payloadInfo) {
      if (normalizeVersionInput(payloadInfo.version) !== normalizedVersion) {
        throw new Error(`Payload sidecar version does not match requested version: ${fileName}`);
      }
      payloadSidecars.set(payloadInfo.updateTarget, filePath);
    }
  }

  const requiredTargets = ["linux-x64", "windows-x64"];
  for (const target of requiredTargets) {
    if (!zipFiles.has(target)) {
      throw new Error(`Missing release zip for updater target: ${target}`);
    }
    if (!payloadSidecars.has(target)) {
      throw new Error(`Missing payload manifest sidecar for updater target: ${target}`);
    }
  }

  const releaseTag = `v${normalizedVersion}`;
  const artifacts = {};
  for (const target of requiredTargets) {
    const zipPath = zipFiles.get(target);
    const payloadPath = payloadSidecars.get(target);
    const zipName = path.basename(zipPath);
    const zipStat = await fs.stat(zipPath);
    const payloadManifest = JSON.parse(await fs.readFile(payloadPath, "utf8"));
    assertManifestHeader(payloadManifest, { schema: PAYLOAD_MANIFEST_SCHEMA, version: normalizedVersion, target });

    artifacts[target] = {
      fileName: zipName,
      url: `${UPDATE_RELEASE_BASE_URL}/download/${releaseTag}/${zipName}`,
      sha256: await sha256File(zipPath),
      sizeBytes: zipStat.size,
      rootDir: PORTABLE_ROOT_DIR_NAME,
      payloadManifest: {
        path: `${PORTABLE_ROOT_DIR_NAME}/${PAYLOAD_MANIFEST_FILE_NAME}`,
        sha256: await sha256File(payloadPath),
      },
    };
  }

  return {
    schema: UPDATE_MANIFEST_SCHEMA,
    appId: APP_ID,
    channel: UPDATE_CHANNEL,
    version: normalizedVersion,
    releaseTag,
    releaseUrl: `${UPDATE_RELEASE_BASE_URL}/tag/${releaseTag}`,
    publishedAt,
    minUpdaterProtocol: 1,
    notes: {
      title: `Video For Lazies ${releaseTag}`,
      summary: "See the GitHub release notes for changes.",
      url: `${UPDATE_RELEASE_BASE_URL}/tag/${releaseTag}`,
    },
    artifacts,
  };
}

export async function writeUpdateManifest({
  releaseAssetDir,
  version,
  publishedAt,
} = {}) {
  const manifest = await buildUpdateManifest({ releaseAssetDir, version, publishedAt });
  const outputPath = path.resolve(releaseAssetDir, UPDATE_MANIFEST_FILE_NAME);
  await fs.writeFile(outputPath, `${JSON.stringify(manifest, null, 2)}\n`);
  return { manifest, outputPath };
}

export function payloadSidecarNameForTarget({ version, target }) {
  return `${getPortableArchiveBaseName({
    version,
    platform: portableTargetFromUpdateTarget(target) === "win-x64" ? "win32" : "linux",
    arch: "x64",
  })}.payload-manifest.json`;
}
