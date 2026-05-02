import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

export const appRoot = path.resolve(__dirname, "..");
export const repoRoot = path.resolve(appRoot, "..");
export const tauriRoot = path.resolve(appRoot, "src-tauri");

export const FFMPEG_BUNDLE = Object.freeze({
  windowsX64: {
    variant: "win64-gpl-shared",
    releaseTag: "autobuild-2026-05-02-13-12",
    assetName: "ffmpeg-n8.1-11-g75d37c499d-win64-gpl-shared-8.1.zip",
    assetSha256: "80a686ecdbb35a1d454ee25c0395dc728a5d280eda129dc7364b16a7474a92d1",
    versionString: "n8.1-11-g75d37c499d-20260502",
    sourceCommit: "75d37c499da2a9fd50e3ef5a69c7dd87cd96f62a",
    sourceSha256: "6eeab8eb0491f8722575d0c2ee07bebc2687bb4e77bbfb89f37eceac42f3ed99",
  },
});

const WINDOWS_X64_BUNDLE = FFMPEG_BUNDLE.windowsX64;

export const windowsBundleRoot = path.resolve(appRoot, ".ffmpeg-bundle", "windows-x64");
export const windowsDownloadsDir = path.resolve(windowsBundleRoot, "downloads");
export const windowsSidecarDir = path.resolve(windowsBundleRoot, "ffmpeg-sidecar");
export const windowsSourceDir = path.resolve(windowsSidecarDir, "source");

export const tauriWindowsSidecarResourceSource = "../.ffmpeg-bundle/windows-x64/ffmpeg-sidecar";
export const tauriWindowsSidecarResourceTarget = "ffmpeg-sidecar";

export const windowsBundleAssetUrl = `https://github.com/BtbN/FFmpeg-Builds/releases/download/${WINDOWS_X64_BUNDLE.releaseTag}/${WINDOWS_X64_BUNDLE.assetName}`;
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
