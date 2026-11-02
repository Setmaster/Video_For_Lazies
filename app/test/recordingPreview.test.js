import assert from "node:assert/strict";
import test from "node:test";
import { INITIAL_PREVIEW, acceptPreviewSnapshot, automaticPreviewAction, canRunRecordingPreview, decodePreviewEnvelope, frameMatchesPreview } from "../src/lib/recordingPreviewState.mjs";
import { captureSourceKey, isCaptureSource, recordingWindowProblem } from "../src/lib/recordingState.mjs";

const source = { kind: "window", windowId: "opaque-window" };
const automatic = { enabled: true, visible: true, documentVisible: true, canPreview: true, recordingPhase: "idle",
  error: null, handoff: false, stopping: false, hasRequest: false, starting: false, previewPhase: "idle" };

test("a selected visible source starts preview without output or audio configuration", () => {
  assert.equal(automaticPreviewAction(automatic), "start");
  for (const patch of [{ hasRequest: true, previewPhase: "previewing" }, { starting: true }, { stopping: true }, { previewPhase: "starting" }]) {
    assert.equal(automaticPreviewAction({ ...automatic, ...patch }), "none");
  }
  assert.equal(automaticPreviewAction({ ...automatic, canPreview: false }), "none", "Area without a region must not fall back to the display");
});

test("native minimize and terminal leases recover even without document visibility events", () => {
  const live = { ...automatic, hasRequest: true, previewPhase: "previewing" };
  assert.equal(automaticPreviewAction({ ...live, canPreview: false }), "stop", "native window state revokes capture");
  assert.equal(automaticPreviewAction({ ...automatic, canPreview: false }), "none", "a minimized window cannot restart");
  assert.equal(automaticPreviewAction({ ...automatic, hasRequest: true }), "stop", "retire a native-ended request on restore");
  assert.equal(automaticPreviewAction({ ...automatic, hasRequest: true, starting: true }), "none", "do not retire an in-flight admission");
  assert.equal(automaticPreviewAction(automatic), "start", "restored native visibility admits a fresh request after cleanup");
});

test("explicit hide survives source availability, visibility and recording changes", () => {
  for (const recordingPhase of ["idle", "recording", "completed"]) {
    assert.equal(automaticPreviewAction({ ...automatic, enabled: false, recordingPhase }), "none");
    assert.equal(automaticPreviewAction({ ...automatic, enabled: false, recordingPhase, hasRequest: true }), "stop");
  }
  for (const patch of [{ visible: false }, { documentVisible: false }, { canPreview: false }]) {
    assert.equal(automaticPreviewAction({ ...automatic, hasRequest: true, ...patch }), "stop");
    assert.equal(canRunRecordingPreview({ ...automatic, ...patch }), false);
  }
  assert.equal(automaticPreviewAction(automatic), "start", "returning to visible Record can reconcile enabled preview");
});

test("recording handoff cannot start another standalone helper", () => {
  assert.equal(automaticPreviewAction({ ...automatic, handoff: true }), "none");
  assert.equal(automaticPreviewAction({ ...automatic, handoff: true, hasRequest: true }), "none");
  for (const recordingPhase of ["preparing", "countdown", "starting", "pausing", "paused", "resuming", "stopping", "finalizing"]) {
    assert.equal(canRunRecordingPreview({ ...automatic, recordingPhase }), false);
    assert.equal(automaticPreviewAction({ ...automatic, recordingPhase, hasRequest: true }), "stop");
  }
  assert.equal(automaticPreviewAction({ ...automatic, recordingPhase: "recording" }), "start", "attach only after actual recording acknowledgement");
});

test("failed start or unconfirmed cleanup waits for an explicit retry", () => {
  for (const patch of [{ error: "Start failed" }, { previewPhase: "failed" }, { error: "Stop unconfirmed", hasRequest: true },
    { error: "Stop unconfirmed", hasRequest: true, enabled: false }]) {
    for (let render = 0; render < 5; render++) assert.equal(automaticPreviewAction({ ...automatic, ...patch }), "none");
  }
  assert.equal(automaticPreviewAction({ ...automatic, stopping: true, hasRequest: true, visible: false }), "none", "do not overlap cleanup calls while stopping");
});
function status(overrides = {}) {
  return { previewId: "native-preview-a", sequence: 10, configEpoch: 1, phase: "previewing", source,
    frameSequence: 8, sourceWidth: 641, sourceHeight: 481, lastFrameAgeMs: 0, message: null, ...overrides };
}
test("native presentation telemetry keeps exact clocks and rejects malformed surfaces", () => {
  const native = status({ transport: "native", presentedFrames: 8, presentedAtQpc100ns: "12345678901234567",
    capturedAtQpc100ns: "12345678901230000", nativeWindow: "123456", nativeVisible: true, viewportWidth: 900, viewportHeight: 500,
    nativeContentRect: { left: 100, top: 0, width: 700, height: 500 } });
  assert.equal(acceptPreviewSnapshot(INITIAL_PREVIEW, native), native);
  for (const patch of [{ transport: "other" }, { presentedFrames: -1 }, { nativeWindow: 123456 },
    { nativeVisible: "true" }, { viewportWidth: 32768 }, { capturedAtQpc100ns: 123 }, { presentedAtQpc100ns: "NaN" },
    { nativeContentRect: { left: 500, top: 0, width: 700, height: 500 } }]) {
    assert.throws(() => acceptPreviewSnapshot(INITIAL_PREVIEW, { ...native, ...patch }), /invalid status/);
  }
  const stopped = { ...native, sequence: 11, phase: "idle", nativeVisible: false, nativeWindow: null };
  assert.equal(acceptPreviewSnapshot(stopped, native), stopped, "late native telemetry cannot resurrect a stopped surface");
});
function frame(overrides = {}) {
  const metadata = { previewId: "native-preview-a", configEpoch: 1, sequence: 9, width: 3, height: 2, capturedAtQpc100ns: "1234567890123456789", ...overrides };
  const json = new TextEncoder().encode(JSON.stringify(metadata));
  const stride = Math.ceil(metadata.width * 3 / 4) * 4;
  const bmpLength = 54 + stride * metadata.height;
  const buffer = new ArrayBuffer(12 + json.length + bmpLength);
  const bytes = new Uint8Array(buffer), view = new DataView(buffer);
  bytes.set([86, 70, 76, 80]); view.setUint32(4, json.length, true); view.setUint32(8, bmpLength, true);
  bytes.set(json, 12);
  const bmp = new DataView(buffer, 12 + json.length);
  bmp.setUint16(0, 0x4d42, true); bmp.setUint32(2, bmpLength, true); bmp.setUint32(10, 54, true);
  bmp.setUint32(14, 40, true); bmp.setInt32(18, metadata.width, true); bmp.setInt32(22, -metadata.height, true);
  bmp.setUint16(26, 1, true); bmp.setUint16(28, 24, true); bmp.setUint32(34, stride * metadata.height, true);
  return buffer;
}

test("capture kinds are explicit and opaque windows cannot become region fallback", () => {
  assert.equal(isCaptureSource(source), true);
  assert.equal(isCaptureSource({ windowId: "opaque-window" }), false);
  assert.equal(isCaptureSource({ kind: "window", windowId: "" }), false);
  assert.equal(isCaptureSource({ kind: "monitor", displayId: "main", x: 0, y: 0, width: 640, height: 480 }), false);
  const region = { kind: "region", displayId: "left", x: -1920, y: -100, width: 641, height: 481 };
  assert.equal(isCaptureSource(region), true);
  assert.notEqual(captureSourceKey(region), captureSourceKey({ ...region, x: -1919 }));
  assert.notEqual(captureSourceKey(source), captureSourceKey({ ...source, windowId: "different-generation" }));
});

test("window eligibility distinguishes unset, missing, excluded and unknown protection", () => {
  const target = { id: source.windowId, available: true, method: "wgc-window", protection: "unknown", reason: null };
  assert.match(recordingWindowProblem("", [target]), /Choose/);
  assert.match(recordingWindowProblem("retired", [target]), /unavailable/);
  assert.match(recordingWindowProblem(source.windowId, [{ ...target, protection: "excluded" }]), /excludes/);
  assert.equal(recordingWindowProblem(source.windowId, [{ ...target, available: false, reason: "Window minimized" }]), "Window minimized");
  assert.equal(recordingWindowProblem(source.windowId, [target]), null);
});

test("preview status accepts global ordering and rejects malformed epochs and source data", () => {
  const live = status();
  assert.equal(acceptPreviewSnapshot(INITIAL_PREVIEW, live), live);
  assert.equal(acceptPreviewSnapshot(live, status({ sequence: 9, phase: "starting" })), live);
  const resized = status({ sequence: 11, configEpoch: 2, sourceWidth: 800, sourceHeight: 600 });
  assert.equal(acceptPreviewSnapshot(live, resized), resized);
  assert.throws(() => acceptPreviewSnapshot(resized, status({ sequence: 12 })), /out-of-order/);
  for (const invalid of [status({ previewId: null }), status({ configEpoch: 0 }), status({ source: null }), status({ frameSequence: NaN }), status({ lastFrameAgeMs: -1 })]) {
    assert.throws(() => acceptPreviewSnapshot(live, invalid), /invalid/);
  }
});

test("bounded BMP envelope retains exact odd rows and lossless QPC text", () => {
  const decoded = decodePreviewEnvelope(frame());
  assert.equal(decoded.width, 3); assert.equal(decoded.height, 2);
  assert.equal(decoded.bmp.byteLength, 78);
  assert.equal(decoded.capturedAtQpc100ns, "1234567890123456789");
  assert.equal(decodePreviewEnvelope(new ArrayBuffer(0)), null);
  assert.equal(decodePreviewEnvelope(frame({ width: 960, height: 540 })).bmp.byteLength, 1555254);
  const buffer = frame();
  assert.equal(decodePreviewEnvelope(buffer).bmp.buffer, buffer, "validated BMP is a bounded view without another frame copy");
});

test("envelope rejects truncated, oversized, malformed UTF8 and unsupported BMP layouts", () => {
  assert.throws(() => decodePreviewEnvelope(new ArrayBuffer(1556291)), /oversized/);
  const good = frame();
  assert.throws(() => decodePreviewEnvelope(good.slice(0, -1)), /malformed/);
  for (const mutate of [
    (view) => view.setUint32(4, 1025, true),
    (view) => view.setUint8(12, 255),
    (view) => { const offset = 12 + view.getUint32(4, true); view.setInt32(offset + 22, 2, true); },
    (view) => { const offset = 12 + view.getUint32(4, true); view.setUint16(offset + 28, 32, true); },
    (view) => { const offset = 12 + view.getUint32(4, true); view.setUint32(offset + 10, 0, true); },
  ]) {
    const altered = good.slice(0); mutate(new DataView(altered));
    assert.throws(() => decodePreviewEnvelope(altered), /malformed/);
  }
  for (const invalid of [{ configEpoch: 0 }, { sequence: -1 }, { capturedAtQpc100ns: 1234 }, { width: 961 }, { height: 541 }]) {
    assert.throws(() => decodePreviewEnvelope(frame(invalid)), /malformed/);
  }
});

test("generation and epoch gates reject late frames both before and after decode", () => {
  const decoded = decodePreviewEnvelope(frame());
  assert.equal(frameMatchesPreview(decoded, status(), 8), true);
  assert.equal(frameMatchesPreview(decoded, status(), 9), false);
  assert.equal(frameMatchesPreview(decoded, status({ previewId: "native-preview-b" }), 0), false);
  assert.equal(frameMatchesPreview(decoded, status({ configEpoch: 2 }), 0), false);
  assert.equal(frameMatchesPreview(decodePreviewEnvelope(frame({ configEpoch: 2 })), status(), 0), false);
  assert.equal(frameMatchesPreview(decoded, status({ phase: "stopping" }), 0), false);
  // An accepted frame finishing decode after resize or source closure cannot reappear.
  const acceptedBeforeDecode = frameMatchesPreview(decoded, status(), 8);
  assert.equal(acceptedBeforeDecode && frameMatchesPreview(decoded, status({ configEpoch: 2 }), 0), false);
  assert.equal(acceptedBeforeDecode && frameMatchesPreview(decoded, status({ phase: "failed" }), 0), false);
});

test("healthy static imagery can repeat while health remains explicit", () => {
  const live = status({ lastFrameAgeMs: 4000 });
  assert.equal(acceptPreviewSnapshot(INITIAL_PREVIEW, live), live);
  assert.equal(frameMatchesPreview(decodePreviewEnvelope(frame()), live, 8), true);
});
