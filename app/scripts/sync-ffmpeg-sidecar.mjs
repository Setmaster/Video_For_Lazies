import crypto from "node:crypto";
import fs from "node:fs";
import fsp from "node:fs/promises";
import path from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";

import {
  buildLinuxBundleNotice,
  buildWindowsBundleNotice,
  currentSidecarDir,
  getTauriSidecarOutputDirs,
  tauriSidecarResourceTarget,
  linuxBuildScriptsArchivePath,
  linuxBuildScriptsUrl,
  linuxBundleArchivePath,
  linuxBundleAssetUrl,
  linuxBundleExpandedRootName,
  linuxBundleRoot,
  linuxDownloadsDir,
  linuxInternalRuntimeExecutables,
  linuxRuntimeExecutables,
  linuxRuntimeLibraries,
  linuxSidecarDir,
  linuxSidecarLicenseName,
  linuxSidecarNoticeName,
  linuxSourceArchivePath,
  linuxSourceDir,
  linuxSourceUrl,
  linuxX264SourceArchivePath,
  linuxX264SourceUrl,
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
  windowsSourceDir,
  windowsSourceUrl,
  windowsX264SourceArchivePath,
  windowsX264SourceUrl,
  FFMPEG_BUNDLE,
} from "./ffmpegBundle.mjs";
import {
  FFMPEG_CAPABILITY_CONTRACT_FILE_NAME,
  assertCapabilityContractCopy,
  ffmpegCapabilityContractPath,
  formatFfmpegCapabilitySummary,
  verifyFfmpegCapabilityContract,
} from "./ffmpegCapabilities.mjs";

const WINDOWS_X64_BUNDLE = FFMPEG_BUNDLE.windowsX64;
const LINUX_X64_BUNDLE = FFMPEG_BUNDLE.linuxX64;
const DOWNLOAD_TIMEOUT_MS = 15 * 60 * 1000;
const ARCHIVE_PROCESS_TIMEOUT_MS = 5 * 60 * 1000;

function waitForSuccessfulClose(child, label, timeoutMs) {
  return new Promise((resolve, reject) => {
    let settled = false;
    const finish = (action) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      action();
    };
    const timeout = setTimeout(() => {
      if (settled) return;
      child.kill();
      finish(() => reject(new Error(`${label} timed out after ${timeoutMs} ms.`)));
    }, timeoutMs);
    child.on("error", (error) => finish(() => reject(error)));
    child.on("close", (code, signal) => finish(() => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(new Error(`${label} exited with code ${String(code)} (signal ${signal ?? "none"}).`));
    }));
  });
}

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
  let response;
  try {
    response = await fetch(url, { signal: AbortSignal.timeout(DOWNLOAD_TIMEOUT_MS) });
  } catch {
    await downloadFileWithCurl(url, destinationPath);
    return;
  }
  if (!response.ok || !response.body) {
    await response.body?.cancel();
    await downloadFileWithCurl(url, destinationPath);
    return;
  }

  await fsp.mkdir(path.dirname(destinationPath), { recursive: true });

  const tempPath = `${destinationPath}.partial`;
  const fileStream = fs.createWriteStream(tempPath);
  await pipeline(Readable.fromWeb(response.body), fileStream);
  await fsp.rename(tempPath, destinationPath);
}

async function downloadFileWithCurl(url, destinationPath) {
  await fsp.mkdir(path.dirname(destinationPath), { recursive: true });
  const tempPath = `${destinationPath}.partial`;
  await fsp.rm(tempPath, { force: true });

  await new Promise((resolve, reject) => {
    const child = spawn(
      "curl",
      [
        "--location",
        "--fail",
        "--retry",
        "3",
        "--retry-delay",
        "2",
        "--connect-timeout",
        "30",
        "--max-time",
        String(DOWNLOAD_TIMEOUT_MS / 1000),
        "--user-agent",
        "Video-For-Lazies-release-builder",
        "--output",
        tempPath,
        url,
      ],
      { stdio: "inherit" },
    );
    waitForSuccessfulClose(child, `curl download ${url}`, DOWNLOAD_TIMEOUT_MS).then(resolve, reject);
  });

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
  const child = spawn(
    "powershell.exe",
    ["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", command],
    { stdio: "inherit" },
  );
  await waitForSuccessfulClose(child, "PowerShell archive extraction", ARCHIVE_PROCESS_TIMEOUT_MS);
}

async function runChecked(command, args, options = {}) {
  const { timeoutMs = ARCHIVE_PROCESS_TIMEOUT_MS, ...spawnOptions } = options;
  const child = spawn(command, args, { stdio: "inherit", ...spawnOptions });
  await waitForSuccessfulClose(child, `${command} ${args.join(" ")}`, timeoutMs);
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

async function extractTarXzToTemp(archivePath) {
  const extractionRoot = await fsp.mkdtemp(path.resolve(tmpdir(), "vfl-ffmpeg-sidecar-"));
  await runChecked("tar", ["-xf", archivePath, "-C", extractionRoot]);
  return extractionRoot;
}

async function clearTauriSidecarOutputs() {
  for (const outputDir of getTauriSidecarOutputDirs()) {
    await fsp.rm(outputDir, { recursive: true, force: true });
  }
}

async function ensurePlaceholderSidecar() {
  await fsp.mkdir(currentSidecarDir, { recursive: true });
  const placeholderPath = path.resolve(currentSidecarDir, "README.txt");
  await fsp.writeFile(
    placeholderPath,
    [
      "FFmpeg sidecar resources are populated on supported release build hosts.",
      `Target folder name: ${tauriSidecarResourceTarget}`,
      "",
    ].join("\n"),
    "utf8",
  );
}

async function syncCurrentSidecarFrom(sourceDir) {
  await fsp.rm(currentSidecarDir, { recursive: true, force: true });
  await fsp.mkdir(path.dirname(currentSidecarDir), { recursive: true });
  await fsp.cp(sourceDir, currentSidecarDir, { recursive: true });
}

async function verifyBundleCapabilities(sidecarDir, ffmpegPath, label) {
  if (!(await pathExists(ffmpegPath))) {
    throw new Error(`Bundled ffmpeg missing after staging for ${label}: ${ffmpegPath}`);
  }

  const result = await verifyFfmpegCapabilityContract({
    ffmpegPath,
    label: `Pinned ${label} FFmpeg bundle`,
  });
  const copiedContractPath = path.resolve(sidecarDir, FFMPEG_CAPABILITY_CONTRACT_FILE_NAME);
  await fsp.copyFile(ffmpegCapabilityContractPath, copiedContractPath);
  await assertCapabilityContractCopy(copiedContractPath);
  console.log(`${label} FFmpeg capability contract passed: ${formatFfmpegCapabilitySummary(result)}`);
}

async function verifyWindowsBundleCapabilities() {
  await verifyBundleCapabilities(
    windowsSidecarDir,
    path.resolve(windowsSidecarDir, "ffmpeg.exe"),
    "Windows",
  );
}

async function verifyLinuxBundleCapabilities() {
  await verifyBundleCapabilities(
    linuxSidecarDir,
    path.resolve(linuxSidecarDir, "ffmpeg"),
    "Linux",
  );
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
    await syncCurrentSidecarFrom(windowsSidecarDir);
  } finally {
    await fsp.rm(extractRoot, { recursive: true, force: true });
  }
}

function linuxWrapperScript(binaryName) {
  return [
    "#!/usr/bin/env sh",
    "set -eu",
    'SIDE_CAR_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)',
    'export LD_LIBRARY_PATH="$SIDE_CAR_DIR/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"',
    `exec "$SIDE_CAR_DIR/bin/${binaryName}" "$@"`,
    "",
  ].join("\n");
}

async function stageLinuxBundle() {
  await fsp.mkdir(linuxBundleRoot, { recursive: true });
  await fsp.mkdir(linuxDownloadsDir, { recursive: true });
  await clearTauriSidecarOutputs();

  await ensureDownloadedFile(
    linuxBundleArchivePath,
    linuxBundleAssetUrl,
    LINUX_X64_BUNDLE.assetSha256,
  );
  await ensureDownloadedFile(
    linuxSourceArchivePath,
    linuxSourceUrl,
    LINUX_X64_BUNDLE.sourceSha256,
  );
  await ensureDownloadedFile(
    linuxBuildScriptsArchivePath,
    linuxBuildScriptsUrl,
    LINUX_X64_BUNDLE.buildScriptsSha256,
  );
  await ensureDownloadedFile(
    linuxX264SourceArchivePath,
    linuxX264SourceUrl,
    LINUX_X64_BUNDLE.x264Sha256,
  );

  const expectedNotice = buildLinuxBundleNotice();
  const noticePath = path.resolve(linuxSidecarDir, linuxSidecarNoticeName);

  const extractRoot = await extractTarXzToTemp(linuxBundleArchivePath);
  try {
    const bundleRoot = path.resolve(extractRoot, linuxBundleExpandedRootName);
    const bundleBinDir = path.resolve(bundleRoot, "bin");
    const bundleLibDir = path.resolve(bundleRoot, "lib");

    await fsp.rm(linuxSidecarDir, { recursive: true, force: true });
    await fsp.mkdir(path.resolve(linuxSidecarDir, "bin"), { recursive: true });
    await fsp.mkdir(path.resolve(linuxSidecarDir, "lib"), { recursive: true });
    await fsp.mkdir(linuxSourceDir, { recursive: true });

    for (const entryName of linuxInternalRuntimeExecutables) {
      const sourcePath = path.resolve(bundleBinDir, entryName);
      const outputPath = path.resolve(linuxSidecarDir, "bin", entryName);
      await fsp.copyFile(sourcePath, outputPath);
      await fsp.chmod(outputPath, 0o755);
    }

    for (const libraryName of linuxRuntimeLibraries) {
      await fsp.copyFile(
        path.resolve(bundleLibDir, libraryName),
        path.resolve(linuxSidecarDir, "lib", libraryName),
      );
    }

    for (const executableName of linuxRuntimeExecutables) {
      const wrapperPath = path.resolve(linuxSidecarDir, executableName);
      await fsp.writeFile(wrapperPath, linuxWrapperScript(executableName), "utf8");
      await fsp.chmod(wrapperPath, 0o755);
    }

    await fsp.copyFile(
      path.resolve(bundleRoot, linuxSidecarLicenseName),
      path.resolve(linuxSidecarDir, linuxSidecarLicenseName),
    );
    for (const sourceArchivePath of [
      linuxSourceArchivePath,
      linuxBuildScriptsArchivePath,
      linuxX264SourceArchivePath,
    ]) {
      await fsp.copyFile(
        sourceArchivePath,
        path.resolve(linuxSourceDir, path.basename(sourceArchivePath)),
      );
    }
    await fsp.writeFile(noticePath, expectedNotice, "utf8");
    await verifyLinuxBundleCapabilities();
    await syncCurrentSidecarFrom(linuxSidecarDir);
  } finally {
    await fsp.rm(extractRoot, { recursive: true, force: true });
  }
}

async function main() {
  if (process.platform === "win32" && process.arch === "x64") {
    await stageWindowsBundle();
    console.log(`FFmpeg sidecar staged at ${currentSidecarDir}`);
    return;
  }

  if (process.platform === "linux" && process.arch === "x64") {
    await stageLinuxBundle();
    console.log(`FFmpeg sidecar staged at ${currentSidecarDir}`);
    return;
  }

  await ensurePlaceholderSidecar();
  await clearTauriSidecarOutputs();
  console.log(`Skipping FFmpeg sidecar sync on unsupported host: ${process.platform}/${process.arch}.`);
}

export {
  clearTauriSidecarOutputs,
  ensureDownloadedFile,
  ensurePlaceholderSidecar,
  main as syncFfmpegSidecar,
  sha256File,
  stageLinuxBundle,
  stageWindowsBundle,
};
