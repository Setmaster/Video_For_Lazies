import assert from "node:assert/strict";
import test from "node:test";

import {
  ENCODE_EVENT_REGISTRATION_TIMEOUT_MS,
  installEncodeEventListeners,
} from "../src/lib/encodeEvents.mjs";
import { createExportQueueState, reduceExportQueue } from "../src/lib/exportQueue.mjs";

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function manualScheduler() {
  let nextId = 1;
  const tasks = new Map();
  const delays = [];
  return {
    scheduler: {
      setTimeout(callback, delayMs) {
        const id = nextId;
        nextId += 1;
        tasks.set(id, callback);
        delays.push(delayMs);
        return id;
      },
      clearTimeout(id) {
        tasks.delete(id);
      },
    },
    delays,
    fire() {
      const pending = [...tasks.entries()];
      tasks.clear();
      for (const [, callback] of pending) callback();
    },
    pendingCount() {
      return tasks.size;
    },
  };
}

test("terminal encode listener is ready before progress and before exports may start", async () => {
  const finishedRegistration = deferred();
  const progressRegistration = deferred();
  const subscriptions = [];
  let ready = false;
  let starts = 0;
  let finishedHandler = null;

  const listeners = installEncodeEventListeners({
    subscribeFinished(handler) {
      subscriptions.push("finished");
      finishedHandler = handler;
      return finishedRegistration.promise;
    },
    subscribeProgress() {
      subscriptions.push("progress");
      return progressRegistration.promise;
    },
    onFinished() {},
    onProgress() {},
    onReady() {
      ready = true;
    },
    onError(error) {
      throw error;
    },
  });

  const startIfReady = () => {
    if (ready) starts += 1;
  };
  startIfReady();
  assert.equal(starts, 0);
  assert.deepEqual(subscriptions, ["finished"]);

  finishedRegistration.resolve(() => {});
  await Promise.resolve();
  assert.deepEqual(subscriptions, ["finished", "progress"]);
  startIfReady();
  assert.equal(starts, 0);

  progressRegistration.resolve(() => {});
  assert.equal(await listeners.ready, true);
  assert.equal(typeof finishedHandler, "function");
  startIfReady();
  assert.equal(starts, 1);
});

test("listener registration failure removes the terminal listener and stays closed", async () => {
  let terminalUnlistens = 0;
  let reportedError = null;
  let ready = false;
  const expected = new Error("progress registration failed");

  const listeners = installEncodeEventListeners({
    async subscribeFinished() {
      return () => {
        terminalUnlistens += 1;
      };
    },
    async subscribeProgress() {
      throw expected;
    },
    onFinished() {},
    onProgress() {},
    onReady() {
      ready = true;
    },
    onError(error) {
      reportedError = error;
    },
  });

  assert.equal(await listeners.ready, false);
  assert.equal(ready, false);
  assert.equal(reportedError, expected);
  assert.equal(terminalUnlistens, 1);
});

test("terminal listener registration failure does not attempt progress registration", async () => {
  const expected = new Error("terminal registration failed");
  let progressSubscriptions = 0;
  let reportedError = null;

  const listeners = installEncodeEventListeners({
    async subscribeFinished() {
      throw expected;
    },
    async subscribeProgress() {
      progressSubscriptions += 1;
      return () => {};
    },
    onFinished() {},
    onProgress() {},
    onReady() {
      assert.fail("listeners must remain closed");
    },
    onError(error) {
      reportedError = error;
    },
  });

  assert.equal(await listeners.ready, false);
  assert.equal(progressSubscriptions, 0);
  assert.equal(reportedError, expected);
});

test("an immediate terminal event after readiness settles queue diagnostics", async () => {
  let finishedHandler = null;
  let queue = reduceExportQueue(createExportQueueState(), {
    type: "enqueue-prepared",
    items: [{
      request: {
        inputPath: "/input/source.mp4",
        outputPath: "/output/source.mp4",
        format: "mp4",
      },
      durationS: 1,
    }],
  });

  const listeners = installEncodeEventListeners({
    async subscribeFinished(handler) {
      finishedHandler = handler;
      return () => {};
    },
    async subscribeProgress() {
      return () => {};
    },
    onFinished(payload) {
      const active = queue.active;
      assert.ok(active);
      queue = reduceExportQueue(queue, {
        type: "settled",
        itemId: active.itemId,
        runId: active.runId,
        outcome: payload,
      });
      queue = reduceExportQueue(queue, { type: "claim-next" });
    },
    onProgress() {},
    onReady() {},
    onError(error) {
      throw error;
    },
  });

  assert.equal(await listeners.ready, true);
  queue = reduceExportQueue(queue, { type: "start-auto-run" });
  queue = reduceExportQueue(queue, { type: "claim-next" });
  assert.equal(queue.items[0].status, "running");
  assert.equal(typeof finishedHandler, "function");

  finishedHandler({
    kind: "failed",
    message: "Output must be different from input.",
    outputPath: null,
    diagnostics: { mode: "failed", attempts: 0, commandPreview: "ffmpeg <input> <output>" },
    completedAtMs: 123,
  });

  assert.equal(queue.autoRun, false);
  assert.equal(queue.active, null);
  assert.equal(queue.items[0].status, "failed");
  assert.equal(queue.items[0].lastOutcome.diagnostics.mode, "failed");
});

test("disposing before asynchronous registration resolves removes the late listener", async () => {
  const finishedRegistration = deferred();
  let terminalUnlistens = 0;
  let progressSubscriptions = 0;
  let ready = false;
  let reportedError = null;

  const listeners = installEncodeEventListeners({
    subscribeFinished() {
      return finishedRegistration.promise;
    },
    async subscribeProgress() {
      progressSubscriptions += 1;
      return () => {};
    },
    onFinished() {},
    onProgress() {},
    onReady() {
      ready = true;
    },
    onError(error) {
      reportedError = error;
    },
  });

  listeners.dispose();
  finishedRegistration.resolve(() => {
    terminalUnlistens += 1;
  });

  assert.equal(await listeners.ready, false);
  assert.equal(terminalUnlistens, 1);
  assert.equal(progressSubscriptions, 0);
  assert.equal(ready, false);
  assert.equal(reportedError, null);
});

test("disposing while progress registration is pending removes both listeners", async () => {
  const progressRegistration = deferred();
  let terminalUnlistens = 0;
  let progressUnlistens = 0;
  let ready = false;
  let reportedError = null;

  const listeners = installEncodeEventListeners({
    async subscribeFinished() {
      return () => {
        terminalUnlistens += 1;
      };
    },
    subscribeProgress() {
      return progressRegistration.promise;
    },
    onFinished() {},
    onProgress() {},
    onReady() {
      ready = true;
    },
    onError(error) {
      reportedError = error;
    },
  });

  await Promise.resolve();
  listeners.dispose();
  assert.equal(terminalUnlistens, 1);
  progressRegistration.resolve(() => {
    progressUnlistens += 1;
  });

  assert.equal(await listeners.ready, false);
  assert.equal(progressUnlistens, 1);
  assert.equal(ready, false);
  assert.equal(reportedError, null);
});

test("asynchronous listener teardown rejection is consumed", async () => {
  let rejectedTeardownCalls = 0;
  const listeners = installEncodeEventListeners({
    async subscribeFinished() {
      return async () => {
        rejectedTeardownCalls += 1;
        throw new Error("async unlisten failed");
      };
    },
    async subscribeProgress() {
      return () => {};
    },
    onFinished() {},
    onProgress() {},
    onReady() {},
    onError(error) {
      throw error;
    },
  });

  assert.equal(await listeners.ready, true);
  listeners.dispose();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(rejectedTeardownCalls, 1);
});

test("one bounded deadline covers both encode listener registrations", async () => {
  const clock = manualScheduler();
  const progressRegistration = deferred();
  let terminalUnlistens = 0;
  let reportedError = null;

  const listeners = installEncodeEventListeners({
    async subscribeFinished() {
      return () => {
        terminalUnlistens += 1;
      };
    },
    subscribeProgress() {
      return progressRegistration.promise;
    },
    onFinished() {},
    onProgress() {},
    onReady() {
      assert.fail("timed-out listeners must not become ready");
    },
    onError(error) {
      reportedError = error;
    },
    registrationTimeoutMs: 250,
    scheduler: clock.scheduler,
  });

  await Promise.resolve();
  assert.deepEqual(clock.delays, [250]);
  clock.fire();

  assert.equal(await listeners.ready, false);
  assert.equal(terminalUnlistens, 1);
  assert.match(reportedError?.message ?? "", /did not become ready in time/);
});

test("a terminal listener that resolves after the deadline is removed exactly once", async () => {
  const clock = manualScheduler();
  const finishedRegistration = deferred();
  let terminalUnlistens = 0;
  let progressSubscriptions = 0;

  const listeners = installEncodeEventListeners({
    subscribeFinished() {
      return finishedRegistration.promise;
    },
    async subscribeProgress() {
      progressSubscriptions += 1;
      return () => {};
    },
    onFinished() {},
    onProgress() {},
    onReady() {
      assert.fail("timed-out listeners must not become ready");
    },
    onError() {},
    scheduler: clock.scheduler,
  });

  assert.deepEqual(clock.delays, [ENCODE_EVENT_REGISTRATION_TIMEOUT_MS]);
  clock.fire();
  assert.equal(await listeners.ready, false);

  finishedRegistration.resolve(() => {
    terminalUnlistens += 1;
  });
  await Promise.resolve();
  assert.equal(terminalUnlistens, 1);
  assert.equal(progressSubscriptions, 0);

  listeners.dispose();
  assert.equal(terminalUnlistens, 1);
});

test("a progress listener that resolves after the deadline is removed exactly once", async () => {
  const clock = manualScheduler();
  const progressRegistration = deferred();
  let terminalUnlistens = 0;
  let progressUnlistens = 0;

  const listeners = installEncodeEventListeners({
    async subscribeFinished() {
      return () => {
        terminalUnlistens += 1;
      };
    },
    subscribeProgress() {
      return progressRegistration.promise;
    },
    onFinished() {},
    onProgress() {},
    onReady() {
      assert.fail("timed-out listeners must not become ready");
    },
    onError() {},
    scheduler: clock.scheduler,
  });

  await Promise.resolve();
  clock.fire();
  assert.equal(await listeners.ready, false);
  assert.equal(terminalUnlistens, 1);

  progressRegistration.resolve(() => {
    progressUnlistens += 1;
  });
  await Promise.resolve();
  assert.equal(progressUnlistens, 1);

  listeners.dispose();
  assert.equal(terminalUnlistens, 1);
  assert.equal(progressUnlistens, 1);
});

test("a listener rejection after the deadline is consumed without duplicate failure", async () => {
  const clock = manualScheduler();
  const progressRegistration = deferred();
  let reportedErrors = 0;
  const unhandled = [];
  const onUnhandled = (error) => unhandled.push(error);
  process.on("unhandledRejection", onUnhandled);

  try {
    const listeners = installEncodeEventListeners({
      async subscribeFinished() {
        return () => {};
      },
      subscribeProgress() {
        return progressRegistration.promise;
      },
      onFinished() {},
      onProgress() {},
      onReady() {
        assert.fail("timed-out listeners must not become ready");
      },
      onError() {
        reportedErrors += 1;
      },
      scheduler: clock.scheduler,
    });

    await Promise.resolve();
    clock.fire();
    assert.equal(await listeners.ready, false);

    progressRegistration.reject(new Error("late progress registration failure"));
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(reportedErrors, 1);
    assert.deepEqual(unhandled, []);
  } finally {
    process.off("unhandledRejection", onUnhandled);
  }
});

test("successful registration clears its pending deadline", async () => {
  const clock = manualScheduler();
  let errors = 0;
  const listeners = installEncodeEventListeners({
    async subscribeFinished() {
      return () => {};
    },
    async subscribeProgress() {
      return () => {};
    },
    onFinished() {},
    onProgress() {},
    onReady() {},
    onError() {
      errors += 1;
    },
    scheduler: clock.scheduler,
  });

  assert.equal(await listeners.ready, true);
  assert.equal(clock.pendingCount(), 0);
  clock.fire();
  assert.equal(errors, 0);
  listeners.dispose();
});
