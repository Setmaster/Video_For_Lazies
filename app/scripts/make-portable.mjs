import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

import {
  listPortableLegacyPaths,
  getPortableOutputDir,
  listPortableCompanionDirs,
  listPortableCompanionFiles,
} from "./ffmpegBundle.mjs";
import { copyPortableArtifacts } from "./portableArtifacts.mjs";

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

const outDir = getPortableOutputDir();
const outName = isWin ? "Video_For_Lazies.exe" : packageName;
const outPath = path.resolve(outDir, outName);

async function main() {
  await copyPortableArtifacts({
    builtBinaryPath,
    outPath,
    companionDirs: isWin ? listPortableCompanionDirs() : [],
    companionFiles: listPortableCompanionFiles(),
    cleanupPaths: listPortableLegacyPaths(),
  });

  console.log(`Portable folder written to: ${outDir}`);
}

await main();
