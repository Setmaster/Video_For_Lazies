import assert from "node:assert/strict";
import test from "node:test";
import { createPreviewViewportQueue, previewViewport } from "../src/lib/recordingPreviewViewport.mjs";

const clip = { left: 0, top: 0, right: 1920, bottom: 1080 };
test("native viewport rounds inward at fractional DPI and preserves the card border", () => {
  assert.deepEqual(previewViewport({ left: 10.2, top: 20.4, right: 650.5, bottom: 380.8 }, clip, 1.25, true),
    { x: 14, y: 27, width: 797, height: 447, visible: true });
});
test("native airspace hides when clipped, hidden, invalid or outside the client", () => {
  const rect = { left: 10, top: 20, right: 650, bottom: 380 };
  for (const args of [[rect, clip, 1, false], [rect, { ...clip, top: 21 }, 1, true],
    [{ ...rect, left: -1 }, clip, 1, true], [{ ...rect, bottom: 1081 }, clip, 1, true],
    [rect, clip, NaN, true], [rect, clip, 0, true], [{ ...rect, right: Infinity }, clip, 1, true],
    [{ ...rect, right: 11 }, clip, 1, true]]) {
    assert.equal(previewViewport(...args).visible, false);
  }
});

test("native placement coalesces geometry and sends Hide after the in-flight Show", async () => {
  let release;
  const calls = [];
  const queue = createPreviewViewportQueue({ send: value => {
    calls.push(value);
    return calls.length === 1 ? new Promise(resolve => { release = resolve; }) : Promise.resolve();
  }, onFailure: assert.fail });
  const show = { previewId: "a", visible: true, x: 1, y: 1, width: 500, height: 300 };
  const done = queue.offer(show);
  for (let x = 2; x < 50; x++) queue.offer({ ...show, x });
  const hide = { ...show, visible: false };
  queue.offer(hide);
  assert.equal(calls.length, 1);
  release(); await done;
  assert.deepEqual(calls, [show, hide]);
});

test("failed Show still drains Stop's queued Hide; failed Hide settles without a retry loop", async () => {
  let reject;
  const calls = [], failures = [];
  let active = true;
  const show = { previewId: "a", visible: true, x: 1, y: 1, width: 500, height: 300 };
  const hide = { ...show, visible: false };
  const queue = createPreviewViewportQueue({ send: value => {
    calls.push(value);
    return calls.length === 1 ? new Promise((_, fail) => { reject = fail; }) : Promise.reject(new Error("offline"));
  }, onFailure: value => {
    failures.push(value);
    if (active) { active = false; queue.offer(hide); }
  } });
  const done = queue.offer(show);
  reject(new Error("lost placement")); await done;
  assert.deepEqual(calls, [show, hide]);
  assert.deepEqual(failures, [show, hide]);
});

test("an obsolete failed placement cannot discard a newer pending preview", async () => {
  let reject;
  const calls = [];
  const queue = createPreviewViewportQueue({ send: value => {
    calls.push(value.previewId);
    return calls.length === 1 ? new Promise((_, fail) => { reject = fail; }) : Promise.resolve();
  }, onFailure: value => assert.equal(value.previewId, "old") });
  const placement = { previewId: "old", visible: true, x: 1, y: 1, width: 500, height: 300 };
  const done = queue.offer(placement);
  queue.offer({ ...placement, previewId: "new" });
  reject(new Error("old preview ended")); await done;
  assert.deepEqual(calls, ["old", "new"]);
});
