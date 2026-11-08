export interface VerifiedFrameProof {
  video: HTMLVideoElement; sourceKey: string;
  logicalS: number; endS: number; clockToleranceS?: number; mediaS: number; presentedMediaS: number; presentedFrames?: number;
  isReusable: () => boolean; dispose: () => void;
}
export function takeFrameProof(proof: VerifiedFrameProof | null): VerifiedFrameProof | null;
export function beginVerifiedFrameSeek(options: {
  video: HTMLVideoElement; sourceKey: string;
  timeS: number; endS: number; timelineOffsetS: number;
  isCurrent: () => boolean;
  previousProof?: VerifiedFrameProof | null;
  onPosition: (mediaS: number) => void;
  onInterrupted?: () => void;
  setTimer?: (callback: () => void, delay: number) => ReturnType<typeof setTimeout>;
  clearTimer?: (timer: ReturnType<typeof setTimeout>) => void;
}): { promise: Promise<VerifiedFrameProof>; cancel: () => void };
