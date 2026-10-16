import test from "node:test";
import assert from "node:assert/strict";
import { buildBugReportUrl } from "../src/lib/bugReport.mjs";

test("bug reporting prefills only app version and coarse platform labels", () => {
  for (const [platform, expected] of [
    ["Win32", "Windows"],
    ["Win64", "Windows"],
    ["Linux x86_64", "Linux"],
    ["Linux aarch64", "Linux"],
    ["MacIntel", "macOS"],
    ["MacPPC", "macOS"],
    ["FreeBSD amd64", "Other"],
    ["", "Other"],
    [undefined, "Other"],
  ]) {
    const url = new URL(buildBugReportUrl("2.2.1", platform));
    assert.equal(url.origin, "https://vfl-bugs.7dev.io");
    assert.equal(url.pathname, "/");
    assert.equal(url.hash, "");
    assert.deepEqual([...url.searchParams], [["version", "2.2.1"], ["platform", expected]]);
  }
});

test("report URL encodes version values without adding fields and discards raw platform details", () => {
  const version = "2.3.0-beta.1+test&diagnostics=unexpected#fragment";
  const url = new URL(buildBugReportUrl(version, "Win32; private browser details"));
  assert.deepEqual([...url.searchParams], [["version", version], ["platform", "Windows"]]);
  assert.equal(url.hash, "");
  assert.equal(url.toString().includes("private"), false);
});
