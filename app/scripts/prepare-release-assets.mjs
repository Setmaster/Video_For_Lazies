#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";

import { repoRoot } from "./ffmpegBundle.mjs";
import { selectUpdateReleasePair, writeUpdateManifest } from "./updateManifests.mjs";

const REQUIRED_UPDATE_TARGETS = ["linux-x64", "windows-x64"];

function parseArgs(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith("--")) {
      throw new Error(`Unexpected argument: ${arg}`);
    }
    const key = arg.slice(2);
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) {
      throw new Error(`Missing value for --${key}`);
    }
    options[key] = value;
    index += 1;
  }
  return options;
}

async function walkFiles(root, excludedDir) {
  const entries = await fs.readdir(root, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const entryPath = path.resolve(root, entry.name);
    if (entry.isDirectory()) {
      if (entryPath !== excludedDir) {
        files.push(...await walkFiles(entryPath, excludedDir));
      }
    } else if (entry.isFile()) {
      files.push(entryPath);
    }
  }
  return files;
}

async function sha256File(filePath) {
  const hash = crypto.createHash("sha256");
  hash.update(await fs.readFile(filePath));
  return hash.digest("hex");
}

function pathContains(parentPath, candidatePath) {
  const relative = path.relative(parentPath, candidatePath);
  return relative === "" || (!relative.startsWith(`..${path.sep}`) && relative !== ".." && !path.isAbsolute(relative));
}

async function resolveThroughExistingAncestor(candidatePath) {
  let currentPath = path.resolve(candidatePath);
  const missingSegments = [];
  while (true) {
    try {
      const realPath = await fs.realpath(currentPath);
      return path.resolve(realPath, ...missingSegments.reverse());
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
      const parentPath = path.dirname(currentPath);
      if (parentPath === currentPath) throw error;
      missingSegments.push(path.basename(currentPath));
      currentPath = parentPath;
    }
  }
}

async function assertSafeReleaseDirectories(inputDir, outputDir) {
  const [canonicalInputDir, canonicalOutputDir] = await Promise.all([
    resolveThroughExistingAncestor(inputDir),
    resolveThroughExistingAncestor(outputDir),
  ]);
  if (pathContains(canonicalOutputDir, canonicalInputDir)) {
    throw new Error("Release asset output directory must not contain the input directory.");
  }
  return { canonicalInputDir, canonicalOutputDir };
}

export async function prepareReleaseAssets({ inputDir, outputDir, version } = {}) {
  const resolvedInputDir = path.resolve(repoRoot, inputDir ?? "release");
  const resolvedOutputDir = path.resolve(repoRoot, outputDir ?? "release/release-assets");
  const initialDirectories = await assertSafeReleaseDirectories(
    resolvedInputDir,
    resolvedOutputDir,
  );
  const files = await walkFiles(resolvedInputDir, resolvedOutputDir);
  const releasePair = selectUpdateReleasePair({ filePaths: files, version });

  const finalDirectories = await assertSafeReleaseDirectories(
    resolvedInputDir,
    resolvedOutputDir,
  );
  if (
    finalDirectories.canonicalInputDir !== initialDirectories.canonicalInputDir
    || finalDirectories.canonicalOutputDir !== initialDirectories.canonicalOutputDir
  ) {
    throw new Error("Release asset input or output directory changed during validation.");
  }

  await fs.rm(resolvedOutputDir, { recursive: true, force: true });
  await fs.mkdir(resolvedOutputDir, { recursive: true });

  const copiedFiles = [];
  const copiedPayloadSidecars = [];
  for (const target of REQUIRED_UPDATE_TARGETS) {
    const { zipPath: zipFile, payloadSidecarPath } = releasePair.artifacts[target];
    const outputPath = path.resolve(resolvedOutputDir, path.basename(zipFile));
    await fs.copyFile(zipFile, outputPath);
    copiedFiles.push(outputPath);
    const copiedPayloadPath = path.resolve(resolvedOutputDir, path.basename(payloadSidecarPath));
    await fs.copyFile(payloadSidecarPath, copiedPayloadPath);
    copiedPayloadSidecars.push(copiedPayloadPath);
  }

  const { outputPath: updateManifestPath } = await writeUpdateManifest({
    releaseAssetDir: resolvedOutputDir,
    version: releasePair.version,
  });
  const checksumLines = [];
  for (const filePath of copiedFiles) {
    checksumLines.push(`${await sha256File(filePath)}  ${path.basename(filePath)}`);
  }
  const checksumPath = path.resolve(resolvedOutputDir, "SHA256SUMS.txt");
  await fs.writeFile(checksumPath, `${checksumLines.join("\n")}\n`);

  await Promise.all(copiedPayloadSidecars.map((payloadSidecar) => fs.rm(payloadSidecar)));

  return { outputDir: resolvedOutputDir, files: [...copiedFiles, checksumPath, updateManifestPath] };
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const result = await prepareReleaseAssets({
    inputDir: options.input,
    outputDir: options.output,
    version: options.version,
  });

  console.log(`Release assets staged in ${result.outputDir}`);
  for (const filePath of result.files) {
    console.log(`- ${filePath}`);
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  try {
    await main();
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
