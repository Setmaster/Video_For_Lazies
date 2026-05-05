import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

test("default capabilities expose only the needed dialog and opener permissions", async () => {
  const capabilitiesPath = path.resolve(__dirname, "../src-tauri/capabilities/default.json");
  const raw = await fs.readFile(capabilitiesPath, "utf8");
  const json = JSON.parse(raw);
  const permissions = Array.isArray(json.permissions) ? json.permissions : [];

  assert.deepEqual(permissions, [
    "core:default",
    "dialog:allow-open",
    "dialog:allow-save",
    "opener:allow-open-path",
    "opener:allow-open-url",
    "opener:allow-default-urls",
  ]);
  assert.equal(permissions.includes("dialog:default"), false);
  assert.equal(permissions.includes("opener:default"), false);
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
