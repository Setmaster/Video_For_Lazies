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
import {
  MAX_UPDATE_ARCHIVE_BYTES,
  MAX_UPDATE_ZIP_ENTRIES,
  readVerifiedZipEntriesFromBuffer,
} from "./zipEntries.mjs";

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
const REQUIRED_UPDATE_TARGETS = ["linux-x64", "windows-x64"];

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
  if (/^[A-Za-z]:/.test(relativePath)) {
    throw new Error(`Portable payload path must not include a drive or UNC prefix: ${relativePath}`);
  }
  if (relativePath.includes("\\")) {
    throw new Error(`Portable payload path must not contain backslashes: ${relativePath}`);
  }
  if (relativePath.startsWith("~") || relativePath.includes(":")) {
    throw new Error(`Portable payload path contains an updater-reserved character: ${relativePath}`);
  }

  const normalized = path.posix.normalize(relativePath);
  if (normalized === "." || normalized.startsWith("../") || normalized === "..") {
    throw new Error(`Portable payload path escapes the portable root: ${relativePath}`);
  }
  if (normalized.split("/", 1)[0].toLowerCase() === ".vfl-updates") {
    throw new Error(`Portable payload path enters the updater state directory: ${relativePath}`);
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

function validatePayloadManifestContract(manifest, { version, target, platform }) {
  assertManifestHeader(manifest, { schema: PAYLOAD_MANIFEST_SCHEMA, version, target });
  if (manifest.rootDir !== PORTABLE_ROOT_DIR_NAME) {
    throw new Error(`Unexpected portable root in payload manifest: ${manifest.rootDir}`);
  }
  if (!Array.isArray(manifest.files) || manifest.files.length === 0) {
    throw new Error("Payload manifest must list at least one file.");
  }
  if (manifest.files.length >= MAX_UPDATE_ZIP_ENTRIES) {
    throw new Error("Payload manifest file count is outside the supported update range.");
  }

  const entries = new Map();
  for (const entry of manifest.files) {
    const safePath = assertSafePortableRelativePath(entry.path);
    if (safePath !== entry.path) {
      throw new Error(`Payload manifest path is not normalized: ${entry.path}`);
    }
    if (entries.has(safePath)) {
      throw new Error(`Duplicate payload manifest path: ${safePath}`);
    }
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
    entries.set(safePath, entry);
  }

  for (const requiredFile of expectedRequiredFiles({ platform })) {
    if (!entries.has(requiredFile)) {
      throw new Error(`Payload manifest is missing required file: ${requiredFile}`);
    }
  }
  return entries;
}

export async function validatePayloadManifest({
  portableDir,
  version,
  platform = process.platform,
  arch = process.arch,
} = {}) {
  const target = getUpdateTargetLabel({ platform, arch });
  const manifest = await readPayloadManifest(portableDir);
  const manifestEntries = validatePayloadManifestContract(manifest, { version, target, platform });
  for (const [safePath, entry] of manifestEntries) {
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
  const manifestFiles = [...manifestEntries.keys()].sort();
  if (JSON.stringify(actualFiles) !== JSON.stringify(manifestFiles)) {
    throw new Error("Payload manifest does not exactly match the portable file list.");
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

export function selectUpdateReleasePair({ filePaths, version } = {}) {
  if (!Array.isArray(filePaths)) {
    throw new Error("filePaths must be an array.");
  }

  const candidates = new Map(REQUIRED_UPDATE_TARGETS.map((target) => [target, {
    zips: [],
    payloadSidecars: [],
  }]));
  for (const filePath of filePaths) {
    const fileName = path.basename(filePath);
    const zipInfo = parseReleaseFile(fileName, RELEASE_ZIP_PATTERN);
    if (zipInfo) {
      candidates.get(zipInfo.updateTarget).zips.push({ filePath, ...zipInfo });
      continue;
    }

    const payloadInfo = parseReleaseFile(fileName, PAYLOAD_SIDECAR_PATTERN);
    if (payloadInfo) {
      candidates.get(payloadInfo.updateTarget).payloadSidecars.push({ filePath, ...payloadInfo });
    }
  }

  for (const target of REQUIRED_UPDATE_TARGETS) {
    const targetCandidates = candidates.get(target);
    if (targetCandidates.zips.length === 0) {
      throw new Error(`Missing release zip for updater target: ${target}`);
    }
    if (targetCandidates.zips.length !== 1) {
      throw new Error(
        `Duplicate release zip candidates for updater target ${target}: ${targetCandidates.zips.map(({ filePath }) => filePath).join(", ")}`,
      );
    }
    if (targetCandidates.payloadSidecars.length === 0) {
      throw new Error(`Missing payload manifest sidecar for updater target: ${target}`);
    }
    if (targetCandidates.payloadSidecars.length !== 1) {
      throw new Error(
        `Duplicate payload manifest sidecar candidates for updater target ${target}: ${targetCandidates.payloadSidecars.map(({ filePath }) => filePath).join(", ")}`,
      );
    }
  }

  const selected = Object.fromEntries(REQUIRED_UPDATE_TARGETS.map((target) => {
    const targetCandidates = candidates.get(target);
    return [target, {
      zipPath: targetCandidates.zips[0].filePath,
      zipVersion: normalizeVersionInput(targetCandidates.zips[0].version),
      payloadSidecarPath: targetCandidates.payloadSidecars[0].filePath,
      payloadSidecarVersion: normalizeVersionInput(targetCandidates.payloadSidecars[0].version),
    }];
  }));
  const pairVersions = new Set(Object.values(selected).flatMap((artifact) => [
    artifact.zipVersion,
    artifact.payloadSidecarVersion,
  ]));
  if (pairVersions.size !== 1) {
    throw new Error(
      `Release zip and payload sidecar candidates must form one version pair: ${[...pairVersions].sort().join(", ")}`,
    );
  }

  const [pairVersion] = pairVersions;
  if (version !== undefined && pairVersion !== normalizeVersionInput(version)) {
    throw new Error(
      `Release artifact pair version ${pairVersion} does not match requested version ${normalizeVersionInput(version)}.`,
    );
  }
  return { version: pairVersion, artifacts: selected };
}

function sha256Bytes(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

function platformFromUpdateTarget(target) {
  if (target === "linux-x64") return "linux";
  if (target === "windows-x64") return "win32";
  throw new Error(`Unsupported update target: ${target}`);
}

function validateReleaseZipPayload({ zipBytes, payloadBytes, version, target, archiveLabel }) {
  const platform = platformFromUpdateTarget(target);
  const archiveEntries = readVerifiedZipEntriesFromBuffer(zipBytes, { archiveLabel });
  const rootPrefix = `${PORTABLE_ROOT_DIR_NAME}/`;
  const archivedFiles = new Map();

  for (const archiveEntry of archiveEntries) {
    if (archiveEntry.name === rootPrefix && archiveEntry.isDirectory) continue;
    if (!archiveEntry.name.startsWith(rootPrefix)) {
      throw new Error(`${archiveLabel} contains an entry outside ${PORTABLE_ROOT_DIR_NAME}.`);
    }
    const relativePath = archiveEntry.name.slice(rootPrefix.length);
    if (archiveEntry.isDirectory) {
      const directoryPath = relativePath.replace(/\/$/, "");
      if (directoryPath !== "") {
        const safeDirectoryPath = assertSafePortableRelativePath(directoryPath);
        if (safeDirectoryPath !== directoryPath) {
          throw new Error(
            `${archiveLabel} contains a non-normalized payload directory: ${directoryPath}`,
          );
        }
      }
      continue;
    }
    const safePath = assertSafePortableRelativePath(relativePath);
    if (safePath !== relativePath) {
      throw new Error(`${archiveLabel} contains a non-normalized payload path: ${relativePath}`);
    }
    if (archivedFiles.has(safePath)) {
      throw new Error(`${archiveLabel} contains duplicate payload paths: ${safePath}`);
    }
    archivedFiles.set(safePath, archiveEntry);
  }

  const embeddedManifestEntry = archivedFiles.get(PAYLOAD_MANIFEST_FILE_NAME);
  if (!embeddedManifestEntry) {
    throw new Error(`${archiveLabel} is missing ${PAYLOAD_MANIFEST_FILE_NAME}.`);
  }
  if (!embeddedManifestEntry.bytes.equals(payloadBytes)) {
    throw new Error(
      `Payload manifest sidecar does not byte-for-byte match the manifest embedded in ${archiveLabel}.`,
    );
  }

  let payloadManifest;
  try {
    payloadManifest = JSON.parse(payloadBytes.toString("utf8"));
  } catch (error) {
    throw new Error(`Payload manifest for ${archiveLabel} is invalid JSON: ${error.message}`);
  }
  const manifestEntries = validatePayloadManifestContract(payloadManifest, { version, target, platform });
  const actualPayloadPaths = [...archivedFiles.keys()]
    .filter((entryPath) => entryPath !== PAYLOAD_MANIFEST_FILE_NAME)
    .sort();
  const manifestPayloadPaths = [...manifestEntries.keys()].sort();
  if (JSON.stringify(actualPayloadPaths) !== JSON.stringify(manifestPayloadPaths)) {
    throw new Error(`${archiveLabel} payload does not exactly match its manifest file list.`);
  }

  for (const [relativePath, manifestEntry] of manifestEntries) {
    const archiveEntry = archivedFiles.get(relativePath);
    if (archiveEntry.bytes.length !== manifestEntry.sizeBytes) {
      throw new Error(`${archiveLabel} payload size mismatch for ${relativePath}.`);
    }
    if (sha256Bytes(archiveEntry.bytes) !== manifestEntry.sha256) {
      throw new Error(`${archiveLabel} payload hash mismatch for ${relativePath}.`);
    }
    if (platform === "linux") {
      if (archiveEntry.unixMode === null || (archiveEntry.unixMode & 0o777) !== manifestEntry.mode) {
        throw new Error(`${archiveLabel} payload mode mismatch for ${relativePath}.`);
      }
    }
  }
  return payloadManifest;
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
  const releasePair = selectUpdateReleasePair({ filePaths: files, version: normalizedVersion });

  const releaseTag = `v${normalizedVersion}`;
  const artifacts = {};
  for (const target of REQUIRED_UPDATE_TARGETS) {
    const { zipPath, payloadSidecarPath: payloadPath } = releasePair.artifacts[target];
    const zipName = path.basename(zipPath);
    const zipStat = await fs.stat(zipPath);
    if (!zipStat.isFile() || zipStat.size === 0 || zipStat.size > MAX_UPDATE_ARCHIVE_BYTES) {
      throw new Error(`${zipName} size is outside the supported update range.`);
    }
    const zipBytes = await fs.readFile(zipPath);
    const payloadBytes = await fs.readFile(payloadPath);
    const payloadManifest = validateReleaseZipPayload({
      zipBytes,
      payloadBytes,
      version: normalizedVersion,
      target,
      archiveLabel: zipName,
    });

    artifacts[target] = {
      fileName: zipName,
      url: `${UPDATE_RELEASE_BASE_URL}/download/${releaseTag}/${zipName}`,
      sha256: sha256Bytes(zipBytes),
      sizeBytes: zipBytes.length,
      rootDir: PORTABLE_ROOT_DIR_NAME,
      payloadManifest: {
        path: `${PORTABLE_ROOT_DIR_NAME}/${PAYLOAD_MANIFEST_FILE_NAME}`,
        sha256: sha256Bytes(payloadBytes),
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
