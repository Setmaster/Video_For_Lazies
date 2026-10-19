import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs/promises";
import http from "node:http";
import { once } from "node:events";

import {
  DISCOVERY_LIMITS, RELEASES_API_URL, RELEASES_BASE_URL,
  discoveryReadiness, selectPlatformRelease, stableReleaseVersion, verifyUpdateChannels,
  nextReleasePage,
} from "../scripts/verify-update-channels.mjs";
import { APP_ID, UPDATE_MANIFEST_SCHEMA } from "../scripts/updateManifests.mjs";

const fixture = JSON.parse(await fs.readFile(new URL("../test-support/update-channel-cases.json", import.meta.url), "utf8"));
for (const entry of fixture.cases) {
  test(`shared channel selection: ${entry.name}`, () => {
    if (entry.expectedError) {
      assert.throws(() => selectPlatformRelease(entry.releases, entry.target), (error) => error.code === entry.expectedError);
    } else {
      assert.equal(selectPlatformRelease(entry.releases, entry.target).tag, entry.expectedTag);
    }
  });
}

const win = fixture.cases[0].releases[1];
const linux = fixture.cases[0].releases[0];
const key = crypto.generateKeyPairSync("ed25519");
const keyId = Buffer.from("0102030405060708", "hex");
const publicBytes = key.publicKey.export({ format: "der", type: "spki" }).subarray(-32);
const publicKey = `untrusted comment: inert test key\n${Buffer.concat([Buffer.from("Ed"), keyId, publicBytes]).toString("base64")}`;
function signature(bytes) {
  const digest = crypto.createHash("blake2b512").update(bytes).digest();
  return `untrusted comment: inert test signature\n${Buffer.concat([Buffer.from("ED"), keyId, crypto.sign(null, digest, key.privateKey)]).toString("base64")}`;
}
function manifest(release, targets) {
  const version = release.tag_name.slice(1);
  return {
    schema: UPDATE_MANIFEST_SCHEMA, appId: APP_ID, channel: "stable", version,
    releaseTag: release.tag_name, releaseUrl: `${RELEASES_BASE_URL}/tag/${release.tag_name}`,
    publishedAt: "2026-10-19T00:00:00Z", minUpdaterProtocol: 1,
    notes: { title: "Test", summary: "Test", url: `${RELEASES_BASE_URL}/tag/${release.tag_name}` },
    artifacts: Object.fromEntries(targets.map((target) => {
      const candidate = selectPlatformRelease([release], target);
      return [target, { fileName: candidate.archive.name, url: candidate.archive.browser_download_url,
        sha256: "a".repeat(64), sizeBytes: candidate.archive.size, rootDir: "Video_For_Lazies",
        payloadManifest: { path: "Video_For_Lazies/VFL_PAYLOAD_MANIFEST.json", sha256: "b".repeat(64) } }];
    })),
  };
}
function urlPath(url) { const parsed = new URL(url); return `/${parsed.hostname}${parsed.pathname}${parsed.search}`; }
function routesFor(releases = [win, linux], mutate) {
  const routes = { [urlPath(`${RELEASES_API_URL}?per_page=100&page=1`)]: { body: releases } };
  for (const [release, targets] of [[win, ["windows-x64"]], [linux, ["windows-x64", "linux-x64"]]]) {
    const value = manifest(release, targets);
    mutate?.(value, release);
    const bytes = Buffer.from(JSON.stringify(value));
    routes[urlPath(`${RELEASES_BASE_URL}/download/${release.tag_name}/vfl-update-manifest-v1.json`)] = { body: bytes };
    routes[urlPath(`${RELEASES_BASE_URL}/download/${release.tag_name}/vfl-update-manifest-v1.json.sig`)] = { body: signature(bytes) };
  }
  return routes;
}
async function serverFixture(t, routes) {
  const requests = [];
  let origin;
  const server = http.createServer((request, response) => {
    const route = routes[request.url];
    if (!route) { response.writeHead(404); response.end("missing fixture route"); return; }
    if (typeof route === "function") { route(request, response, origin); return; }
    response.writeHead(route.status ?? 200, route.headers ?? {});
    response.end(Buffer.isBuffer(route.body) || typeof route.body === "string" ? route.body : JSON.stringify(route.body));
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  origin = `http://127.0.0.1:${server.address().port}`;
  t.after(async () => { server.closeAllConnections(); await new Promise((resolve) => server.close(resolve)); });
  return { requests, fetchImpl: async (url, options) => {
    requests.push({ url, redirect: options.redirect, headers: options.headers });
    assert.ok(!Object.keys(options.headers).some((name) => name.toLowerCase() === "authorization"));
    return fetch(origin + urlPath(url), options);
  } };
}
function verify(fetchImpl, options = {}) { return verifyUpdateChannels({ fetchImpl, publicKeys: [publicKey], ...options }); }

test("real mock HTTP verifies divergent signed platform heads with no auth or archive downloads", async (t) => {
  const { requests, fetchImpl } = await serverFixture(t, routesFor());
  const result = await verify(fetchImpl);
  assert.equal(result.channels["windows-x64"].version, "2.6.0");
  assert.equal(result.channels["linux-x64"].version, "2.5.0");
  assert.equal(result.inventory.ready, true);
  assert.equal(requests.length, 5);
  assert.equal(requests[0].redirect, "manual");
  assert.ok(requests.slice(1).every((request) => request.redirect === "follow"));
  assert.ok(requests.every((request) => !request.url.endsWith(".zip")));
});

test("metadata keeps CDN redirects while inventory redirects are refused", async (t) => {
  const routes = routesFor();
  const metadata = urlPath(`${RELEASES_BASE_URL}/download/v2.6.0/vfl-update-manifest-v1.json`);
  routes["/cdn/manifest"] = routes[metadata];
  routes[metadata] = (_request, response, origin) => { response.writeHead(302, { Location: origin + "/cdn/manifest" }); response.end(); };
  const mapped = await serverFixture(t, routes);
  assert.equal((await verify(mapped.fetchImpl)).channels["windows-x64"].signatureVerified, true);
  routes[urlPath(`${RELEASES_API_URL}?per_page=100&page=1`)] = { status: 302, headers: { Location: "https://untrusted.invalid/catalog" }, body: "redirect" };
  await assert.rejects(verify(mapped.fetchImpl), (error) => error.code === "inventory-redirect");
});

test("pagination follows fixed generated pages and rechecks a stable head", async (t) => {
  const routes = routesFor();
  routes[urlPath(`${RELEASES_API_URL}?per_page=100&page=1`)] = { body: [win], headers: { Link: `<${RELEASES_API_URL}?per_page=100&page=2>; rel="next"` } };
  routes[urlPath(`${RELEASES_API_URL}?per_page=100&page=2`)] = { body: [linux] };
  const { fetchImpl, requests } = await serverFixture(t, routes);
  const result = await verify(fetchImpl);
  assert.equal(result.inventory.pages, 2);
  assert.equal(result.inventory.headRechecked, true);
  assert.equal(result.channels["linux-x64"].version, "2.5.0");
  assert.ok(requests.every((request) => !request.url.includes("untrusted.invalid")));
});

test("off-origin pagination is rejected without making a second request", async (t) => {
  const routes = routesFor();
  routes[urlPath(`${RELEASES_API_URL}?per_page=100&page=1`)].headers = { Link: '<https://untrusted.invalid/next?per_page=100&page=2>; rel="next"' };
  const { fetchImpl, requests } = await serverFixture(t, routes);
  await assert.rejects(verify(fetchImpl), (error) => error.code === "invalid-pagination");
  assert.equal(requests.length, 1);
});

test("an advertised empty continuation cannot select from a partial inventory", async (t) => {
  const routes = routesFor();
  routes[urlPath(`${RELEASES_API_URL}?per_page=100&page=1`)].headers = { Link: `<${RELEASES_API_URL}?per_page=100&page=2>; rel="next"` };
  routes[urlPath(`${RELEASES_API_URL}?per_page=100&page=2`)] = { body: [] };
  const { fetchImpl, requests } = await serverFixture(t, routes);
  await assert.rejects(verify(fetchImpl), (error) => error.code === "invalid-pagination");
  assert.equal(requests.length, 2);
});

for (const limitedRead of ["continuation", "head recheck"]) {
  test(`remaining aggregate budget constrains the ${limitedRead} before its body is read`, async (t) => {
    const routes = routesFor();
    let headReads = 0;
    const headBytes = Buffer.byteLength(JSON.stringify([win]));
    const pageBytes = Buffer.byteLength(JSON.stringify([linux]));
    routes[urlPath(`${RELEASES_API_URL}?per_page=100&page=1`)] = (_request, response) => {
      headReads += 1;
      response.writeHead(200, {
        Link: `<${RELEASES_API_URL}?per_page=100&page=2>; rel="next"`,
        "Content-Length": String(headBytes),
      });
      if (headReads === 2) response.flushHeaders();
      else response.end(JSON.stringify([win]));
    };
    routes[urlPath(`${RELEASES_API_URL}?per_page=100&page=2`)] = (_request, response) => {
      response.writeHead(200, { "Content-Length": String(pageBytes) });
      if (limitedRead === "continuation") response.flushHeaders();
      else response.end(JSON.stringify([linux]));
    };
    const { fetchImpl, requests } = await serverFixture(t, routes);
    const inventoryBytes = headBytes + (limitedRead === "head recheck" ? pageBytes : 0) + 1;
    await assert.rejects(verify(fetchImpl, { limits: { inventoryBytes, timeoutMs: 1000 } }), (error) => error.code === "metadata-too-large");
    assert.equal(requests.length, limitedRead === "continuation" ? 2 : 3);
  });
}

test("pagination cap is unavailable rather than a false up-to-date result", async (t) => {
  const routes = routesFor();
  routes[urlPath(`${RELEASES_API_URL}?per_page=100&page=1`)].headers = { Link: `<${RELEASES_API_URL}?per_page=100&page=2>; rel="next"` };
  const { fetchImpl } = await serverFixture(t, routes);
  await assert.rejects(verify(fetchImpl, { limits: { maxPages: 1 } }), (error) => error.code === "history-cap" && /newer app manually/.test(error.message));
});

test("rate limits preserve retry headers and make no automatic retries", async (t) => {
  for (const status of [403, 429]) {
    const routes = { [urlPath(`${RELEASES_API_URL}?per_page=100&page=1`)]: { status, body: "rate limit", headers: { "retry-after": "120", "x-ratelimit-remaining": "0", "x-ratelimit-reset": "1792400000" } } };
    const { fetchImpl, requests } = await serverFixture(t, routes);
    await assert.rejects(verify(fetchImpl), (error) => error.code === "rate-limited" && error.retryAfter === "120" && error.rateLimitReset === "1792400000");
    assert.equal(requests.length, 1);
  }
});

test("whole HTTP deadline aborts a stalled response body", async (t) => {
  const routes = { [urlPath(`${RELEASES_API_URL}?per_page=100&page=1`)]: (_request, response) => { response.writeHead(200); response.write("["); } };
  const { fetchImpl } = await serverFixture(t, routes);
  await assert.rejects(verify(fetchImpl, { limits: { timeoutMs: 100 } }), (error) => error.code === "deadline-exceeded");
});

test("synchronous signature work cannot return success beyond the whole deadline", async (t) => {
  const { fetchImpl } = await serverFixture(t, routesFor());
  let verified = false;
  const delayedKeys = {
    *[Symbol.iterator]() {
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 300);
      verified = true;
      yield publicKey;
    },
  };
  await assert.rejects(verify(fetchImpl, {
    targets: ["windows-x64"], publicKeys: delayedKeys, limits: { timeoutMs: 250 },
  }), (error) => error.code === "deadline-exceeded");
  assert.equal(verified, true);
});

test("streamed and aggregate inventory byte caps reject oversized data", async (t) => {
  const { fetchImpl } = await serverFixture(t, routesFor());
  await assert.rejects(verify(fetchImpl, { limits: { pageBytes: 20 } }), (error) => error.code === "metadata-too-large");
  await assert.rejects(verify(fetchImpl, { limits: { inventoryBytes: 20 } }), (error) => error.code === "metadata-too-large");
});

test("declared inventory length is rejected before reading a misleading body", async (t) => {
  const routes = { [urlPath(`${RELEASES_API_URL}?per_page=100&page=1`)]: { body: "[]", headers: { "Content-Length": "2000" } } };
  const { fetchImpl } = await serverFixture(t, routes);
  await assert.rejects(verify(fetchImpl, { limits: { pageBytes: 100 } }), (error) => error.code === "metadata-too-large");
});

test("invalid catalog JSON, offline transport and HTTP failure do not fall back", async (t) => {
  const routes = { [urlPath(`${RELEASES_API_URL}?per_page=100&page=1`)]: { body: "{" } };
  const { fetchImpl, requests } = await serverFixture(t, routes);
  await assert.rejects(verify(fetchImpl), (error) => error.code === "invalid-json");
  routes[urlPath(`${RELEASES_API_URL}?per_page=100&page=1`)] = { status: 500, body: "down" };
  await assert.rejects(verify(fetchImpl), (error) => error.code === "http-error");
  assert.equal(requests.length, 2);
  await assert.rejects(verify(async () => { throw new Error("offline"); }), (error) => error.code === "network-error");
});

test("selected signature failure stops before trying an older release", async (t) => {
  const routes = routesFor();
  routes[urlPath(`${RELEASES_BASE_URL}/download/v2.6.0/vfl-update-manifest-v1.json.sig`)] = { body: "invalid signature" };
  const { fetchImpl, requests } = await serverFixture(t, routes);
  await assert.rejects(verify(fetchImpl, { targets: ["windows-x64"] }), (error) => error.code === "untrusted-signature");
  assert.ok(requests.every((request) => !request.url.includes("/v2.5.0/")));
});

test("multi-page discovery rechecks the head and rejects concurrent publication", async (t) => {
  const routes = routesFor();
  let headReads = 0;
  routes[urlPath(`${RELEASES_API_URL}?per_page=100&page=1`)] = (_request, response) => {
    headReads += 1;
    response.writeHead(200, { Link: `<${RELEASES_API_URL}?per_page=100&page=2>; rel="next"` });
    response.end(JSON.stringify(headReads === 1 ? [win] : [linux]));
  };
  routes[urlPath(`${RELEASES_API_URL}?per_page=100&page=2`)] = { body: [linux] };
  const { fetchImpl, requests } = await serverFixture(t, routes);
  await assert.rejects(verify(fetchImpl), (error) => error.code === "inventory-changed");
  assert.equal(headReads, 2);
  assert.equal(requests.length, 3);
});

test("a complete inventory at the exact record cap still requires a newer client", async (t) => {
  const routes = { [urlPath(`${RELEASES_API_URL}?per_page=2&page=1`)]: { body: [win, linux] } };
  const { fetchImpl } = await serverFixture(t, routes);
  await assert.rejects(verify(fetchImpl, { limits: { perPage: 2, maxPages: 1 } }), (error) => error.code === "history-cap");
});

test("irrelevant API metadata differences do not make duplicate release records conflict", () => {
  const withBody = structuredClone(win);
  withBody.body = "Updated release notes";
  withBody.assets[0].label = "Changed label";
  assert.equal(selectPlatformRelease([win, withBody], "windows-x64").tag, "v2.6.0");
});

test("conflicting IDs, excessive assets and incomplete pagination cannot hide a release", () => {
  const conflict = structuredClone(linux);
  conflict.id = win.id;
  assert.throws(() => selectPlatformRelease([win, conflict], "windows-x64"), (error) => error.code === "conflicting-release");
  const excessive = structuredClone(win);
  excessive.assets = Array.from({ length: 257 }, () => structuredClone(win.assets[0]));
  assert.throws(() => selectPlatformRelease([excessive], "windows-x64"), (error) => error.code === "invalid-inventory");
  for (const link of [
    `<${RELEASES_API_URL}?per_page=100&page=3>; rel="last"`,
    `<${RELEASES_API_URL}?per_page=100&page=2>; rel="next", <${RELEASES_API_URL}?per_page=100&page=2>; rel="next"`,
    `<${RELEASES_API_URL}?per_page=100&page=3>; rel="next"`,
  ]) assert.throws(() => nextReleasePage(link, 1), (error) => error.code === "invalid-pagination");
});

test("validly signed wrong version, target, URL, size and protocol bindings fail closed", async (t) => {
  const changes = [
    (value) => { value.version = "2.7.0"; },
    (value) => { value.releaseTag = "v2.7.0"; },
    (value) => { value.releaseUrl += "/other"; },
    (value) => { value.minUpdaterProtocol = 2; },
    (value) => { delete value.artifacts["windows-x64"]; },
    (value) => { value.artifacts["windows-x64"].url = "https://untrusted.invalid/evil.zip"; },
    (value) => { value.artifacts["windows-x64"].sizeBytes += 1; },
    (value) => { value.artifacts["windows-x64"].payloadManifest.sha256 = "invalid"; },
    (value) => { value.artifacts["unsupported-x64"] = structuredClone(value.artifacts["windows-x64"]); },
  ];
  for (const change of changes) {
    const routes = routesFor([win, linux], (value, release) => { if (release.tag_name === "v2.6.0") change(value); });
    const { fetchImpl } = await serverFixture(t, routes);
    await assert.rejects(verify(fetchImpl), (error) => error.code === "manifest-binding");
  }
});

test("selected asset URLs and distinct IDs are required before any download", () => {
  const wrong = structuredClone(win);
  wrong.assets[0].browser_download_url = "https://untrusted.invalid/manifest";
  assert.throws(() => selectPlatformRelease([wrong, linux], "windows-x64"), (error) => error.code === "selected-metadata-invalid");
  const duplicate = structuredClone(win);
  duplicate.assets[0].id = duplicate.assets[1].id;
  assert.throws(() => selectPlatformRelease([duplicate], "windows-x64"), (error) => error.code === "selected-metadata-invalid");
});

test("headroom warns at 800 and never recommends deleting old history", () => {
  assert.equal(discoveryReadiness(799).ready, true);
  const readiness = discoveryReadiness(800);
  assert.equal(readiness.ready, false);
  assert.match(readiness.guidance, /signed discovery index/);
  assert.match(readiness.guidance, /do not delete or truncate/);
  assert.equal(DISCOVERY_LIMITS.maxPages * DISCOVERY_LIMITS.perPage, 1000);
  assert.equal(DISCOVERY_LIMITS.inventoryBytes, 20 * 1024 * 1024);
  assert.equal(stableReleaseVersion("v18446744073709551615.0.0").version, "18446744073709551615.0.0");
  assert.equal(stableReleaseVersion("v1.2.3\n"), null);
  assert.equal(stableReleaseVersion(`v${"9".repeat(10000)}.0.0`), null);
});

test("inventory-only readiness check downloads no manifests or signatures", async (t) => {
  const { fetchImpl, requests } = await serverFixture(t, routesFor());
  const result = await verify(fetchImpl, { inventoryOnly: true });
  assert.equal(result.inventory.releaseCount, 2);
  assert.deepEqual(result.channels, {});
  assert.equal(requests.length, 1);
});

test("complete 800-record HTTP inventory fails readiness before the client cap", async (t) => {
  const routes = {};
  for (let page = 1; page <= 8; page += 1) {
    routes[urlPath(`${RELEASES_API_URL}?per_page=100&page=${page}`)] = {
      body: Array.from({ length: 100 }, (_entry, index) => ({
        id: (page - 1) * 100 + index + 1, tag_name: `v0.0.${(page - 1) * 100 + index + 1}`,
        draft: false, prerelease: false, published_at: "2026-10-19T00:00:00Z", assets: [],
      })),
      headers: page < 8 ? { Link: `<${RELEASES_API_URL}?per_page=100&page=${page + 1}>; rel="next"` } : {},
    };
  }
  const { fetchImpl, requests } = await serverFixture(t, routes);
  const result = await verify(fetchImpl, { inventoryOnly: true });
  assert.equal(result.inventory.releaseCount, 800);
  assert.equal(result.inventory.ready, false);
  assert.equal(result.inventory.headRechecked, true);
  assert.equal(requests.length, 9);
  assert.match(result.inventory.guidance, /signed discovery index/);
  assert.deepEqual(result.channels, {});
});
