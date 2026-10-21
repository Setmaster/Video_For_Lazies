import assert from "node:assert/strict";
import test from "node:test";
import { acceptRecordingSnapshot, formatRecordingDuration, isRecordingActive, isRecordingRegionWithinDisplay, recordingSourceProblem } from "../src/lib/recordingState.mjs";

function snapshot(sequence, phase = "idle", overrides = {}) {
  return { sessionId: phase === "idle" ? null : "session-a", sequence, phase,
    elapsedMs: 0, bytesWritten: 0, countdownRemaining: null, outputPath: null, message: null, config: null, audio: null, resolvedGeometry: null, selectionRequestId: null, recoveries: [], ...overrides };
}

test("late start and poll replies cannot roll back stop/finalization", () => {
  const finalizing = snapshot(14, "finalizing");
  assert.equal(acceptRecordingSnapshot(finalizing, snapshot(12, "countdown")), finalizing);
  assert.equal(acceptRecordingSnapshot(finalizing, snapshot(13, "recording")), finalizing);
  const completed = snapshot(15, "completed", { outputPath: "C:\\Videos\\saved.mp4" });
  assert.equal(acceptRecordingSnapshot(finalizing, completed), completed);
  assert.equal(acceptRecordingSnapshot(completed, finalizing), completed);
});

test("sequence comparison spans sessions and recovery, including equal-sequence reads", () => {
  const newerSession = snapshot(22, "preparing", { sessionId: "session-b" });
  assert.equal(acceptRecordingSnapshot(newerSession, snapshot(20, "failed")), newerSession);
  assert.equal(acceptRecordingSnapshot(newerSession, snapshot(22)), newerSession);
  const recovered = snapshot(23, "completed", { sessionId: "old-recovery", outputPath: "/saved.mp4" });
  assert.equal(acceptRecordingSnapshot(newerSession, recovered), recovered);
});

test("every setup and save phase keeps activity admission; terminal phases release it", () => {
  for (const phase of ["preparing", "countdown", "starting", "recording", "stopping", "finalizing"]) assert.equal(isRecordingActive(phase), true, phase);
  for (const phase of ["idle", "completed", "recoverable", "failed"]) assert.equal(isRecordingActive(phase), false, phase);
});

test("malformed status cannot report a false idle or file-ready result", () => {
  const active = snapshot(8, "recording");
  for (const invalid of [snapshot(NaN), snapshot(9, "unknown"), snapshot(9, "recording", { sessionId: null }),
    snapshot(9, "completed"), snapshot(9, "idle", { elapsedMs: -1 }), snapshot(9, "idle", { recoveries: [{}] }),
    snapshot(9, "countdown", { countdownRemaining: -1 })]) {
    assert.throws(() => acceptRecordingSnapshot(active, invalid), /invalid status/);
  }
});

test("physical regions support negative origins while rejecting another display and escaped bounds", () => {
  const display = { id: "left", x: -1920, y: -120, width: 1920, height: 1080 };
  const whole = { displayId: "left", x: -1920, y: -120, width: 1920, height: 1080 };
  assert.equal(isRecordingRegionWithinDisplay(whole, display), true);
  assert.equal(isRecordingRegionWithinDisplay({ ...whole, x: -1910, width: 640, height: 480 }, display), true);
  for (const invalid of [{ ...whole, displayId: "right" }, { ...whole, x: -1921 }, { ...whole, width: 1921 },
    { ...whole, height: 0 }, { ...whole, x: -1919.5 }, null]) assert.equal(isRecordingRegionWithinDisplay(invalid, display), false);
  assert.equal(isRecordingRegionWithinDisplay(whole, undefined), false);
});

test("duration uses authoritative elapsed time without wrapping hours or rounding up", () => {
  assert.equal(formatRecordingDuration(999), "00:00:00");
  assert.equal(formatRecordingDuration(3_661_999), "01:01:01");
  assert.equal(formatRecordingDuration(90_000_000), "25:00:00");
});

test("source eligibility preserves exact odd selections within the native 16 to 7680 by 4320 contract", () => {
  const display = { id: "large", x: 0, y: 0, width: 15360, height: 8640 };
  const source = { displayId: "large", x: 0, y: 0, width: 7680, height: 4320 };
  assert.equal(recordingSourceProblem(source, display), null);
  assert.equal(recordingSourceProblem({ ...source, width: 16, height: 16 }, display), null);
  assert.match(recordingSourceProblem({ ...source, width: 15 }, display), /at least 16/);
  assert.match(recordingSourceProblem({ ...source, width: 7682 }, display), /up to 7680/);
  assert.match(recordingSourceProblem({ ...source, height: 4322 }, display), /up to 7680/);
  assert.equal(recordingSourceProblem({ ...source, width: 641 }, display), null);
  assert.equal(isRecordingRegionWithinDisplay({ ...source, height: 481 }, display), true);
});
