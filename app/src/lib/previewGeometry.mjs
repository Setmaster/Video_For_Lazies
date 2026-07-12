function clamp01(value) {
  return Math.max(0, Math.min(1, Number(value) || 0));
}

export function normalizeQuarterTurn(rotationDeg) {
  const rotation = Number(rotationDeg);
  if (!Number.isFinite(rotation)) return 0;
  const normalized = ((rotation % 360) + 360) % 360;
  return normalized === 90 || normalized === 180 || normalized === 270 ? normalized : 0;
}

export function quarterTurnSwapsAxes(rotationDeg) {
  const rotation = normalizeQuarterTurn(rotationDeg);
  return rotation === 90 || rotation === 270;
}

export function rotatedAspectRatio(width, height, rotationDeg = 0) {
  const safeWidth = Number(width);
  const safeHeight = Number(height);
  if (!Number.isFinite(safeWidth) || safeWidth <= 0 || !Number.isFinite(safeHeight) || safeHeight <= 0) {
    return null;
  }
  return quarterTurnSwapsAxes(rotationDeg) ? safeHeight / safeWidth : safeWidth / safeHeight;
}

export function sourcePointToDisplayPoint(point, rotationDeg = 0) {
  const x = clamp01(point?.x);
  const y = clamp01(point?.y);
  switch (normalizeQuarterTurn(rotationDeg)) {
    case 90:
      return { x: 1 - y, y: x };
    case 180:
      return { x: 1 - x, y: 1 - y };
    case 270:
      return { x: y, y: 1 - x };
    default:
      return { x, y };
  }
}

export function displayPointToSourcePoint(point, rotationDeg = 0) {
  const x = clamp01(point?.x);
  const y = clamp01(point?.y);
  switch (normalizeQuarterTurn(rotationDeg)) {
    case 90:
      return { x: y, y: 1 - x };
    case 180:
      return { x: 1 - x, y: 1 - y };
    case 270:
      return { x: 1 - y, y: x };
    default:
      return { x, y };
  }
}

export function sourceRectToDisplayRect(rect, rotationDeg = 0) {
  const x = clamp01(rect?.x);
  const y = clamp01(rect?.y);
  const right = clamp01(x + Math.max(0, Number(rect?.w) || 0));
  const bottom = clamp01(y + Math.max(0, Number(rect?.h) || 0));
  const corners = [
    sourcePointToDisplayPoint({ x, y }, rotationDeg),
    sourcePointToDisplayPoint({ x: right, y }, rotationDeg),
    sourcePointToDisplayPoint({ x, y: bottom }, rotationDeg),
    sourcePointToDisplayPoint({ x: right, y: bottom }, rotationDeg),
  ];
  const xs = corners.map((point) => point.x);
  const ys = corners.map((point) => point.y);
  const left = Math.min(...xs);
  const top = Math.min(...ys);
  return {
    x: left,
    y: top,
    w: Math.max(...xs) - left,
    h: Math.max(...ys) - top,
  };
}
