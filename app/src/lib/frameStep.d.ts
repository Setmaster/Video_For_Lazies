export type FrameTarget = "preview" | "start" | "end";
export interface FrameStepRequest { path: string; sourceIdentity: string; videoStreamIndex: number; timeS: number; step: number; minS: number; maxS: number; durationS: number }
export interface FrameStepResult { sourceIdentity: string; timeS: number; frameEndS: number; didMove: boolean; atLimit: boolean }
export interface FrameStepStatus { pending: boolean; error: boolean; message: string | null }
export interface FrameStepContext {
  key: string; enabled: boolean; blocked: boolean; activeTarget: FrameTarget;
  path: string; sourceIdentity: string; videoStreamIndex: number; durationS: number;
  values: Record<FrameTarget, number>;
  limits: Record<FrameTarget, { min: number; max: number }>;
}
export function createFrameStepper(options: {
  read: () => FrameStepContext | null;
  fetchStep: (request: FrameStepRequest) => Promise<FrameStepResult>;
  pause: () => void; seek: (timeS: number, endS: number) => Promise<void>;
  apply: (target: FrameTarget, timeS: number) => void;
  status: (status: FrameStepStatus) => void; cancelSeek?: () => void;
}): { enqueue: (target: FrameTarget, step: number) => boolean; reset: () => void };
export function frameDisplaySeek(timeS: number, endS: number): { seekS: number; toleranceS: number };
