import type { InterpolationSettings, InterpolationOperationId, InterpolationInstallProgress, InterpolationModelId, OutputFormat } from "./types";
export const INTERPOLATION_MODELS: readonly InterpolationModelId[];
export const INTERPOLATION_TARGETS: readonly (60 | 120)[];
export function normalizeInterpolation(value: unknown): InterpolationSettings | null;
export function interpolationConflict(value: unknown, format: OutputFormat, cap: number | string | null | undefined): string | null;
export interface InterpolationInstallState {
  modelId: InterpolationModelId;
  operationId: InterpolationOperationId | null;
  state: "idle" | "starting" | InterpolationInstallProgress["state"];
  completedBytes: number;
  totalBytes: number;
  message: string | null;
  pendingEvents: InterpolationInstallProgress[];
}
export function createInterpolationInstall(modelId: InterpolationModelId): InterpolationInstallState;
export function beginInterpolationInstall(modelId: InterpolationModelId): InterpolationInstallState;
export function reduceInterpolationInstall(previous: InterpolationInstallState, event: InterpolationInstallProgress): InterpolationInstallState;
export function bindInterpolationInstall(previous: InterpolationInstallState, operationId: InterpolationOperationId): InterpolationInstallState;
export function interpolationInstallActive(state: InterpolationInstallState): boolean;

export function interpolationPublicMessage(value: unknown, fallback: string): string;
