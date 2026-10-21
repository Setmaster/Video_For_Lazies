import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { acceptRecordingSnapshot, isRecordingActive } from "./recordingState";

export type RecordingPhase = "idle" | "preparing" | "countdown" | "starting" | "recording" | "stopping" | "finalizing" | "completed" | "recoverable" | "failed";
export type RecordingRecovery = { sessionId: string; createdAt: string; message: string };
export type RecordingSnapshot = {
  sessionId: string | null;
  sequence: number;
  phase: RecordingPhase;
  elapsedMs: number;
  bytesWritten: number;
  countdownRemaining: number | null;
  outputPath: string | null;
  message: string | null;
  config: RecordingConfig | null;
  resolvedGeometry: { width: number; height: number } | null;
  selectionRequestId: string | null;
  recoveries: RecordingRecovery[];
};
export type RecordingCapabilities = { available: boolean; reason: string | null; defaultOutputDirectory: string | null };
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
  systemAudioDeviceId: string | null;
  microphoneDeviceId: string | null;
  outputDirectory: string;
};
export type RecordingStateChange = (snapshot: RecordingSnapshot, busy: boolean) => void;

const INITIAL_SNAPSHOT: RecordingSnapshot = {
  sessionId: null, sequence: -1, phase: "idle", elapsedMs: 0, bytesWritten: 0,
  countdownRemaining: null, outputPath: null, message: null, config: null, resolvedGeometry: null, selectionRequestId: null, recoveries: [],
};

function errorText(error: unknown) {
  return error instanceof Error ? error.message : typeof error === "string" ? error : "The recording action could not be completed.";
}

export function useRecording(onStateChange?: RecordingStateChange) {
  const [snapshot, setSnapshot] = useState(INITIAL_SNAPSHOT);
  const [capabilities, setCapabilities] = useState<RecordingCapabilities | null>(null);
  const [sources, setSources] = useState<RecordingSource[]>([]);
  const [windows, setWindows] = useState<RecordingWindow[]>([]);
  const [windowsTruncated, setWindowsTruncated] = useState(false);
  const [audioDevices, setAudioDevices] = useState<RecordingAudioDevice[]>([]);
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [connected, setConnected] = useState(false);
  const current = useRef(INITIAL_SNAPSHOT);
  const operation = useRef<string | null>(null);
  const uncertain = useRef(false);
  const mounted = useRef(false);
  const callback = useRef(onStateChange);
  callback.current = onStateChange;

  const notify = useCallback(() => {
    callback.current?.(current.current, uncertain.current || operation.current !== null || current.current.selectionRequestId !== null || isRecordingActive(current.current.phase));
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
    if (operation.current !== null) return null;
    operation.current = name;
    setPending(name);
    setError(null);
    notify();
    try {
      return await action();
    } catch (failure) {
      if (mounted.current) setError(errorText(failure));
      return null;
    } finally {
      if (reconcile) {
        uncertain.current = true;
        await refreshSnapshot();
      }
      operation.current = null;
      if (mounted.current) {
        setPending(null);
        notify();
      }
    }
  }, [notify, refreshSnapshot]);

  const refreshDevices = useCallback(() => run("refresh", async () => {
    const available = await invoke<RecordingCapabilities>("recording_capabilities");
    if (!mounted.current) return;
    setCapabilities(available);
    if (available.available) {
      const [displays, devices, targets] = await Promise.all([
        invoke<RecordingSource[]>("recording_sources"),
        invoke<RecordingAudioDevice[]>("recording_audio_devices"),
        invoke<{ windows: RecordingWindow[]; truncated: boolean }>("recording_windows"),
      ]);
      if (mounted.current) {
        setSources(displays);
        setAudioDevices(devices);
        setWindows(targets.windows);
        setWindowsTruncated(targets.truncated);
      }
    } else {
      setSources([]);
      setAudioDevices([]);
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

  const command = useCallback((name: string, args?: Record<string, unknown>) => run(name, async () => {
    try {
      return accept(await invoke<RecordingSnapshot>(name, args));
    } catch (failure) {
      // A lost command reply cannot prove that native capture never started.
      uncertain.current = true;
      await refreshSnapshot();
      throw failure;
    }
  }), [accept, refreshSnapshot, run]);

  return {
    snapshot, capabilities, sources, windows, windowsTruncated, audioDevices, pending, error, statusError, connected,
    active: isRecordingActive(snapshot.phase), selecting: snapshot.selectionRequestId !== null,
    run, command, refreshDevices, refreshSnapshot,
  };
}
