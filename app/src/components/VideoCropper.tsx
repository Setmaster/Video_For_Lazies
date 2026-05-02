import { forwardRef, useEffect, useImperativeHandle, useMemo, useRef, useState, type PointerEvent } from "react";

export type NormalizedRect = { x: number; y: number; w: number; h: number };
export type VideoCropperHandle = {
  play: () => Promise<boolean>;
  pause: () => boolean;
  togglePlayback: () => Promise<boolean>;
  seekTo: (timeS: number) => void;
};

type AspectMode = { locked: boolean; ratio: number | null };

type DragMode =
  | { kind: "none" }
  | { kind: "draw"; anchorX: number; anchorY: number }
  | { kind: "move"; offsetX: number; offsetY: number }
  | { kind: "resize"; corner: "nw" | "ne" | "sw" | "se"; anchor: NormalizedRect }
  | { kind: "resize-edge"; edge: "n" | "s" | "e" | "w"; anchor: NormalizedRect };

function clamp01(v: number) {
  return Math.max(0, Math.min(1, v));
}

function clamp(v: number, min: number, max: number) {
  return Math.max(min, Math.min(max, v));
}

function normalizeRect(r: NormalizedRect): NormalizedRect {
  const x1 = r.x;
  const y1 = r.y;
  const x2 = r.x + r.w;
  const y2 = r.y + r.h;
  const left = Math.min(x1, x2);
  const top = Math.min(y1, y2);
  const right = Math.max(x1, x2);
  const bottom = Math.max(y1, y2);
  return { x: clamp01(left), y: clamp01(top), w: clamp01(right) - clamp01(left), h: clamp01(bottom) - clamp01(top) };
}

function minSizedRect(r: NormalizedRect, minSize: number): NormalizedRect {
  const w = Math.max(r.w, minSize);
  const h = Math.max(r.h, minSize);
  const x = clamp(r.x, 0, 1 - w);
  const y = clamp(r.y, 0, 1 - h);
  return { x, y, w, h };
}

function applyAspect(anchor: { x: number; y: number }, current: { x: number; y: number }, ratio: number) {
  const dx = current.x - anchor.x;
  const dy = current.y - anchor.y;
  const absDx = Math.abs(dx);
  const absDy = Math.abs(dy);

  let w = absDx;
  let h = w / ratio;
  if (h > absDy) {
    h = absDy;
    w = h * ratio;
  }

  const sx = dx >= 0 ? 1 : -1;
  const sy = dy >= 0 ? 1 : -1;
  return { w: w * sx, h: h * sy };
}

function cornerFromPoint(
  x: number,
  y: number,
  rect: NormalizedRect,
  threshold: number,
): "nw" | "ne" | "sw" | "se" | null {
  const left = rect.x;
  const top = rect.y;
  const right = rect.x + rect.w;
  const bottom = rect.y + rect.h;

  const nearLeft = Math.abs(x - left) <= threshold;
  const nearRight = Math.abs(x - right) <= threshold;
  const nearTop = Math.abs(y - top) <= threshold;
  const nearBottom = Math.abs(y - bottom) <= threshold;

  if (nearLeft && nearTop) return "nw";
  if (nearRight && nearTop) return "ne";
  if (nearLeft && nearBottom) return "sw";
  if (nearRight && nearBottom) return "se";
  return null;
}

function edgeFromPoint(
  x: number,
  y: number,
  rect: NormalizedRect,
  threshold: number,
): "n" | "s" | "e" | "w" | null {
  const left = rect.x;
  const top = rect.y;
  const right = rect.x + rect.w;
  const bottom = rect.y + rect.h;

  const nearLeft = Math.abs(x - left) <= threshold;
  const nearRight = Math.abs(x - right) <= threshold;
  const nearTop = Math.abs(y - top) <= threshold;
  const nearBottom = Math.abs(y - bottom) <= threshold;

  const withinX = x >= left - threshold && x <= right + threshold;
  const withinY = y >= top - threshold && y <= bottom + threshold;

  if (nearLeft && withinY) return "w";
  if (nearRight && withinY) return "e";
  if (nearTop && withinX) return "n";
  if (nearBottom && withinX) return "s";
  return null;
}

function insideRect(x: number, y: number, rect: NormalizedRect) {
  return x >= rect.x && x <= rect.x + rect.w && y >= rect.y && y <= rect.y + rect.h;
}

function resizeWithAspect(
  corner: "nw" | "ne" | "sw" | "se",
  anchor: NormalizedRect,
  px: number,
  py: number,
  ratio: number,
): NormalizedRect {
  const x1 = anchor.x;
  const y1 = anchor.y;
  const x2 = anchor.x + anchor.w;
  const y2 = anchor.y + anchor.h;

  const fixed = (() => {
    switch (corner) {
      case "nw":
        return { fx: x2, fy: y2 };
      case "ne":
        return { fx: x1, fy: y2 };
      case "sw":
        return { fx: x2, fy: y1 };
      case "se":
        return { fx: x1, fy: y1 };
    }
  })();

  const dx = px - fixed.fx;
  const dy = py - fixed.fy;
  const absDx = Math.abs(dx);
  const absDy = Math.abs(dy);

  let w = absDx;
  let h = w / ratio;
  if (h > absDy) {
    h = absDy;
    w = h * ratio;
  }

  const sx = dx >= 0 ? 1 : -1;
  const sy = dy >= 0 ? 1 : -1;
  const cx = fixed.fx + w * sx;
  const cy = fixed.fy + h * sy;

  return normalizeRect({ x: fixed.fx, y: fixed.fy, w: cx - fixed.fx, h: cy - fixed.fy });
}

function fitToBounds(rect: NormalizedRect): NormalizedRect {
  const r = normalizeRect(rect);
  const x = clamp(r.x, 0, 1 - r.w);
  const y = clamp(r.y, 0, 1 - r.h);
  return { x, y, w: r.w, h: r.h };
}

function toNorm(
  clientX: number,
  clientY: number,
  container: DOMRect,
  content: { x: number; y: number; width: number; height: number },
) {
  const x = (clientX - container.left - content.x) / content.width;
  const y = (clientY - container.top - content.y) / content.height;
  return { x: clamp01(x), y: clamp01(y) };
}

function getContentRectPx(
  elementWidth: number,
  elementHeight: number,
  videoWidth: number,
  videoHeight: number,
) {
  if (!videoWidth || !videoHeight || !elementWidth || !elementHeight) {
    return { x: 0, y: 0, width: elementWidth, height: elementHeight };
  }
  const elementRatio = elementWidth / elementHeight;
  const videoRatio = videoWidth / videoHeight;
  if (elementRatio > videoRatio) {
    const height = elementHeight;
    const width = height * videoRatio;
    return { x: (elementWidth - width) / 2, y: 0, width, height };
  }
  const width = elementWidth;
  const height = width / videoRatio;
  return { x: 0, y: (elementHeight - height) / 2, width, height };
}

export const VideoCropper = forwardRef<VideoCropperHandle, {
  src: string;
  rect: NormalizedRect;
  onChange: (r: NormalizedRect) => void;
  aspect: AspectMode;
  frameAspectRatio?: number;
  cropEnabled: boolean;
  disabled?: boolean;
  onTimeUpdate?: (t: number) => void;
  onPlaybackChange?: (playing: boolean) => void;
  onSourceReadyChange?: (ready: boolean) => void;
}>(function VideoCropper({
  src,
  rect,
  onChange,
  aspect,
  frameAspectRatio,
  cropEnabled,
  disabled,
  onTimeUpdate,
  onPlaybackChange,
  onSourceReadyChange,
}, ref) {
  const containerRef = useRef<HTMLDivElement>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const [videoSize, setVideoSize] = useState<{ w: number; h: number }>({ w: 0, h: 0 });
  const dragRef = useRef<DragMode>({ kind: "none" });
  const [cursor, setCursor] = useState("default");

  const [containerSize, setContainerSize] = useState<{ w: number; h: number }>({ w: 0, h: 0 });

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      const r = el.getBoundingClientRect();
      setContainerSize({ w: r.width, h: r.height });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const content = useMemo(
    () => getContentRectPx(containerSize.w, containerSize.h, videoSize.w, videoSize.h),
    [containerSize, videoSize],
  );

  const rectPx = useMemo(() => {
    return {
      left: content.x + rect.x * content.width,
      top: content.y + rect.y * content.height,
      width: rect.w * content.width,
      height: rect.h * content.height,
    };
  }, [content, rect]);

  function setRect(next: NormalizedRect) {
    onChange(fitToBounds(minSizedRect(next, 0.03)));
  }

  const cropInteractive = cropEnabled && !disabled;

  function cursorForCorner(corner: "nw" | "ne" | "sw" | "se") {
    return corner === "nw" || corner === "se" ? "nwse-resize" : "nesw-resize";
  }

  function cursorForEdge(edge: "n" | "s" | "e" | "w") {
    return edge === "n" || edge === "s" ? "ns-resize" : "ew-resize";
  }

  function onPointerDown(e: PointerEvent<HTMLDivElement>) {
    if (!cropInteractive) return;
    const containerEl = containerRef.current;
    if (!containerEl) return;
    const container = containerEl.getBoundingClientRect();
    const p = toNorm(e.clientX, e.clientY, container, content);

    const threshold = 10 / Math.max(1, content.width);
    const corner = cornerFromPoint(p.x, p.y, rect, threshold);

    if (corner) {
      dragRef.current = { kind: "resize", corner, anchor: rect };
      setCursor(cursorForCorner(corner));
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
      e.preventDefault();
      return;
    }

    const edge = edgeFromPoint(p.x, p.y, rect, threshold);
    if (edge) {
      dragRef.current = { kind: "resize-edge", edge, anchor: rect };
      setCursor(cursorForEdge(edge));
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
      e.preventDefault();
      return;
    }

    if (insideRect(p.x, p.y, rect)) {
      dragRef.current = { kind: "move", offsetX: p.x - rect.x, offsetY: p.y - rect.y };
      setCursor("move");
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
      e.preventDefault();
      return;
    }

    dragRef.current = { kind: "draw", anchorX: p.x, anchorY: p.y };
    setCursor("crosshair");
    setRect({ x: p.x, y: p.y, w: 0.001, h: 0.001 });
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    e.preventDefault();
  }

  function onPointerMove(e: PointerEvent<HTMLDivElement>) {
    if (!cropInteractive) return;
    const mode = dragRef.current;
    const containerEl = containerRef.current;
    if (!containerEl) return;
    const container = containerEl.getBoundingClientRect();
    const p = toNorm(e.clientX, e.clientY, container, content);

    if (mode.kind === "none") {
      const threshold = 10 / Math.max(1, content.width);
      const corner = cornerFromPoint(p.x, p.y, rect, threshold);
      if (corner) {
        setCursor(cursorForCorner(corner));
        return;
      }
      const edge = edgeFromPoint(p.x, p.y, rect, threshold);
      if (edge) {
        setCursor(cursorForEdge(edge));
        return;
      }
      if (insideRect(p.x, p.y, rect)) {
        setCursor("move");
        return;
      }
      setCursor("crosshair");
      return;
    }

    if (mode.kind === "move") {
      const next = {
        x: clamp(p.x - mode.offsetX, 0, 1 - rect.w),
        y: clamp(p.y - mode.offsetY, 0, 1 - rect.h),
        w: rect.w,
        h: rect.h,
      };
      setRect(next);
      return;
    }

    if (mode.kind === "draw") {
      if (aspect.locked && aspect.ratio) {
        const d = applyAspect({ x: mode.anchorX, y: mode.anchorY }, p, aspect.ratio);
        setRect(normalizeRect({ x: mode.anchorX, y: mode.anchorY, w: d.w, h: d.h }));
        return;
      }
      setRect(normalizeRect({ x: mode.anchorX, y: mode.anchorY, w: p.x - mode.anchorX, h: p.y - mode.anchorY }));
      return;
    }

    if (mode.kind === "resize") {
      if (aspect.locked && aspect.ratio) {
        setRect(resizeWithAspect(mode.corner, mode.anchor, p.x, p.y, aspect.ratio));
        return;
      }

      const a = mode.anchor;
      const x1 = a.x;
      const y1 = a.y;
      const x2 = a.x + a.w;
      const y2 = a.y + a.h;

      const next = (() => {
        switch (mode.corner) {
          case "nw":
            return normalizeRect({ x: p.x, y: p.y, w: x2 - p.x, h: y2 - p.y });
          case "ne":
            return normalizeRect({ x: x1, y: p.y, w: p.x - x1, h: y2 - p.y });
          case "sw":
            return normalizeRect({ x: p.x, y: y1, w: x2 - p.x, h: p.y - y1 });
          case "se":
            return normalizeRect({ x: x1, y: y1, w: p.x - x1, h: p.y - y1 });
        }
      })();
      setRect(next);
    }

    if (mode.kind === "resize-edge") {
      if (aspect.locked && aspect.ratio) {
        const a = mode.anchor;
        const x1 = a.x;
        const y1 = a.y;
        const x2 = a.x + a.w;
        const y2 = a.y + a.h;
        const cx = (x1 + x2) / 2;
        const cy = (y1 + y2) / 2;
        const corner = (() => {
          switch (mode.edge) {
            case "n":
              return p.x <= cx ? "nw" : "ne";
            case "s":
              return p.x <= cx ? "sw" : "se";
            case "w":
              return p.y <= cy ? "nw" : "sw";
            case "e":
              return p.y <= cy ? "ne" : "se";
          }
        })();
        setCursor(cursorForCorner(corner));
        setRect(resizeWithAspect(corner, mode.anchor, p.x, p.y, aspect.ratio));
        return;
      }

      const a = mode.anchor;
      const x1 = a.x;
      const y1 = a.y;
      const x2 = a.x + a.w;
      const y2 = a.y + a.h;

      setCursor(cursorForEdge(mode.edge));

      const next = (() => {
        switch (mode.edge) {
          case "w":
            return normalizeRect({ x: p.x, y: y1, w: x2 - p.x, h: a.h });
          case "e":
            return normalizeRect({ x: x1, y: y1, w: p.x - x1, h: a.h });
          case "n":
            return normalizeRect({ x: x1, y: p.y, w: a.w, h: y2 - p.y });
          case "s":
            return normalizeRect({ x: x1, y: y1, w: a.w, h: p.y - y1 });
        }
      })();
      setRect(next);
    }
  }

  function onPointerUp(e: PointerEvent<HTMLDivElement>) {
    if (!cropInteractive) return;
    dragRef.current = { kind: "none" };
    setCursor("crosshair");
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      // ignore
    }
  }

  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    const reportSize = () => setVideoSize({ w: v.videoWidth, h: v.videoHeight });
    const reportReady = () => onSourceReadyChange?.(v.readyState >= 1 && v.videoWidth > 0 && v.videoHeight > 0);
    const onLoaded = () => {
      reportSize();
      reportReady();
    };
    const onReset = () => onSourceReadyChange?.(false);

    v.addEventListener("loadedmetadata", onLoaded);
    v.addEventListener("loadeddata", onLoaded);
    v.addEventListener("canplay", reportReady);
    v.addEventListener("emptied", onReset);
    v.addEventListener("error", onReset);
    if (src) {
      v.load();
    }
    onLoaded();

    return () => {
      v.removeEventListener("loadedmetadata", onLoaded);
      v.removeEventListener("loadeddata", onLoaded);
      v.removeEventListener("canplay", reportReady);
      v.removeEventListener("emptied", onReset);
      v.removeEventListener("error", onReset);
      onReset();
    };
  }, [src, onSourceReadyChange]);

  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    if (!onTimeUpdate) return;

    const report = () => onTimeUpdate(v.currentTime || 0);
    v.addEventListener("timeupdate", report);
    v.addEventListener("seeked", report);
    v.addEventListener("loadedmetadata", report);
    report();

    return () => {
      v.removeEventListener("timeupdate", report);
      v.removeEventListener("seeked", report);
      v.removeEventListener("loadedmetadata", report);
    };
  }, [src, onTimeUpdate]);

  useEffect(() => {
    const v = videoRef.current;
    if (!v || !onPlaybackChange) return;

    const report = () => onPlaybackChange(!v.paused && !v.ended);
    v.addEventListener("play", report);
    v.addEventListener("pause", report);
    v.addEventListener("ended", report);
    v.addEventListener("loadedmetadata", report);
    report();

    return () => {
      v.removeEventListener("play", report);
      v.removeEventListener("pause", report);
      v.removeEventListener("ended", report);
      v.removeEventListener("loadedmetadata", report);
    };
  }, [src, onPlaybackChange]);

  useEffect(() => {
    dragRef.current = { kind: "none" };
    setCursor(cropInteractive ? "crosshair" : "default");
  }, [cropInteractive]);

  useImperativeHandle(ref, () => ({
    async play() {
      const v = videoRef.current;
      if (!v) return false;
      try {
        await v.play();
      } catch {
        // ignore
      }
      return !v.paused;
    },
    pause() {
      const v = videoRef.current;
      if (!v) return false;
      v.pause();
      return !v.paused;
    },
    async togglePlayback() {
      const v = videoRef.current;
      if (!v) return false;
      if (v.paused) {
        try {
          await v.play();
        } catch {
          // ignore
        }
      } else {
        v.pause();
      }
      return !v.paused;
    },
    seekTo(timeS: number) {
      const v = videoRef.current;
      if (!v) return;
      const duration = Number.isFinite(v.duration) ? v.duration : Number.POSITIVE_INFINITY;
      v.currentTime = clamp(timeS, 0, duration);
      onTimeUpdate?.(v.currentTime || 0);
    },
  }), [onTimeUpdate]);

  const handleStyle = (corner: "nw" | "ne" | "sw" | "se") => {
    const x = corner.includes("w") ? rectPx.left : rectPx.left + rectPx.width;
    const y = corner.includes("n") ? rectPx.top : rectPx.top + rectPx.height;
    return {
      left: x,
      top: y,
    } as const;
  };

  return (
    <div
      className="vfl-cropper"
      style={{
        aspectRatio: Number.isFinite(frameAspectRatio) && frameAspectRatio && frameAspectRatio > 0 ? frameAspectRatio : undefined,
      }}
    >
      <div className="vfl-cropper-stage" ref={containerRef}>
        <video className="vfl-video" ref={videoRef} src={src} preload="auto" playsInline />
        {cropEnabled ? (
          <div
            className="vfl-overlay"
            onPointerDown={onPointerDown}
            onPointerMove={onPointerMove}
            onPointerUp={onPointerUp}
            onPointerLeave={() => {
              if (!cropInteractive) return;
              if (dragRef.current.kind !== "none") return;
              setCursor("crosshair");
            }}
            style={{
              cursor,
              pointerEvents: cropInteractive ? "auto" : "none",
            }}
          >
            <div
              className="vfl-selection"
              style={{
                left: rectPx.left,
                top: rectPx.top,
                width: rectPx.width,
                height: rectPx.height,
              }}
            />
            {(["nw", "ne", "sw", "se"] as const).map((c) => (
              <div key={c} className={`vfl-handle vfl-handle-${c}`} style={handleStyle(c)} />
            ))}
          </div>
        ) : null}
      </div>
    </div>
  );
});
