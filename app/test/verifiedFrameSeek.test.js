import { test as nodeTest } from "node:test";
import assert from "node:assert/strict";
import { beginVerifiedFrameSeek, takeFrameProof } from "../src/lib/verifiedFrameSeek.mjs";

const test = (name, fn) => nodeTest(name, { timeout: 1000 }, fn);

class Clock {
  time = 0; next = 0; timers = new Map();
  set = (fn, delay) => { const id = ++this.next; this.timers.set(id, { fn, at: this.time + delay }); return id; };
  clear = id => this.timers.delete(id);
  tick(ms) {
    const end = this.time + ms;
    while (true) {
      const row = [...this.timers].filter(([, t]) => t.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
      if (!row) break;
      this.time = row[1].at; this.timers.delete(row[0]); row[1].fn();
    }
    this.time = end;
  }
}
class Video extends EventTarget {
  paused = true; seeking = false; readyState = 4; duration = 5;
  assigned = []; callbacks = new Map(); cancelled = new Map(); next = 0; time = 0;
  listeners = new Map();
  addEventListener(type, listener, options) {
    if (!this.listeners.has(type)) this.listeners.set(type, new Set());
    this.listeners.get(type).add(listener); super.addEventListener(type, listener, options);
  }
  removeEventListener(type, listener, options) { this.listeners.get(type)?.delete(listener); super.removeEventListener(type, listener, options); }
  listenerCount() { return [...this.listeners.values()].reduce((total, rows) => total + rows.size, 0); }
  get currentTime() { return this.time; }
  set currentTime(time) { this.time = time; this.assigned.push(time); this.seeking = true; this.dispatchEvent(new Event("seeking")); }
  pause() { this.paused = true; }
  requestVideoFrameCallback = fn => { const id = ++this.next; this.callbacks.set(id, fn); return id; };
  cancelVideoFrameCallback = id => { if (this.callbacks.has(id)) this.cancelled.set(id, this.callbacks.get(id)); this.callbacks.delete(id); };
  presented(mediaTime, presentedFrames = 1) {
    const callbacks = [...this.callbacks.values()]; this.callbacks.clear();
    for (const fn of callbacks) fn(0, { mediaTime, presentedFrames });
  }
  complete(event = "seeked") { this.seeking = false; if (event) this.dispatchEvent(new Event(event)); }
}
function harness(overrides = {}) {
  const video = overrides.video ?? new Video(), clock = new Clock(), positions = [];
  let current = true, interruptions = 0;
  const options = { ...overrides };
  if (options.previousProof) options.previousProof = { video, sourceKey: "source", ...options.previousProof };
  const op = beginVerifiedFrameSeek({ video, sourceKey: "source", timeS: 1, endS: 1.04, timelineOffsetS: 0,
    isCurrent: () => current, onPosition: t => positions.push(t), onInterrupted: () => interruptions++,
    setTimer: clock.set, clearTimer: clock.clear, ...options });
  const outcome = op.promise.then(value => ({ value }), error => ({ error }));
  return { video, clock, positions, op, outcome, sourceChanged: () => { current = false; }, interruptions: () => interruptions };
}
const previous = { logicalS: .5, endS: .54, mediaS: .5004, presentedMediaS: .5, isReusable: () => true, dispose() {} };

test("callback before or after seeked verifies only actual matching picture", async () => {
  for (const callbackFirst of [true, false]) {
    const h = harness();
    if (callbackFirst) { h.video.presented(1); h.video.complete(); }
    else { h.video.complete(); h.video.presented(1); }
    const proof = (await h.outcome).value; assert.equal(proof.logicalS, 1);
    proof.dispose();
    assert.equal(h.clock.timers.size, 0); assert.equal(h.video.callbacks.size, 0);
  }
});

test("matching callback plus settled current seek does not depend on a seeked event", async () => {
  const h = harness(); h.video.complete(null); h.video.presented(1);
  assert.equal((await h.outcome).value.presentedMediaS, 1);
  const early = harness(); early.video.presented(1); early.video.complete(null); early.clock.tick(16);
  assert.equal((await early.outcome).value.presentedMediaS, 1);
  assert.equal(early.video.assigned.length, 1);
});

test("seeked and currentTime without a callback never claim picture verification", async () => {
  const h = harness(); h.video.complete(); h.clock.tick(2499);
  let done = false; h.outcome.then(() => { done = true; }); await Promise.resolve(); assert.equal(done, false);
  h.clock.tick(1); assert.match((await h.outcome).error.message, /could not verify/);
  assert.equal(h.video.assigned.length, 1); assert.equal(h.clock.timers.size, 0);
});

test("a newer wrong callback invalidates an earlier match before seek completion", async () => {
  const h = harness(); h.video.presented(1, 2); h.video.presented(.96, 3); h.video.complete(); h.clock.tick(2500);
  assert.match((await h.outcome).error.message, /could not verify/);
});

test("pending decoder is never sought elsewhere; total wait remains bounded", async () => {
  const h = harness({ previousProof: previous }); h.clock.tick(1000);
  assert.equal(h.video.assigned.length, 1);
  h.video.complete(); h.video.presented(1, 50); assert.equal((await h.outcome).value.logicalS, 1);
});

test("same verified paused target is reused without demanding a new compositor submission", async () => {
  const video = new Video(); video.time = previous.mediaS;
  const h = harness({ video, timeS: .5, endS: .54, previousProof: previous });
  assert.equal((await h.outcome).value.presentedMediaS, .5); assert.deepEqual(video.assigned, []);
  assert.equal(h.clock.timers.size, 0);
});

test("identical-looking pictures at different PTS still require actual target proof", async () => {
  const video = new Video(); video.time = previous.mediaS;
  const h = harness({ video, previousProof: previous }); video.complete(); video.presented(.5, 1); h.clock.tick(2500);
  assert.deepEqual(video.assigned, [1.0004]); assert.match((await h.outcome).error.message, /could not verify/);
});

test("missing or invalid prior proof never replaces a genuine callback", async () => {
  for (const proof of [null, { ...previous, mediaS: -1 }, { ...previous, mediaS: 5 }, { ...previous, presentedMediaS: NaN }, { ...previous, isReusable: () => false }]) {
    const video = new Video(); video.time = previous.mediaS;
    const h = harness({ video, timeS: .5, endS: .54, previousProof: proof }); h.video.complete(); h.clock.tick(2500);
    assert.match((await h.outcome).error.message, /could not verify/); assert.equal(h.video.assigned.length, 1);
  }
});

test("source changes and cancellation reject stale callbacks in either event order", async () => {
  for (const order of ["initial", "callback-first", "seeked-first"]) {
    const h = harness();
    if (order === "callback-first") h.video.presented(1);
    if (order === "seeked-first") h.video.complete();
    const assigned = h.video.assigned.length; const stale = [...h.video.callbacks.values()][0];
    h.sourceChanged(); stale?.(0, { mediaTime: 1, presentedFrames: 11 }); h.clock.tick(2500);
    assert.equal((await h.outcome).error.name, "AbortError"); assert.equal(h.video.assigned.length, assigned); assert.equal(h.clock.timers.size, 0);
  }
  const h = harness(); const stale = [...h.video.callbacks.values()][0];
  h.op.cancel(); h.video.complete(); stale(0, { mediaTime: 1, presentedFrames: 1 }); h.clock.tick(2500);
  assert.equal((await h.outcome).error.name, "AbortError"); assert.equal(h.video.assigned.length, 1);
});

test("play/manual seek/source unload interrupt; late own seeking at stable clock does not", async () => {
  for (const event of ["play", "seeking", "emptied", "abort"]) {
    const h = harness();
    if (event === "play") h.video.paused = false;
    if (event === "seeking") h.video.time = 2;
    h.video.dispatchEvent(new Event(event)); assert.equal((await h.outcome).error.name, "AbortError");
    assert.equal(h.interruptions(), ["play", "seeking"].includes(event) ? 1 : 0);
  }
  const h = harness(); h.video.complete(); h.video.dispatchEvent(new Event("seeking")); h.video.presented(1);
  assert.equal((await h.outcome).value.logicalS, 1);
});

test("nonzero proxy offset is applied once and browser PTS rounding remains strict", async () => {
  const h = harness({ timelineOffsetS: 2 }); h.video.complete(); h.video.presented(3.0000004, 1);
  const result = (await h.outcome).value; assert.equal(result.logicalS, 1); assert.equal(result.mediaS, 3.0004);
  const wrong = harness(); wrong.video.complete(); wrong.video.presented(1.00001); wrong.clock.tick(2500);
  assert.match((await wrong.outcome).error.message, /could not verify/);
});


test("proof lease invalidates on away-and-back seek/play even when the final clock is identical", async () => {
  for (const event of ["seeking", "play"]) {
    const h = harness(); h.video.complete(); h.video.presented(1);
    const proof = (await h.outcome).value;
    assert.equal(proof.isReusable(), true);
    if (event === "seeking") h.video.time = 2;
    if (event === "play") h.video.paused = false;
    h.video.dispatchEvent(new Event(event));
    h.video.time = proof.mediaS; h.video.paused = true; h.video.seeking = false;
    assert.equal(proof.isReusable(), false);
    const next = harness({ video: h.video, previousProof: proof });
    next.video.complete(); next.clock.tick(2500);
    assert.match((await next.outcome).error.message, /could not verify/);
    proof.dispose();
  }
});

test("source/video-specific proof never transfers to a new source or element", async () => {
  for (const prior of [{ ...previous, sourceKey: "old-source" }, { ...previous, video: new Video() }]) {
    const video = new Video(); video.time = previous.mediaS;
    const h = harness({ video, previousProof: prior, timeS: .5, endS: .54 });
    video.complete(); h.clock.tick(2500);
    assert.match((await h.outcome).error.message, /could not verify/);
    assert.equal(video.assigned.length, 1);
  }
});

test("raw media source change cancels even before the owner source effect runs", async () => {
  const h = harness(); h.video.currentSrc = "different-source";
  h.video.complete(); h.video.presented(1);
  assert.equal((await h.outcome).error.name, "AbortError");
});

test("microsecond clock quantization still needs strict matching source PTS", async () => {
  const video = new Video();
  Object.defineProperty(video, "currentTime", { get() { return Math.round(this.time * 1e6) / 1e6; }, set(t) { this.time = t; this.assigned.push(t); this.seeking = true; this.dispatchEvent(new Event("seeking")); } });
  const h = harness({ video, timeS: 1 / 30, endS: 2 / 30 });
  video.complete(); video.presented(.033333, 1);
  assert.equal((await h.outcome).value.logicalS, 1 / 30);
});

test("adapter captures eligibility before replacing owner and releases each old proof lease", async () => {
  let owner = 1;
  const h = harness({ isCurrent: () => owner === 1 }); h.video.complete(); h.video.presented(1);
  let proof = (await h.outcome).value;
  assert.equal(h.video.listenerCount(), 5);
  for (let generation = 2; generation <= 10; generation++) {
    const prior = takeFrameProof(proof); // Real component adapter order.
    assert.equal(h.video.listenerCount(), 0);
    assert.equal(h.video.callbacks.size, 0);
    owner = generation;
    const next = harness({ video: h.video, previousProof: prior, isCurrent: () => owner === generation });
    proof = (await next.outcome).value;
    assert.equal(h.video.assigned.length, 1); // Identical verified target needs no new seek.
    assert.equal(h.video.listenerCount(), 5); // No growing listener set.
    assert.equal(h.video.callbacks.size, 1); // One bounded proof observer.
  }
  proof.dispose(); assert.equal(h.video.listenerCount(), 0);
});

test("failed/canceled operations leave no active callbacks/listeners/timers", async () => {
  for (const action of [h => { h.clock.tick(2500); }, h => h.op.cancel(), h => h.video.dispatchEvent(new Event("error"))]) {
    const h = harness(); action(h); assert.ok((await h.outcome).error);
    assert.equal(h.video.listenerCount(), 0); assert.equal(h.video.callbacks.size, 0); assert.equal(h.clock.timers.size, 0);
  }
});

test("a newer presented picture invalidates reusable proof even without a clock/event change", async () => {
  const h = harness(); h.video.complete(); h.video.presented(1, 5);
  const proof = (await h.outcome).value; assert.equal(proof.isReusable(), true);
  assert.equal(h.video.callbacks.size, 1);
  h.video.presented(.96, 6);
  assert.equal(proof.isReusable(), false); assert.equal(h.video.callbacks.size, 0);
  proof.dispose(); assert.equal(h.video.listenerCount(), 0);
});


test("beginning, last and single-picture intervals never trigger speculative extra seeks", async () => {
  for (const [timeS, endS, duration] of [[0, .04, 5], [4.96, 5, 5], [0, 1, 1]]) {
    const video = new Video(); video.duration = duration;
    const h = harness({ video, timeS, endS }); video.complete(); h.clock.tick(2500);
    assert.match((await h.outcome).error.message, /could not verify/);
    assert.equal(video.assigned.length, 1); assert.equal(h.positions.length, 1);
  }
});

test("observed unchanged .933333 source picture reuses only its still-valid real proof", async () => {
  const video = new Video();
  Object.defineProperty(video, "currentTime", { get() { return Math.trunc(this.time * 1e6) / 1e6; }, set(t) { this.time = t; this.assigned.push(t); this.seeking = true; this.dispatchEvent(new Event("seeking")); } });
  const h = harness({ video, timeS: 14 / 15, endS: 29 / 30 }); video.complete(); video.presented(.933333, 26);
  const proof = (await h.outcome).value;
  assert.equal(video.currentTime, .933666);
  const prior = takeFrameProof(proof);
  const next = harness({ video, timeS: 14 / 15, endS: 29 / 30, previousProof: prior });
  const result = (await next.outcome).value;
  assert.equal(result.presentedMediaS, .933333); assert.equal(video.assigned.length, 1);
  result.dispose();
});
