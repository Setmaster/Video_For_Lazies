import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

test("default capabilities expose only the needed window, dialog, and opener permissions", async () => {
  const capabilitiesPath = path.resolve(__dirname, "../src-tauri/capabilities/default.json");
  const raw = await fs.readFile(capabilitiesPath, "utf8");
  const json = JSON.parse(raw);
  const permissions = Array.isArray(json.permissions) ? json.permissions : [];

  assert.deepEqual(permissions, [
    "core:default",
    // core:window:default is getters-only. The JS onCloseRequested listener
    // takes over the close flow in tauri v2 and finishes it with destroy();
    // without allow-destroy the title bar X silently does nothing.
    "core:window:allow-destroy",
    // The min-window-size enforcement in App.tsx calls these setters.
    "core:window:allow-set-size",
    "core:window:allow-set-min-size",
    "core:window:allow-set-size-constraints",
    "dialog:allow-open",
    "dialog:allow-save",
    // Close-confirm prompt while an export is running or queued.
    "dialog:allow-confirm",
    "opener:allow-open-path",
    "opener:allow-open-url",
    "opener:allow-default-urls",
  ]);
  assert.equal(permissions.includes("dialog:default"), false);
  assert.equal(permissions.includes("opener:default"), false);
  assert.equal(permissions.includes("core:window:allow-close"), false);
});

test("tauri config enables asset protocol for previews", async () => {
  const confPath = path.resolve(__dirname, "../src-tauri/tauri.conf.json");
  const raw = await fs.readFile(confPath, "utf8");
  const json = JSON.parse(raw);

  const enabled = json?.app?.security?.assetProtocol?.enable;
  assert.equal(
    enabled,
    true,
    `Expected app.security.assetProtocol.enable to be true. Got: ${JSON.stringify(enabled)}`,
  );

  const csp = json?.app?.security?.csp;
  assert.equal(typeof csp, "string");
  assert.match(csp, /default-src 'self'/);
  assert.match(csp, /media-src .*asset:/);
  assert.match(csp, /connect-src .*ipc:/);
  assert.match(csp, /object-src 'none'/);
});
