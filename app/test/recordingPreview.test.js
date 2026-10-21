import assert from "node:assert/strict";
import test from "node:test";
import { INITIAL_PREVIEW, acceptPreviewSnapshot, decodePreviewEnvelope, frameMatchesPreview } from "../src/lib/recordingPreviewState.mjs";
import { captureSourceKey, isCaptureSource, recordingWindowProblem } from "../src/lib/recordingState.mjs";

const source = { kind: "window", windowId: "opaque-window" };
function status(overrides = {}) {
  return { previewId: "native-preview-a", sequence: 10, configEpoch: 1, phase: "previewing", source,
    frameSequence: 8, sourceWidth: 641, sourceHeight: 481, lastFrameAgeMs: 0, message: null, ...overrides };
}
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
  assert.equal(decodePreviewEnvelope(frame({ width: 480, height: 270 })).bmp.byteLength, 388854);
});

test("envelope rejects truncated, oversized, malformed UTF8 and unsupported BMP layouts", () => {
  assert.throws(() => decodePreviewEnvelope(new ArrayBuffer(389891)), /oversized/);
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
  for (const invalid of [{ configEpoch: 0 }, { sequence: -1 }, { capturedAtQpc100ns: 1234 }, { width: 481 }, { height: 271 }]) {
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
