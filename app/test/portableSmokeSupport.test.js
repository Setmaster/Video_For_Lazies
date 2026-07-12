import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  assertTextExcludesPathValues,
  shutdownSmokeProcessAndLogs,
} from "../scripts/portable-smoke-support.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

test("portable smoke path privacy rejects native, JSON, extended, UNC, and 8.3 Windows aliases", () => {
  const protectedPath = String.raw`C:\Users\VeryLongOperator\AppData\Local\Temp\vfl-portable-private\results\case-01\output.mp4`;
  for (const leak of [
    "opened c:/users/verylongoperator/appdata/local/temp/VFL-PORTABLE-PRIVATE/results/case-01/OUTPUT.MP4",
    String.raw`opened \\?\C:\Users\VeryLongOperator\AppData\Local\Temp\vfl-portable-private\results\case-01\output.mp4`,
    JSON.stringify({ output: protectedPath }),
    String.raw`opened C:\USERS~1\VERYLO~1\APPDAT~1\LOCAL~1\TEMP~1\VFL-PO~1\RESULT~1\CASE-0~1\OUTPUT.MP4`,
  ]) {
    assert.throws(
      () => assertTextExcludesPathValues(leak, [protectedPath], "Windows evidence", { platform: "win32" }),
      /protected filesystem path or basename/i,
    );
  }
  assert.throws(
    () => assertTextExcludesPathValues(
      String.raw`opened \\?\UNC\Server01\PrivateShare\results\case-01\status.json`,
      [String.raw`\\server01\privateshare\results\case-01\status.json`],
      "Windows UNC evidence",
      { platform: "win32" },
    ),
    /protected filesystem path or basename/i,
  );
  assert.doesNotThrow(() => assertTextExcludesPathValues(
    String.raw`unrelated C:\Windows\Temp\browser-cache.log`,
    [protectedPath],
    "Unrelated Windows evidence",
    { platform: "win32" },
  ));
});

test("portable smoke path privacy preserves platform case semantics", () => {
  assert.throws(
    () => assertTextExcludesPathValues("opened /tmp/private/input.mp4", ["/tmp/private/input.mp4"], "Linux evidence", { platform: "linux" }),
    /protected filesystem path or basename/i,
  );
  assert.doesNotThrow(() => assertTextExcludesPathValues(
    "opened /TMP/PRIVATE/INPUT.MP4",
    ["/tmp/private/input.mp4"],
    "Linux evidence",
    { platform: "linux" },
  ));
});

test("portable smoke shutdown syncs and closes every log handle without a child", async () => {
  const events = [];
  const handles = [1, 2].map((id) => ({
    async sync() { events.push(`sync-${id}`); },
    async close() { events.push(`close-${id}`); },
  }));
  await shutdownSmokeProcessAndLogs(null, "linux", handles);
  assert.deepEqual(events.slice().sort(), ["close-1", "close-2", "sync-1", "sync-2"]);
  assert.ok(events.indexOf("sync-1") < events.indexOf("close-1"));
  assert.ok(events.indexOf("sync-2") < events.indexOf("close-2"));
});

test("G7 imports neutral portable support and support retains full process-tree termination", async () => {
  const [g7, support] = await Promise.all([
    fs.readFile(path.resolve(__dirname, "../scripts/run-portable-g7-operational-ux-smoke.mjs"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../scripts/portable-smoke-support.mjs"), "utf8"),
  ]);
  assert.match(g7, /from "\.\/portable-smoke-support\.mjs"/);
  assert.doesNotMatch(g7, /run-portable-g6-fast-trim-smoke/);
  assert.match(support, /\["\/PID", String\(child\.pid\), "\/T", "\/F"\]/);
  assert.match(support, /process\.kill\(-child\.pid, "SIGTERM"\)/);
  assert.match(support, /process\.kill\(-child\.pid, "SIGKILL"\)/);
  assert.match(support, /validatePayloadManifest/);
  assert.match(support, /verifyFfmpegCapabilityContract/);
});
