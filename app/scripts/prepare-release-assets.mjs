#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";

import { repoRoot } from "./ffmpegBundle.mjs";
import { writeUpdateManifest } from "./updateManifests.mjs";

const REQUIRED_ZIP_SUFFIXES = ["linux-x64.zip", "win-x64.zip"];
const RELEASE_ZIP_PATTERN = /^Video_For_Lazies-v(.+)-(linux|win)-x64\.zip$/;

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

async function sha256File(filePath) {
  const hash = crypto.createHash("sha256");
  hash.update(await fs.readFile(filePath));
  return hash.digest("hex");
}

export async function prepareReleaseAssets({ inputDir, outputDir } = {}) {
  const resolvedInputDir = path.resolve(repoRoot, inputDir ?? "release");
  const resolvedOutputDir = path.resolve(repoRoot, outputDir ?? "release/release-assets");
  const files = await walkFiles(resolvedInputDir);
  const zipFiles = files
    .filter((filePath) => /Video_For_Lazies-v.+-(linux|win)-x64\.zip$/.test(path.basename(filePath)))
    .sort((left, right) => path.basename(left).localeCompare(path.basename(right)));

  const zipNames = zipFiles.map((filePath) => path.basename(filePath));
  const missingSuffixes = REQUIRED_ZIP_SUFFIXES.filter(
    (suffix) => !zipNames.some((zipName) => zipName.endsWith(`-${suffix}`)),
  );

  if (!zipFiles.length) {
    throw new Error(`No release zip files found under ${resolvedInputDir}.`);
  }
  if (missingSuffixes.length) {
    throw new Error(`Missing required release zip files for: ${missingSuffixes.join(", ")}.`);
  }

  await fs.rm(resolvedOutputDir, { recursive: true, force: true });
  await fs.mkdir(resolvedOutputDir, { recursive: true });

  const copiedFiles = [];
  for (const zipFile of zipFiles) {
    const outputPath = path.resolve(resolvedOutputDir, path.basename(zipFile));
    await fs.copyFile(zipFile, outputPath);
    copiedFiles.push(outputPath);
  }

  const payloadManifestFiles = files
    .filter((filePath) => /Video_For_Lazies-v.+-(linux|win)-x64\.payload-manifest\.json$/.test(path.basename(filePath)))
    .sort((left, right) => path.basename(left).localeCompare(path.basename(right)));

  for (const payloadManifestFile of payloadManifestFiles) {
    await fs.copyFile(payloadManifestFile, path.resolve(resolvedOutputDir, path.basename(payloadManifestFile)));
  }

  const releaseVersions = new Set(
    zipNames.map((zipName) => {
      const match = zipName.match(RELEASE_ZIP_PATTERN);
      return match?.[1] ?? null;
    }),
  );
  releaseVersions.delete(null);
  if (releaseVersions.size !== 1) {
    throw new Error(`Release zips must all use the same version: ${zipNames.join(", ")}`);
  }
  const [releaseVersion] = releaseVersions;

  const checksumLines = [];
  for (const filePath of copiedFiles) {
    checksumLines.push(`${await sha256File(filePath)}  ${path.basename(filePath)}`);
  }

  const checksumPath = path.resolve(resolvedOutputDir, "SHA256SUMS.txt");
  await fs.writeFile(checksumPath, `${checksumLines.join("\n")}\n`);
  const { outputPath: updateManifestPath } = await writeUpdateManifest({
    releaseAssetDir: resolvedOutputDir,
    version: releaseVersion,
  });

  await Promise.all(payloadManifestFiles.map((payloadManifestFile) =>
    fs.rm(path.resolve(resolvedOutputDir, path.basename(payloadManifestFile)), { force: true })
  ));

  return { outputDir: resolvedOutputDir, files: [...copiedFiles, checksumPath, updateManifestPath] };
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const result = await prepareReleaseAssets({
    inputDir: options.input,
    outputDir: options.output,
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
