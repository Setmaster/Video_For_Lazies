// Maps the export color adjustments onto a CSS filter for the live preview.
//
// ffmpeg's eq filter and CSS filters do not share exact math: eq brightness is
// additive on luma while CSS brightness() is multiplicative, so the preview is
// an approximation meant to show direction and rough magnitude rather than the
// final encoded pixels. Contrast and saturation pivot the same way in both.

function clampNumber(value, min, max) {
  return Math.max(min, Math.min(max, value));
}

function parseAdjustment(raw, fallback) {
  if (raw === null || raw === undefined) return fallback;
  const text = String(raw).trim();
  if (text === "") return fallback;
  const parsed = Number(text);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function formatFilterValue(value) {
  return Number(value.toFixed(6)).toString();
}

export function buildPreviewColorFilter(brightness, contrast, saturation) {
  const b = parseAdjustment(brightness, 0);
  const c = parseAdjustment(contrast, 1);
  const s = parseAdjustment(saturation, 1);

  const parts = [];
  if (Math.abs(b) > 1e-9) {
    parts.push(`brightness(${formatFilterValue(clampNumber(1 + b, 0, 2))})`);
  }
  if (Math.abs(c - 1) > 1e-9) {
    parts.push(`contrast(${formatFilterValue(clampNumber(c, 0, 2))})`);
  }
  if (Math.abs(s - 1) > 1e-9) {
    parts.push(`saturate(${formatFilterValue(clampNumber(s, 0, 3))})`);
  }
  return parts.length ? parts.join(" ") : null;
}
