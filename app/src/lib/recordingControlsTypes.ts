import type { RecordingConfig } from "./recording";
export type ShortcutAction = "start" | "pause" | "resume" | "stop";
export type ShortcutChord = { modifiers: "ctrlShift" | "ctrlAlt" | "ctrlShiftAlt"; key: string };
export type ShortcutBindings = Record<ShortcutAction, ShortcutChord | null>;
export type StartDraft = Omit<RecordingConfig, "requestId">;
export type ControlsSnapshot = {
  revision: number; ownerToken: string | null; invalidationEpoch: number; minimizeCycle: number;
  bindings: ShortcutBindings;
  rows: Record<ShortcutAction, { configured: ShortcutChord | null; registered: ShortcutChord | null; dispatchEnabled: boolean;
    status: "unset" | "registered" | "suppressed" | "conflict" | "unavailable" | "cleanupFailed"; reason: string | null }>;
  assignment: { token: string; expiresInMs: number } | null;
  arm: { token: string; configRevision: number; mode: "leased" | "retained"; expiresInMs: number | null } | null;
  nativeWindow: "visible" | "minimized" | "hidden" | "unavailable"; message: string | null;
};
export type ControlsRequest =
  | { operation: "snapshot" }
  | { operation: "attachOwner"; expectedRevision: number; expectedOwnerToken: string | null; rendererInstanceId: string }
  | { operation: "beginAssignment"; expectedRevision: number; ownerToken: string; operationId: string }
  | { operation: "renewAssignment"; ownerToken: string; assignmentToken: string }
  | { operation: "applyBindings"; expectedRevision: number; ownerToken: string; assignmentToken: string; operationId: string; captureClosed: true; bindings: ShortcutBindings }
  | { operation: "endAssignment"; expectedRevision: number; ownerToken: string; assignmentToken: string; captureClosed: true }
  | { operation: "reset"; expectedRevision: number; ownerToken: string; operationId: string }
  | { operation: "publishDraft"; ownerToken: string; invalidationEpoch: number; minimizeCycle: number; expectedArmToken: string | null;
      clientRevision: number; configRevision: number; draft: StartDraft }
  | { operation: "renewDraft"; ownerToken: string; armToken: string; configRevision: number }
  | { operation: "invalidateDraft"; ownerToken: string; clientRevision: number }
  | { operation: "classifyVisibility"; ownerToken: string };
export type ControlsReply = { ok: true; value: ControlsSnapshot } | { ok: false; error: { code: string; message: string }; state: ControlsSnapshot };
export type DraftProjection = { visible: boolean; busy: boolean; config: StartDraft | null };
