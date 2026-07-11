const DEFAULT_MIN_CROP_PX = 2;

function clamp(value, min, max) {
  return Math.max(min, Math.min(max, value));
}

function finiteFrameDimension(value) {
  const parsed = Math.floor(Number(value));
  return Number.isFinite(parsed) && parsed > 0 ? parsed : 1;
}

export function cropRectToPixels(rect, frameWidth, frameHeight) {
  const width = finiteFrameDimension(frameWidth);
  const height = finiteFrameDimension(frameHeight);
  const x = clamp(Math.round(Number(rect?.x ?? 0) * width), 0, Math.max(0, width - 1));
  const y = clamp(Math.round(Number(rect?.y ?? 0) * height), 0, Math.max(0, height - 1));
  const cropWidth = clamp(Math.round(Number(rect?.w ?? 1) * width), 1, width - x);
  const cropHeight = clamp(Math.round(Number(rect?.h ?? 1) * height), 1, height - y);

  return { x, y, width: cropWidth, height: cropHeight };
}

export function pixelAspectToNormalizedRatio(pixelAspectRatio, contentWidth, contentHeight) {
  const ratio = Number(pixelAspectRatio);
  const width = Number(contentWidth);
  const height = Number(contentHeight);
  if (!Number.isFinite(ratio) || ratio <= 0 || !Number.isFinite(width) || width <= 0 || !Number.isFinite(height) || height <= 0) {
    return null;
  }
  return ratio * (height / width);
}

export function updateCropRectFromPixelField(
  rect,
  field,
  rawValue,
  frameWidth,
  frameHeight,
  options = {},
) {
  const width = finiteFrameDimension(frameWidth);
  const height = finiteFrameDimension(frameHeight);
  const parsed = Math.round(Number(rawValue));
  if (!Number.isFinite(parsed)) return rect;

  const minCropPx = clamp(
    Math.floor(Number(options.minCropPx ?? DEFAULT_MIN_CROP_PX)),
    1,
    Math.min(width, height),
  );
  const next = cropRectToPixels(rect, width, height);

  if (field === "x") {
    next.x = clamp(parsed, 0, Math.max(0, width - next.width));
  } else if (field === "y") {
    next.y = clamp(parsed, 0, Math.max(0, height - next.height));
  } else if (field === "width") {
    next.width = clamp(parsed, Math.min(minCropPx, width - next.x), width - next.x);
  } else if (field === "height") {
    next.height = clamp(parsed, Math.min(minCropPx, height - next.y), height - next.y);
  } else {
    return rect;
  }

  const aspectRatio = Number(options.aspectRatio);
  if (Number.isFinite(aspectRatio) && aspectRatio > 0 && (field === "width" || field === "height")) {
    if (field === "width") {
      next.height = Math.max(minCropPx, Math.round(next.width / aspectRatio));
      if (next.height > height - next.y) {
        next.height = height - next.y;
        next.width = Math.max(minCropPx, Math.round(next.height * aspectRatio));
      }
    } else {
      next.width = Math.max(minCropPx, Math.round(next.height * aspectRatio));
      if (next.width > width - next.x) {
        next.width = width - next.x;
        next.height = Math.max(minCropPx, Math.round(next.width / aspectRatio));
      }
    }
    next.width = clamp(next.width, Math.min(minCropPx, width - next.x), width - next.x);
    next.height = clamp(next.height, Math.min(minCropPx, height - next.y), height - next.y);
  }

  return {
    x: next.x / width,
    y: next.y / height,
    w: next.width / width,
    h: next.height / height,
  };
}

export function resolveTrimSliderKey({ key, shiftKey = false, value, min, max, fineStep = 0.1, coarseStep = 1 }) {
  const safeMin = Math.min(Number(min), Number(max));
  const safeMax = Math.max(Number(min), Number(max));
  const current = clamp(Number(value), safeMin, safeMax);
  const fine = Math.max(0, Number(fineStep));
  const coarse = Math.max(fine, Number(coarseStep));

  if (key === "Home") return safeMin;
  if (key === "End") return safeMax;

  const step = shiftKey || key === "PageUp" || key === "PageDown" ? coarse : fine;
  if (key === "ArrowLeft" || key === "ArrowDown" || key === "PageDown") {
    return clamp(current - step, safeMin, safeMax);
  }
  if (key === "ArrowRight" || key === "ArrowUp" || key === "PageUp") {
    return clamp(current + step, safeMin, safeMax);
  }
  return null;
}
