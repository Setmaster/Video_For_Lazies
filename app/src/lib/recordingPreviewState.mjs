import { isCaptureSource } from "./recordingState.mjs";

export const INITIAL_PREVIEW = Object.freeze({
  previewId: null, sequence: -1, configEpoch: 0, phase: "idle", source: null,
  frameSequence: 0, sourceWidth: 0, sourceHeight: 0, lastFrameAgeMs: null, message: null,
});
const phases = new Set(["idle", "starting", "previewing", "recording", "stopping", "failed"]);
export function isPreviewLive(phase) { return phase === "previewing" || phase === "recording"; }

export function acceptPreviewSnapshot(current, incoming) {
  const integer = (value) => Number.isSafeInteger(value) && value >= 0;
  if (!incoming || !phases.has(incoming.phase) ||
      ![incoming.sequence, incoming.configEpoch, incoming.frameSequence, incoming.sourceWidth, incoming.sourceHeight].every(integer) ||
      incoming.sourceWidth > 7680 || incoming.sourceHeight > 4320 ||
      (incoming.previewId !== null && (typeof incoming.previewId !== "string" || !incoming.previewId || incoming.previewId.length > 256)) ||
      (incoming.source !== null && !isCaptureSource(incoming.source)) ||
      (incoming.message !== null && typeof incoming.message !== "string") ||
      (incoming.lastFrameAgeMs !== null && (!Number.isFinite(incoming.lastFrameAgeMs) || incoming.lastFrameAgeMs < 0)) ||
      (["starting", "previewing", "recording", "stopping"].includes(incoming.phase) &&
        (!incoming.previewId || !incoming.source || incoming.configEpoch < 1))) {
    throw new Error("Preview returned an invalid status. Stop preview and try again.");
  }
  if (incoming.sequence <= current.sequence) return current;
  if (incoming.previewId && incoming.previewId === current.previewId &&
      (incoming.configEpoch < current.configEpoch || incoming.frameSequence < current.frameSequence)) {
    throw new Error("Preview returned an out-of-order configuration. Stop preview and try again.");
  }
  return incoming;
}

export function frameMatchesPreview(frame, snapshot, afterSequence = 0) {
  return !!frame && isPreviewLive(snapshot.phase) && frame.previewId === snapshot.previewId &&
    frame.configEpoch === snapshot.configEpoch && frame.sequence > afterSequence;
}

// Validate before handing encoded bytes to the browser's image decoder.
export function decodePreviewEnvelope(buffer) {
  if (!(buffer instanceof ArrayBuffer)) throw new Error("Preview returned an invalid binary frame.");
  if (buffer.byteLength === 0) return null;
  const fail = () => { throw new Error("Preview returned a malformed or oversized frame."); };
  if (buffer.byteLength < 12 || buffer.byteLength > 389890) return fail();
  const bytes = new Uint8Array(buffer), view = new DataView(buffer);
  if (bytes[0] !== 86 || bytes[1] !== 70 || bytes[2] !== 76 || bytes[3] !== 80) return fail();
  const headerLength = view.getUint32(4, true), bmpLength = view.getUint32(8, true);
  if (headerLength < 1 || headerLength > 1024 || bmpLength < 54 || bmpLength > 388854 ||
      12 + headerLength + bmpLength !== buffer.byteLength) return fail();
  let header;
  try { header = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(12, 12 + headerLength))); }
  catch { return fail(); }
  if (!header || typeof header.previewId !== "string" || !header.previewId || header.previewId.length > 256 ||
      !Number.isSafeInteger(header.sequence) || header.sequence < 1 ||
      !Number.isSafeInteger(header.configEpoch) || header.configEpoch < 1 ||
      !Number.isSafeInteger(header.width) || header.width < 1 || header.width > 480 ||
      !Number.isSafeInteger(header.height) || header.height < 1 || header.height > 270 ||
      typeof header.capturedAtQpc100ns !== "string" || !/^\d{1,20}$/.test(header.capturedAtQpc100ns)) return fail();
  const offset = 12 + headerLength, bmp = new DataView(buffer, offset, bmpLength);
  const imageBytes = Math.ceil(header.width * 3 / 4) * 4 * header.height;
  if (bmp.getUint16(0, true) !== 0x4d42 || bmp.getUint32(2, true) !== bmpLength ||
      bmp.getUint32(10, true) !== 54 || bmp.getUint32(14, true) !== 40 ||
      bmp.getInt32(18, true) !== header.width || bmp.getInt32(22, true) !== -header.height ||
      bmp.getUint16(26, true) !== 1 || bmp.getUint16(28, true) !== 24 || bmp.getUint32(30, true) !== 0 ||
      (bmp.getUint32(34, true) !== 0 && bmp.getUint32(34, true) !== imageBytes) ||
      imageBytes + 54 !== bmpLength) return fail();
  return { ...header, bmp: buffer.slice(offset) };
}
