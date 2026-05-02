import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import url from "node:url";
import { spawn } from "node:child_process";

import {
  appRoot,
  getPortableOutputDir,
  getPortableReleaseParentDir,
  windowsSourceArchiveName,
} from "./ffmpegBundle.mjs";
import { runPortableSmoke } from "./run-portable-smoke.mjs";
import { runPortableExportSmoke } from "./run-portable-export-smoke.mjs";
import {
  buildChecksumLines,
  getPortableChecksumPath,
  getPortableExecutableName,
  getPortableTargetLabel,
  getPortableZipPath,
} from "./portableRelease.mjs";
import { getProjectVersion } from "./versioning.mjs";

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

async function createZipArchive(portableDir, zipPath) {
  let lastError = null;
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    try {
      await fs.rm(zipPath, { force: true });
      if (process.platform === "win32") {
        await runChecked("powershell.exe", [
          "-NoProfile",
          "-Command",
          `$ErrorActionPreference='Stop'; Compress-Archive -LiteralPath ${psQuote(portableDir)} -DestinationPath ${psQuote(zipPath)} -Force`,
        ]);
      } else {
        await runChecked("zip", ["-r", "-q", zipPath, path.basename(portableDir)], {
          cwd: path.dirname(portableDir),
        });
      }
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
  if (process.platform === "win32") {
    await runChecked("powershell.exe", [
      "-NoProfile",
      "-Command",
      `$ErrorActionPreference='Stop'; Expand-Archive -LiteralPath ${psQuote(zipPath)} -DestinationPath ${psQuote(extractRoot)} -Force`,
    ]);
    return;
  }

  await fs.mkdir(extractRoot, { recursive: true });
  await runChecked("unzip", ["-q", zipPath, "-d", extractRoot]);
}

async function locateExtractedPortableDir(extractRoot, { platform = process.platform } = {}) {
  const executableName = getPortableExecutableName({ platform });
  const directPath = path.resolve(extractRoot, path.basename(getPortableOutputDir()));
  if (await exists(path.resolve(directPath, executableName))) {
    return directPath;
  }

  const queue = [extractRoot];
  while (queue.length) {
    const currentDir = queue.shift();
    const entries = await fs.readdir(currentDir, { withFileTypes: true });
    for (const entry of entries) {
      const entryPath = path.resolve(currentDir, entry.name);
      if (entry.isDirectory()) {
        if (await exists(path.resolve(entryPath, executableName))) {
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

async function assertPortableFile(portableDir, relativePath) {
  const filePath = path.resolve(portableDir, relativePath);
  if (!(await exists(filePath))) {
    throw new Error(`Portable artifact is missing required file: ${relativePath}`);
  }
}

async function assertPortableLegalPayload(portableDir, { platform = process.platform } = {}) {
  const requiredFiles = [
    "README.md",
    "LICENSE.txt",
    "THIRD_PARTY_NOTICES.md",
    "SOURCE.md",
    "FFMPEG_BUNDLING.md",
  ];

  for (const relativePath of requiredFiles) {
    await assertPortableFile(portableDir, relativePath);
  }

  if (platform === "win32") {
    const windowsRequiredFiles = [
      path.join("ffmpeg-sidecar", "LICENSE.txt"),
      path.join("ffmpeg-sidecar", "FFMPEG_BUNDLE_NOTICES.txt"),
      path.join("ffmpeg-sidecar", "source", windowsSourceArchiveName),
    ];
    for (const relativePath of windowsRequiredFiles) {
      await assertPortableFile(portableDir, relativePath);
    }
  }
}

async function assertPortableExecutable(portableDir, { platform = process.platform } = {}) {
  await assertPortableFile(portableDir, getPortableExecutableName({ platform }));
}

async function verifyPortableArtifact(portableDir, label, { platform = process.platform } = {}) {
  console.log(`Verifying extracted ${label} artifact: ${portableDir}`);
  await assertPortableExecutable(portableDir, { platform });
  await assertPortableLegalPayload(portableDir, { platform });

  if (platform !== "win32") {
    console.log("Linux portable payload verified. FFmpeg and FFprobe are still resolved from PATH or env vars at runtime.");
    return;
  }

  await assertBundledLibx264(portableDir);
  await runPortableSmoke({ portableDir });
  await runPortableExportSmoke({ portableDir });
  await runPortableExportSmoke({
    portableDir,
    timeoutSeconds: 180,
    inputWidth: 1920,
    inputHeight: 1080,
    inputVideoBitrateKbps: 2400,
    sizeLimitMb: 0.3,
  });
}

async function runPortableBuild() {
  if (process.platform === "win32") {
    await runChecked("cmd.exe", ["/d", "/s", "/c", "npm run portable"], { cwd: appRoot });
    return;
  }

  await runChecked("npm", ["run", "portable"], { cwd: appRoot });
}

async function main() {
  const version = await getProjectVersion();
  const targetLabel = getPortableTargetLabel();

  await runPortableBuild();

  const portableDir = getPortableOutputDir();
  if (!(await exists(portableDir))) {
    throw new Error(`Portable folder not found: ${portableDir}`);
  }

  const zipPath = getPortableZipPath({ version });
  const checksumPath = getPortableChecksumPath();

  await createZipArchive(portableDir, zipPath);
  await fs.writeFile(checksumPath, await buildChecksumLines([zipPath]));

  const extractRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-portable-release-"));
  let keepExtractRoot = false;

  try {
    const zipExtractRoot = path.resolve(extractRoot, "zip");
    await extractZipArchive(zipPath, zipExtractRoot);
    await verifyPortableArtifact(await locateExtractedPortableDir(zipExtractRoot), "zip");
  } catch (error) {
    keepExtractRoot = true;
    console.error(`Portable release verification artifacts kept at: ${extractRoot}`);
    throw error;
  } finally {
    if (!keepExtractRoot) {
      await fs.rm(extractRoot, { recursive: true, force: true });
    }
  }

  console.log(`Portable ${targetLabel} release artifacts ready:`);
  console.log(`- ${zipPath}`);
  console.log(`- ${checksumPath}`);
  console.log(`Release directory: ${getPortableReleaseParentDir()}`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === __filename) {
  await main();
}

export { main as runPortableRelease };
