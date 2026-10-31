import type { PreviewFrame } from "./recordingPreviewState";
export function createPreviewPollCadence(interval?: number): (started: number, finished: number) => number;
type Options = {
  isCurrent: (frame: PreviewFrame) => boolean;
  onFirstFrame: (frame: PreviewFrame) => void;
  onError: (failure: unknown) => void;
  decode?: (bytes: PreviewFrame["bmp"]) => Promise<ImageBitmap>;
  requestFrame?: (callback: FrameRequestCallback) => number;
  cancelFrame?: (handle: number) => void;
  now?: () => number;
};
export function createRecordingPreviewRenderer(options: Options): {
  clear: () => void;
  setCanvas: (canvas: HTMLCanvasElement | null) => void;
  afterSequence: () => number;
  offer: (frame: PreviewFrame) => Promise<boolean>;
};
