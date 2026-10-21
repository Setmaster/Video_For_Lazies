import assert from "node:assert/strict";
import test from "node:test";
import { DEFAULT_AUDIO, acceptAudioStatus, enqueueAudioControl, isAudioStatus, isRecordingAudioConfig } from "../src/lib/recordingAudioState.mjs";
import { isAudioSelection, validateAudioPreviewAsset } from "../src/lib/audioTrackState.mjs";
import { reusableSettingsFromEncodeRequest } from "../src/lib/userRecipes.mjs";
const levels = (renderMuted = false, micMuted = false, gain = 1) => ({ renderLevel: { gain, muted: renderMuted }, microphoneLevel: { gain: 1, muted: micMuted } });
const meter = { state: "active", peak: .5, rms: .2, overload: false };
const status = (sequence, overrides = {}) => ({ sessionId: "session", sourceGeneration: "source", sequence, acceptedCommandSequence: 7, appliedCommandSequence: 0, appliedAtSample: "18446744073709551615", rejectedCommandSequence: null, ...levels(), render: meter, microphone: meter, mixed: meter, message: null, ...overrides });

test("recording defaults activate neither render nor microphone and reject ambiguous sources", () => {
  assert.equal(isRecordingAudioConfig(DEFAULT_AUDIO), true);
  assert.equal(DEFAULT_AUDIO.render.kind, "off"); assert.equal(DEFAULT_AUDIO.microphone, null);
  for (const render of [{ kind: "application", applicationId: "" }, { kind: "both" }, { kind: "system", deviceId: "" }]) assert.equal(isRecordingAudioConfig({ ...DEFAULT_AUDIO, render }), false);
  assert.equal(isRecordingAudioConfig({ ...DEFAULT_AUDIO, microphone: { deviceId: "mic", suppression: "live" } }), false);
  assert.equal(isRecordingAudioConfig({ ...DEFAULT_AUDIO, renderLevel: { gain: Infinity, muted: false } }), false);
  const system = deviceId => ({ ...DEFAULT_AUDIO, render: { kind: "system", deviceId } });
  assert.equal(isRecordingAudioConfig(system("x".repeat(2048))), true);
  assert.equal(isRecordingAudioConfig(system("é".repeat(1024))), true);
  for (const id of ["x".repeat(2049), "é".repeat(1025), "device\0suffix"]) assert.equal(isRecordingAudioConfig(system(id)), false);
  for (const id of ["x".repeat(81), "application/id"]) assert.equal(isRecordingAudioConfig({ ...DEFAULT_AUDIO, render: { kind: "application", applicationId: id } }), false);
});
test("ordered full-state lane retains both sources' mute edges while coalescing only unsent gains", () => {
  let queue = [], previous = levels();
  for (const [sequence, next] of [[1, levels(false, false, .5)], [2, levels(false, false, .6)], [3, levels(true)], [4, levels(true, false, .7)], [5, levels(true, false, .8)], [6, levels(true, true)], [7, levels(false, true)], [8, levels(false, false)]]) {
    queue = enqueueAudioControl(queue, next, sequence, previous); previous = next;
  }
  assert.deepEqual(queue.map(value => value.commandSequence), [2, 3, 5, 6, 7, 8]);
  assert.deepEqual(queue.filter(value => value.muteEdge).map(value => [value.renderLevel.muted, value.microphoneLevel.muted]), [[true, false], [true, true], [false, true], [false, false]]);
  queue[0].submitted = true;
  const next = enqueueAudioControl(queue.slice(0, 1), levels(false, false, .9), 9, levels());
  assert.equal(next.length, 2, "already submitted state is never rewritten");
});
test("edge overflow fails before erasing the 64-command history", () => {
  let queue = [], previous = levels();
  for (let sequence = 1; sequence <= 64; sequence++) { const next = levels(sequence % 2 === 1); queue = enqueueAudioControl(queue, next, sequence, previous); previous = next; }
  const saved = JSON.stringify(queue);
  assert.throws(() => enqueueAudioControl(queue, levels(true), 65, previous), /stop and save/i);
  assert.equal(JSON.stringify(queue), saved);
});
test("audio ACKs preserve u64 sample text and reject malformed, stale-source or rollback state", () => {
  const first = status(1), applied = status(2, { appliedCommandSequence: 7 });
  assert.equal(isAudioStatus(first), true); assert.equal(acceptAudioStatus(first, applied), applied);
  assert.equal(acceptAudioStatus(applied, first), applied);
  assert.throws(() => acceptAudioStatus(applied, status(3, { sourceGeneration: "different" })), /identity/);
  assert.throws(() => acceptAudioStatus(applied, status(3, { appliedCommandSequence: 6 })), /backwards/);
  for (const patch of [{ appliedAtSample: "18446744073709551616" }, { appliedAtSample: 42 }, { renderLevel: { gain: NaN, muted: false } }, { mixed: { ...meter, rms: Infinity } }]) assert.equal(isAudioStatus(status(1, patch)), false);
  assert.equal(isAudioStatus(status(3, { rejectedCommandSequence: 8, message: "Stopped before application" })), true);
});
test("track choice binds absolute index to source identity without treating mute as a missing choice", () => {
  const identity = "a".repeat(64);
  assert.equal(isAudioSelection({ audioStreamIndex: null, audioSourceIdentity: null }), true);
  assert.equal(isAudioSelection({ audioStreamIndex: 7, audioSourceIdentity: identity }), true);
  for (const value of [{ audioStreamIndex: 7, audioSourceIdentity: null }, { audioStreamIndex: -1, audioSourceIdentity: identity }, { audioStreamIndex: 7, audioSourceIdentity: "source" }]) assert.equal(isAudioSelection(value), false);
});
test("preview asset union cannot turn a borrowed original into an owned file or cross a generation", () => {
  const request = { requestId: "req", sourceIdentity: "a".repeat(64), streamIndex: 7 };
  const original = { kind: "original", requestId: "req", proxyId: null, path: "/original.mp4", sourceIdentity: request.sourceIdentity, audioStreamIndex: 7, timelineOffsetS: 0 };
  assert.equal(validateAudioPreviewAsset(original, request), original);
  assert.equal(validateAudioPreviewAsset({ ...original, kind: "proxy", proxyId: "proxy", timelineOffsetS: -.25 }, request).kind, "proxy");
  for (const patch of [{ proxyId: "owned" }, { timelineOffsetS: .25 }, { requestId: "old" }, { audioStreamIndex: 1 }, { kind: "proxy", proxyId: "" }, { timelineOffsetS: Infinity }]) assert.throws(() => validateAudioPreviewAsset({ ...original, ...patch }, request), /identity/);
});
test("reusable recipes omit clip-specific audio binding", () => {
  const result = reusableSettingsFromEncodeRequest({ inputPath: "/clip.mp4", outputPath: "/out.mp4", format: "mp4", sizeLimitMb: 0, audioEnabled: true, normalizeAudio: false, perturbFirstFrame: false, strictFit: false, resize: { mode: "source" }, audioStreamIndex: 7, audioSourceIdentity: "a".repeat(64) });
  assert.ok(result); assert.equal("audioStreamIndex" in result, false); assert.equal("audioSourceIdentity" in result, false);
});
