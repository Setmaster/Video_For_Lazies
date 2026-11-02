export type PreviewViewport = { x: number; y: number; width: number; height: number; visible: boolean };
type Rectangle = { left: number; top: number; right: number; bottom: number };
export function previewViewport(rect: Rectangle, clip: Rectangle, scale: number, visible: boolean): PreviewViewport;
type Placement = PreviewViewport & { previewId: string };
export function createPreviewViewportQueue(options: {
  send: (value: Placement) => Promise<unknown>;
  onFailure: (value: Placement, error: unknown) => void;
}): { offer: (value: Placement) => Promise<void> };
