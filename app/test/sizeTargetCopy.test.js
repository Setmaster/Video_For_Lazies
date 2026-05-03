import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

test("size target copy discloses audio removal before and after export", async () => {
  const appRaw = await fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8");
  const cssRaw = await fs.readFile(path.resolve(__dirname, "../src/App.css"), "utf8");
  const videoRaw = await fs.readFile(path.resolve(__dirname, "../src-tauri/src/video.rs"), "utf8");

  assert.match(appRaw, /too tight to keep audio/);
  assert.match(appRaw, /lastExport\.message/);
  assert.match(cssRaw, /\.vfl-export-result-note/);
  assert.match(videoRaw, /Audio was removed to fit the size target/);
});
