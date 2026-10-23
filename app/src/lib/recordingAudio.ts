import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { RecordingPhase, RecordingSnapshot } from "./recording";
import type { AudioControl, AudioLevels, AudioStatus, RecordingAudioConfig } from "./recordingAudioTypes";
import type { ControlFailure, ControlResult, LevelReply, ReconcileReply, ReconcileRequest, ReconcileRetryRequest } from "./recordingSessionTypes";
import { acceptAudioStatus, enqueueAudioControl } from "./recordingAudioState";
import { freezeAudioIntents, validateReconcileReply } from "./recordingSessionState";
import { recordingControlId } from "./recordingControls";

type Options = { visible: boolean; ownerToken: string | null; snapshot: RecordingSnapshot; phase: RecordingPhase;
  config: RecordingAudioConfig; onOverflow: () => void; refreshRecording: () => Promise<RecordingSnapshot> };
const text = (failure: unknown) => failure instanceof Error ? failure.message : String(failure);
const pausedPhase = (phase: RecordingPhase) => ["pausing", "paused", "resuming"].includes(phase);
const terminal = (phase: RecordingPhase) => ["completed", "recoverable", "failed", "idle"].includes(phase);
const wait = (ms: number) => new Promise<void>(resolve => setTimeout(resolve, ms));
export function useRecordingAudio(options: Options) {
  const [status, setStatus] = useState<AudioStatus | null>(null);
  const [desired, setDesired] = useState<AudioLevels>({ renderLevel: options.config.renderLevel, microphoneLevel: options.config.microphoneLevel });
  const [pending, setPending] = useState(0), [error, setError] = useState<string | null>(null);
  const [syncing, setSyncing] = useState(false), [fenceToken, setFenceToken] = useState<string | null>(null);
  const [syncFailure, setSyncFailure] = useState<ControlFailure | null>(null);
  const latest = useRef(options); latest.current = options;
  const mounted = useRef(true), current = useRef<AudioStatus | null>(null);
  const draft = useRef(desired), queue = useRef<AudioControl[]>([]), sequence = useRef(0);
  const sending = useRef<symbol | null>(null), blocked = useRef(false), contextEpoch = useRef(0);
  const selectedSession = useRef<string | null>(null), selectedOwner = useRef<string | null>(null);
  const frozen = useRef<ReturnType<typeof freezeAudioIntents> | null>(null);
  const batch = useRef<ReconcileRequest | null>(null), synchronization = useRef<symbol | null>(null);
  const uncertainRetry = useRef<ReconcileRetryRequest | null>(null);
  const pumpRef = useRef<() => void>(() => {}), freezeRef = useRef<() => void>(() => {});
  const synchronizationRef = useRef<(review?: boolean, retry?: boolean) => Promise<void>>(async () => {});

  const freeze = useCallback(() => {
    if (frozen.current !== null) return;
    try { frozen.current = freezeAudioIntents(queue.current); contextEpoch.current += 1; }
    catch (failure) { blocked.current = true; setError(text(failure)); latest.current.onOverflow(); }
  }, []);
  freezeRef.current = freeze;

  const installLevels = useCallback((value: AudioLevels) => {
    draft.current = { renderLevel: value.renderLevel, microphoneLevel: value.microphoneLevel };
    setDesired(draft.current);
  }, []);

  const accept = useCallback((incoming: AudioStatus | null) => {
    const selected = latest.current;
    if (!incoming || incoming.sessionId !== selected.snapshot.sessionId || !mounted.current) return;
    if (pausedPhase(selected.phase)) freezeRef.current();
    const next = acceptAudioStatus(current.current, incoming);
    current.current = next;
    sequence.current = Math.max(sequence.current, next.acceptedCommandSequence, next.appliedCommandSequence, next.rejectedCommandSequence ?? 0);
    // Pause owns a sealed copy of the ENTIRE queue, including submitted and
    // uncertain controls. Meter/status watermarks never mutate that copy.
    if (frozen.current === null) {
      queue.current = queue.current.filter(command => command.commandSequence > next.appliedCommandSequence && command.commandSequence !== next.rejectedCommandSequence);
      if (!queue.current.length) installLevels(next);
    }
    if (next.rejectedCommandSequence !== null) { blocked.current = true; setError(next.message || "An audio change was not applied. Recording is stopping and saving."); }
    setStatus(next); setPending(Math.max(queue.current.length, next.acceptedCommandSequence > next.appliedCommandSequence ? 1 : 0));
  }, [installLevels]);

  const pump = useCallback(async () => {
    const selected = latest.current, active = current.current;
    if (sending.current || blocked.current || frozen.current !== null || selected.phase !== "recording" || selected.snapshot.resolvedRoleMask === 0 || !selected.ownerToken || !active || active.sessionId !== selected.snapshot.sessionId) return;
    const command = queue.current.find(value => !value.submitted);
    if (!command) return;
    const own = Symbol("level"), epoch = contextEpoch.current;
    const request = { ownerToken: selected.ownerToken, expectedControlRevision: selected.snapshot.controlRevision,
      sessionId: active.sessionId, sourceGeneration: active.sourceGeneration, ...freezeAudioIntents([command])[0] };
    command.submitted = true; sending.current = own;
    try {
      let reply: ControlResult<LevelReply>;
      try { reply = await invoke("recording_audio_set_levels", request); }
      catch (failure) {
        await wait(250);
        if (epoch !== contextEpoch.current || latest.current.phase !== "recording") return;
        reply = await invoke("recording_audio_set_levels", request);
      }
      if (epoch !== contextEpoch.current || selected.ownerToken !== latest.current.ownerToken || selected.snapshot.sessionId !== latest.current.snapshot.sessionId) return;
      if (!reply.ok) {
        accept(reply.state.audioStatus);
        if (pausedPhase(reply.state.phase)) { freezeRef.current(); void latest.current.refreshRecording(); return; }
        blocked.current = true; setError(reply.error.message); return;
      }
      const value = reply.value;
      if (value.ownerToken !== request.ownerToken || value.sessionId !== request.sessionId || value.sourceGeneration !== request.sourceGeneration || value.disposition.commandSequence !== command.commandSequence) throw new Error("Audio returned a different control identity.");
      if (value.disposition.kind === "deferredNotAccepted") { freezeRef.current(); void latest.current.refreshRecording(); return; }
      accept(value.audioStatus);
      if (value.disposition.kind === "settled" && frozen.current === null) queue.current = queue.current.filter(item => item !== command);
      // Accepted pending is NOT PCM-applied. The exact command stays retained.
    } catch (failure) {
      if (epoch === contextEpoch.current && selected.snapshot.sessionId === latest.current.snapshot.sessionId) {
        command.submitted = false; blocked.current = true;
        if (mounted.current) setError(`Audio change not confirmed: ${text(failure)}. Retry the same change or stop and save.`);
      }
    } finally {
      if (sending.current === own) { sending.current = null; if (mounted.current) { setPending(queue.current.length); pumpRef.current(); } }
    }
  }, [accept]);
  pumpRef.current = () => { void pump(); };

  const synchronize = useCallback(async (review = false, explicitRetry = false) => {
    const selected = latest.current, seal = selected.snapshot.pause;
    const audio = seal?.audio, ownerToken = selected.ownerToken, sessionId = selected.snapshot.sessionId;
    if (synchronization.current || selected.phase !== "paused" || !seal || !audio || !ownerToken || !sessionId) return;
    if (audio.fence.state === "replacementReviewRequired" && (!review || !selected.visible)) return;
    freezeRef.current();
    const keyMatches = (request: ReconcileRequest) => request.ownerToken === ownerToken && request.sessionId === sessionId && request.sourceGeneration === audio.sourceGeneration && request.pauseRevision === seal.pauseRevision;
    if (!batch.current || !keyMatches(batch.current)) {
      uncertainRetry.current = null;
      batch.current = review ? { ownerToken, sessionId, sourceGeneration: audio.sourceGeneration, pauseRevision: seal.pauseRevision,
        batchId: recordingControlId(), commands: [], mode: "authoritativeOnly", replacementReviewToken: audio.fence.replacementReviewToken!, acknowledgeLostLocalIntent: true }
        : { ownerToken, sessionId, sourceGeneration: audio.sourceGeneration, pauseRevision: seal.pauseRevision,
            batchId: recordingControlId(), commands: frozen.current || [], mode: "retainedQueue" };
    }
    const request = batch.current;
    const timedOutAttempt = explicitRetry ? syncFailure?.context.timedOutAttempt : null;
    if (!uncertainRetry.current && timedOutAttempt) uncertainRetry.current = { ...request, operationId: recordingControlId(), expectedTimedOutAttempt: timedOutAttempt };
    // A lost reply leaves the exact retry operation uncertain. A later user
    // retry must join that operation, never consume the old attempt again.
    const payload: ReconcileRequest | ReconcileRetryRequest = uncertainRetry.current || request;
    const command = uncertainRetry.current ? "recording_audio_reconcile_retry" : "recording_audio_reconcile";
    const own = Symbol("reconcile"), epoch = contextEpoch.current; synchronization.current = own;
    setSyncing(true); setError(null);
    const stillCurrent = () => mounted.current && epoch === contextEpoch.current && batch.current === request && latest.current.ownerToken === ownerToken &&
      latest.current.snapshot.sessionId === sessionId && latest.current.snapshot.pause?.pauseRevision === seal.pauseRevision && latest.current.phase === "paused";
    try {
      let reply: ControlResult<ReconcileReply>;
      try { reply = await invoke(command, { request: payload }); }
      catch {
        await wait(250);
        if (!stillCurrent()) return;
        // One identical transport retry joins the original attempt/deadline.
        reply = await invoke(command, { request: payload });
      }
      if (!stillCurrent()) return;
      if (!reply.ok) {
        uncertainRetry.current = null;
        accept(reply.state.audioStatus); setSyncFailure(reply); setError(reply.error.message);
        void latest.current.refreshRecording(); return;
      }
      const value = validateReconcileReply(request, reply.value);
      uncertainRetry.current = null;
      accept(value.audioStatus);
      queue.current = value.ledger.pending.map(item => ({ ...item, submitted: true, muteEdge: true }));
      setPending(queue.current.length); setFenceToken(value.fenceToken); setSyncFailure(null); setError(null);
      // Pending Resume-bound values are kept, never labelled applied in Pause.
      installLevels(value.ledger.pending[value.ledger.pending.length - 1] || value.audioStatus);
      void latest.current.refreshRecording();
    } catch (failure) { if (stillCurrent()) setError(`Audio synchronization is not confirmed: ${text(failure)}. Retry synchronization or stop and save.`); }
    finally { if (synchronization.current === own) { synchronization.current = null; if (mounted.current) setSyncing(false); } }
  }, [accept, installLevels, syncFailure]);
  synchronizationRef.current = synchronize;

  const change = useCallback((levels: AudioLevels) => {
    if (latest.current.phase !== "recording" || latest.current.snapshot.resolvedRoleMask === 0 || !current.current || !latest.current.ownerToken || blocked.current || frozen.current !== null) return;
    try { queue.current = enqueueAudioControl(queue.current, levels, ++sequence.current, draft.current); installLevels(levels); setPending(queue.current.length); setError(null); pumpRef.current(); }
    catch (failure) { blocked.current = true; setError(text(failure)); latest.current.onOverflow(); }
  }, [installLevels]);
  const retry = useCallback(async () => {
    if (pausedPhase(latest.current.phase)) { await synchronizationRef.current(false, true); return; }
    const id = latest.current.snapshot.sessionId;
    if (!id) return;
    try {
      accept(await invoke<AudioStatus | null>("recording_audio_snapshot", { sessionId: id }));
      if (id !== latest.current.snapshot.sessionId || current.current?.rejectedCommandSequence !== null) return;
      blocked.current = false; setError(null); pumpRef.current();
    } catch (failure) { if (mounted.current && id === latest.current.snapshot.sessionId) setError(`Audio status unavailable: ${text(failure)}`); }
  }, [accept]);

  useLayoutEffect(() => {
    if (selectedSession.current === options.snapshot.sessionId) return;
    selectedSession.current = options.snapshot.sessionId; contextEpoch.current += 1;
    sending.current = null; synchronization.current = null; current.current = null; queue.current = []; frozen.current = null; batch.current = null; uncertainRetry.current = null; sequence.current = 0; blocked.current = false;
    installLevels(options.config); setStatus(null); setPending(0); setError(null); setFenceToken(null); setSyncFailure(null); setSyncing(false);
  }, [options.snapshot.sessionId, options.config, installLevels]);
  useLayoutEffect(() => {
    if (selectedOwner.current === options.ownerToken) return;
    selectedOwner.current = options.ownerToken; contextEpoch.current += 1;
    frozen.current = null; batch.current = null; uncertainRetry.current = null; synchronization.current = null; setFenceToken(null); setSyncFailure(null); setSyncing(false);
    if (options.snapshot.audio) { queue.current = (options.snapshot.audioControlLedger?.pending || []).map(item => ({ ...item, submitted: true, muteEdge: true })); installLevels(queue.current[queue.current.length - 1] || options.snapshot.audio); }
  }, [options.ownerToken, options.snapshot, installLevels]);
  const previousPhase = useRef(options.phase);
  useLayoutEffect(() => {
    const before = previousPhase.current; previousPhase.current = options.phase;
    if (pausedPhase(options.phase)) freeze();
    if (options.phase === "recording" && pausedPhase(before)) {
      contextEpoch.current += 1; frozen.current = null; batch.current = null; uncertainRetry.current = null; setFenceToken(null); setSyncFailure(null); blocked.current = false;
      queue.current = (options.snapshot.audioControlLedger?.pending || []).map(item => ({ ...item, submitted: true, muteEdge: true }));
      setPending(queue.current.length); if (!queue.current.length && options.snapshot.audio) installLevels(options.snapshot.audio);
    }
    if (terminal(options.phase) && (queue.current.length || options.snapshot.audioControlLedger?.terminalUnapplied.length)) {
      const acceptedUnapplied = options.snapshot.audioControlLedger?.terminalUnapplied.length || 0;
      setError(acceptedUnapplied ? `${acceptedUnapplied} accepted audio change${acceptedUnapplied === 1 ? " was" : "s were"} not applied before recording stopped. Some local changes may also be unconfirmed.` : "Some local audio changes were not confirmed before recording stopped. Saved audio follows the recorder's applied state.");
      queue.current = []; frozen.current = null; batch.current = null; uncertainRetry.current = null; contextEpoch.current += 1; setPending(0); blocked.current = true;
    }
  }, [options.phase, options.snapshot, freeze, installLevels]);
  useEffect(() => { try { accept(options.snapshot.audio); } catch (failure) { setError(text(failure)); blocked.current = true; } }, [options.snapshot.audio, accept]);
  const pauseKey = JSON.stringify([options.ownerToken, options.snapshot.sessionId, options.snapshot.pause?.pauseRevision, options.snapshot.pause?.audio?.sourceGeneration, options.snapshot.pause?.audio?.fence.replacementReviewToken]);
  useEffect(() => {
    if (options.phase === "paused" && options.snapshot.pause?.audio && options.snapshot.pause.audio.fence.state !== "replacementReviewRequired") void synchronizationRef.current();
    if (options.snapshot.resolvedRoleMask === 0 && queue.current.length && pausedPhase(options.phase)) { setError("Audio changes do not match this recording's disabled audio sources. Stop and save before continuing."); blocked.current = true; }
  }, [pauseKey, options.phase, options.snapshot.resolvedRoleMask]);
  useEffect(() => {
    mounted.current = true;
    if (!options.snapshot.sessionId || terminal(options.phase)) return () => { mounted.current = false; };
    const id = options.snapshot.sessionId;
    let disposed = false, unlisten: (() => void) | undefined, timer: ReturnType<typeof setTimeout>;
    const receive = (value: AudioStatus | null) => { if (disposed || id !== latest.current.snapshot.sessionId) return; try { accept(value); } catch (failure) { setError(text(failure)); blocked.current = true; } };
    // Reconciliation/status ownership continues while Record is hidden. These
    // inert requests neither lease nor open input; meters render only Recording.
    void listen<AudioStatus>("recording-audio-status", ({ payload }) => receive(payload)).then(handle => { if (disposed) handle(); else unlisten = handle; }).catch(() => {});
    const poll = async () => {
      try { receive(await invoke<AudioStatus | null>("recording_audio_snapshot", { sessionId: id })); }
      catch { if (!disposed) setError("Audio status unavailable. Retry status or stop and save."); }
      if (!disposed) timer = setTimeout(() => void poll(), 400);
    };
    void poll();
    return () => { disposed = true; mounted.current = false; clearTimeout(timer); unlisten?.(); };
  }, [options.snapshot.sessionId, options.phase, accept]);

  const reviewRequired = options.snapshot.pause?.audio?.fence.state === "replacementReviewRequired";
  return { status, desired, pending, error, change, retry, freeze, syncing, syncFailure, fenceToken, reviewRequired,
    acknowledgeRestored: () => synchronize(true), retrySynchronization: () => synchronize(false, true),
    synchronized: options.snapshot.pause?.audio === null && options.snapshot.resolvedRoleMask === 0 || fenceToken !== null,
    metersActive: options.phase === "recording", phase: options.phase, blocked: blocked.current,
    ready: options.phase === "recording" && options.snapshot.resolvedRoleMask !== 0 && options.ownerToken !== null && status?.sessionId === options.snapshot.sessionId && !blocked.current && frozen.current === null &&
      !(status && status.acceptedCommandSequence > status.appliedCommandSequence && queue.current.length === 0) };
}
