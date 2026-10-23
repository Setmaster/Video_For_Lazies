import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AudioLedger, ControlResult, PauseSeal, PendingTransition, TransitionRequest } from "./recordingSessionTypes";
import type { AudioApplication, AudioStatus, RecordingAudioConfig } from "./recordingAudioTypes";
import { acceptRecordingSnapshot, isRecordingActive } from "./recordingState";

export type RecordingPhase = "idle" | "preparing" | "countdown" | "starting" | "recording" | "pausing" | "paused" | "resuming" | "stopping" | "finalizing" | "completed" | "recoverable" | "failed";
export type RecordingRecovery = { sessionId: string; createdAt: string; message: string };
export type RecordingSnapshot = {
  sessionId: string | null;
  sequence: number;
  controlRevision: number;
  resolvedRoleMask: number;
  pause: PauseSeal | null;
  audioControlLedger: AudioLedger | null;
  pendingTransition: PendingTransition | null;
  clockEpoch?: { activeSample: string; physicalQpc100ns: string } | null;
  lastPauseClock?: { pauseRevision: number; cutoffSample: string; physicalCutoffQpc100ns: string; releasedQpc100ns: string } | null;
  phase: RecordingPhase;
  elapsedMs: number;
  bytesWritten: number;
  countdownRemaining: number | null;
  outputPath: string | null;
  message: string | null;
  config: RecordingConfig | null;
  audio: AudioStatus | null;
  resolvedGeometry: { width: number; height: number } | null;
  selectionRequestId: string | null;
  recoveries: RecordingRecovery[];
};
export type RecordingCapabilities = { available: boolean; reason: string | null; defaultOutputDirectory: string | null; audio: { application: { available: boolean; reason: string | null }; microphoneSuppression: { available: boolean; reason: string | null } } };
export type RecordingSource = { id: string; name: string; x: number; y: number; width: number; height: number };
export type RecordingRegion = { displayId: string; x: number; y: number; width: number; height: number };
export type CaptureSource = (RecordingRegion & { kind: "region" }) | { kind: "window"; windowId: string };
export type RecordingWindow = {
  id: string; name: string; processName: string; width: number; height: number; displayId: string;
  available: boolean; reason: string | null; method: "wgc-window"; protection: "allowed" | "excluded" | "unknown";
};
export type RecordingAudioDevice = { id: string; name: string; kind: "system" | "microphone" };
export type RecordingConfig = {
  requestId: string;
  source: CaptureSource;
  fps: number;
  quality: "standard" | "high";
  countdownSeconds: number;
  showCursor: boolean;
  audio: RecordingAudioConfig;
  outputDirectory: string;
};
export type RecordingStateChange = (snapshot: RecordingSnapshot, busy: boolean) => void;

const INITIAL_SNAPSHOT: RecordingSnapshot = {
  controlRevision: 0, resolvedRoleMask: 0, pause: null, audioControlLedger: null, pendingTransition: null, clockEpoch: null, lastPauseClock: null,
  sessionId: null, sequence: -1, phase: "idle", elapsedMs: 0, bytesWritten: 0,
  countdownRemaining: null, outputPath: null, message: null, config: null, audio: null, resolvedGeometry: null, selectionRequestId: null, recoveries: [],
};

function errorText(error: unknown) {
  return error instanceof Error ? error.message : typeof error === "string" ? error : "The recording action could not be completed.";
}

export function useRecording(onStateChange?: RecordingStateChange, beforeDraftCommit?: () => Promise<boolean>) {
  const [snapshot, setSnapshot] = useState(INITIAL_SNAPSHOT);
  const [capabilities, setCapabilities] = useState<RecordingCapabilities | null>(null);
  const [sources, setSources] = useState<RecordingSource[]>([]);
  const [windows, setWindows] = useState<RecordingWindow[]>([]);
  const [windowsTruncated, setWindowsTruncated] = useState(false);
  const [applications, setApplications] = useState<AudioApplication[]>([]);
  const [applicationsTruncated, setApplicationsTruncated] = useState(false);
  const [audioDevices, setAudioDevices] = useState<RecordingAudioDevice[]>([]);
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [connected, setConnected] = useState(false);
  const current = useRef(INITIAL_SNAPSHOT);
  const operation = useRef<{ name: string; token: symbol } | null>(null);
  const stopOperation = useRef<{ token: symbol; promise: Promise<RecordingSnapshot | null> } | null>(null);
  const commandEpoch = useRef(0);
  const uncertain = useRef(false);
  const mounted = useRef(false);
  const callback = useRef(onStateChange);
  callback.current = onStateChange;
  const beforeCommit = useRef(beforeDraftCommit); beforeCommit.current = beforeDraftCommit;

  const notify = useCallback(() => {
    callback.current?.(current.current, uncertain.current || operation.current !== null || stopOperation.current !== null || current.current.selectionRequestId !== null || isRecordingActive(current.current.phase));
  }, []);

  const accept = useCallback((incoming: RecordingSnapshot) => {
    const next = acceptRecordingSnapshot(current.current, incoming);
    if (mounted.current) {
      current.current = next;
      uncertain.current = false;
      setSnapshot(next);
      setConnected(true);
      setStatusError(null);
      notify();
    }
    return next;
  }, [notify]);

  const refreshSnapshot = useCallback(async () => {
    try {
      return accept(await invoke<RecordingSnapshot>("recording_snapshot"));
    } catch {
      if (mounted.current) {
        setConnected(false);
        setStatusError("Recording status is unavailable. An active recording may still be running. Retry status or Stop; do not close the app to save.");
      }
      return current.current;
    }
  }, [accept]);

  const run = useCallback(async <T,>(name: string, action: () => Promise<T>, reconcile = false): Promise<T | null> => {
    if (operation.current !== null || stopOperation.current !== null) return null;
    const own = { name, token: Symbol(name) };
    operation.current = own;
    setPending(name);
    setError(null);
    notify();
    try {
      return await action();
    } catch (failure) {
      if (mounted.current && operation.current === own) setError(errorText(failure));
      return null;
    } finally {
      if (reconcile && operation.current === own) {
        uncertain.current = true;
        await refreshSnapshot();
      }
      if (operation.current === own) {
        operation.current = null;
        if (mounted.current) { setPending(stopOperation.current ? "recording_stop" : null); notify(); }
      }
    }
  }, [notify, refreshSnapshot]);

  const refreshDevices = useCallback(() => run("refresh", async () => {
    const available = await invoke<RecordingCapabilities>("recording_capabilities");
    if (!mounted.current) return;
    if (available.available) {
      const [displays, devices, targets, apps] = await Promise.all([
        invoke<RecordingSource[]>("recording_sources"),
        invoke<RecordingAudioDevice[]>("recording_audio_devices"),
        invoke<{ windows: RecordingWindow[]; truncated: boolean }>("recording_windows"),
        invoke<{ applications: AudioApplication[]; truncated: boolean }>("recording_audio_applications"),
      ]);
      if (mounted.current && (!beforeCommit.current || await beforeCommit.current())) {
        setCapabilities(available);
        setSources(displays);
        setAudioDevices(devices);
        setWindows(targets.windows);
        setWindowsTruncated(targets.truncated);
        setApplications(apps.applications);
        setApplicationsTruncated(apps.truncated);
      }
    } else if (!beforeCommit.current || await beforeCommit.current()) {
      setCapabilities(available);
      setSources([]);
      setAudioDevices([]);
      setApplications([]);
      setApplicationsTruncated(false);
      setWindows([]);
      setWindowsTruncated(false);
    }
  }), [run]);

  useEffect(() => {
    mounted.current = true;
    void refreshDevices();
    let timer: ReturnType<typeof setTimeout>;
    let disposed = false;
    const poll = async () => {
      await refreshSnapshot();
      if (!disposed) timer = setTimeout(() => void poll(), isRecordingActive(current.current.phase) ? 350 : 1000);
    };
    void poll();
    return () => {
      disposed = true;
      mounted.current = false;
      clearTimeout(timer);
    };
  }, [refreshDevices, refreshSnapshot]);

  const command = useCallback((name: string, args?: Record<string, unknown>): Promise<RecordingSnapshot | null> => {
    if (name === "recording_stop") {
      if (stopOperation.current) return stopOperation.current.promise;
      // Stop retires the ordinary transition token immediately. It never waits
      // for a paused/resuming request, an uncertain Start, or its finally block.
      commandEpoch.current += 1;
      operation.current = null;
      const token = Symbol("stop");
      const promise = Promise.resolve().then(async () => {
        try { return accept(await invoke<RecordingSnapshot>(name, args)); }
        catch (failure) {
          uncertain.current = true;
          if (mounted.current) setError(errorText(failure));
          await refreshSnapshot();
          return null;
        } finally {
          if (stopOperation.current?.token === token) {
            stopOperation.current = null;
            if (mounted.current) { setPending(operation.current?.name ?? null); notify(); }
          }
        }
      });
      stopOperation.current = { token, promise };
      setPending("recording_stop"); setError(null); notify();
      return promise;
    }
    const epoch = commandEpoch.current;
    return run(name, async () => {
      try {
        const incoming = await invoke<RecordingSnapshot>(name, args);
        return epoch === commandEpoch.current ? accept(incoming) : null;
      } catch (failure) {
        if (epoch !== commandEpoch.current) return null;
        // An absent reply does not prove that capture never started.
        uncertain.current = true;
        await refreshSnapshot();
        throw failure;
      }
    });
  }, [accept, notify, refreshSnapshot, run]);

  const transition = useCallback((action: "pause" | "resume", request: TransitionRequest) => {
    const epoch = commandEpoch.current;
    return run(`recording_${action}`, async () => {
      let reply: ControlResult<RecordingSnapshot>;
      try { reply = await invoke(`recording_${action}`, { request }); }
      catch {
        await new Promise(resolve => setTimeout(resolve, 250));
        if (epoch !== commandEpoch.current) return null;
        reply = await invoke(`recording_${action}`, { request });
      }
      if (epoch !== commandEpoch.current) return null;
      if (reply.ok) return accept(reply.value);
      if (mounted.current) setError(reply.error.message);
      await refreshSnapshot();
      return null;
    }, true);
  }, [accept, refreshSnapshot, run]);

  return {
    snapshot, capabilities, sources, windows, windowsTruncated, audioDevices, applications, applicationsTruncated, pending, error, statusError, connected,
    active: isRecordingActive(snapshot.phase), selecting: snapshot.selectionRequestId !== null,
    run, command, transition, refreshDevices, refreshSnapshot,
  };
}
