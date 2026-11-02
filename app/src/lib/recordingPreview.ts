import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { CaptureSource, RecordingPhase } from "./recording";
import { captureSourceKey, isRecordingActive } from "./recordingState";
import { INITIAL_PREVIEW, acceptPreviewSnapshot, automaticPreviewAction, canRunRecordingPreview, decodePreviewEnvelope, frameMatchesPreview, isPreviewLive } from "./recordingPreviewState";
import { createPreviewPollCadence, createRecordingPreviewRenderer } from "./recordingPreviewRenderer";
import { createPreviewViewportQueue, previewViewport, type PreviewViewport } from "./recordingPreviewViewport";

export type RecordingPreviewSnapshot = {
  previewId: string | null; sequence: number; configEpoch: number;
  phase: "idle" | "starting" | "previewing" | "recording" | "stopping" | "failed";
  source: CaptureSource | null; frameSequence: number; sourceWidth: number; sourceHeight: number;
  lastFrameAgeMs: number | null; message: string | null;
  transport?: "native" | "bitmap";
  presentedFrames?: number; presentedAtQpc100ns?: string | null; capturedAtQpc100ns?: string | null;
  nativeWindow?: string | null; nativeVisible?: boolean; viewportWidth?: number; viewportHeight?: number;
  nativeContentRect?: { left: number; top: number; width: number; height: number } | null;
  captureBorder?: { requested: boolean; apiSupported: boolean; accessStatus: number | null;
    accessHresult: string | null; setHresult: string | null; isBorderRequired: boolean | null } | null;
};
type Request = { requestId: string; previewId: string | null; generation: number; windowCycle: number; sourceKey: string; showCursor: boolean; showCaptureBorder: boolean };
export type PreviewHandoff = { generation: number; windowCycle: number; sourceKey: string; showCursor: boolean; showCaptureBorder: boolean; resume: boolean };
type Options = { enabled: boolean; canPreview: boolean; visible: boolean; windowCycle: number; source: CaptureSource | null; showCursor: boolean; showCaptureBorder: boolean; recordingPhase: RecordingPhase; recordingSessionId: string | null; onAdmissionBusyChange?: (busy: boolean) => void };
type ImageFrame = { sequence: number; previewId: string; configEpoch: number };
const FRAME_INTERVAL_MS = 1000 / 60;
const STATUS_INTERVAL_MS = 250;
const message = (failure: unknown) => failure instanceof Error ? failure.message : String(failure);

export function useRecordingPreview(options: Options) {
  const [snapshot, setSnapshot] = useState(INITIAL_PREVIEW);
  const [image, setImage] = useState<ImageFrame | null>(null);
  const [starting, setStarting] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [eventsReady, setEventsReady] = useState(false);
  const [documentVisible, setDocumentVisible] = useState(document.visibilityState !== "hidden");
  const [surfaceVisible, setSurfaceVisible] = useState(false);
  const consumerVisible = useRef(document.visibilityState !== "hidden");
  const [handoff, setHandoff] = useState(false);
  const [startAttempt, setStartAttempt] = useState(0);
  const handoffPending = useRef(false);
  const startInFlight = useRef(false);
  const eventsConnected = useRef(false);
  const current = useRef(INITIAL_PREVIEW);
  const currentOptions = useRef({ ...options, canPreview: options.canPreview && surfaceVisible, sourceKey: captureSourceKey(options.source), documentVisible });
  currentOptions.current = { ...options, canPreview: options.canPreview && surfaceVisible, sourceKey: captureSourceKey(options.source), documentVisible };
  const mounted = useRef(false);
  const generation = useRef(0);
  const request = useRef<Request | null>(null);
  const retired = useRef(new Map<string, Request>());
  const cleanupCount = useRef(0);
  const admissionBusy = useRef(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const renderer = useRef<ReturnType<typeof createRecordingPreviewRenderer> | null>(null);
  const heartbeatAt = useRef(0);
  const polling = useRef(false);
  const previousRecording = useRef({ phase: options.recordingPhase, sessionId: options.recordingSessionId });
  const previousIntent = useRef({ enabled: options.enabled, sourceKey: captureSourceKey(options.source), showCursor: options.showCursor, showCaptureBorder: options.showCaptureBorder, windowCycle: options.windowCycle });
  const [viewportNode, setViewportNode] = useState<HTMLDivElement | null>(null);
  const viewportNodeRef = useRef<HTMLDivElement | null>(null);
  const placementFailed = useRef<() => void>(() => {});
  const viewportQueue = useRef<ReturnType<typeof createPreviewViewportQueue> | null>(null);
  if (!viewportQueue.current) viewportQueue.current = createPreviewViewportQueue({
    send: value => invoke("recording_preview_viewport", value),
    onFailure: (value, failure) => {
      if (mounted.current && request.current?.previewId === value.previewId) {
        setError(`Preview placement failed: ${message(failure)}`);
        placementFailed.current();
      }
    },
  });
  const viewportRef = useCallback((node: HTMLDivElement | null) => {
    viewportNodeRef.current = node;
    setViewportNode(node);
  }, []);
  const queueViewport = useCallback((value: PreviewViewport & { previewId: string }) => {
    void viewportQueue.current?.offer(value);
  }, []);

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
    renderer.current?.clear();
    if (mounted.current) setImage(null);
  }, []);

  const matches = useCallback((candidate: Request) => {
    const latest = currentOptions.current;
    return mounted.current && request.current === candidate && generation.current === candidate.generation && latest.windowCycle === candidate.windowCycle &&
      canRunRecordingPreview({ ...latest, documentVisible: consumerVisible.current && latest.documentVisible && document.visibilityState !== "hidden" }) &&
      latest.sourceKey === candidate.sourceKey && latest.showCursor === candidate.showCursor && latest.showCaptureBorder === candidate.showCaptureBorder;
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
    if (old?.previewId && current.current.transport === "native") {
      queueViewport({ previewId: old.previewId, x: 0, y: 0, width: 0, height: 0, visible: false });
    }
    const ticket = { generation: generation.current + 1, windowCycle: currentOptions.current.windowCycle, sourceKey: currentOptions.current.sourceKey,
      showCursor: currentOptions.current.showCursor, showCaptureBorder: currentOptions.current.showCaptureBorder, resume: !!old && matches(old) && isPreviewLive(current.current.phase) };
    generation.current += 1;
    clearTimeout(timer.current);
    request.current = null;
    if (old) retired.current.set(old.requestId, old);
    publishAdmission();
    clearImage();
    current.current = { ...INITIAL_PREVIEW, sequence: current.current.sequence };
    if (mounted.current) { setSnapshot(current.current); setStarting(false); }
    return ticket;
  }, [clearImage, matches, publishAdmission, queueViewport]);

  const stop = useCallback(() => {
    detach();
    return cleanupRetired();
  }, [cleanupRetired, detach]);
  placementFailed.current = () => { void stop(); };

  if (!renderer.current) renderer.current = createRecordingPreviewRenderer({
    isCurrent: (frame) => {
      const candidate = request.current;
      return !!candidate && matches(candidate) && frameMatchesPreview(frame, current.current);
    },
    onFirstFrame: (frame) => setImage({ sequence: frame.sequence, previewId: frame.previewId, configEpoch: frame.configEpoch }),
    onError: (failure) => { setError(`Preview stopped: ${message(failure)}`); void stop(); },
  });
  const canvasRef = useCallback((canvas: HTMLCanvasElement | null) => renderer.current?.setCanvas(canvas), []);

  const placeViewport = useCallback(() => {
    const candidate = request.current;
    const node = viewportNodeRef.current;
    let visible = !!node && currentOptions.current.visible && consumerVisible.current;
    const clip = { left: 0, top: 0, right: window.innerWidth, bottom: window.innerHeight };
    if (node) {
      // Native airspace cannot sit above HTML dialogs or menus.
      if (document.querySelector('dialog[open], [role="dialog"][aria-modal="true"], [role="alertdialog"], [role="menu"]')) visible = false;
      for (let ancestor: HTMLElement | null = node; ancestor; ancestor = ancestor.parentElement) {
        const style = getComputedStyle(ancestor);
        if (style.visibility === "hidden" || style.display === "none" || ancestor.inert) visible = false;
        const rect = ancestor.getBoundingClientRect();
        if (ancestor !== node && /(auto|scroll|hidden|clip)/.test(style.overflowX)) {
          clip.left = Math.max(clip.left, rect.left); clip.right = Math.min(clip.right, rect.right);
        }
        if (ancestor !== node && /(auto|scroll|hidden|clip)/.test(style.overflowY)) {
          clip.top = Math.max(clip.top, rect.top); clip.bottom = Math.min(clip.bottom, rect.bottom);
        }
      }
    }
    const rect = node?.getBoundingClientRect() ?? { left: 0, top: 0, right: 0, bottom: 0 };
    const viewport = previewViewport(rect, clip, window.devicePixelRatio, visible);
    if (mounted.current) setSurfaceVisible(viewport.visible);
    if (!candidate?.previewId || current.current.transport !== "native") return;
    queueViewport({ previewId: candidate.previewId, ...viewport, visible: viewport.visible && matches(candidate) &&
      ["starting", "previewing", "recording"].includes(current.current.phase) });
  }, [matches, queueViewport]);

  useEffect(() => {
    if (!viewportNode) return;
    let scheduled = 0;
    const changed = () => {
      if (scheduled) return;
      scheduled = requestAnimationFrame(() => { scheduled = 0; placeViewport(); });
    };
    const resize = new ResizeObserver(changed);
    resize.observe(viewportNode);
    const mutation = new MutationObserver(changed);
    mutation.observe(document.body, { subtree: true, childList: true, attributes: true,
      attributeFilter: ["open", "aria-modal", "role", "inert", "hidden", "class", "style"] });
    window.addEventListener("resize", changed);
    document.addEventListener("scroll", changed, true);
    changed();
    return () => {
      cancelAnimationFrame(scheduled); resize.disconnect(); mutation.disconnect();
      window.removeEventListener("resize", changed); document.removeEventListener("scroll", changed, true);
    };
  }, [viewportNode, placeViewport]);
  useEffect(() => { placeViewport(); }, [options.visible, documentVisible, placeViewport]);

  const accept = useCallback((incoming: RecordingPreviewSnapshot, candidate: Request) => {
    if (!matches(candidate) || (incoming.previewId !== candidate.previewId &&
      !(incoming.previewId === null && ["idle", "failed"].includes(incoming.phase)))) return current.current;
    if (incoming.source && captureSourceKey(incoming.source) !== candidate.sourceKey) throw new Error("Preview returned a different capture source.");
    const next = acceptPreviewSnapshot(current.current, incoming);
    if (incoming.sequence >= current.current.sequence) heartbeatAt.current = performance.now();
    if (next.previewId !== current.current.previewId || next.configEpoch !== current.current.configEpoch ||
        next.transport !== current.current.transport || !isPreviewLive(next.phase)) clearImage();
    current.current = next;
    setSnapshot(next);
    if (next.transport === "native") {
      if (next.nativeVisible && next.frameSequence > 0 && next.previewId && isPreviewLive(next.phase)) {
        setImage(previous => previous?.previewId === next.previewId && previous.configEpoch === next.configEpoch ? previous :
          { sequence: next.frameSequence, previewId: next.previewId!, configEpoch: next.configEpoch });
      } else setImage(null);
      placeViewport();
    }
    return next;
  }, [clearImage, matches, placeViewport]);

  const begin = useCallback(async () => {
    const selected = currentOptions.current;
    if (!eventsConnected.current || handoffPending.current || !selected.source ||
        !canRunRecordingPreview({ ...selected, documentVisible: consumerVisible.current && selected.documentVisible && document.visibilityState !== "hidden" })) return;
    const selectedKey = selected.sourceKey, cursor = selected.showCursor, border = selected.showCaptureBorder;
    const cleanup = stop();
    const startGeneration = generation.current;
    if (!await cleanup || !mounted.current || generation.current !== startGeneration || handoffPending.current ||
        !canRunRecordingPreview({ ...currentOptions.current, documentVisible: consumerVisible.current && currentOptions.current.documentVisible && document.visibilityState !== "hidden" }) ||
        currentOptions.current.windowCycle !== selected.windowCycle || currentOptions.current.sourceKey !== selectedKey || currentOptions.current.showCursor !== cursor || currentOptions.current.showCaptureBorder !== border) return;
    const candidate: Request = { requestId: crypto.randomUUID(), previewId: null, generation: startGeneration, windowCycle: selected.windowCycle, sourceKey: selectedKey, showCursor: cursor, showCaptureBorder: border };
    request.current = candidate;
    publishAdmission();
    setStarting(true); setError(null);
    try {
      const incoming = await invoke<RecordingPreviewSnapshot>("recording_preview_start", { requestId: candidate.requestId, source: selected.source, showCursor: cursor, showCaptureBorder: border });
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
      let nextStatusAt = 0;
      const nextFrameDelay = createPreviewPollCadence(FRAME_INTERVAL_MS);
      const poll = async () => {
        if (!matches(candidate) || !candidate.previewId) return;
        // A superseded request can still be returning. Never accumulate polls
        // or decodes across generations; Stop remains on its own control lane.
        if (polling.current) { timer.current = setTimeout(() => void poll(), FRAME_INTERVAL_MS); return; }
        polling.current = true;
        const cycleStarted = performance.now();
        try {
          // Status/lease feedback has a separate cadence; each image needs only
          // the existing binary response, not a second status round trip.
          if (cycleStarted >= nextStatusAt) {
            nextStatusAt = cycleStarted + STATUS_INTERVAL_MS;
            const status = await invoke<RecordingPreviewSnapshot>("recording_preview_snapshot", { previewId: candidate.previewId });
            accept(status, candidate);
          }
          if (!matches(candidate) || !isPreviewLive(current.current.phase)) return;
          if (current.current.transport === "native") {
            placeViewport();
            return;
          }
          const payload = await invoke<ArrayBuffer>("recording_preview_frame", { previewId: candidate.previewId, afterSequence: renderer.current?.afterSequence() ?? 0 });
          if (!matches(candidate)) return;
          const frame = decodePreviewEnvelope(payload);
          if (frame && frameMatchesPreview(frame, current.current, renderer.current?.afterSequence() ?? 0)) await renderer.current?.offer(frame);
        } catch (failure) {
          if (matches(candidate)) { setError(`Preview stopped: ${message(failure)}`); void stop(); }
        } finally {
          polling.current = false;
          // Decode and IPC time count toward the frame period. A late cycle
          // never creates overlapping requests or a catch-up queue.
          if (matches(candidate) && ["starting", "previewing", "recording"].includes(current.current.phase)) {
            timer.current = setTimeout(() => void poll(), current.current.transport === "native" ? STATUS_INTERVAL_MS : nextFrameDelay(cycleStarted, performance.now()));
          }
        }
      };
      void poll();
    } catch (failure) {
      if (matches(candidate)) { setError(`Preview could not start: ${message(failure)}`); void stop(); }
    } finally { if (matches(candidate)) setStarting(false); }
  }, [accept, cleanupRetired, matches, publishAdmission, stop, placeViewport]);

  const start = useCallback(async () => {
    if (startInFlight.current) return;
    startInFlight.current = true;
    try { await begin(); }
    finally {
      startInFlight.current = false;
      if (mounted.current) setStartAttempt(value => value + 1);
    }
  }, [begin]);

  const detachForRecording = useCallback(() => {
    handoffPending.current = true;
    setHandoff(true);
    return detach();
  }, [detach]);
  const finishHandoff = useCallback(async () => {
    try { return await cleanupRetired(); }
    finally {
      handoffPending.current = false;
      if (mounted.current) setHandoff(false);
    }
  }, [cleanupRetired]);

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
    const hidden = () => {
      consumerVisible.current = document.visibilityState !== "hidden";
      setDocumentVisible(consumerVisible.current);
      if (!consumerVisible.current) void stop();
    };
    document.addEventListener("visibilitychange", hidden);
    const leaving = () => { consumerVisible.current = false; setDocumentVisible(false); void stop(); };
    const showing = () => { consumerVisible.current = document.visibilityState !== "hidden"; setDocumentVisible(consumerVisible.current); };
    window.addEventListener("pagehide", leaving);
    window.addEventListener("pageshow", showing);
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
      window.removeEventListener("pageshow", showing);
      clearInterval(heartbeat);
    };
  }, [accept, matches, stop]);

  useEffect(() => {
    const before = previousIntent.current;
    const next = { enabled: options.enabled, sourceKey: currentOptions.current.sourceKey, showCursor: options.showCursor, showCaptureBorder: options.showCaptureBorder, windowCycle: options.windowCycle };
    previousIntent.current = next;
    // A start can race native minimization before its status reaches React.
    // A new native cycle retires that attempt and permits recovery on restore,
    // even if WebView never delivered a document visibility event.
    if (before.sourceKey !== next.sourceKey || before.showCursor !== next.showCursor || before.showCaptureBorder !== next.showCaptureBorder || before.windowCycle !== next.windowCycle || !before.enabled && next.enabled) setError(null);
    void stop();
  }, [options.enabled, options.visible, options.windowCycle, currentOptions.current.sourceKey, options.showCursor, options.showCaptureBorder, stop]);
  useEffect(() => {
    const before = previousRecording.current;
    previousRecording.current = { phase: options.recordingPhase, sessionId: options.recordingSessionId };
    if (["pausing", "paused", "resuming", "stopping", "finalizing"].includes(options.recordingPhase) ||
        (isRecordingActive(before.phase) && !isRecordingActive(options.recordingPhase)) ||
        (before.sessionId && before.sessionId !== options.recordingSessionId && current.current.phase === "recording")) void stop();
  }, [options.recordingPhase, options.recordingSessionId, stop]);

  const hasRequest = request.current !== null || retired.current.size > 0;
  useEffect(() => {
    if (!eventsReady) return;
    const action = automaticPreviewAction({ ...options, canPreview: options.canPreview && surfaceVisible, documentVisible, handoff: handoff || handoffPending.current,
      hasRequest, starting: starting || startInFlight.current, stopping, error, previewPhase: snapshot.phase });
    if (action === "start") void start();
    else if (action === "stop") void stop();
  }, [options.enabled, options.canPreview, options.visible, options.recordingPhase, currentOptions.current.sourceKey,
    options.showCursor, options.showCaptureBorder, documentVisible, surfaceVisible, eventsReady, handoff, hasRequest, starting, stopping, error, snapshot.phase, startAttempt, start, stop]);

  const isCurrent = useCallback((ticket: PreviewHandoff) => generation.current === ticket.generation &&
    currentOptions.current.windowCycle === ticket.windowCycle &&
    currentOptions.current.visible && document.visibilityState !== "hidden" &&
    currentOptions.current.sourceKey === ticket.sourceKey && currentOptions.current.showCursor === ticket.showCursor && currentOptions.current.showCaptureBorder === ticket.showCaptureBorder, []);
  const visibleImage = image && request.current && matches(request.current) &&
    image.previewId === snapshot.previewId && image.configEpoch === snapshot.configEpoch && isPreviewLive(snapshot.phase) &&
    !["pausing", "paused", "resuming", "stopping", "finalizing"].includes(options.recordingPhase) ? image : null;
  return { snapshot, image: visibleImage, canvasRef, viewportRef, starting, stopping, error, eventsReady, start, stop,
    detachForRecording, finishHandoff, isCurrent, hasRequest };
}
