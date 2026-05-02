import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

export const appRoot = path.resolve(__dirname, "..");
export const repoRoot = path.resolve(appRoot, "..");
export const tauriRoot = path.resolve(appRoot, "src-tauri");

export const FFMPEG_BUNDLE = Object.freeze({
  windowsX64: {
    variant: "win64-gpl-shared",
    assetName: "ffmpeg-n8.0-latest-win64-gpl-shared-8.0.zip",
    assetSha256: "68de96f55cc76d4ce656fee1889fab5b694edb42d23383d3d37f867d85727fc0",
    versionString: "n8.0.1-76-gfa4ee7ab3c-20260315",
    sourceCommit: "fa4ee7ab3c1734795149f6dbc3746e834e859e8c",
    sourceSha256: "b362a977a041c89494172007244e89b183b621a04af392cef90ae8a9609bdfac",
  },
});

const WINDOWS_X64_BUNDLE = FFMPEG_BUNDLE.windowsX64;

export const windowsBundleRoot = path.resolve(appRoot, ".ffmpeg-bundle", "windows-x64");
export const windowsDownloadsDir = path.resolve(windowsBundleRoot, "downloads");
export const windowsSidecarDir = path.resolve(windowsBundleRoot, "ffmpeg-sidecar");
export const windowsSourceDir = path.resolve(windowsSidecarDir, "source");

export const tauriWindowsSidecarResourceSource = "../.ffmpeg-bundle/windows-x64/ffmpeg-sidecar";
export const tauriWindowsSidecarResourceTarget = "ffmpeg-sidecar";

export const windowsBundleAssetUrl = `https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/${WINDOWS_X64_BUNDLE.assetName}`;
export const windowsBundleArchivePath = path.resolve(windowsDownloadsDir, WINDOWS_X64_BUNDLE.assetName);
export const windowsBundleExpandedRootName = WINDOWS_X64_BUNDLE.assetName.replace(/\.zip$/i, "");
export const windowsSourceArchiveName = `ffmpeg-${WINDOWS_X64_BUNDLE.sourceCommit}.tar.gz`;
export const windowsSourceUrl = `https://codeload.github.com/FFmpeg/FFmpeg/tar.gz/${WINDOWS_X64_BUNDLE.sourceCommit}`;
export const windowsSourceArchivePath = path.resolve(windowsDownloadsDir, windowsSourceArchiveName);

export const windowsRuntimeExecutables = Object.freeze(["ffmpeg.exe", "ffprobe.exe"]);
export const windowsSidecarNoticeName = "FFMPEG_BUNDLE_NOTICES.txt";
export const windowsSidecarLicenseName = "LICENSE.txt";

export function getWindowsPortableSidecarSourceDir() {
  return windowsSidecarDir;
}

export function getPortableReleaseParentDir() {
  return path.resolve(repoRoot, "release");
}

export function getPortableOutputDir() {
  return path.resolve(getPortableReleaseParentDir(), "Video_For_Lazies");
}

export function listPortableLegacyPaths() {
  return [
    path.resolve(getPortableReleaseParentDir(), "Video_For_Lazies.exe"),
    path.resolve(getPortableReleaseParentDir(), tauriWindowsSidecarResourceTarget),
  ];
}

export function getTauriSidecarOutputDirs() {
  return [
    path.resolve(appRoot, "src-tauri", "target", "debug", tauriWindowsSidecarResourceTarget),
    path.resolve(appRoot, "src-tauri", "target", "release", tauriWindowsSidecarResourceTarget),
  ];
}

export function buildWindowsBundleNotice() {
  return [
    "Bundled FFmpeg runtime for Video For Lazies",
    "",
    `Package variant: ${WINDOWS_X64_BUNDLE.variant}`,
    `Binary archive: ${windowsBundleAssetUrl}`,
    `Binary archive SHA256: ${WINDOWS_X64_BUNDLE.assetSha256}`,
    `Bundled FFmpeg version string: ${WINDOWS_X64_BUNDLE.versionString}`,
    "",
    "Corresponding FFmpeg source snapshot:",
    `Source archive: ${windowsSourceUrl}`,
    `Source archive SHA256: ${WINDOWS_X64_BUNDLE.sourceSha256}`,
    `Source commit: ${WINDOWS_X64_BUNDLE.sourceCommit}`,
    "",
    "The Windows runtime files in this folder came from the pinned GPL shared build above.",
    "LICENSE.txt is copied from the upstream bundle. The exact source tarball is included under source/.",
    "",
  ].join("\n");
}

export function listPortableCompanionDirs() {
  return [
    {
      name: tauriWindowsSidecarResourceTarget,
      sourcePath: getWindowsPortableSidecarSourceDir(),
      outputPath: path.resolve(getPortableOutputDir(), tauriWindowsSidecarResourceTarget),
    },
  ];
}

export function listPortableCompanionFiles() {
  const portableDir = getPortableOutputDir();
  return [
    {
      name: "README.md",
      sourcePath: path.resolve(repoRoot, "README.md"),
      outputPath: path.resolve(portableDir, "README.md"),
    },
    {
      name: "LICENSE.txt",
      sourcePath: path.resolve(repoRoot, "LICENSE"),
      outputPath: path.resolve(portableDir, "LICENSE.txt"),
    },
    {
      name: "THIRD_PARTY_NOTICES.md",
      sourcePath: path.resolve(repoRoot, "THIRD_PARTY_NOTICES.md"),
      outputPath: path.resolve(portableDir, "THIRD_PARTY_NOTICES.md"),
    },
    {
      name: "SOURCE.md",
      sourcePath: path.resolve(repoRoot, "SOURCE.md"),
      outputPath: path.resolve(portableDir, "SOURCE.md"),
    },
    {
      name: "FFMPEG_BUNDLING.md",
      sourcePath: path.resolve(repoRoot, "docs", "ffmpeg-bundling.md"),
      outputPath: path.resolve(portableDir, "FFMPEG_BUNDLING.md"),
    },
  ];
}
