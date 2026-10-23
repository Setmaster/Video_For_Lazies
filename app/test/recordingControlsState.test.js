import test from "node:test";
import assert from "node:assert/strict";
import { EMPTY_SHORTCUTS, SHORTCUT_ACTIONS, acceptControlsSnapshot, captureShortcutEvent, isShortcutChord, shortcutLabel, startAdmissionFromAck } from "../src/lib/recordingControlsState.mjs";
const token = "a".repeat(32);
function state(revision, invalidationEpoch, minimizeCycle = 0) {
  return { revision, invalidationEpoch, minimizeCycle, ownerToken: token, nativeWindow: "visible", bindings: { ...EMPTY_SHORTCUTS },
    rows: Object.fromEntries(SHORTCUT_ACTIONS.map(action => [action, { configured: null, registered: null, dispatchEnabled: false, status: "unset", reason: null }])),
    assignment: null, arm: null, message: null };
}
test("a newer native event cannot replace the exact older Start acknowledgement", () => {
  const acknowledged = state(8, 3, 1);
  for (const observed of [state(9, 4, 1), state(10, 4, 2)]) {
    assert.equal(acceptControlsSnapshot(observed, acknowledged), observed);
    // UI state keeps the new event. The independent Start request must echo
    // its original ACK; native compares it against the newer current fence.
    const admission = startAdmissionFromAck(acknowledged);
    assert.deepEqual(admission, { ownerToken: token, invalidationEpoch: 3, minimizeCycle: 1 });
    const admitted = admission.ownerToken === observed.ownerToken && admission.invalidationEpoch === observed.invalidationEpoch && admission.minimizeCycle === observed.minimizeCycle;
    assert.equal(admitted, false);
  }
});
test("all four initial shortcuts are unset; supported canonical chords exclude reserved F12", () => {
  assert.deepEqual(EMPTY_SHORTCUTS, { start: null, pause: null, resume: null, stop: null });
  for (const modifiers of ["ctrlShift", "ctrlAlt", "ctrlShiftAlt"]) for (const key of ["A", "0", "F1", "F11", "F13", "F24"]) assert.equal(isShortcutChord({ modifiers, key }), true);
  for (const key of ["F12", "F25", "F0", "Enter", "KeyA", "a"]) assert.equal(isShortcutChord({ modifiers: "ctrlShift", key }), false);
  assert.equal(shortcutLabel(null), "Not set");
  assert.equal(shortcutLabel({ modifiers: "ctrlAlt", key: "F13" }), "Ctrl+Alt+F13");
});
test("local capture forwards logical key to native normalization without mapping DOM code", () => {
  const event = { key: "!", code: "Digit1", ctrlKey: true, shiftKey: true, altKey: false, metaKey: false, repeat: false, isComposing: false };
  assert.deepEqual(captureShortcutEvent(event), { key: "!", ctrl: true, shift: true, alt: false, meta: false });
  for (const patch of [{ repeat: true }, { isComposing: true }, { ctrlKey: false }, { metaKey: true }, { key: "Dead" }]) assert.equal(captureShortcutEvent({ ...event, ...patch }), null);
});
test("malformed status cannot enable a field or synthesize native registration", () => {
  const previous = state(2, 1);
  for (const invalid of [{ ...previous, assignment: { token, expiresInMs: 4000 } }, { ...previous, ownerToken: "bad" }, { ...previous, arm: { token, configRevision: 2, mode: "retained", expiresInMs: 3 } }]) assert.throws(() => acceptControlsSnapshot(previous, invalid), /invalid status/);
});
