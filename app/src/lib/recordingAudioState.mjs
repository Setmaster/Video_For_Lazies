export const DEFAULT_AUDIO = Object.freeze({ render: { kind: "off" }, microphone: null,
  renderLevel: { gain: 1, muted: false }, microphoneLevel: { gain: 1, muted: false } });
const identifier = value => typeof value === "string" && value.length > 0 && value.length <= 128;
const deviceIdentifier = value => typeof value === "string" && value.length > 0 && !value.includes("\0") && new TextEncoder().encode(value).length <= 2048;
const applicationIdentifier = value => typeof value === "string" && /^[A-Za-z0-9-]{1,80}$/.test(value);
export const isMixLevel = value => !!value && Number.isFinite(value.gain) && value.gain >= 0 && value.gain <= 2 && typeof value.muted === "boolean";
export function isRecordingAudioConfig(value) {
  return !!value && isMixLevel(value.renderLevel) && isMixLevel(value.microphoneLevel) &&
    (value.render?.kind === "off" || value.render?.kind === "system" && deviceIdentifier(value.render.deviceId) ||
      value.render?.kind === "application" && applicationIdentifier(value.render.applicationId)) &&
    (value.microphone === null || !!value.microphone && deviceIdentifier(value.microphone.deviceId) && ["off", "save"].includes(value.microphone.suppression));
}
export function isAudioStatus(value) {
  const integer = value => Number.isSafeInteger(value) && value >= 0;
  const meter = value => !!value && ["off", "pending", "active", "silent", "muted", "failed"].includes(value.state) &&
    Number.isFinite(value.peak) && value.peak >= 0 && Number.isFinite(value.rms) && value.rms >= 0 && typeof value.overload === "boolean";
  return !!value && identifier(value.sessionId) && identifier(value.sourceGeneration) && integer(value.sequence) &&
    integer(value.acceptedCommandSequence) && integer(value.appliedCommandSequence) && value.appliedCommandSequence <= value.acceptedCommandSequence && (value.rejectedCommandSequence === null || integer(value.rejectedCommandSequence)) &&
    typeof value.appliedAtSample === "string" && /^(0|[1-9]\d{0,19})$/.test(value.appliedAtSample) &&
    (value.appliedAtSample.length < 20 || value.appliedAtSample <= "18446744073709551615") &&
    isMixLevel(value.renderLevel) && isMixLevel(value.microphoneLevel) && meter(value.render) && meter(value.microphone) && meter(value.mixed) &&
    (value.message === null || typeof value.message === "string" && value.message.length <= 4096);
}
export function acceptAudioStatus(current, incoming) {
  if (!isAudioStatus(incoming)) throw new Error("Audio returned an invalid status. Refresh audio status before changing levels.");
  if (current && current.sessionId === incoming.sessionId) {
    if (current.sourceGeneration !== incoming.sourceGeneration) throw new Error("Audio source identity changed. Stop and save before changing the source.");
    if (incoming.sequence <= current.sequence) return current;
    if (incoming.appliedCommandSequence < current.appliedCommandSequence || incoming.acceptedCommandSequence < current.acceptedCommandSequence) throw new Error("Audio acknowledgement moved backwards.");
  }
  return incoming;
}
export const sameMuteState = (a, b) => a.renderLevel.muted === b.renderLevel.muted && a.microphoneLevel.muted === b.microphoneLevel.muted;
export function enqueueAudioControl(queue, levels, commandSequence, previous) {
  if (!isMixLevel(levels.renderLevel) || !isMixLevel(levels.microphoneLevel) || !Number.isSafeInteger(commandSequence) || commandSequence < 1) throw new Error("Invalid audio level change.");
  const command = { ...structuredClone(levels), commandSequence, submitted: false, muteEdge: !sameMuteState(previous, levels) };
  const last = queue.at(-1);
  // An edge retains its own place and receipt boundary. Only a later unsent
  // gain-only update in the same mute state may replace another gain update.
  if (last && !last.submitted && !last.muteEdge && !command.muteEdge && sameMuteState(last, command)) return [...queue.slice(0, -1), command];
  if (queue.length >= 64) throw new Error("Audio control queue is full. Recording will stop and save without dropping a mute change.");
  return [...queue, command];
}
