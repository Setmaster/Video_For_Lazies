import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { RecordingPhase } from "./recording";
import type { AudioControl, AudioControlSubmission, AudioLevels, AudioStatus, RecordingAudioConfig } from "./recordingAudioTypes";
import { acceptAudioStatus, enqueueAudioControl } from "./recordingAudioState";

type Options = { visible: boolean; sessionId: string | null; phase: RecordingPhase; audio: AudioStatus | null; config: RecordingAudioConfig; onOverflow: () => void };
const errorText = (failure: unknown) => failure instanceof Error ? failure.message : String(failure);
export function useRecordingAudio(options: Options) {
  const [status, setStatus] = useState<AudioStatus | null>(null);
  const [desired, setDesired] = useState<AudioLevels>({ renderLevel: options.config.renderLevel, microphoneLevel: options.config.microphoneLevel });
  const [pending, setPending] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const latest = useRef(options); latest.current = options;
  const mounted = useRef(true), current = useRef<AudioStatus | null>(null);
  const draft = useRef(desired), queue = useRef<AudioControl[]>([]), sequence = useRef(0);
  const sending = useRef<string | null>(null), blocked = useRef(false), session = useRef<string | null>(null);
  const pumpRef = useRef<() => void>(() => {});

  const accept = useCallback((incoming: AudioStatus | null) => {
    if (!incoming || incoming.sessionId !== latest.current.sessionId || !mounted.current) return;
    const before = current.current;
    const next = acceptAudioStatus(before, incoming);
    current.current = next;
    sequence.current = Math.max(sequence.current, next.acceptedCommandSequence, next.appliedCommandSequence, next.rejectedCommandSequence ?? 0);
    queue.current = queue.current.filter(command => command.commandSequence > next.appliedCommandSequence && command.commandSequence !== next.rejectedCommandSequence);
    if (!queue.current.length) {
      draft.current = { renderLevel: next.renderLevel, microphoneLevel: next.microphoneLevel };
      setDesired(draft.current);
    }
    if (next.rejectedCommandSequence !== null) {
      blocked.current = true;
      setError(next.message || "An audio change was not applied. Recording is stopping and saving.");
    }
    setStatus(next); setPending(Math.max(queue.current.length, next.acceptedCommandSequence > Math.max(next.appliedCommandSequence, next.rejectedCommandSequence ?? 0) ? 1 : 0));
  }, []);

  const pump = useCallback(async () => {
    const active = current.current, selectedSession = latest.current.sessionId;
    if (sending.current || blocked.current || latest.current.phase !== "recording" || !active || active.sessionId !== selectedSession) return;
    const command = queue.current.find(value => !value.submitted);
    if (!command) return;
    command.submitted = true;
    const sendIdentity = `${active.sessionId}:${active.sourceGeneration}:${command.commandSequence}`;
    sending.current = sendIdentity;
    try {
      const reply = await invoke<AudioControlSubmission>("recording_audio_set_levels", {
        sessionId: active.sessionId, sourceGeneration: active.sourceGeneration,
        commandSequence: command.commandSequence, renderLevel: command.renderLevel, microphoneLevel: command.microphoneLevel,
      });
      if (latest.current.sessionId !== selectedSession || current.current?.sourceGeneration !== active.sourceGeneration) return;
      if (reply.sessionId !== active.sessionId || reply.sourceGeneration !== active.sourceGeneration || reply.acceptedCommandSequence !== command.commandSequence) throw new Error("Audio returned a different control identity.");
      // Acceptance is deliberately not an applied ACK. Retain the command
      // until native status names its applied sample boundary.
    } catch (failure) {
      if (latest.current.sessionId === selectedSession && current.current?.sourceGeneration === active.sourceGeneration) {
        command.submitted = false;
        blocked.current = true;
        if (mounted.current) setError(`Audio change not confirmed: ${errorText(failure)}. Retry the same change or stop and save.`);
      }
    } finally {
      if (sending.current === sendIdentity) {
        sending.current = null;
        if (mounted.current) { setPending(queue.current.length); pumpRef.current(); }
      }
    }
  }, []);
  pumpRef.current = () => { void pump(); };

  const change = useCallback((levels: AudioLevels) => {
    if (latest.current.phase !== "recording" || !current.current || blocked.current) return;
    try {
      queue.current = enqueueAudioControl(queue.current, levels, ++sequence.current, draft.current);
      draft.current = levels; setDesired(levels); setPending(queue.current.length); setError(null);
      pumpRef.current();
    } catch (failure) {
      blocked.current = true;
      setError(errorText(failure)); latest.current.onOverflow();
    }
  }, []);
  const retry = useCallback(async () => {
    const id = latest.current.sessionId;
    if (!id) return;
    try {
      accept(await invoke<AudioStatus | null>("recording_audio_snapshot", { sessionId: id }));
      if (id !== latest.current.sessionId || current.current?.rejectedCommandSequence !== null) return;
      blocked.current = false; setError(null); pumpRef.current();
    } catch (failure) { if (mounted.current && id === latest.current.sessionId) setError(`Audio status unavailable: ${errorText(failure)}`); }
  }, [accept]);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  useEffect(() => {
    if (session.current === options.sessionId) return;
    session.current = options.sessionId; sending.current = null; current.current = null; queue.current = []; sequence.current = 0; blocked.current = false;
    draft.current = { renderLevel: options.config.renderLevel, microphoneLevel: options.config.microphoneLevel };
    setStatus(null); setPending(0); setDesired(draft.current); setError(null);
  }, [options.sessionId, options.config.renderLevel, options.config.microphoneLevel]);
  useEffect(() => {
    try { accept(options.audio); } catch (failure) { setError(errorText(failure)); blocked.current = true; }
  }, [options.audio, accept]);
  useEffect(() => {
    if (["completed", "recoverable", "failed", "idle"].includes(options.phase) && queue.current.length) {
      queue.current = []; setPending(0); blocked.current = true;
      setError("Some audio changes were not confirmed before recording stopped. Saved audio follows the recorder's applied state.");
    }
  }, [options.phase]);
  useEffect(() => {
    if (!options.visible || !options.sessionId || !["starting", "recording", "stopping", "finalizing"].includes(options.phase)) return;
    const id = options.sessionId;
    let disposed = false, unlisten: (() => void) | undefined, timer: ReturnType<typeof setTimeout>;
    const receive = (value: AudioStatus | null) => {
      if (disposed || id !== latest.current.sessionId) return;
      try { accept(value); } catch (failure) { setError(errorText(failure)); blocked.current = true; }
    };
    void listen<AudioStatus>("recording-audio-status", ({ payload }) => receive(payload)).then(handle => { if (disposed) handle(); else unlisten = handle; }).catch(() => { if (!disposed) setError("Audio events unavailable. Levels use refreshed native status."); });
    const poll = async () => {
      try { receive(await invoke<AudioStatus | null>("recording_audio_snapshot", { sessionId: id })); }
      catch { if (!disposed) setError("Audio status unavailable. Retry status or stop and save; displayed levels may be stale."); }
      if (!disposed) timer = setTimeout(() => void poll(), 300);
    };
    void poll();
    return () => { disposed = true; clearTimeout(timer); unlisten?.(); };
  }, [options.visible, options.sessionId, options.phase, accept]);
  return { status, desired, pending, error, change, retry, blocked: blocked.current,
    ready: options.phase === "recording" && status?.sessionId === options.sessionId && !blocked.current &&
      !(status && status.acceptedCommandSequence > status.appliedCommandSequence && queue.current.length === 0) };
}
