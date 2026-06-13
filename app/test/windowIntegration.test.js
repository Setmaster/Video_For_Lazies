import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

test("drag-drop listens on the webview window, not the window", async () => {
  const appPath = path.resolve(__dirname, "../src/App.tsx");
  const raw = await fs.readFile(appPath, "utf8");

  // tauri 2.11 emits drag events to Webview/WebviewWindow targets only
  // (manager/webview.rs emit_to_webview); a Window-scoped listener registers
  // with target kind "Window" and never receives them.
  assert.match(raw, /getCurrentWebviewWindow\(\)\.onDragDropEvent/);
  assert.doesNotMatch(raw, /getCurrentWindow\(\)\.onDragDropEvent/);
  assert.match(raw, /import \{ getCurrentWebviewWindow \} from "@tauri-apps\/api\/webviewWindow";/);
});

test("close-confirm flow keeps the JS destroy path working", async () => {
  const appPath = path.resolve(__dirname, "../src/App.tsx");
  const raw = await fs.readFile(appPath, "utf8");

  // The JS onCloseRequested listener takes over the close flow; the
  // capabilities test asserts the destroy permission that completes it.
  assert.match(raw, /getCurrentWindow\(\)\.onCloseRequested/);
  assert.match(raw, /if \(!ok\) event\.preventDefault\(\);/);
});

test("elevated update flow is wired end to end", async () => {
  const appRaw = await fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8");
  const libRaw = await fs.readFile(path.resolve(__dirname, "../src-tauri/src/lib.rs"), "utf8");
  const updaterRaw = await fs.readFile(path.resolve(__dirname, "../src-tauri/src/updater.rs"), "utf8");

  // Backend: CLI flag, writability probe, UAC relaunch, shell relaunch drop.
  assert.match(updaterRaw, /pub const ELEVATED_UPDATE_ARG: &str = "--apply-update-elevated";/);
  assert.match(updaterRaw, /fn install_dir_writable\(/);
  assert.match(updaterRaw, /-Verb RunAs/);
  assert.match(updaterRaw, /relaunch_via_shell/);
  assert.match(updaterRaw, /explorer\.exe/);
  assert.match(libRaw, /updater::set_elevated_update_run\(true\);/);
  assert.match(libRaw, /updater::elevated_update_pending,/);

  // Frontend: startup auto-resume and the elevating status surface.
  assert.match(appRaw, /invoke<boolean>\("elevated_update_pending"\)/);
  assert.match(appRaw, /setElevatedUpdateRun\(true\);/);
});
