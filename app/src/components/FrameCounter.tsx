import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { sourceFrameNumber, validFrameIndex } from "../lib/frameCounter.mjs";

type Props = {
  path: string; sourceIdentity: string; videoStreamIndex: number;
  timeS: number; durationS: number; sourceToken: string; timelineOffsetS: number;
  getVideo: () => HTMLVideoElement | null;
};

export function FrameCounter({ path, sourceIdentity, videoStreamIndex, timeS, durationS, sourceToken, timelineOffsetS, getVideo }: Props) {
  const [times, setTimes] = useState<number[] | null>(null);
  const [error, setError] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [presentedTime, setPresentedTime] = useState<number | null>(null);

  useEffect(() => {
    let cancelled = false;
    setTimes(null);
    setError(false);
    // Coalesce source changes and React's development effect replay before
    // starting the bounded native scan. Late results never replace a new source.
    const timer = window.setTimeout(() => {
      void invoke<unknown>("source_frame_index", { request: { path, sourceIdentity, videoStreamIndex } })
        .then(result => {
          if (cancelled) return;
          if (!validFrameIndex(result, sourceIdentity)) throw new Error("Invalid source frame index");
          setTimes(result.timesS);
        })
        .catch(() => { if (!cancelled) setError(true); });
    }, 150);
    return () => { cancelled = true; window.clearTimeout(timer); };
  }, [path, sourceIdentity, videoStreamIndex, attempt]);

  useEffect(() => {
    const video = getVideo();
    setPresentedTime(null);
    if (!video || typeof video.requestVideoFrameCallback !== "function") return;
    let cancelled = false;
    let callback = 0;
    const report: VideoFrameRequestCallback = (_, metadata) => {
      if (cancelled) return;
      setPresentedTime(metadata.mediaTime - timelineOffsetS);
      callback = video.requestVideoFrameCallback(report);
    };
    const seeking = () => setPresentedTime(null);
    video.addEventListener("seeking", seeking);
    callback = video.requestVideoFrameCallback(report);
    return () => {
      cancelled = true;
      video.removeEventListener("seeking", seeking);
      video.cancelVideoFrameCallback(callback);
    };
  }, [getVideo, sourceToken, timelineOffsetS]);

  const usePresented = presentedTime !== null;
  const number = times ? sourceFrameNumber(times, usePresented ? presentedTime : timeS, durationS, usePresented) : null;
  const text = error ? "Frame unavailable" : !times ? "Frame …" : number === null ? "No frame" : `Frame ${number}`;
  const hint = error
    ? "Source frame numbers could not be read. The source may be too large, busy, changed or have unsupported timing. Retry when ready."
    : !times ? "Reading source frame numbers…"
    : "Current source frame, numbered from 1. Uses source timestamps, including variable frame rates.";

  return <span className="vfl-frame-counter" title={hint}>
    <span id="vfl-frame-counter" aria-label={number !== null ? `Current source frame ${number}` : text}>{text}</span>
    {error ? <button type="button" className="vfl-frame-counter-retry" aria-label="Retry reading source frame numbers" onClick={() => setAttempt(n => n + 1)}>Retry</button> : null}
  </span>;
}
