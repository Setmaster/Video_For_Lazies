import test from "node:test";
import assert from "node:assert/strict";
import { enqueueAudioControl } from "../src/lib/recordingAudioState.mjs";
import { freezeAudioIntents, isAudioLedger, isPauseSeal, isSampleIndex, validateReconcileReply } from "../src/lib/recordingSessionState.mjs";

const token = "a".repeat(32);
const level = (renderMuted = false, microphoneMuted = false, gain = .1) => ({ renderLevel: { gain, muted: renderMuted }, microphoneLevel: { gain: 1, muted: microphoneMuted } });
const intent = (commandSequence, ...args) => ({ commandSequence, ...level(...args) });
const emptyLedger = () => ({ pending: [], terminalUnapplied: [] });
function replyFor(commands) {
  const request = { ownerToken: token, sessionId: "session", sourceGeneration: "source", pauseRevision: 4, batchId: "b".repeat(32), mode: "retainedQueue", commands };
  const meter = { state: "off", peak: 0, rms: 0, overload: false };
  const reply = { ...request, batchDigest: "c".repeat(64), fenceToken: "d".repeat(32),
    dispositions: commands.map(command => ({ kind: "acceptedPending", commandSequence: command.commandSequence, targetSample: "48000", placement: "resumeBound" })),
    audioStatus: { sessionId: "session", sourceGeneration: "source", sequence: 7, acceptedCommandSequence: commands[commands.length - 1]?.commandSequence ?? 0,
      appliedCommandSequence: 0, appliedAtSample: "0", rejectedCommandSequence: null, ...level(), render: meter, microphone: meter, mixed: meter, message: null },
    ledger: { pending: commands.map(command => ({ ...command, targetSample: "48000", placement: "resumeBound" })), terminalUnapplied: [] } };
  return { request, reply };
}

test("Pause freezes submitted, uncertain, and unsent full states without dropping either mute edge or renumbering gaps", () => {
  let queue = [], previous = level();
  for (const [sequence, next] of [[1, level(false, false, .2)], [2, level(false, false, .3)], [3, level(true)], [4, level(true, true)], [5, level(false, true)], [6, level(false, false)]]) {
    queue = enqueueAudioControl(queue, next, sequence, previous); previous = next;
  }
  queue[0].submitted = true; queue[1].submitted = true;
  const frozen = freezeAudioIntents(queue);
  assert.deepEqual(frozen.map(command => command.commandSequence), [2, 3, 4, 5, 6]);
  assert.deepEqual(frozen.map(command => [command.renderLevel.muted, command.microphoneLevel.muted]), [[false, false], [true, false], [true, true], [false, true], [false, false]]);
  assert.equal(frozen[0].renderLevel.gain, Math.fround(.3));
  assert.equal("submitted" in frozen[0], false);
  queue[0].renderLevel.gain = .9;
  assert.equal(frozen[0].renderLevel.gain, Math.fround(.3), "later queue/status mutation cannot change the sealed batch");
  assert.equal(Object.is(freezeAudioIntents([{ ...intent(8), renderLevel: { gain: -0, muted: false } }])[0].renderLevel.gain, -0), false);
});

test("all 64 protected controls survive Pause and invalid or reordered batches fail before transmission", () => {
  const queue = Array.from({ length: 64 }, (_, i) => ({ ...intent(i * 2 + 1, i % 2 === 0), submitted: i % 3 === 0 }));
  assert.equal(freezeAudioIntents(queue).length, 64);
  for (const invalid of [[...queue, intent(129)], [queue[1], queue[0]], [intent(0)], [{ ...intent(1), microphoneLevel: { gain: Infinity, muted: false } }]]) assert.throws(() => freezeAudioIntents(invalid), /synchronized safely/);
});

test("accepted main-queue intent can await receipt but a ready Resume fence cannot", () => {
  const pending = { ...intent(3), targetSample: null, placement: "awaitingReceipt" };
  assert.equal(isAudioLedger({ pending: [pending], terminalUnapplied: [] }), true);
  const { request, reply } = replyFor([intent(3)]);
  assert.equal(validateReconcileReply(request, reply), reply);
  for (const patch of [
    { ledger: { pending: [pending], terminalUnapplied: [] } },
    { dispositions: [{ kind: "acceptedPending", commandSequence: 3, targetSample: null, placement: "awaitingReceipt" }] },
    { dispositions: [{ kind: "deferredNotAccepted", commandSequence: 3, reason: "paused", retryAfterRevision: 4 }] },
    { ledger: { pending: [], terminalUnapplied: [intent(3)] } },
  ]) assert.throws(() => validateReconcileReply(request, { ...reply, ...patch }), /exact paused changes/);
});

test("reconciliation requires exact context/order and settled identity never claims PCM application while paused", () => {
  const { request, reply } = replyFor([intent(3), intent(8, true)]);
  const settled = { ...reply, dispositions: request.commands.map(command => ({ kind: "settled", commandSequence: command.commandSequence, identityVerified: true, appliedAsRequested: null })), ledger: emptyLedger() };
  assert.equal(validateReconcileReply(request, settled), settled);
  for (const patch of [{ ownerToken: "e".repeat(32) }, { pauseRevision: 5 }, { sourceGeneration: "replacement" }, { dispositions: [...reply.dispositions].reverse() },
    { dispositions: settled.dispositions.map(item => ({ ...item, appliedAsRequested: true })) }, { dispositions: settled.dispositions.map(item => ({ ...item, identityVerified: false })) }]) assert.throws(() => validateReconcileReply(request, { ...reply, ...patch }), /exact paused changes/);
});

test("pause seals distinguish disabled audio from a required review/fence and retain exact u64 samples", () => {
  const seal = { pauseRevision: 1, cutoffSample: "18446744073709551615", audio: null };
  assert.equal(isPauseSeal(seal), true);
  const audio = { sourceGeneration: "source", sealedAcceptedSequence: 0, fence: { state: "waiting", token: null, replacementReviewToken: null } };
  assert.equal(isPauseSeal({ ...seal, audio }), true);
  assert.equal(isPauseSeal({ ...seal, audio: { ...audio, fence: { ...audio.fence, state: "ready" } } }), false);
  assert.equal(isPauseSeal({ ...seal, audio: { ...audio, fence: { ...audio.fence, state: "replacementReviewRequired", replacementReviewToken: token } } }), true);
  for (const invalid of ["18446744073709551616", "01", "-1", 48000]) assert.equal(isSampleIndex(invalid), false);
  assert.equal(isAudioLedger({ pending: [{ ...intent(1), targetSample: "48000", placement: "active" }], terminalUnapplied: [intent(2)] }), false);
});
