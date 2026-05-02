import type { OutputFormat } from "./types";

export const SUPPORTED_INPUT_EXTENSIONS: string[];
export const DEFAULT_UNSUPPORTED_DROP_MESSAGE: string;

export function pickDroppedVideoPath(paths: string[]): string | null;
export function inferDroppedFormat(path: string, currentFormat: OutputFormat): OutputFormat;
export function resolveDroppedVideo(
  paths: string[],
  currentFormat: OutputFormat,
): { path: string; nextFormat: OutputFormat } | null;
export function isFileDragTypeList(types: Iterable<string> | ArrayLike<string> | null | undefined): boolean;
export type DroppedVideoAction =
  | { kind: "ignore"; clearDragActive: true }
  | { kind: "status"; clearDragActive: true; message: string }
  | { kind: "applyInput"; clearDragActive: true; path: string; nextFormat: OutputFormat };
export function resolveDroppedVideoAction(args: {
  paths: string[];
  currentFormat: OutputFormat;
  jobId: number | null;
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
