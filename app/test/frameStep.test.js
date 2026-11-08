import test from "node:test";
import assert from "node:assert/strict";
import { createFrameStepper, frameDisplaySeek } from "../src/lib/frameStep.mjs";

const tick = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };
function harness(overrides = {}) {
  const state = { key: "source:proxy", enabled: true, blocked: false, activeTarget: "preview", path: "source.mp4", sourceIdentity: "identity", videoStreamIndex: 0, durationS: 4,
    values: { preview: 0, start: 0, end: 4 }, limits: { preview: { min: 0, max: 4 }, start: { min: 0, max: 3.95 }, end: { min: .05, max: 4 } } };
  const calls = [], applied = [], statuses = [], seeks = [];
  let pauses = 0, cancels = 0;
  const stepper = createFrameStepper({ read: () => state, pause: () => pauses++, cancelSeek: () => cancels++,
    fetchStep: async request => { calls.push(request); return { sourceIdentity: "identity", timeS: (Math.round(request.timeS * 30) + request.step) / 30, frameEndS: (Math.round(request.timeS * 30) + request.step + 1) / 30, didMove: true, atLimit: false }; },
    seek: async (...args) => { seeks.push(args); }, apply: (target, time) => applied.push([target, time]), status: value => statuses.push(value), ...overrides });
  return { state, calls, applied, statuses, seeks, stepper, pauses: () => pauses, cancels: () => cancels };
}

test("repeated keys serialize from committed full precision logical PTS", async () => {
  const h = harness();
  h.stepper.enqueue("preview", 1); h.stepper.enqueue("preview", 1); h.stepper.enqueue("preview", 10);
  await tick();
  assert.deepEqual(h.calls.map(c => [c.timeS, c.step]), [[0, 1], [1 / 30, 1], [2 / 30, 10]]);
  assert.equal(h.applied.at(-1)[1], .4);
  assert.equal(h.pauses(), 3);
  assert.deepEqual(h.seeks[0], [1 / 30, 2 / 30]);
  assert.equal(h.statuses.at(-1).message, "Source frame selected.");
});

test("mode, source, target, and blocked guards discard pending native results", async () => {
  for (const mutate of [s => { s.enabled = false; }, s => { s.key = "new source"; }, s => { s.activeTarget = "end"; }, s => { s.blocked = true; }]) {
    const pending = deferred(); const h = harness({ fetchStep: () => pending.promise });
    h.stepper.enqueue("preview", 1); mutate(h.state);
    pending.resolve({ sourceIdentity: "identity", timeS: .1, frameEndS: .2, didMove: true });
    await tick(); assert.deepEqual(h.applied, []); assert.deepEqual(h.seeks, []);
  }
});

test("manual seek/reset supersedes both queued requests and in-flight verified seek", async () => {
  const pending = deferred(); const h = harness({ seek: () => pending.promise });
  h.stepper.enqueue("preview", 1); h.stepper.enqueue("preview", 1);
  await tick(); h.stepper.reset(); pending.resolve(); await tick();
  assert.equal(h.calls.length, 1); assert.deepEqual(h.applied, []); assert.equal(h.cancels(), 1);
  h.state.values.preview = 2;
  h.stepper.enqueue("preview", 1); await tick(); assert.equal(h.calls.at(-1).timeS, 2);
});

test("native errors and invalid source responses are visible without seconds fallback", async () => {
  for (const fetchStep of [async () => { throw new Error("Timing unavailable"); }, async () => ({ sourceIdentity: "wrong", timeS: .1, frameEndS: .2, didMove: true })]) {
    const h = harness({ fetchStep }); h.stepper.enqueue("preview", 1); await tick();
    assert.equal(h.statuses.at(-1).error, true); assert.match(h.statuses.at(-1).message, /Frame stepping unavailable/); assert.deepEqual(h.applied, []);
  }
});

test("bounds stay put and never seek or clamp onto an invented time", async () => {
  const h = harness({ fetchStep: async () => ({ sourceIdentity: "identity", timeS: 0, frameEndS: 0, didMove: false, atLimit: true }) });
  h.stepper.enqueue("preview", -1); await tick();
  assert.deepEqual(h.applied, []); assert.deepEqual(h.seeks, []); assert.match(h.statuses.at(-1).message, /limit/);
});

test("changed opposite trim bound invalidates returned target even before reset", async () => {
  const pending = deferred(); const h = harness({ fetchStep: () => pending.promise }); h.state.activeTarget = "start";
  h.stepper.enqueue("start", 10); h.state.limits.start.max = .2;
  pending.resolve({ sourceIdentity: "identity", timeS: .3, frameEndS: .4, didMove: true }); await tick();
  assert.deepEqual(h.applied, []); assert.equal(h.statuses.at(-1).error, true);
});

test("off and blocked actions make no requests; pending queue has bounded backpressure", async () => {
  const pending = deferred(); const h = harness({ fetchStep: () => pending.promise });
  h.state.enabled = false; assert.equal(h.stepper.enqueue("preview", 1), false);
  h.state.enabled = true; h.state.blocked = true; assert.equal(h.stepper.enqueue("preview", 1), false);
  h.state.blocked = false;
  for (let i = 0; i < 100; i++) assert.equal(h.stepper.enqueue("preview", 1), true);
  assert.match(h.statuses.at(-1).message, /Release/); h.stepper.reset(); pending.resolve({}); await tick(); assert.deepEqual(h.applied, []);
});

test("display seek bias is inside the known interval and leaves exact boundary untouched", () => {
  const pts = 1 / 30, result = frameDisplaySeek(pts, 2 / 30);
  assert.equal(result.seekS, pts + (1 / 30) / 100); assert.equal(result.toleranceS, .000002);
  assert.equal(frameDisplaySeek(1, 2).seekS, 1.001);
  assert.throws(() => frameDisplaySeek(1, 1.000001), /cannot distinguish/);
});

test("native didMove responses must advance in the requested direction before preview verification", async () => {
  for (const [step, target] of [[1, .5], [1, .4], [-1, .5], [-1, .6]]) {
    const h = harness({ fetchStep: async () => ({ sourceIdentity: "identity", timeS: target, frameEndS: target + .03, didMove: true, atLimit: false }) });
    h.state.values.preview = .5;
    h.stepper.enqueue("preview", step); await tick();
    assert.deepEqual(h.seeks, []); assert.deepEqual(h.applied, []);
    assert.equal(h.statuses.at(-1).error, true); assert.match(h.statuses.at(-1).message, /did not advance/);
  }
});
