export type NormalizedCropRect = { x: number; y: number; w: number; h: number };
export type CropPixelRect = { x: number; y: number; width: number; height: number };
export type CropPixelField = keyof CropPixelRect;

export function cropRectToPixels(
  rect: NormalizedCropRect,
  frameWidth: number,
  frameHeight: number,
): CropPixelRect;

export function isFullFramePixelCrop(
  rect: CropPixelRect | null | undefined,
  frameWidth: number,
  frameHeight: number,
): boolean;

export function alignCropRectForEncoding(rect: CropPixelRect): CropPixelRect;

export function pixelAspectToNormalizedRatio(
  pixelAspectRatio: number,
  contentWidth: number,
  contentHeight: number,
): number | null;

export function updateCropRectFromPixelField(
  rect: NormalizedCropRect,
  field: CropPixelField,
  rawValue: string | number,
  frameWidth: number,
  frameHeight: number,
  options?: { minCropPx?: number; aspectRatio?: number | null },
): NormalizedCropRect;

export function resolveTrimSliderKey(options: {
  key: string;
  shiftKey?: boolean;
  value: number;
  min: number;
  max: number;
  fineStep?: number;
  coarseStep?: number;
}): number | null;
