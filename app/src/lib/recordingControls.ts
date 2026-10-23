import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ControlsReply, ControlsRequest, ControlsSnapshot, DraftProjection, ShortcutBindings, ShortcutChord } from "./recordingControlsTypes";
import { acceptControlsSnapshot, controlToken, isShortcutChord, startAdmissionFromAck } from "./recordingControlsState";
export type { ControlsSnapshot, DraftProjection, ShortcutBindings, ShortcutChord } from "./recordingControlsTypes";
export const recordingControlId = () => crypto.randomUUID().replace(/-/g, "");
const explanation = (failure: unknown) => failure instanceof Error ? failure.message : String(failure);
const pause = (ms: number) => new Promise<void>(resolve => setTimeout(resolve, ms));
export type RecordingControls = ReturnType<typeof useRecordingControls>;

export function useRecordingControls({ blocked }: { blocked: boolean }) {
  const [snapshot, setSnapshot] = useState<ControlsSnapshot | null>(null);
  const [ownerToken, setOwnerToken] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [connected, setConnected] = useState(false);
  const mounted = useRef(true), current = useRef<ControlsSnapshot | null>(null), owner = useRef<string | null>(null);
  const ownerLost = useRef(false), attachRequest = useRef<Extract<ControlsRequest, { operation: "attachOwner" }> | null>(null);
  const instance = useRef(recordingControlId()), generation = useRef(0), clientRevision = useRef(0), configRevision = useRef(0);
  const projection = useRef<DraftProjection>({ visible: false, busy: true, config: null });
  const blockedRef = useRef(blocked); blockedRef.current = blocked;
  const publicationRequested = useRef(false), localAssignment = useRef<string | null>(null);
  const publicationContext = useRef<{ generation: number; ownerToken: string; invalidationEpoch: number; minimizeCycle: number } | null>(null);
  const held = useRef(true), publishing = useRef<symbol | null>(null), initialization = useRef<Promise<void> | null>(null);
  const publicationGeneration = useRef<number | null>(null);
  const reconcileRef = useRef<() => Promise<void>>(async () => {});

  const accept = useCallback((value: ControlsSnapshot) => {
    const before = current.current, next = acceptControlsSnapshot(before, value);
    if (!mounted.current || next === before) return next;
    current.current = next; setSnapshot(next); setConnected(true);
    if (owner.current && next.ownerToken !== owner.current) {
      // Actual renderer replacement is not blur, minimize or a status retry.
      owner.current = null; ownerLost.current = true; setOwnerToken(null); held.current = true; generation.current += 1;
      publicationRequested.current = false; publicationContext.current = null;
      setError("Recording controls moved to another app page. Reload to reconnect.");
    }
    const restored = before && next.nativeWindow === "visible" &&
      (before.nativeWindow !== "visible" || next.minimizeCycle > before.minimizeCycle);
    if (before && next.invalidationEpoch > before.invalidationEpoch && !restored && !held.current) {
      // A native close/retirement fence is not a restored-window projection.
      // Retire both a current publication and its pending follow-up locally.
      held.current = true; generation.current += 1; publicationRequested.current = false; publicationContext.current = null;
    }
    if (restored && !blockedRef.current && (!held.current || publishing.current && publicationGeneration.current === generation.current)) void reconcileRef.current();
    return next;
  }, []);

  const send = useCallback(async (request: ControlsRequest, retryTransport = false): Promise<ControlsSnapshot | null> => {
    let reply: ControlsReply;
    try { reply = await invoke<ControlsReply>("recording_controls", { request }); }
    catch (failure) {
      if (!retryTransport) throw failure;
      await pause(250);
      // Exact payload and operation identity are retained on this one retry.
      reply = await invoke<ControlsReply>("recording_controls", { request });
    }
    if (!reply || typeof reply.ok !== "boolean") throw new Error("Recording controls returned an invalid reply.");
    if (reply.ok) { accept(reply.value); return reply.value; }
    accept(reply.state);
    if (mounted.current) setError(reply.error.message);
    return null;
  }, [accept]);

  const refresh = useCallback(async () => {
    try { return await send({ operation: "snapshot" }); }
    catch (failure) { if (mounted.current) { setConnected(false); setError(`Shortcut status is unavailable: ${explanation(failure)}`); } return null; }
  }, [send]);

  const initialize = useCallback(() => {
    if (initialization.current) return initialization.current;
    initialization.current = (async () => {
      const initial = await send({ operation: "snapshot" });
      if (!initial || !mounted.current) return;
      attachRequest.current ??= { operation: "attachOwner", expectedRevision: initial.revision,
        expectedOwnerToken: initial.ownerToken, rendererInstanceId: instance.current };
      const attached = await send(attachRequest.current, true);
      if (attached && mounted.current && controlToken(attached.ownerToken)) {
        owner.current = attached.ownerToken; setOwnerToken(attached.ownerToken); setError(null);
      }
    })().catch(failure => { if (mounted.current) { setConnected(false); setError(`Recording controls are unavailable: ${explanation(failure)}`); } });
    return initialization.current;
  }, [send]);

  const retireDraft = useCallback(async (_reason: string): Promise<ControlsSnapshot | null> => {
    // Retire local publication/renewal BEFORE awaiting IPC. Only a genuinely
    // committed context projection or explicit reconciliation releases this.
    const own = ++generation.current; held.current = true;
    publicationRequested.current = false; publicationContext.current = null;
    const revision = ++clientRevision.current;
    await initialize();
    if (!owner.current || !mounted.current || own !== generation.current) return null;
    const selectedOwner = owner.current;
    try {
      const result = await send({ operation: "invalidateDraft", ownerToken: selectedOwner, clientRevision: revision }, true);
      return result && mounted.current && own === generation.current && owner.current === selectedOwner && result.ownerToken === selectedOwner ? result : null;
    } catch (failure) {
      if (mounted.current && own === generation.current) { setConnected(false); setError(`Could not retire the recording shortcut: ${explanation(failure)}`); }
      return null;
    }
  }, [initialize, send]);
  const invalidate = useCallback(async (reason: string) => !!await retireDraft(reason), [retireDraft]);
  const prepareStart = useCallback(async () => {
    const acknowledged = await retireDraft("start");
    return acknowledged ? startAdmissionFromAck(acknowledged) : null;
  }, [retireDraft]);

  const reconcile = useCallback(async () => {
    const own = generation.current, selected = projection.current;
    await initialize();
    const selectedOwner = owner.current;
    if (own !== generation.current || selected !== projection.current || !selectedOwner || blockedRef.current || !selected.visible || selected.busy || !selected.config ) return;
    if (publishing.current) {
      const state = current.current;
      if (state?.ownerToken === selectedOwner) {
        publicationRequested.current = true;
        publicationContext.current = { generation: own, ownerToken: selectedOwner, invalidationEpoch: state.invalidationEpoch, minimizeCycle: state.minimizeCycle };
      }
      return;
    }
    const token = Symbol("draft"); publishing.current = token;
    publicationGeneration.current = own;
    const initialContext = current.current;
    let committedEpoch: number | null = null;
    held.current = true;
    try {
      // Every publication resolves the current native epoch/window again.
      const state = await send({ operation: "snapshot" });
      if (!state || !mounted.current || own !== generation.current || selected !== projection.current ||
          selectedOwner !== owner.current || blockedRef.current || state.assignment || state.nativeWindow !== "visible" || !state.bindings.start ||
          !initialContext || state.invalidationEpoch !== initialContext.invalidationEpoch || state.minimizeCycle !== initialContext.minimizeCycle ||
          current.current?.invalidationEpoch !== state.invalidationEpoch || current.current?.minimizeCycle !== state.minimizeCycle) return;
      configRevision.current = Math.max(configRevision.current, state.arm?.configRevision ?? 0) + 1;
      const request: ControlsRequest = { operation: "publishDraft", ownerToken: selectedOwner,
        invalidationEpoch: state.invalidationEpoch, minimizeCycle: state.minimizeCycle, expectedArmToken: state.arm?.token ?? null,
        clientRevision: ++clientRevision.current, configRevision: configRevision.current, draft: selected.config! };
      const reply = await invoke<ControlsReply>("recording_controls", { request });
      if (own !== generation.current || selected !== projection.current || !mounted.current || blockedRef.current) return;
      if (reply.ok) {
        const acknowledged = reply.value, observed = accept(acknowledged), arm = acknowledged.arm;
        // Native retires the previous draft BEFORE validation. Only this raw
        // successful ACK may release our publication hold, never its event.
        if (acknowledged.ownerToken === selectedOwner && acknowledged.nativeWindow === "visible" &&
            acknowledged.invalidationEpoch === request.invalidationEpoch + 1 && acknowledged.minimizeCycle === request.minimizeCycle &&
            arm?.mode === "leased" && arm.configRevision === request.configRevision && observed.ownerToken === selectedOwner &&
            observed.nativeWindow === "visible" && observed.invalidationEpoch === acknowledged.invalidationEpoch &&
            observed.minimizeCycle === acknowledged.minimizeCycle && observed.arm?.token === arm.token && observed.arm.configRevision === arm.configRevision) {
          committedEpoch = acknowledged.invalidationEpoch; held.current = false; setError(null);
        }
      } else { accept(reply.state); setError(reply.error.message); }
    } catch (failure) {
      if (mounted.current && own === generation.current) { held.current = true; setConnected(false); setError(`Recording shortcut is not ready: ${explanation(failure)}`); }
    } finally {
      if (publishing.current === token) { publishing.current = null; publicationGeneration.current = null; }
      const followUp = publicationRequested.current ? publicationContext.current : null;
      publicationRequested.current = false; publicationContext.current = null;
      const observed = current.current;
      if (followUp && !blockedRef.current && owner.current === followUp.ownerToken && generation.current === followUp.generation &&
          observed?.ownerToken === followUp.ownerToken && observed.nativeWindow === "visible" && observed.minimizeCycle === followUp.minimizeCycle &&
          (observed.invalidationEpoch === followUp.invalidationEpoch || committedEpoch !== null && followUp.generation === own &&
            followUp.invalidationEpoch === initialContext?.invalidationEpoch && observed.invalidationEpoch === committedEpoch)) {
        held.current = false; void reconcileRef.current();
      }
    }
  }, [accept, initialize, send]);
  reconcileRef.current = reconcile;

  const projectDraft = useCallback((next: DraftProjection) => {
    projection.current = next;
    if (!next.visible || next.busy || !next.config || blockedRef.current) {
      held.current = true; publicationRequested.current = false; publicationContext.current = null; return;
    }
    // Called after an actual Workspace/config/busy React commit, not in
    // response to a timer or a snapshot/owner revision render.
    if (!publishing.current) held.current = false;
    void reconcileRef.current();
  }, []);

  const reconnect = useCallback(async () => {
    if (ownerLost.current) { setError("Recording controls moved to another app page. Reload to reconnect."); return false; }
    if (!owner.current) { initialization.current = null; await initialize(); }
    else await refresh();
    if (owner.current) { void reconcileRef.current(); return true; }
    return false;
  }, [initialize, refresh]);

  const beginAssignment = useCallback(async () => {
    const own = ++generation.current; held.current = true;
    await initialize();
    if (!owner.current || !current.current) return false;
    try {
      const result = await send({ operation: "beginAssignment", ownerToken: owner.current,
        expectedRevision: current.current.revision, operationId: recordingControlId() }, true);
      const admitted = !!result?.assignment && own === generation.current && !blockedRef.current;
      if (admitted) localAssignment.current = result!.assignment!.token;
      return admitted;
    } catch (failure) { if (mounted.current) setError(explanation(failure)); return false; }
  }, [initialize, send]);

  const normalize = useCallback(async (event: { key: string; ctrl: boolean; shift: boolean; alt: boolean; meta: boolean }): Promise<ShortcutChord | null> => {
    const state = current.current, selectedOwner = owner.current, own = generation.current;
    if (!selectedOwner || !state?.assignment || blockedRef.current) return null;
    const assignmentToken = state.assignment.token;
    try {
      const chord = await invoke<ShortcutChord>("recording_controls_normalize", { ownerToken: selectedOwner, assignmentToken, event });
      if (!isShortcutChord(chord)) throw new Error("This shortcut is not supported.");
      return mounted.current && own === generation.current && current.current?.assignment?.token === assignmentToken ? chord : null;
    } catch (failure) { if (mounted.current && own === generation.current) setError(explanation(failure)); return null; }
  }, []);

  const finishAssignment = useCallback(async (bindings?: ShortcutBindings) => {
    const state = current.current, selectedOwner = owner.current;
    if (!selectedOwner || !state?.assignment) return false;
    try {
      // Caller blurred capture first; native proves physical key release.
      const request: ControlsRequest = bindings
        ? { operation: "applyBindings", expectedRevision: state.revision, ownerToken: selectedOwner,
            assignmentToken: state.assignment.token, operationId: recordingControlId(), captureClosed: true, bindings }
        : { operation: "endAssignment", expectedRevision: state.revision, ownerToken: selectedOwner,
            assignmentToken: state.assignment.token, captureClosed: true };
      const result = await send(request, true);
      if (result && result.assignment === null) localAssignment.current = null;
      return !!result && result.assignment === null;
    } catch (failure) { if (mounted.current) setError(explanation(failure)); return false; }
  }, [send]);

  const reset = useCallback(async () => {
    generation.current += 1; held.current = true;
    if (!owner.current || !current.current) return false;
    try { return !!await send({ operation: "reset", expectedRevision: current.current.revision,
      ownerToken: owner.current, operationId: recordingControlId() }, true); }
    catch (failure) { if (mounted.current) setError(explanation(failure)); return false; }
  }, [send]);

  useEffect(() => {
    mounted.current = true; void initialize();
    let disposed = false, unlisten: (() => void) | undefined, timer: ReturnType<typeof setTimeout>;
    void listen<ControlsSnapshot>("recording-controls-state", ({ payload }) => {
      if (disposed) return;
      try {
        accept(payload);
      } catch (failure) { setConnected(false); setError(explanation(failure)); }
    }).then(handle => { if (disposed) handle(); else unlisten = handle; }).catch(() => {});
    const tick = async () => {
      if (owner.current) {
        const state = current.current;
        try {
          if (state?.assignment && localAssignment.current === state.assignment.token && projection.current.visible && !projection.current.busy && !blockedRef.current && state.nativeWindow === "visible") await send({ operation: "renewAssignment", ownerToken: owner.current, assignmentToken: state.assignment.token });
          else if (!held.current && !blockedRef.current && state?.arm?.mode === "leased" && projection.current.visible && !projection.current.busy) {
            await send({ operation: "renewDraft", ownerToken: owner.current, armToken: state.arm.token, configRevision: state.arm.configRevision });
          } else await send({ operation: "snapshot" });
        } catch { if (!disposed) { setConnected(false); setError("Shortcut status is unavailable. Refresh controls before changing a shortcut."); } }
      }
      if (!disposed) timer = setTimeout(() => void tick(), 900);
    };
    void tick();
    const visibility = () => {
      const selectedOwner = owner.current;
      if (selectedOwner) void send({ operation: "classifyVisibility", ownerToken: selectedOwner }).catch(() => {});
    };
    document.addEventListener("visibilitychange", visibility);
    return () => {
      disposed = true; mounted.current = false; clearTimeout(timer); unlisten?.(); document.removeEventListener("visibilitychange", visibility);
      held.current = true; generation.current += 1;
      if (owner.current) void invoke("recording_controls", { request: { operation: "invalidateDraft", ownerToken: owner.current, clientRevision: ++clientRevision.current } }).catch(() => {});
    };
  }, [accept, initialize, send]);

  useEffect(() => {
    if (blocked) { held.current = true; publicationRequested.current = false; publicationContext.current = null; }
    else void reconcileRef.current();
  }, [blocked]);
  return { snapshot, ownerToken, error, connected, ready: connected && ownerToken !== null,
    invalidate, prepareStart, projectDraft, reconcile, refresh, reconnect, beginAssignment, normalize, finishAssignment, reset };
}
