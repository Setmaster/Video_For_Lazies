import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

test("size target copy keeps Include audio authoritative across Strict Fit plans", async () => {
  const appRaw = await fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8");
  const cssRaw = await fs.readFile(path.resolve(__dirname, "../src/App.css"), "utf8");
  const videoRaw = await fs.readFile(path.resolve(__dirname, "../src-tauri/src/video.rs"), "utf8");
  const strictFitRaw = await fs.readFile(path.resolve(__dirname, "../src/lib/strictFit.mjs"), "utf8");

  assert.match(appRaw, /requested dimensions and audio are preserved/);
  assert.doesNotMatch(appRaw, /Allow the final applicable plan to remove audio/);
  assert.doesNotMatch(appRaw, /strictFitAllowAudioRemoval/);
  assert.match(strictFitRaw, /keeps audio at.*STRICT_FIT_REDUCED_AUDIO_KBPS.*kbps/);
  assert.doesNotMatch(strictFitRaw, /may remove audio/);
  assert.match(strictFitRaw, /Audio disabled\. No export started\./);
  assert.match(cssRaw, /\.vfl-export-result\.target-missed/);
  assert.match(videoRaw, /tight_size_target_preserves_requested_audio/);
  assert.match(videoRaw, /audio was preserved/);
  assert.doesNotMatch(videoRaw, /Audio was removed to fit the size target/);
});
