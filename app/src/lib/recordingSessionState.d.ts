import type { AudioControl } from "./recordingAudioTypes";
import type { AudioLedger, LevelIntent, PauseSeal, ReconcileReply, ReconcileRequest } from "./recordingSessionTypes";
export function isSampleIndex(value: unknown): value is string;
export function isLevelIntent(value: unknown): value is LevelIntent;
export function isAudioLedger(value: unknown): value is AudioLedger;
export function isPauseSeal(value: unknown): value is PauseSeal;
export function freezeAudioIntents(queue: readonly (AudioControl | LevelIntent)[]): LevelIntent[];
export function validateReconcileReply(request: ReconcileRequest, reply: ReconcileReply): ReconcileReply;
