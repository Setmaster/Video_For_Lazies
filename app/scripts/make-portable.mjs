import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

import {
  getPortableReleaseParentDir,
  listPortableLegacyPaths,
  getPortableOutputDir,
  listPortableCompanionDirs,
  listPortableCompanionFiles,
} from "./ffmpegBundle.mjs";
import { generatePortableDocs } from "./generate-portable-docs.mjs";
import { copyPortableArtifacts } from "./portableArtifacts.mjs";
import { getProjectVersion } from "./versioning.mjs";
import { generatePayloadManifest } from "./updateManifests.mjs";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));
const appRoot = path.resolve(__dirname, "..");

const isWin = process.platform === "win32";

async function readCargoPackageName() {
  const cargoTomlPath = path.resolve(appRoot, "src-tauri", "Cargo.toml");
  const raw = await fs.readFile(cargoTomlPath, "utf8");

  const pkgSectionIndex = raw.indexOf("[package]");
  const rawToSearch = pkgSectionIndex >= 0 ? raw.slice(pkgSectionIndex) : raw;
  const m = rawToSearch.match(/^\s*name\s*=\s*"([^"]+)"/m);
  return m?.[1] ?? "video_for_lazies";
}

const packageName = await readCargoPackageName();
const builtName = isWin ? `${packageName}.exe` : packageName;
const builtBinaryPath = path.resolve(appRoot, "src-tauri", "target", "release", builtName);
const helperBuiltName = isWin ? "vfl-update-helper.exe" : "vfl-update-helper";
const helperBuiltBinaryPath = path.resolve(appRoot, "src-tauri", "target", "release", helperBuiltName);

const outDir = getPortableOutputDir();
const outName = isWin ? "Video_For_Lazies.exe" : packageName;
const outPath = path.resolve(outDir, outName);
const helperOutPath = path.resolve(outDir, helperBuiltName);

async function main() {
  const generatedDocs = await generatePortableDocs();
  const version = await getProjectVersion();

  await copyPortableArtifacts({
    builtBinaryPath,
    outPath,
    companionDirs: listPortableCompanionDirs(),
    companionFiles: [
      ...listPortableCompanionFiles(generatedDocs),
      {
        name: helperBuiltName,
        sourcePath: helperBuiltBinaryPath,
        outputPath: helperOutPath,
        mode: isWin ? undefined : 0o755,
      },
    ],
    cleanupPaths: listPortableLegacyPaths(),
  });

  await generatePayloadManifest({
    portableDir: outDir,
    releaseDir: getPortableReleaseParentDir(),
    version,
  });

  console.log(`Portable folder written to: ${outDir}`);
}

await main();
