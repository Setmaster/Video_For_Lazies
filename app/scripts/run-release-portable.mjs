import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import url from "node:url";
import { spawn } from "node:child_process";

import { appRoot, getPortableOutputDir } from "./ffmpegBundle.mjs";
import { runPortableSmoke } from "./run-portable-smoke.mjs";
import { runPortableExportSmoke } from "./run-portable-export-smoke.mjs";
import {
  buildChecksumLines,
  getPortableChecksumPath,
  getPortableSevenZipPath,
  getPortableZipPath,
} from "./portableRelease.mjs";

const __filename = url.fileURLToPath(import.meta.url);
function psQuote(value) {
  return `'${String(value).replace(/'/g, "''")}'`;
}

async function runChecked(command, args, options = {}) {
  const spawnOptions = {
    stdio: "inherit",
    cwd: options.cwd,
  };
  if (options.env) {
    spawnOptions.env = options.env;
  }

  await new Promise((resolve, reject) => {
    const child = spawn(command, args, spawnOptions);

    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(new Error(`${command} exited with code ${code}`));
    });
  });
}

async function captureStdout(command, args) {
  return await new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";

    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) {
        resolve(stdout);
        return;
      }
      reject(new Error(`${command} exited with code ${code}\n${stderr}`));
    });
  });
}

async function exists(filePath) {
  try {
    await fs.access(filePath);
    return true;
  } catch {
    return false;
  }
}

function sleep(ms) {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

async function findSevenZip() {
  try {
    const output = await captureStdout("where.exe", ["7z.exe"]);
    return output
      .split(/\r?\n/)
      .map((line) => line.trim())
      .find(Boolean) ?? null;
  } catch {
    return null;
  }
}

async function createZipArchive(portableDir, zipPath) {
  let lastError = null;
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    try {
      await fs.rm(zipPath, { force: true });
      await runChecked("powershell.exe", [
        "-NoProfile",
        "-Command",
        `$ErrorActionPreference='Stop'; Compress-Archive -LiteralPath ${psQuote(portableDir)} -DestinationPath ${psQuote(zipPath)} -Force`,
      ]);
      return;
    } catch (error) {
      lastError = error;
      if (attempt === 3) {
        throw error;
      }
      console.warn(`Zip creation attempt ${attempt} failed, retrying...`);
      await sleep(2000);
    }
  }

  throw lastError ?? new Error("Zip creation failed.");
}

async function extractZipArchive(zipPath, extractRoot) {
  await fs.rm(extractRoot, { recursive: true, force: true });
  await runChecked("powershell.exe", [
    "-NoProfile",
    "-Command",
    `$ErrorActionPreference='Stop'; Expand-Archive -LiteralPath ${psQuote(zipPath)} -DestinationPath ${psQuote(extractRoot)} -Force`,
  ]);
}

async function createSevenZipArchive(sevenZipExe, portableDir, archivePath) {
  await fs.rm(archivePath, { force: true });
  await runChecked(sevenZipExe, ["a", "-t7z", archivePath, portableDir]);
}

async function extractSevenZipArchive(sevenZipExe, archivePath, extractRoot) {
  await fs.rm(extractRoot, { recursive: true, force: true });
  await fs.mkdir(extractRoot, { recursive: true });
  await runChecked(sevenZipExe, ["x", archivePath, `-o${extractRoot}`, "-y"]);
}

async function locateExtractedPortableDir(extractRoot) {
  const directPath = path.resolve(extractRoot, path.basename(getPortableOutputDir()));
  if (await exists(path.resolve(directPath, "Video_For_Lazies.exe"))) {
    return directPath;
  }

  const queue = [extractRoot];
  while (queue.length) {
    const currentDir = queue.shift();
    const entries = await fs.readdir(currentDir, { withFileTypes: true });
    for (const entry of entries) {
      const entryPath = path.resolve(currentDir, entry.name);
      if (entry.isDirectory()) {
        if (await exists(path.resolve(entryPath, "Video_For_Lazies.exe"))) {
          return entryPath;
        }
        queue.push(entryPath);
      }
    }
  }

  throw new Error(`Could not locate extracted portable folder under ${extractRoot}`);
}

async function assertBundledLibx264(portableDir) {
  const ffmpegPath = path.resolve(portableDir, "ffmpeg-sidecar", "ffmpeg.exe");
  const encoders = await captureStdout(ffmpegPath, ["-hide_banner", "-loglevel", "error", "-encoders"]);
  if (!/\blibx264\b/.test(encoders)) {
    throw new Error(`Bundled ffmpeg is missing libx264: ${ffmpegPath}`);
  }
}

async function assertPortableLegalPayload(portableDir) {
  const requiredFiles = [
    "README.md",
    "LICENSE.txt",
    "THIRD_PARTY_NOTICES.md",
    "SOURCE.md",
    "FFMPEG_BUNDLING.md",
    path.join("ffmpeg-sidecar", "LICENSE.txt"),
    path.join("ffmpeg-sidecar", "FFMPEG_BUNDLE_NOTICES.txt"),
  ];

  for (const relativePath of requiredFiles) {
    const filePath = path.resolve(portableDir, relativePath);
    if (!(await exists(filePath))) {
      throw new Error(`Portable artifact is missing required legal/source file: ${relativePath}`);
    }
  }
}

async function verifyPortableArtifact(portableDir, label) {
  console.log(`Verifying extracted ${label} artifact: ${portableDir}`);
  await assertPortableLegalPayload(portableDir);
  await assertBundledLibx264(portableDir);
  await runPortableSmoke({ portableDir });
  await runPortableExportSmoke({ portableDir });
  await runPortableExportSmoke({
    portableDir,
    timeoutSeconds: 90,
    inputWidth: 1920,
    inputHeight: 1080,
    inputVideoBitrateKbps: 2400,
    sizeLimitMb: 0.3,
  });
}

async function main() {
  if (process.platform !== "win32") {
    throw new Error("release:portable must run on Windows.");
  }

  await runChecked("cmd.exe", ["/d", "/s", "/c", "npm run portable"], { cwd: appRoot });

  const portableDir = getPortableOutputDir();
  if (!(await exists(portableDir))) {
    throw new Error(`Portable folder not found: ${portableDir}`);
  }

  const zipPath = getPortableZipPath();
  const sevenZipPath = getPortableSevenZipPath();
  const checksumPath = getPortableChecksumPath();
  const archives = [];

  await createZipArchive(portableDir, zipPath);
  archives.push(zipPath);

  const sevenZipExe = await findSevenZip();
  if (sevenZipExe) {
    await createSevenZipArchive(sevenZipExe, portableDir, sevenZipPath);
    archives.push(sevenZipPath);
  } else {
    console.log("Skipping .7z artifact because 7z.exe is not available.");
    await fs.rm(sevenZipPath, { force: true });
  }

  await fs.writeFile(checksumPath, await buildChecksumLines(archives));

  const extractRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-portable-release-"));
  let keepExtractRoot = false;

  try {
    const zipExtractRoot = path.resolve(extractRoot, "zip");
    await extractZipArchive(zipPath, zipExtractRoot);
    await verifyPortableArtifact(await locateExtractedPortableDir(zipExtractRoot), "zip");

    if (sevenZipExe && await exists(sevenZipPath)) {
      const sevenZipExtractRoot = path.resolve(extractRoot, "7z");
      await extractSevenZipArchive(sevenZipExe, sevenZipPath, sevenZipExtractRoot);
      await verifyPortableArtifact(await locateExtractedPortableDir(sevenZipExtractRoot), "7z");
    }
  } catch (error) {
    keepExtractRoot = true;
    console.error(`Portable release verification artifacts kept at: ${extractRoot}`);
    throw error;
  } finally {
    if (!keepExtractRoot) {
      await fs.rm(extractRoot, { recursive: true, force: true });
    }
  }

  console.log("Portable release artifacts ready:");
  for (const archivePath of archives) {
    console.log(`- ${archivePath}`);
  }
  console.log(`- ${checksumPath}`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === __filename) {
  await main();
}

export { main as runPortableRelease };
