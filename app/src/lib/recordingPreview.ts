import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { CaptureSource, RecordingPhase } from "./recording";
import { captureSourceKey, isRecordingActive } from "./recordingState";
import { INITIAL_PREVIEW, acceptPreviewSnapshot, decodePreviewEnvelope, frameMatchesPreview, isPreviewLive } from "./recordingPreviewState";

export type RecordingPreviewSnapshot = {
  previewId: string | null; sequence: number; configEpoch: number;
  phase: "idle" | "starting" | "previewing" | "recording" | "stopping" | "failed";
  source: CaptureSource | null; frameSequence: number; sourceWidth: number; sourceHeight: number;
  lastFrameAgeMs: number | null; message: string | null;
};
type Request = { requestId: string; previewId: string | null; generation: number; sourceKey: string; showCursor: boolean };
export type PreviewHandoff = { generation: number; sourceKey: string; showCursor: boolean; resume: boolean };
type Options = { visible: boolean; source: CaptureSource | null; showCursor: boolean; recordingPhase: RecordingPhase; recordingSessionId: string | null; onAdmissionBusyChange?: (busy: boolean) => void };
type ImageFrame = { url: string; sequence: number; previewId: string; configEpoch: number };
const message = (failure: unknown) => failure instanceof Error ? failure.message : String(failure);

export function useRecordingPreview(options: Options) {
  const [snapshot, setSnapshot] = useState(INITIAL_PREVIEW);
  const [image, setImage] = useState<ImageFrame | null>(null);
  const [starting, setStarting] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [eventsReady, setEventsReady] = useState(false);
  const eventsConnected = useRef(false);
  const current = useRef(INITIAL_PREVIEW);
  const currentOptions = useRef({ ...options, sourceKey: captureSourceKey(options.source) });
  currentOptions.current = { ...options, sourceKey: captureSourceKey(options.source) };
  const mounted = useRef(false);
  const generation = useRef(0);
  const request = useRef<Request | null>(null);
  const retired = useRef(new Map<string, Request>());
  const cleanupCount = useRef(0);
  const admissionBusy = useRef(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const imageUrl = useRef<string | null>(null);
  const decoding = useRef<{ url: string; image: HTMLImageElement } | null>(null);
  const renderedSequence = useRef(0);
  const heartbeatAt = useRef(0);
  const polling = useRef(false);
  const previousRecording = useRef({ phase: options.recordingPhase, sessionId: options.recordingSessionId });

  // Native admission lasts until Stop confirms teardown, even after local
  // imagery is cleared or a start reply is lost. Publish before invoking IPC
  // so a workspace switch cannot race an export attempt or queue claim.
  const publishAdmission = useCallback(() => {
    const busy = request.current !== null || retired.current.size > 0 || cleanupCount.current > 0;
    if (busy !== admissionBusy.current) {
      admissionBusy.current = busy;
      currentOptions.current.onAdmissionBusyChange?.(busy);
    }
  }, []);

  const clearImage = useCallback(() => {
    if (imageUrl.current) URL.revokeObjectURL(imageUrl.current);
    imageUrl.current = null;
    if (decoding.current) {
      URL.revokeObjectURL(decoding.current.url);
      decoding.current.image.src = "";
      decoding.current = null;
    }
    renderedSequence.current = 0;
    if (mounted.current) setImage(null);
  }, []);

  const matches = useCallback((candidate: Request) => {
    const latest = currentOptions.current;
    return mounted.current && request.current === candidate && generation.current === candidate.generation &&
      latest.visible && document.visibilityState !== "hidden" && latest.sourceKey === candidate.sourceKey && latest.showCursor === candidate.showCursor;
  }, []);

  const cleanupRetired = useCallback(async () => {
    if (retired.current.size === 0) return true;
    cleanupCount.current += 1;
    publishAdmission();
    if (mounted.current) setStopping(true);
    try {
      const results = await Promise.all([...retired.current.values()].map(async (candidate) => {
        try {
          await invoke("recording_preview_stop", candidate.previewId ? { previewId: candidate.previewId } : { requestId: candidate.requestId });
          retired.current.delete(candidate.requestId);
          return true;
        } catch (failure) {
          if (mounted.current) setError(`Could not confirm preview stopped: ${message(failure)}. Retry Stop preview.`);
          return false;
        }
      }));
      return results.every(Boolean);
    } finally {
      cleanupCount.current -= 1;
      publishAdmission();
      if (mounted.current) setStopping(cleanupCount.current > 0);
    }
  }, [publishAdmission]);

  // Invalidation is synchronous and never waits for a frame request or decode.
  const detach = useCallback(() => {
    const old = request.current;
    const ticket = { generation: generation.current + 1, sourceKey: currentOptions.current.sourceKey,
      showCursor: currentOptions.current.showCursor, resume: !!old && matches(old) && isPreviewLive(current.current.phase) };
    generation.current += 1;
    clearTimeout(timer.current);
    request.current = null;
    if (old) retired.current.set(old.requestId, old);
    publishAdmission();
    clearImage();
    current.current = { ...INITIAL_PREVIEW, sequence: current.current.sequence };
    if (mounted.current) { setSnapshot(current.current); setStarting(false); }
    return ticket;
  }, [clearImage, matches, publishAdmission]);

  const stop = useCallback(() => {
    detach();
    return cleanupRetired();
  }, [cleanupRetired, detach]);

  const accept = useCallback((incoming: RecordingPreviewSnapshot, candidate: Request) => {
    if (!matches(candidate) || (incoming.previewId !== candidate.previewId &&
      !(incoming.previewId === null && ["idle", "failed"].includes(incoming.phase)))) return current.current;
    if (incoming.source && captureSourceKey(incoming.source) !== candidate.sourceKey) throw new Error("Preview returned a different capture source.");
    const next = acceptPreviewSnapshot(current.current, incoming);
    if (incoming.sequence >= current.current.sequence) heartbeatAt.current = performance.now();
    if (next.previewId !== current.current.previewId || next.configEpoch !== current.current.configEpoch || !isPreviewLive(next.phase)) clearImage();
    current.current = next;
    setSnapshot(next);
    return next;
  }, [clearImage, matches]);

  const start = useCallback(async () => {
    const selected = currentOptions.current;
    if (!eventsConnected.current || !selected.visible || document.visibilityState === "hidden" || !selected.source) return;
    const selectedKey = selected.sourceKey, cursor = selected.showCursor;
    const cleanup = stop();
    const startGeneration = generation.current;
    if (!await cleanup || !mounted.current || generation.current !== startGeneration ||
        !currentOptions.current.visible || currentOptions.current.sourceKey !== selectedKey || currentOptions.current.showCursor !== cursor) return;
    const candidate: Request = { requestId: crypto.randomUUID(), previewId: null, generation: startGeneration, sourceKey: selectedKey, showCursor: cursor };
    request.current = candidate;
    publishAdmission();
    setStarting(true); setError(null);
    try {
      const incoming = await invoke<RecordingPreviewSnapshot>("recording_preview_start", { requestId: candidate.requestId, source: selected.source, showCursor: cursor });
      // A stop by requestId may already have cancelled this successful late reply.
      if (!matches(candidate)) {
        if (incoming.previewId) {
          candidate.previewId = incoming.previewId;
          retired.current.set(candidate.requestId, candidate);
          publishAdmission();
          await cleanupRetired();
        }
        return;
      }
      acceptPreviewSnapshot(INITIAL_PREVIEW, incoming);
      if (!incoming.previewId) throw new Error(incoming.message || "Preview could not start.");
      candidate.previewId = incoming.previewId;
      accept(incoming, candidate);
      const poll = async () => {
        if (!matches(candidate) || !candidate.previewId) return;
        // A superseded request can still be returning. Never accumulate polls
        // or decodes across generations; Stop remains on its own control lane.
        if (polling.current) { timer.current = setTimeout(() => void poll(), 200); return; }
        polling.current = true;
        try {
          const status = await invoke<RecordingPreviewSnapshot>("recording_preview_snapshot", { previewId: candidate.previewId });
          const accepted = accept(status, candidate);
          if (!matches(candidate) || !isPreviewLive(accepted.phase)) return;
          const payload = await invoke<ArrayBuffer>("recording_preview_frame", { previewId: candidate.previewId, afterSequence: renderedSequence.current });
          if (!matches(candidate)) return;
          const frame = decodePreviewEnvelope(payload);
          if (frameMatchesPreview(frame, current.current, renderedSequence.current) && frame) {
            const url = URL.createObjectURL(new Blob([frame.bmp], { type: "image/bmp" }));
            const decoded = new Image();
            decoding.current = { url, image: decoded };
            decoded.src = url;
            try { await decoded.decode(); }
            catch (failure) {
              if (decoding.current?.url === url) { decoding.current = null; URL.revokeObjectURL(url); }
              if (matches(candidate) && frameMatchesPreview(frame, current.current, renderedSequence.current)) throw failure;
              return;
            }
            if (decoding.current?.url === url) decoding.current = null;
            // Status events can change the epoch while decoding is pending.
            if (!matches(candidate) || !frameMatchesPreview(frame, current.current, renderedSequence.current)) { URL.revokeObjectURL(url); return; }
            if (imageUrl.current) URL.revokeObjectURL(imageUrl.current);
            imageUrl.current = url;
            renderedSequence.current = frame.sequence;
            setImage({ url, sequence: frame.sequence, previewId: frame.previewId, configEpoch: frame.configEpoch });
          }
        } catch (failure) {
          if (matches(candidate)) { setError(`Preview stopped: ${message(failure)}`); void stop(); }
        } finally {
          polling.current = false;
          if (matches(candidate) && ["starting", "previewing", "recording"].includes(current.current.phase)) timer.current = setTimeout(() => void poll(), 200);
        }
      };
      void poll();
    } catch (failure) {
      if (matches(candidate)) { setError(`Preview could not start: ${message(failure)}`); void stop(); }
    } finally { if (matches(candidate)) setStarting(false); }
  }, [accept, cleanupRetired, matches, publishAdmission, stop]);

  useEffect(() => {
    mounted.current = true;
    eventsConnected.current = false;
    setEventsReady(false);
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<RecordingPreviewSnapshot>("recording-preview-status", ({ payload }) => {
      const candidate = request.current;
      if (!candidate?.previewId || !matches(candidate) || payload.previewId !== candidate.previewId) return;
      try { accept(payload, candidate); }
      catch (failure) { setError(message(failure)); void stop(); }
    }).then((handle) => {
      if (disposed) handle();
      else { unlisten = handle; eventsConnected.current = true; setEventsReady(true); }
    }).catch(() => {
      if (!disposed) { setError("Preview status events are unavailable. Reopen the app before previewing."); void stop(); }
    });
    const hidden = () => { if (document.visibilityState === "hidden") void stop(); };
    document.addEventListener("visibilitychange", hidden);
    const leaving = () => { void stop(); };
    window.addEventListener("pagehide", leaving);
    const heartbeat = setInterval(() => {
      const candidate = request.current;
      if (candidate?.previewId && matches(candidate) && ["starting", "previewing", "recording"].includes(current.current.phase) &&
          performance.now() - heartbeatAt.current > 3000) {
        setError("Preview status is unavailable. Preview stopped; try again when status returns.");
        void stop();
      }
    }, 500);
    return () => {
      disposed = true;
      eventsConnected.current = false;
      void stop();
      mounted.current = false;
      unlisten?.();
      document.removeEventListener("visibilitychange", hidden);
      window.removeEventListener("pagehide", leaving);
      clearInterval(heartbeat);
    };
  }, [accept, matches, stop]);

  useEffect(() => { void stop(); }, [options.visible, currentOptions.current.sourceKey, options.showCursor, stop]);
  useEffect(() => {
    const before = previousRecording.current;
    previousRecording.current = { phase: options.recordingPhase, sessionId: options.recordingSessionId };
    if (["stopping", "finalizing"].includes(options.recordingPhase) ||
        (isRecordingActive(before.phase) && !isRecordingActive(options.recordingPhase)) ||
        (before.sessionId && before.sessionId !== options.recordingSessionId && current.current.phase === "recording")) void stop();
  }, [options.recordingPhase, options.recordingSessionId, stop]);

  const isCurrent = useCallback((ticket: PreviewHandoff) => generation.current === ticket.generation &&
    currentOptions.current.visible && document.visibilityState !== "hidden" &&
    currentOptions.current.sourceKey === ticket.sourceKey && currentOptions.current.showCursor === ticket.showCursor, []);
  const visibleImage = image && request.current && matches(request.current) &&
    image.previewId === snapshot.previewId && image.configEpoch === snapshot.configEpoch && isPreviewLive(snapshot.phase) &&
    !["stopping", "finalizing"].includes(options.recordingPhase) ? image : null;
  return { snapshot, image: visibleImage, starting, stopping, error, eventsReady, start, stop,
    detachForRecording: detach, finishHandoff: cleanupRetired, isCurrent,
    hasRequest: request.current !== null || retired.current.size > 0 };
}
