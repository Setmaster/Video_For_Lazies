import assert from "node:assert/strict";
import test from "node:test";
import { createPreviewPollCadence, createRecordingPreviewRenderer } from "../src/lib/recordingPreviewRenderer.mjs";

test("preview pulls preserve their phase through ordinary timer jitter", () => {
  const nextDelay = createPreviewPollCadence();
  let started = 0, pulls = 0;
  while (started < 8000) {
    const finished = started + 4;
    const delay = nextDelay(started, finished);
    started = finished + delay + [0.7, 4, 1.2, 6, 3][pulls++ % 5];
  }
  assert.ok(pulls >= 478 && pulls <= 481, `${pulls} pulls in eight seconds`);
});

test("a late preview pull resets rather than replaying missed timer slots", () => {
  const nextDelay = createPreviewPollCadence();
  assert.equal(nextDelay(0, 4), 1000 / 60 - 4);
  assert.ok(nextDelay(1063, 1067) > 12);
  const started = 1067 + (1000 / 60 - 4);
  assert.ok(nextDelay(started, started + 4) > 12);
});

test("slow IPC remains single-flight and schedules no catch-up queue", () => {
  const nextDelay = createPreviewPollCadence();
  assert.equal(nextDelay(0, 25), 0);
  assert.equal(nextDelay(25, 50), 0);
  assert.ok(nextDelay(50, 54) > 12);
});

const frame = (sequence, configEpoch = 1) => ({ previewId: "preview-a", configEpoch, sequence,
  width: 960, height: 540, capturedAtQpc100ns: "1234567890123456", bmp: new Uint8Array([sequence]) });

function fixture(decodeOverride) {
  let time = 0, nextHandle = 0, live = true;
  const callbacks = new Map(), bitmaps = [], drawn = [], first = [], errors = [];
  const container = { dataset: {} };
  const canvas = { width: 1, height: 1, dataset: {}, closest: () => container,
    getContext: () => ({ drawImage: (bitmap) => drawn.push(bitmap.sequence) }) };
  const bitmap = (sequence) => {
    const value = { sequence, closed: 0, close() { this.closed += 1; } };
    bitmaps.push(value); return value;
  };
  const renderer = createRecordingPreviewRenderer({
    isCurrent: () => live,
    onFirstFrame: (value) => first.push(value.sequence),
    onError: (failure) => errors.push(failure),
    decode: decodeOverride ?? (async (bytes) => bitmap(bytes[0])),
    requestFrame: (callback) => { const handle = ++nextHandle; callbacks.set(handle, callback); return handle; },
    cancelFrame: (handle) => callbacks.delete(handle),
    now: () => time,
  });
  renderer.setCanvas(canvas);
  return { renderer, canvas, container, bitmaps, bitmap, drawn, first, errors, callbacks,
    hide() { live = false; },
    paint() { time += 16; const work = [...callbacks.values()]; callbacks.clear(); work.forEach(callback => callback(time)); } };
}

test("presentation telemetry describes actual canvas draws, not decoded arrivals", async () => {
  const f = fixture();
  assert.equal(await f.renderer.offer(frame(1)), true);
  assert.deepEqual(f.drawn, []);
  assert.equal(f.canvas.dataset.previewSequence, undefined);
  assert.equal(f.renderer.afterSequence(), 1);
  f.paint();
  assert.deepEqual(f.drawn, [1]);
  assert.equal(f.canvas.width, 960); assert.equal(f.canvas.height, 540);
  assert.equal(f.canvas.dataset.previewSequence, "1");
  assert.equal(f.container.dataset.previewSequence, "1");
  assert.equal(f.canvas.dataset.previewPresentedAtMs, "16");
  assert.equal(f.canvas.dataset.previewCapturedAtQpc100ns, "1234567890123456");
  assert.equal(f.bitmaps[0].closed, 1);
  await f.renderer.offer(frame(2)); f.paint();
  assert.deepEqual(f.first, [1], "React visibility is announced only once for this source epoch");
  assert.equal(f.canvas.dataset.previewPresentedCount, "2");
  assert.equal(await f.renderer.offer(frame(1)), false, "old frames do not invoke the decoder again");
  assert.equal(f.bitmaps.length, 2);
});

test("a stalled display retains only the newest decoded frame and releases replacements", async () => {
  const f = fixture();
  for (let sequence = 1; sequence <= 50; sequence++) {
    assert.equal(await f.renderer.offer(frame(sequence)), true);
    assert.equal(f.callbacks.size, 1);
  }
  assert.equal(f.bitmaps.filter(bitmap => bitmap.closed === 0).length, 1);
  assert.deepEqual(f.drawn, []);
  f.paint();
  assert.deepEqual(f.drawn, [50]);
  assert.equal(f.canvas.dataset.previewDroppedCount, "49");
  assert.ok(f.bitmaps.every(bitmap => bitmap.closed === 1));
});

test("Hide clears pixels synchronously and prevents a queued frame from repainting", async () => {
  const f = fixture();
  await f.renderer.offer(frame(1)); f.paint();
  await f.renderer.offer(frame(2));
  f.renderer.clear();
  assert.equal(f.canvas.width, 1); assert.equal(f.canvas.height, 1);
  assert.equal(f.canvas.dataset.previewSequence, "0");
  assert.equal(f.callbacks.size, 0);
  f.paint();
  assert.deepEqual(f.drawn, [1]);
  assert.ok(f.bitmaps.every(bitmap => bitmap.closed === 1));
});

test("late decoding after source invalidation cannot publish and does not permit a second decoder", async () => {
  let resolve;
  const f = fixture(() => new Promise(done => { resolve = done; }));
  const old = f.renderer.offer(frame(1));
  f.renderer.clear();
  assert.equal(await f.renderer.offer(frame(2)), false);
  resolve(f.bitmap(1));
  assert.equal(await old, false);
  assert.equal(f.bitmaps[0].closed, 1);
  assert.equal(f.callbacks.size, 0);
  const next = f.renderer.offer(frame(2, 2));
  resolve(f.bitmap(2));
  assert.equal(await next, true);
  f.paint();
  assert.deepEqual(f.drawn, [2]);
});

test("an epoch or eligibility change after decode drops the frame at presentation", async () => {
  const f = fixture();
  await f.renderer.offer(frame(1));
  f.hide();
  f.paint();
  assert.deepEqual(f.drawn, []);
  assert.deepEqual(f.first, []);
  assert.equal(f.bitmaps[0].closed, 1);
});

test("decoder failure releases its single-flight slot and canvas removal cancels delivery", async () => {
  let fail = true;
  const f = fixture(async bytes => { if (fail) throw new Error("decode failed"); return f.bitmap(bytes[0]); });
  assert.equal(await f.renderer.offer(frame(1)), false);
  assert.match(f.errors[0].message, /decode failed/);
  fail = false;
  assert.equal(await f.renderer.offer(frame(2)), true);
  f.renderer.setCanvas(null);
  assert.equal(f.callbacks.size, 0);
  assert.equal(f.bitmaps[0].closed, 1);
  assert.equal(await f.renderer.offer(frame(3)), false);
});
