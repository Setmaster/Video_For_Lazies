// Native child surfaces cannot be clipped by CSS. Hide an obscured or partially
// scrolled surface rather than drawing over the app's controls.
export function previewViewport(rect, clip, scale, visible) {
  const hidden = { x: 0, y: 0, width: 0, height: 0, visible: false };
  if (!visible || !Number.isFinite(scale) || scale <= 0 || scale > 8 ||
      ![rect.left, rect.top, rect.right, rect.bottom, clip.left, clip.top, clip.right, clip.bottom].every(Number.isFinite) ||
      rect.left < clip.left || rect.top < clip.top || rect.right > clip.right || rect.bottom > clip.bottom) return hidden;
  // Leave the existing card border visible, rounding inward in physical pixels.
  const x = Math.ceil((rect.left + 1) * scale), y = Math.ceil((rect.top + 1) * scale);
  const width = Math.floor((rect.right - 1) * scale) - x;
  const height = Math.floor((rect.bottom - 1) * scale) - y;
  if (x < 0 || y < 0 || width < 2 || height < 2 || x + width > 32767 || y + height > 32767) return hidden;
  return { x, y, width, height, visible: true };
}

// Native placement is ordered independently of capture Stop. Keep one request
// in flight and the latest pending placement, including a final Hide on error.
export function createPreviewViewportQueue({ send, onFailure }) {
  let pending = null, busy = false, last = "", flight = Promise.resolve();
  const offer = (value) => {
    const key = JSON.stringify(value);
    if (key === last) return flight;
    last = key;
    pending = value;
    if (busy) return flight;
    busy = true;
    flight = (async () => {
      try {
        while (pending) {
          const next = pending;
          pending = null;
          try { await send(next); }
          catch (failure) {
            last = "";
            onFailure(next, failure);
          }
        }
      } finally { busy = false; }
    })();
    return flight;
  };
  return { offer };
}
