import type { OutputFormat } from "./types";
import type { QueuePathPlatform } from "./exportQueue";

export const SUPPORTED_INPUT_EXTENSIONS: string[];
export const DEFAULT_UNSUPPORTED_DROP_MESSAGE: string;
export const DEFAULT_QUEUE_FULL_DROP_MESSAGE: string;

export function pickDroppedVideoPath(paths: string[]): string | null;
export function inferDroppedFormat(path: string, currentFormat: OutputFormat): OutputFormat;
export function resolveDroppedVideo(
  paths: string[],
  currentFormat: OutputFormat,
): { path: string; nextFormat: OutputFormat } | null;
export function isFileDragTypeList(types: Iterable<string> | ArrayLike<string> | null | undefined): boolean;
export interface DroppedVideoPathClassification {
  supportedPaths: string[];
  unsupportedPaths: string[];
  unsupportedCount: number;
  duplicateCount: number;
  invalidCount: number;
}
export interface DroppedVideoActionCounts {
  clearDragActive: true;
  unsupportedCount: number;
  duplicateCount: number;
  invalidCount: number;
  overflowCount: number;
}
export type DroppedVideoAction =
  | ({ kind: "ignore" } & DroppedVideoActionCounts)
  | ({ kind: "status"; message: string } & DroppedVideoActionCounts)
  | ({ kind: "applyInput"; path: string; nextFormat: OutputFormat } & DroppedVideoActionCounts)
  | ({
      kind: "queueInputs";
      paths: string[];
      format: OutputFormat;
      reason: "busy" | "multiple";
    } & DroppedVideoActionCounts);
export function classifyDroppedVideoPaths(
  paths: Iterable<unknown> | null | undefined,
  options?: { platform?: QueuePathPlatform },
): DroppedVideoPathClassification;
export function resolveDroppedVideoAction(args: {
  paths: string[];
  currentFormat: OutputFormat;
  jobId?: number | null;
  busy?: boolean;
  queueCapacity?: number;
  platform?: QueuePathPlatform;
}): DroppedVideoAction;
export function parseDroppedUriList(raw: string): string[];
export function extractDroppedPathsFromDataTransfer(dataTransfer: {
  files?: FileList | ArrayLike<File | { path?: string | null }> | null;
  getData?: ((type: string) => string) | null;
} | null | undefined): string[];
export function bindWindowFileDrop(
  target: {
    addEventListener: (type: string, listener: (event: {
      dataTransfer?: {
        files?: FileList | ArrayLike<File | { path?: string | null }> | null;
        getData?: ((type: string) => string) | null;
        types?: ArrayLike<string> | null;
      } | null;
      relatedTarget?: unknown;
      preventDefault: () => void;
    }) => void) => void;
    removeEventListener: (type: string, listener: (event: {
      dataTransfer?: {
        files?: FileList | ArrayLike<File | { path?: string | null }> | null;
        getData?: ((type: string) => string) | null;
        types?: ArrayLike<string> | null;
      } | null;
      relatedTarget?: unknown;
      preventDefault: () => void;
    }) => void) => void;
  },
  options: {
    isDropAllowed?: (() => boolean) | null;
    onDragActiveChange?: ((active: boolean) => void) | null;
    onPathsDropped?: ((paths: string[]) => void) | null;
  },
): () => void;
