export type QuarterTurn = 0 | 90 | 180 | 270;
export type NormalizedPoint = { x: number; y: number };
export type NormalizedRect = { x: number; y: number; w: number; h: number };

export function normalizeQuarterTurn(rotationDeg: number): QuarterTurn;
export function quarterTurnSwapsAxes(rotationDeg: number): boolean;
export function rotatedAspectRatio(width: number, height: number, rotationDeg?: number): number | null;
export function sourcePointToDisplayPoint(
  point: NormalizedPoint,
  rotationDeg?: number,
): NormalizedPoint;
export function displayPointToSourcePoint(
  point: NormalizedPoint,
  rotationDeg?: number,
): NormalizedPoint;
export function sourceRectToDisplayRect(
  rect: NormalizedRect,
  rotationDeg?: number,
): NormalizedRect;
