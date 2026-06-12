#!/usr/bin/env node
// Preflight for the pinned FFmpeg bundle downloads. BtbN auto-build releases
// get pruned upstream, so a release run must fail in seconds at this step
// instead of ~40 minutes into both build legs.
import {
  linuxBuildScriptsUrl,
  linuxBundleAssetUrl,
  linuxSourceUrl,
  linuxX264SourceUrl,
  windowsBuildScriptsUrl,
  windowsBundleAssetUrl,
  windowsSourceUrl,
  windowsX264SourceUrl,
} from "./ffmpegBundle.mjs";

export const PINNED_BUNDLE_URLS = Object.freeze([
  ...new Set([
    windowsBundleAssetUrl,
    windowsSourceUrl,
    windowsBuildScriptsUrl,
    windowsX264SourceUrl,
    linuxBundleAssetUrl,
    linuxSourceUrl,
    linuxBuildScriptsUrl,
    linuxX264SourceUrl,
  ]),
]);

export async function checkUrl(url, { timeoutMs = 30_000, fetchImpl = fetch } = {}) {
  // HEAD first; some hosts reject HEAD, so fall back to a one-byte ranged GET.
  let last = { url, ok: false, status: null, error: "not attempted" };
  for (const init of [
    { method: "HEAD", redirect: "follow" },
    { method: "GET", redirect: "follow", headers: { range: "bytes=0-0" } },
  ]) {
    try {
      const response = await fetchImpl(url, {
        ...init,
        signal: AbortSignal.timeout(timeoutMs),
      });
      if (response.ok || response.status === 206) {
        return { url, ok: true, status: response.status };
      }
      last = { url, ok: false, status: response.status, error: null };
    } catch (error) {
      last = { url, ok: false, status: null, error: String(error) };
    }
  }
  return last;
}

export async function checkPinnedBundleUrls(options = {}) {
  return Promise.all(PINNED_BUNDLE_URLS.map((url) => checkUrl(url, options)));
}

async function main() {
  const results = await checkPinnedBundleUrls();
  let failed = false;
  for (const result of results) {
    if (result.ok) {
      console.log(`ok ${result.status} ${result.url}`);
    } else {
      failed = true;
      console.error(`FAIL ${result.status ?? result.error} ${result.url}`);
    }
  }
  if (failed) {
    console.error(
      "Pinned FFmpeg bundle URLs are unreachable. Repin the sidecar in app/scripts/ffmpegBundle.mjs (and docs/ffmpeg-bundling.md) before dispatching a release.",
    );
    process.exitCode = 1;
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  await main();
}
