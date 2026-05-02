import crypto from "node:crypto";
import fs from "node:fs";
import fsp from "node:fs/promises";
import path from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";

import {
  buildWindowsBundleNotice,
  getTauriSidecarOutputDirs,
  tauriWindowsSidecarResourceTarget,
  windowsBundleArchivePath,
  windowsBundleAssetUrl,
  windowsBundleExpandedRootName,
  windowsBundleRoot,
  windowsDownloadsDir,
  windowsBuildScriptsArchivePath,
  windowsBuildScriptsUrl,
  windowsRuntimeExecutables,
  windowsSidecarDir,
  windowsSidecarLicenseName,
  windowsSidecarNoticeName,
  windowsSourceArchivePath,
  windowsSourceArchiveName,
  windowsSourceArchiveNames,
  windowsSourceDir,
  windowsSourceUrl,
  windowsX264SourceArchivePath,
  windowsX264SourceUrl,
  FFMPEG_BUNDLE,
} from "./ffmpegBundle.mjs";

const WINDOWS_X64_BUNDLE = FFMPEG_BUNDLE.windowsX64;

async function pathExists(targetPath) {
  try {
    await fsp.access(targetPath);
    return true;
  } catch {
    return false;
  }
}

async function sha256File(filePath) {
  const hash = crypto.createHash("sha256");
  await new Promise((resolve, reject) => {
    const stream = fs.createReadStream(filePath);
    stream.on("data", (chunk) => hash.update(chunk));
    stream.on("end", resolve);
    stream.on("error", reject);
  });
  return hash.digest("hex");
}

async function downloadFile(url, destinationPath) {
  const response = await fetch(url);
  if (!response.ok || !response.body) {
    throw new Error(`Download failed (${response.status}) for ${url}`);
  }

  await fsp.mkdir(path.dirname(destinationPath), { recursive: true });

  const tempPath = `${destinationPath}.partial`;
  const fileStream = fs.createWriteStream(tempPath);
  await pipeline(Readable.fromWeb(response.body), fileStream);
  await fsp.rename(tempPath, destinationPath);
}

async function ensureDownloadedFile(filePath, url, expectedSha256) {
  if (await pathExists(filePath)) {
    const existingSha = await sha256File(filePath);
    if (existingSha === expectedSha256) {
      return;
    }
    await fsp.rm(filePath, { force: true });
  }

  await downloadFile(url, filePath);
  const actualSha = await sha256File(filePath);
  if (actualSha !== expectedSha256) {
    await fsp.rm(filePath, { force: true });
    throw new Error(`Checksum mismatch for ${path.basename(filePath)} (expected ${expectedSha256}, got ${actualSha})`);
  }
}

function escapePowerShellLiteral(value) {
  return value.replaceAll("'", "''");
}

async function runPowerShell(command) {
  await new Promise((resolve, reject) => {
    const child = spawn(
      "powershell.exe",
      ["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", command],
      { stdio: "inherit" },
    );
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(new Error(`PowerShell exited with code ${code}`));
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

async function extractZipToTemp(zipPath) {
  const extractionRoot = await fsp.mkdtemp(path.resolve(tmpdir(), "vfl-ffmpeg-sidecar-"));
  const literalZipPath = escapePowerShellLiteral(zipPath);
  const literalExtractionRoot = escapePowerShellLiteral(extractionRoot);

  await runPowerShell(
    `Expand-Archive -LiteralPath '${literalZipPath}' -DestinationPath '${literalExtractionRoot}' -Force`,
  );

  return extractionRoot;
}

async function clearTauriSidecarOutputs() {
  for (const outputDir of getTauriSidecarOutputDirs()) {
    await fsp.rm(outputDir, { recursive: true, force: true });
  }
}

async function ensurePlaceholderSidecar() {
  await fsp.mkdir(windowsSidecarDir, { recursive: true });
  const placeholderPath = path.resolve(windowsSidecarDir, "README.txt");
  await fsp.writeFile(
    placeholderPath,
    [
      "Windows FFmpeg sidecar resources are populated on Windows build hosts.",
      `Target folder name: ${tauriWindowsSidecarResourceTarget}`,
      "",
    ].join("\n"),
    "utf8",
  );
}

async function verifyWindowsBundleCapabilities() {
  const ffmpegPath = path.resolve(windowsSidecarDir, "ffmpeg.exe");
  if (!(await pathExists(ffmpegPath))) {
    throw new Error(`Bundled ffmpeg.exe missing after staging: ${ffmpegPath}`);
  }

  const encoders = await captureStdout(ffmpegPath, ["-hide_banner", "-loglevel", "error", "-encoders"]);
  if (!/\blibx264\b/.test(encoders)) {
    throw new Error(`Pinned Windows FFmpeg bundle is missing libx264: ${ffmpegPath}`);
  }
}

async function stageWindowsBundle() {
  await fsp.mkdir(windowsBundleRoot, { recursive: true });
  await fsp.mkdir(windowsDownloadsDir, { recursive: true });
  await clearTauriSidecarOutputs();

  await ensureDownloadedFile(
    windowsBundleArchivePath,
    windowsBundleAssetUrl,
    WINDOWS_X64_BUNDLE.assetSha256,
  );
  await ensureDownloadedFile(
    windowsSourceArchivePath,
    windowsSourceUrl,
    WINDOWS_X64_BUNDLE.sourceSha256,
  );
  await ensureDownloadedFile(
    windowsBuildScriptsArchivePath,
    windowsBuildScriptsUrl,
    WINDOWS_X64_BUNDLE.buildScriptsSha256,
  );
  await ensureDownloadedFile(
    windowsX264SourceArchivePath,
    windowsX264SourceUrl,
    WINDOWS_X64_BUNDLE.x264Sha256,
  );

  const expectedNotice = buildWindowsBundleNotice();
  const noticePath = path.resolve(windowsSidecarDir, windowsSidecarNoticeName);
  const placeholderReadmePath = path.resolve(windowsSidecarDir, "README.txt");
  const expectedRuntimePaths = windowsRuntimeExecutables.map((name) => path.resolve(windowsSidecarDir, name));
  const stagedSourcePaths = windowsSourceArchiveNames.map((name) => path.resolve(windowsSourceDir, name));

  if (
    await pathExists(noticePath)
    && (await Promise.all(stagedSourcePaths.map(pathExists))).every(Boolean)
    && (await Promise.all(expectedRuntimePaths.map(pathExists))).every(Boolean)
  ) {
    const currentNotice = await fsp.readFile(noticePath, "utf8");
    if (currentNotice === expectedNotice) {
      await fsp.rm(placeholderReadmePath, { force: true });
      await verifyWindowsBundleCapabilities();
      return;
    }
  }

  const extractRoot = await extractZipToTemp(windowsBundleArchivePath);
  try {
    const bundleRoot = path.resolve(extractRoot, windowsBundleExpandedRootName);
    const bundleBinDir = path.resolve(bundleRoot, "bin");
    const runtimeEntries = await fsp.readdir(bundleBinDir, { withFileTypes: true });

    await fsp.rm(windowsSidecarDir, { recursive: true, force: true });
    await fsp.mkdir(windowsSidecarDir, { recursive: true });
    await fsp.mkdir(windowsSourceDir, { recursive: true });

    for (const entry of runtimeEntries) {
      if (!entry.isFile()) {
        continue;
      }
      const isRuntimeExecutable = windowsRuntimeExecutables.includes(entry.name);
      const isRuntimeDll = entry.name.endsWith(".dll");
      if (!isRuntimeExecutable && !isRuntimeDll) {
        continue;
      }
      await fsp.copyFile(
        path.resolve(bundleBinDir, entry.name),
        path.resolve(windowsSidecarDir, entry.name),
      );
    }

    await fsp.copyFile(
      path.resolve(bundleRoot, windowsSidecarLicenseName),
      path.resolve(windowsSidecarDir, windowsSidecarLicenseName),
    );
    for (const sourceArchivePath of [
      windowsSourceArchivePath,
      windowsBuildScriptsArchivePath,
      windowsX264SourceArchivePath,
    ]) {
      await fsp.copyFile(
        sourceArchivePath,
        path.resolve(windowsSourceDir, path.basename(sourceArchivePath)),
      );
    }
    await fsp.writeFile(noticePath, expectedNotice, "utf8");
    await verifyWindowsBundleCapabilities();
  } finally {
    await fsp.rm(extractRoot, { recursive: true, force: true });
  }
}

async function main() {
  if (process.platform !== "win32") {
    await ensurePlaceholderSidecar();
    await clearTauriSidecarOutputs();
    console.log("Skipping FFmpeg sidecar sync on non-Windows host.");
    return;
  }

  await stageWindowsBundle();
  console.log(`FFmpeg sidecar staged at ${windowsSidecarDir}`);
}

export {
  clearTauriSidecarOutputs,
  ensureDownloadedFile,
  ensurePlaceholderSidecar,
  main as syncFfmpegSidecar,
  sha256File,
  stageWindowsBundle,
};
