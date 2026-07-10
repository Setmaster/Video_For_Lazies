import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

export const appRoot = path.resolve(__dirname, "..");
export const repoRoot = path.resolve(appRoot, "..");
export const tauriRoot = path.resolve(appRoot, "src-tauri");

// BtbN retains the last build of each month for two years, while ordinary
// daily builds are pruned after the last 14. Keep release pins on month-end builds.
export const FFMPEG_BUNDLE = Object.freeze({
  windowsX64: {
    provider: "BtbN FFmpeg Builds",
    variant: "win64-gpl-shared",
    releaseTag: "autobuild-2026-06-30-13-34",
    assetName: "ffmpeg-n8.1.2-21-gce3c09c101-win64-gpl-shared-8.1.zip",
    assetSha256: "ec51253085a831b517e68cb7a1e46d13fcc8324f5e61ac0b3fd73c56af41ca21",
    versionString: "n8.1.2-21-gce3c09c101-20260630",
    sourceCommit: "ce3c09c101c83add623774d414a9f9498caf5c25",
    sourceSha256: "39bfd9846bea941da736683f79cdf7c87117c20efebc0734981d5e7033434dc5",
    buildScriptsCommit: "7a83528ea3431e9eca982a712bc3a7cd0789d5d0",
    buildScriptsSha256: "0f0f15e02b4fd1b1bc37d2e3a6f57cd7a2078c31a51c8546110d3ccb40029d30",
    x264Commit: "0480cb05fa188d37ae87e8f4fd8f1aea3711f7ee",
    x264Sha256: "d0967a1348c85dfde363bb52610403be898171493100561efa0dd05d5fd1ae50",
  },
  linuxX64: {
    provider: "BtbN FFmpeg Builds",
    variant: "linux64-gpl-shared",
    releaseTag: "autobuild-2026-06-30-13-34",
    assetName: "ffmpeg-n8.1.2-21-gce3c09c101-linux64-gpl-shared-8.1.tar.xz",
    assetSha256: "23f5d4c8e6fdc24fbbfcbbb8e83a727154f1ef70830b108ac7fd131856777405",
    versionString: "n8.1.2-21-gce3c09c101-20260630",
    sourceCommit: "ce3c09c101c83add623774d414a9f9498caf5c25",
    sourceSha256: "39bfd9846bea941da736683f79cdf7c87117c20efebc0734981d5e7033434dc5",
    buildScriptsCommit: "7a83528ea3431e9eca982a712bc3a7cd0789d5d0",
    buildScriptsSha256: "0f0f15e02b4fd1b1bc37d2e3a6f57cd7a2078c31a51c8546110d3ccb40029d30",
    x264Commit: "0480cb05fa188d37ae87e8f4fd8f1aea3711f7ee",
    x264Sha256: "d0967a1348c85dfde363bb52610403be898171493100561efa0dd05d5fd1ae50",
  },
});

const WINDOWS_X64_BUNDLE = FFMPEG_BUNDLE.windowsX64;
const LINUX_X64_BUNDLE = FFMPEG_BUNDLE.linuxX64;
const CURRENT_SIDECAR_ROOT = "current";
export const ffmpegSidecarResourceTarget = "ffmpeg-sidecar";

export const windowsBundleRoot = path.resolve(appRoot, ".ffmpeg-bundle", "windows-x64");
export const windowsDownloadsDir = path.resolve(windowsBundleRoot, "downloads");
export const windowsSidecarDir = path.resolve(windowsBundleRoot, ffmpegSidecarResourceTarget);
export const windowsSourceDir = path.resolve(windowsSidecarDir, "source");

export const linuxBundleRoot = path.resolve(appRoot, ".ffmpeg-bundle", "linux-x64");
export const linuxDownloadsDir = path.resolve(linuxBundleRoot, "downloads");
export const linuxSidecarDir = path.resolve(linuxBundleRoot, ffmpegSidecarResourceTarget);
export const linuxSourceDir = path.resolve(linuxSidecarDir, "source");

export const currentBundleRoot = path.resolve(appRoot, ".ffmpeg-bundle", CURRENT_SIDECAR_ROOT);
export const currentSidecarDir = path.resolve(currentBundleRoot, ffmpegSidecarResourceTarget);

export const tauriSidecarResourceSource = `../.ffmpeg-bundle/${CURRENT_SIDECAR_ROOT}/${ffmpegSidecarResourceTarget}`;
export const tauriSidecarResourceTarget = ffmpegSidecarResourceTarget;
export const portableLinuxDesktopFileName = "Video_For_Lazies.desktop";
export const portableLinuxIconFileName = "Video_For_Lazies.png";

// Backward-compatible export names for tests/scripts that still refer to the old Windows-only config.
export const tauriWindowsSidecarResourceSource = tauriSidecarResourceSource;
export const tauriWindowsSidecarResourceTarget = tauriSidecarResourceTarget;

export const windowsBundleAssetUrl = `https://github.com/BtbN/FFmpeg-Builds/releases/download/${WINDOWS_X64_BUNDLE.releaseTag}/${WINDOWS_X64_BUNDLE.assetName}`;
export const windowsBundleArchivePath = path.resolve(windowsDownloadsDir, WINDOWS_X64_BUNDLE.assetName);
export const windowsBundleExpandedRootName = WINDOWS_X64_BUNDLE.assetName.replace(/\.zip$/i, "");
export const windowsSourceArchiveName = `ffmpeg-${WINDOWS_X64_BUNDLE.sourceCommit}.tar.gz`;
export const windowsSourceUrl = `https://codeload.github.com/FFmpeg/FFmpeg/tar.gz/${WINDOWS_X64_BUNDLE.sourceCommit}`;
export const windowsSourceArchivePath = path.resolve(windowsDownloadsDir, windowsSourceArchiveName);
export const windowsBuildScriptsArchiveName = `btbn-ffmpeg-builds-${WINDOWS_X64_BUNDLE.buildScriptsCommit}.tar.gz`;
export const windowsBuildScriptsUrl = `https://github.com/BtbN/FFmpeg-Builds/archive/${WINDOWS_X64_BUNDLE.buildScriptsCommit}.tar.gz`;
export const windowsBuildScriptsArchivePath = path.resolve(windowsDownloadsDir, windowsBuildScriptsArchiveName);
export const windowsX264SourceArchiveName = `x264-${WINDOWS_X64_BUNDLE.x264Commit}.tar.gz`;
export const windowsX264SourceUrl = `https://code.videolan.org/videolan/x264/-/archive/${WINDOWS_X64_BUNDLE.x264Commit}/${windowsX264SourceArchiveName}`;
export const windowsX264SourceArchivePath = path.resolve(windowsDownloadsDir, windowsX264SourceArchiveName);
export const windowsSourceArchiveNames = Object.freeze([
  windowsSourceArchiveName,
  windowsBuildScriptsArchiveName,
  windowsX264SourceArchiveName,
]);

export const linuxBundleAssetUrl = `https://github.com/BtbN/FFmpeg-Builds/releases/download/${LINUX_X64_BUNDLE.releaseTag}/${LINUX_X64_BUNDLE.assetName}`;
export const linuxBundleArchivePath = path.resolve(linuxDownloadsDir, LINUX_X64_BUNDLE.assetName);
export const linuxBundleExpandedRootName = LINUX_X64_BUNDLE.assetName.replace(/\.tar\.xz$/i, "");
export const linuxSourceArchiveName = `ffmpeg-${LINUX_X64_BUNDLE.sourceCommit}.tar.gz`;
export const linuxSourceUrl = `https://codeload.github.com/FFmpeg/FFmpeg/tar.gz/${LINUX_X64_BUNDLE.sourceCommit}`;
export const linuxSourceArchivePath = path.resolve(linuxDownloadsDir, linuxSourceArchiveName);
export const linuxBuildScriptsArchiveName = `btbn-ffmpeg-builds-${LINUX_X64_BUNDLE.buildScriptsCommit}.tar.gz`;
export const linuxBuildScriptsUrl = `https://github.com/BtbN/FFmpeg-Builds/archive/${LINUX_X64_BUNDLE.buildScriptsCommit}.tar.gz`;
export const linuxBuildScriptsArchivePath = path.resolve(linuxDownloadsDir, linuxBuildScriptsArchiveName);
export const linuxX264SourceArchiveName = `x264-${LINUX_X64_BUNDLE.x264Commit}.tar.gz`;
export const linuxX264SourceUrl = `https://code.videolan.org/videolan/x264/-/archive/${LINUX_X64_BUNDLE.x264Commit}/${linuxX264SourceArchiveName}`;
export const linuxX264SourceArchivePath = path.resolve(linuxDownloadsDir, linuxX264SourceArchiveName);
export const linuxSourceArchiveNames = Object.freeze([
  linuxSourceArchiveName,
  linuxBuildScriptsArchiveName,
  linuxX264SourceArchiveName,
]);

export const ffmpegSourceArchiveNames = windowsSourceArchiveNames;

export function getFfmpegSourceArchiveNames({ platform = process.platform } = {}) {
  if (platform === "linux") {
    return linuxSourceArchiveNames;
  }
  return windowsSourceArchiveNames;
}

export const windowsRuntimeExecutables = Object.freeze(["ffmpeg.exe", "ffprobe.exe"]);
export const linuxRuntimeExecutables = Object.freeze(["ffmpeg", "ffprobe"]);
export const linuxInternalRuntimeExecutables = Object.freeze(["ffmpeg", "ffprobe"]);
export const linuxRuntimeLibraries = Object.freeze([
  "libavdevice.so.62",
  "libavfilter.so.11",
  "libavformat.so.62",
  "libavcodec.so.62",
  "libavutil.so.60",
  "libswscale.so.9",
  "libswresample.so.6",
]);
export const sidecarNoticeName = "FFMPEG_BUNDLE_NOTICES.txt";
export const sidecarLicenseName = "LICENSE.txt";
export const windowsSidecarNoticeName = sidecarNoticeName;
export const windowsSidecarLicenseName = sidecarLicenseName;
export const linuxSidecarNoticeName = sidecarNoticeName;
export const linuxSidecarLicenseName = sidecarLicenseName;

export function getWindowsPortableSidecarSourceDir() {
  return windowsSidecarDir;
}

export function getLinuxPortableSidecarSourceDir() {
  return linuxSidecarDir;
}

export function getCurrentPortableSidecarSourceDir() {
  return currentSidecarDir;
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
    path.resolve(getPortableReleaseParentDir(), tauriSidecarResourceTarget),
  ];
}

export function getTauriSidecarOutputDirs() {
  return [
    path.resolve(appRoot, "src-tauri", "target", "debug", tauriWindowsSidecarResourceTarget),
    path.resolve(appRoot, "src-tauri", "target", "release", tauriWindowsSidecarResourceTarget),
  ];
}

function buildBundleNotice({ bundle, bundleAssetUrl, sourceUrl, buildScriptsUrl, x264SourceUrl, platformLabel, runtimeNote }) {
  return [
    "Bundled FFmpeg runtime for Video For Lazies",
    "",
    `Platform: ${platformLabel}`,
    `Package variant: ${bundle.variant}`,
    `Binary archive: ${bundleAssetUrl}`,
    `Binary archive SHA256: ${bundle.assetSha256}`,
    `Bundled FFmpeg version string: ${bundle.versionString}`,
    "",
    "Corresponding FFmpeg source snapshot:",
    `Source archive: ${sourceUrl}`,
    `Source archive SHA256: ${bundle.sourceSha256}`,
    `Source commit: ${bundle.sourceCommit}`,
    "",
    "Pinned BtbN build recipe snapshot:",
    `Build scripts archive: ${buildScriptsUrl}`,
    `Build scripts archive SHA256: ${bundle.buildScriptsSha256}`,
    `Build scripts commit: ${bundle.buildScriptsCommit}`,
    "",
    "Pinned x264 source snapshot used by the selected GPL build recipe:",
    `x264 source archive: ${x264SourceUrl}`,
    `x264 source archive SHA256: ${bundle.x264Sha256}`,
    `x264 commit: ${bundle.x264Commit}`,
    "",
    runtimeNote,
    "LICENSE.txt is copied from the upstream bundle. Source and build-provenance archives are included under source/.",
    "",
  ].join("\n");
}

export function buildWindowsBundleNotice() {
  return buildBundleNotice({
    bundle: WINDOWS_X64_BUNDLE,
    bundleAssetUrl: windowsBundleAssetUrl,
    sourceUrl: windowsSourceUrl,
    buildScriptsUrl: windowsBuildScriptsUrl,
    x264SourceUrl: windowsX264SourceUrl,
    platformLabel: "Windows x64",
    runtimeNote: "The Windows runtime files in this folder came from the pinned GPL shared build above.",
  });
}

export function buildLinuxBundleNotice() {
  return buildBundleNotice({
    bundle: LINUX_X64_BUNDLE,
    bundleAssetUrl: linuxBundleAssetUrl,
    sourceUrl: linuxSourceUrl,
    buildScriptsUrl: linuxBuildScriptsUrl,
    x264SourceUrl: linuxX264SourceUrl,
    platformLabel: "Linux x64",
    runtimeNote: "The Linux runtime files in this folder came from the pinned GPL shared build above. The root ffmpeg and ffprobe files are wrapper scripts that set LD_LIBRARY_PATH to the bundled lib directory before executing bin/ffmpeg or bin/ffprobe.",
  });
}

export function listPortableCompanionDirs() {
  return [
    {
      name: tauriSidecarResourceTarget,
      sourcePath: getCurrentPortableSidecarSourceDir(),
      outputPath: path.resolve(getPortableOutputDir(), tauriSidecarResourceTarget),
    },
  ];
}

export function listPortableCompanionFiles({ thirdPartyNoticesPath, sourceNoticePath, platform = process.platform } = {}) {
  const portableDir = getPortableOutputDir();
  const files = [
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
      sourcePath: thirdPartyNoticesPath ?? path.resolve(repoRoot, "THIRD_PARTY_NOTICES.md"),
      outputPath: path.resolve(portableDir, "THIRD_PARTY_NOTICES.md"),
    },
    {
      name: "SOURCE.md",
      sourcePath: sourceNoticePath ?? path.resolve(repoRoot, "SOURCE.md"),
      outputPath: path.resolve(portableDir, "SOURCE.md"),
    },
    {
      name: "FFMPEG_BUNDLING.md",
      sourcePath: path.resolve(repoRoot, "docs", "ffmpeg-bundling.md"),
      outputPath: path.resolve(portableDir, "FFMPEG_BUNDLING.md"),
    },
  ];

  if (platform === "linux") {
    files.push(
      {
        name: portableLinuxIconFileName,
        sourcePath: path.resolve(appRoot, "src-tauri", "icons", "icon.png"),
        outputPath: path.resolve(portableDir, portableLinuxIconFileName),
      },
      {
        name: portableLinuxDesktopFileName,
        sourcePath: path.resolve(appRoot, "packaging", "linux", portableLinuxDesktopFileName),
        outputPath: path.resolve(portableDir, portableLinuxDesktopFileName),
        mode: 0o755,
      },
    );
  }

  return files;
}
