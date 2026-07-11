import type { EncodeRequest, ExportDiagnostics, OutputFormat } from "./types";

export type QueuePathPlatform = "auto" | "windows" | "win32" | "posix" | "linux" | "darwin";
export type ExportQueueItemStatus = "queued" | "running" | "done" | "failed" | "cancelled";
export type ExportQueueOutcomeKind = "done" | "failed" | "cancelled";

export interface ExportQueueOutcome {
  runId: number;
  kind: ExportQueueOutcomeKind;
  message: string | null;
  outputPath: string | null;
  outputSizeBytes: number | null;
  diagnostics: ExportDiagnostics | null;
  completedAtMs: number | null;
}

export interface ExportQueueItem {
  id: number;
  inputPath: string;
  outputPath: string;
  format: OutputFormat;
  durationS: number | null;
  request: EncodeRequest;
  status: ExportQueueItemStatus;
  history: ExportQueueOutcome[];
  lastOutcome: ExportQueueOutcome | null;
}

export interface ExportQueueState {
  items: ExportQueueItem[];
  nextItemId: number;
  nextRunId: number;
  autoRun: boolean;
  active: { itemId: number; runId: number } | null;
  pathRevision: number;
  maxItems: number;
}

export interface PreparedExportQueueItem {
  request: EncodeRequest;
  durationS?: number | null;
}

export interface ExportQueueOutcomeInput {
  kind: ExportQueueOutcomeKind;
  message?: string | null;
  outputPath?: string | null;
  outputSizeBytes?: number | null;
  diagnostics?: ExportDiagnostics | null;
  completedAtMs?: number | null;
}

export type ExportQueueAction =
  | { type: "enqueue-prepared"; items: PreparedExportQueueItem[] }
  | { type: "start-auto-run" }
  | { type: "stop-auto-run" }
  | { type: "claim-next" }
  | {
      type: "start-failed";
      itemId: number;
      runId: number;
      message?: string | null;
      outputPath?: string | null;
      diagnostics?: ExportDiagnostics | null;
      completedAtMs?: number | null;
    }
  | { type: "settled"; itemId: number; runId: number; outcome: ExportQueueOutcomeInput }
  | { type: "retry-prepared"; itemId: number; request?: EncodeRequest; durationS?: number | null }
  | { type: "duplicate-prepared"; sourceItemId: number; request?: EncodeRequest; durationS?: number | null }
  | { type: "remove"; itemId: number }
  | { type: "clear-terminal" }
  | { type: "reset" };

export interface ExportQueueCounts {
  total: number;
  queued: number;
  running: number;
  done: number;
  failed: number;
  cancelled: number;
}

export const MAX_EXPORT_QUEUE_ITEMS: 100;
export const MAX_EXPORT_QUEUE_OUTCOMES_PER_ITEM: 10;
export function queuePathIdentity(path: string, platform?: QueuePathPlatform): string | null;
export function stableUniqueQueuePaths(
  paths: Iterable<unknown> | null | undefined,
  platform?: QueuePathPlatform,
): { paths: string[]; duplicateCount: number; invalidCount: number };
export function createExportQueueState(options?: number | { maxItems?: number }): ExportQueueState;
export function exportQueueRemainingCapacity(state: ExportQueueState): number;
export function exportQueueOutputPaths(state: ExportQueueState): string[];
export function exportQueueClaimsOutputPath(
  state: ExportQueueState,
  path: string,
  platform?: QueuePathPlatform,
  exceptItemId?: number | null,
): boolean;
export function summarizeExportQueue(state: ExportQueueState): ExportQueueCounts;
export function getActiveExportQueueItem(state: ExportQueueState): ExportQueueItem | null;
export function getNextQueuedExportItem(state: ExportQueueState): ExportQueueItem | null;
export function reduceExportQueue(state: ExportQueueState, action: ExportQueueAction): ExportQueueState;
