export const SHORTCUT_ACTIONS = Object.freeze(["start", "pause", "resume", "stop"]);
export const EMPTY_SHORTCUTS = Object.freeze({ start: null, pause: null, resume: null, stop: null });
export const controlToken = value => typeof value === "string" && /^[a-f0-9]{32}$/.test(value);
const integer = value => Number.isSafeInteger(value) && value >= 0;
export function isShortcutChord(value) {
  return !!value && ["ctrlShift", "ctrlAlt", "ctrlShiftAlt"].includes(value.modifiers) && typeof value.key === "string" &&
    /^(?:[A-Z0-9]|F(?:[1-9]|1[0-1]|1[3-9]|2[0-4]))$/.test(value.key);
}
export function shortcutLabel(chord) {
  return isShortcutChord(chord) ? `${({ ctrlShift: "Ctrl+Shift", ctrlAlt: "Ctrl+Alt", ctrlShiftAlt: "Ctrl+Shift+Alt" })[chord.modifiers]}+${chord.key}` : "Not set";
}
export function isControlsSnapshot(value) {
  if (!value || ![value.revision, value.invalidationEpoch, value.minimizeCycle].every(integer) ||
      value.ownerToken !== null && !controlToken(value.ownerToken) ||
      !["visible", "minimized", "hidden", "unavailable"].includes(value.nativeWindow) ||
      value.message !== null && typeof value.message !== "string") return false;
  if (!value.bindings || !value.rows || SHORTCUT_ACTIONS.some(action => {
    const row = value.rows[action], chord = value.bindings[action];
    return chord !== null && !isShortcutChord(chord) || !row ||
      row.configured !== null && !isShortcutChord(row.configured) || row.registered !== null && !isShortcutChord(row.registered) ||
      typeof row.dispatchEnabled !== "boolean" || !["unset", "registered", "suppressed", "conflict", "unavailable", "cleanupFailed"].includes(row.status) ||
      row.reason !== null && typeof row.reason !== "string";
  })) return false;
  if (value.assignment !== null && (!controlToken(value.assignment?.token) || !Number.isFinite(value.assignment.expiresInMs) || value.assignment.expiresInMs < 0 || value.assignment.expiresInMs > 3000)) return false;
  const arm = value.arm;
  return arm === null || !!arm && controlToken(arm.token) && integer(arm.configRevision) && ["leased", "retained"].includes(arm.mode) &&
    (arm.mode === "retained" ? arm.expiresInMs === null : Number.isFinite(arm.expiresInMs) && arm.expiresInMs >= 0 && arm.expiresInMs <= 3000);
}
export function acceptControlsSnapshot(current, incoming) {
  if (!isControlsSnapshot(incoming)) throw new Error("Recording shortcuts returned an invalid status. Refresh controls before assigning a shortcut.");
  if (current && (incoming.revision < current.revision || incoming.invalidationEpoch < current.invalidationEpoch || incoming.minimizeCycle < current.minimizeCycle)) return current;
  return incoming;
}
export function captureShortcutEvent(event) {
  if (event.repeat || event.isComposing || !event.ctrlKey || event.metaKey || !(event.shiftKey || event.altKey) ||
      ["Control", "Shift", "Alt", "Meta", "Dead", "Unidentified"].includes(event.key)) return null;
  // Windows resolves the actual logical key/layout. DOM code is never a VK.
  return { key: event.key, ctrl: event.ctrlKey, shift: event.shiftKey, alt: event.altKey, meta: event.metaKey };
}
export function startAdmissionFromAck(acknowledged) {
  if (!isControlsSnapshot(acknowledged) || !controlToken(acknowledged.ownerToken) || acknowledged.nativeWindow !== "visible") return null;
  return { ownerToken: acknowledged.ownerToken, invalidationEpoch: acknowledged.invalidationEpoch, minimizeCycle: acknowledged.minimizeCycle };
}
