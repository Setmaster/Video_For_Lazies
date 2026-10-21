import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { VideoProbe } from "./types";
import { validateAudioPreviewAsset } from "./audioTrackState";
export type AudioPreviewAsset = { requestId: string; path: string; sourceIdentity: string; audioStreamIndex: number } & (
  { kind: "original"; proxyId: null; timelineOffsetS: 0 } | { kind: "proxy"; proxyId: string; timelineOffsetS: number });
type Playback = { timeS: number; playing: boolean };
type Request = { requestId: string; path: string; sourceIdentity: string; streamIndex: number; generation: number; asset: AudioPreviewAsset | null };
type Options = { visible: boolean; enabled: boolean; path: string; probe: VideoProbe | null;
  unload: () => Playback; restore: (playback: Playback) => void; onAdmissionBusyChange: (busy: boolean) => void };
const errorText = (failure: unknown) => failure instanceof Error ? failure.message : String(failure);
export function useAudioTrackPreview(options: Options) {
  const [asset, setAsset] = useState<AudioPreviewAsset | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [revision, setRevision] = useState(0);
  const latest = useRef(options); latest.current = options;
  const mounted = useRef(true), generation = useRef(0), current = useRef<Request | null>(null);
  const retired = useRef(new Map<string, Request>()), cleaning = useRef(0), busy = useRef(false);
  const playback = useRef<(Playback & { path: string }) | null>(null);
  const restored = useRef<string | null>(null);
  const closing = useRef(false), cancelled = useRef(false);
  const publish = useCallback(() => {
    const next = !!current.current && !current.current.asset || retired.current.size > 0 || cleaning.current > 0;
    if (next !== busy.current) { busy.current = next; latest.current.onAdmissionBusyChange(next); }
    if (mounted.current) setPending(next);
  }, []);
  const cleanup = useCallback(async () => {
    if (!retired.current.size) return true;
    cleaning.current += 1; publish();
    try {
      return (await Promise.all([...retired.current.values()].map(async request => {
        try {
          if (request.asset?.kind === "proxy") await invoke("release_audio_preview", { requestId: request.requestId, proxyId: request.asset.proxyId });
          else {
            const ack = await invoke<{ requestId: string; quiescent: true }>("cancel_audio_preview", { requestId: request.requestId });
            if (ack.requestId !== request.requestId || ack.quiescent !== true) throw new Error("Audio preview cancellation was not confirmed.");
          }
          retired.current.delete(request.requestId); return true;
        } catch (failure) {
          if (mounted.current) setError(`Audio preview cleanup not confirmed: ${errorText(failure)}. Retry preview cleanup before exporting.`);
          return false;
        }
      }))).every(Boolean);
    } finally { cleaning.current -= 1; publish(); }
  }, [publish]);
  const stop = useCallback((preservePlayback = false) => {
    const old = current.current;
    // Unload the actual media element before releasing any owned Windows file.
    const state = latest.current.unload();
    if (old?.asset) playback.current = { ...state, path: old.path, playing: preservePlayback && state.playing };
    else if (!preservePlayback && playback.current) playback.current.playing = false;
    generation.current += 1; current.current = null; restored.current = null;
    if (old) retired.current.set(old.requestId, old);
    if (mounted.current) setAsset(null);
    publish();
    return cleanup();
  }, [cleanup, publish]);
  const matches = useCallback((request: Request) => {
    const now = latest.current;
    return mounted.current && !closing.current && current.current === request && generation.current === request.generation && now.visible && now.enabled &&
      now.path === request.path && now.probe?.sourceIdentity === request.sourceIdentity && now.probe.audioStreamIndex === request.streamIndex;
  }, []);
  useEffect(() => {
    mounted.current = true;
    return () => { void stop(false); mounted.current = false; };
  }, [stop]);
  const key = `${options.path}\n${options.probe?.sourceIdentity || ""}\n${options.probe?.audioStreamIndex ?? ""}`;
  useEffect(() => {
    if (!options.visible || !options.enabled || !options.probe?.hasAudio) { void stop(options.visible && !!options.path); return; }
    const selected = options.probe;
    const path = options.path;
    const launch = async () => {
      const done = stop(true), token = generation.current;
      if (closing.current || cancelled.current || !await done || token !== generation.current || !mounted.current || !latest.current.visible || !latest.current.enabled || latest.current.path !== path || latest.current.probe?.sourceIdentity !== selected.sourceIdentity || latest.current.probe.audioStreamIndex !== selected.audioStreamIndex) return;
      if (!selected.sourceIdentity || selected.audioStreamIndex === null || selected.audioStreamIndex === undefined) { setError("The selected audio track is unavailable. Reload or reselect the source."); return; }
      const request: Request = { requestId: crypto.randomUUID(), path, sourceIdentity: selected.sourceIdentity, streamIndex: selected.audioStreamIndex, generation: token, asset: null };
      current.current = request; setError(null); publish();
      try {
        const reply = await invoke<AudioPreviewAsset>("prepare_audio_preview", { requestId: request.requestId, path, audioStreamIndex: request.streamIndex, audioSourceIdentity: request.sourceIdentity });
        if (!matches(request)) {
          // Only owned proxies are released. Borrowed original files are never
          // deleted, released or forbidden, including a late original reply.
          request.asset = reply.kind === "proxy" ? reply : null;
          retired.current.set(request.requestId, request); publish(); await cleanup(); return;
        }
        request.asset = validateAudioPreviewAsset(reply, request);
        setAsset(request.asset); publish();
      } catch (failure) {
        if (matches(request)) { setError(`Audio preview unavailable: ${errorText(failure)}`); void stop(true); }
      }
    };
    void launch();
  }, [key, options.enabled, options.visible, options.probe?.hasAudio, revision, cleanup, matches, publish, stop]);
  const sourceToken = asset?.requestId || (!options.probe?.hasAudio ? `silent:${options.path}` : "");
  const ready = useCallback((token: string) => {
    const request = current.current;
    if (!request?.asset || request.requestId !== token || !matches(request)) return;
    if (restored.current === token) return;
    restored.current = token;
    const saved = playback.current;
    if (saved?.path === request.path) latest.current.restore(saved);
  }, [matches]);
  return { asset, sourceToken, pending, error, stop, ready, cancelled: cancelled.current,
    playbackError: (token: string) => { if (current.current?.requestId === token && mounted.current) setError("The selected audio/video cannot be played in this browser. The export track choice is unchanged."); },
    path: asset?.path || (options.visible && options.enabled && options.probe && !options.probe.hasAudio ? options.path : ""),
    timelineOffsetS: asset?.timelineOffsetS || 0,
    close: () => { closing.current = true; return stop(false); },
    cancel: () => { cancelled.current = true; return stop(false); },
    resetIntent: () => { cancelled.current = false; },
    retry: () => { cancelled.current = false; closing.current = false; setError(null); setRevision(value => value + 1); },
  };
}
