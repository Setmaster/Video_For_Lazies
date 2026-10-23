import { isAudioStatus, isMixLevel } from "./recordingAudioState.mjs";
import { controlToken } from "./recordingControlsState.mjs";
const safe = value => Number.isSafeInteger(value) && value >= 0;
export const isSampleIndex = value => typeof value === "string" && /^(0|[1-9]\d{0,19})$/.test(value) && (value.length < 20 || value <= "18446744073709551615");
export const isLevelIntent = value => !!value && safe(value.commandSequence) && value.commandSequence > 0 && isMixLevel(value.renderLevel) && isMixLevel(value.microphoneLevel);
export function isAudioLedger(value) {
  if (!value || !Array.isArray(value.pending) || !Array.isArray(value.terminalUnapplied) || value.pending.length + value.terminalUnapplied.length > 64 || value.pending.length && value.terminalUnapplied.length) return false;
  return value.pending.every((item, index, list) => isLevelIntent(item) && (!index || item.commandSequence > list[index - 1].commandSequence) &&
    (item.placement === "awaitingReceipt" ? item.targetSample === null : ["active", "resumeBound"].includes(item.placement) && isSampleIndex(item.targetSample))) &&
    value.terminalUnapplied.every((item, index, list) => isLevelIntent(item) && (!index || item.commandSequence > list[index - 1].commandSequence));
}
export function isPauseSeal(value) {
  if (!value || !safe(value.pauseRevision) || value.pauseRevision === 0 || !isSampleIndex(value.cutoffSample)) return false;
  if (value.audio === null) return true;
  const audio = value.audio, fence = audio?.fence;
  return !!audio && typeof audio.sourceGeneration === "string" && /^[A-Za-z0-9-]{1,80}$/.test(audio.sourceGeneration) && safe(audio.sealedAcceptedSequence) && !!fence &&
    ["waiting", "ready", "replacementReviewRequired"].includes(fence.state) && (fence.token === null || controlToken(fence.token)) &&
    (fence.replacementReviewToken === null || controlToken(fence.replacementReviewToken)) &&
    (fence.state !== "ready" || controlToken(fence.token)) && (fence.state !== "replacementReviewRequired" || controlToken(fence.replacementReviewToken));
}
export function freezeAudioIntents(queue) {
  if (!Array.isArray(queue) || queue.length > 64 || queue.some((item, index) => !isLevelIntent(item) || index > 0 && item.commandSequence <= queue[index - 1].commandSequence)) throw new Error("Audio changes cannot be synchronized safely. Stop and save the recording.");
  const level = value => ({ gain: Math.fround(value.gain) || 0, muted: value.muted });
  return queue.map(item => ({ commandSequence: item.commandSequence, renderLevel: level(item.renderLevel), microphoneLevel: level(item.microphoneLevel) }));
}
export function validateReconcileReply(request, reply) {
  if (!reply || ["ownerToken", "sessionId", "sourceGeneration", "pauseRevision", "batchId"].some(key => reply[key] !== request[key]) ||
      typeof reply.batchDigest !== "string" || !/^[a-f0-9]{64}$/.test(reply.batchDigest) || !controlToken(reply.fenceToken) ||
      !isAudioStatus(reply.audioStatus) || reply.audioStatus.sessionId !== request.sessionId || reply.audioStatus.sourceGeneration !== request.sourceGeneration ||
      !isAudioLedger(reply.ledger) || reply.ledger.terminalUnapplied.length || reply.ledger.pending.some(item => item.placement === "awaitingReceipt" || !isSampleIndex(item.targetSample)) || !Array.isArray(reply.dispositions) || reply.dispositions.length !== request.commands.length ||
      reply.dispositions.some((item, index) => !item || item.commandSequence !== request.commands[index].commandSequence ||
        !(item.kind === "settled" && item.identityVerified === true && item.appliedAsRequested === null || item.kind === "acceptedPending" &&
          ["active", "resumeBound"].includes(item.placement) && isSampleIndex(item.targetSample)))) {
    throw new Error("Audio synchronization did not confirm the exact paused changes. Stop remains available.");
  }
  return reply;
}
