export const MAX_EXPORT_QUEUE_ITEMS = 100;
export const MAX_EXPORT_QUEUE_OUTCOMES_PER_ITEM = 10;

const TERMINAL_STATUSES = new Set(["done", "failed", "cancelled"]);
const OUTCOME_KINDS = new Set(["done", "failed", "cancelled"]);

function positiveSafeInteger(value, fallback) {
  return Number.isSafeInteger(value) && value > 0 ? value : fallback;
}

function cloneJson(value) {
  if (value === null || value === undefined) return value;
  return JSON.parse(JSON.stringify(value));
}

function normalizePlatform(platform, path) {
  if (platform === "windows" || platform === "win32") return "windows";
  if (platform === "posix" || platform === "linux" || platform === "darwin") return "posix";

  const value = typeof path === "string" ? path : "";
  if (
    /^[A-Za-z]:[\\/]/.test(value) ||
    value.startsWith("\\\\") ||
    value.startsWith("//") ||
    value.includes("\\")
  ) return "windows";
  return "posix";
}

/**
 * Return a comparison key without changing the path shown to the user.
 * Windows file identities are case-insensitive and accept either slash.
 * Auto mode treats a leading // as file-URI-style UNC; callers handling a
 * POSIX double-slash path can pass an explicit POSIX platform to preserve it.
 */
export function queuePathIdentity(path, platform = "auto") {
  if (typeof path !== "string" || path.length === 0) return null;
  if (normalizePlatform(platform, path) === "windows") {
    return `windows:${path.replaceAll("\\", "/").replace(/\/{2,}/g, "/").toLowerCase()}`;
  }
  return `posix:${path}`;
}

/** Stable first-wins de-duplication for picker and drop batches. */
export function stableUniqueQueuePaths(paths, platform = "auto") {
  const uniquePaths = [];
  const seen = new Set();
  let duplicateCount = 0;
  let invalidCount = 0;

  for (const path of paths ?? []) {
    const identity = queuePathIdentity(path, platform);
    if (identity === null) {
      invalidCount += 1;
      continue;
    }
    if (seen.has(identity)) {
      duplicateCount += 1;
      continue;
    }
    seen.add(identity);
    uniquePaths.push(path);
  }

  return { paths: uniquePaths, duplicateCount, invalidCount };
}

export function createExportQueueState(options = {}) {
  const requestedMaxItems = typeof options === "number" ? options : options?.maxItems;
  return {
    items: [],
    nextItemId: 1,
    nextRunId: 1,
    autoRun: false,
    active: null,
    pathRevision: 0,
    maxItems: Math.min(
      MAX_EXPORT_QUEUE_ITEMS,
      positiveSafeInteger(requestedMaxItems, MAX_EXPORT_QUEUE_ITEMS),
    ),
  };
}

export function exportQueueRemainingCapacity(state) {
  const maxItems = Math.min(
    MAX_EXPORT_QUEUE_ITEMS,
    positiveSafeInteger(state?.maxItems, MAX_EXPORT_QUEUE_ITEMS),
  );
  const itemCount = Array.isArray(state?.items) ? state.items.length : 0;
  return Math.max(0, maxItems - itemCount);
}

export function exportQueueOutputPaths(state) {
  if (!Array.isArray(state?.items)) return [];
  return state.items
    .map((item) => item?.request?.outputPath)
    .filter((path) => typeof path === "string" && path.length > 0);
}

export function exportQueueClaimsOutputPath(state, path, platform = "auto", exceptItemId = null) {
  const identity = queuePathIdentity(path, platform);
  if (identity === null || !Array.isArray(state?.items)) return false;

  return state.items.some((item) =>
    item?.id !== exceptItemId &&
    queuePathIdentity(item?.request?.outputPath, platform) === identity
  );
}

export function summarizeExportQueue(state) {
  const counts = {
    total: 0,
    queued: 0,
    running: 0,
    done: 0,
    failed: 0,
    cancelled: 0,
  };

  for (const item of state?.items ?? []) {
    counts.total += 1;
    if (Object.hasOwn(counts, item?.status)) counts[item.status] += 1;
  }
  return counts;
}

export function getActiveExportQueueItem(state) {
  if (!state?.active || !Array.isArray(state?.items)) return null;
  return state.items.find((item) => item.id === state.active.itemId) ?? null;
}

export function getNextQueuedExportItem(state) {
  if (!Array.isArray(state?.items)) return null;
  return state.items.find((item) => item.status === "queued") ?? null;
}

function normalizeDuration(value, fallback = null) {
  if (value === null || value === undefined) return fallback;
  return Number.isFinite(value) && value >= 0 ? value : fallback;
}

function queueItemFromRequest(id, request, durationS) {
  if (!request || typeof request !== "object") return null;
  if (typeof request.inputPath !== "string" || request.inputPath.length === 0) return null;
  if (typeof request.outputPath !== "string" || request.outputPath.length === 0) return null;

  const snapshot = cloneJson(request);
  return {
    id,
    inputPath: snapshot.inputPath,
    outputPath: snapshot.outputPath,
    format: snapshot.format,
    durationS: normalizeDuration(durationS),
    request: snapshot,
    status: "queued",
    history: [],
    lastOutcome: null,
  };
}

function replaceItemRequest(item, request, durationS) {
  const replacement = queueItemFromRequest(item.id, request, normalizeDuration(durationS, item.durationS));
  if (!replacement) return null;
  return {
    ...replacement,
    history: item.history,
    lastOutcome: item.lastOutcome,
  };
}

function normalizeOutcome(outcome, runId, request) {
  if (!outcome || !OUTCOME_KINDS.has(outcome.kind)) return null;

  const outputPath = typeof outcome.outputPath === "string" && outcome.outputPath.length > 0
    ? outcome.outputPath
    : outcome.kind === "done"
      ? request.outputPath
      : null;
  const outputSizeBytes = Number.isFinite(outcome.outputSizeBytes) && outcome.outputSizeBytes >= 0
    ? outcome.outputSizeBytes
    : null;
  const completedAtMs = Number.isFinite(outcome.completedAtMs) && outcome.completedAtMs >= 0
    ? outcome.completedAtMs
    : null;

  return {
    runId,
    kind: outcome.kind,
    message: typeof outcome.message === "string" && outcome.message.length > 0 ? outcome.message : null,
    outputPath,
    outputSizeBytes,
    diagnostics: outcome.diagnostics === null || outcome.diagnostics === undefined
      ? null
      : cloneJson(outcome.diagnostics),
    completedAtMs,
  };
}

function appendOutcome(history, outcome) {
  return [...history, outcome].slice(-MAX_EXPORT_QUEUE_OUTCOMES_PER_ITEM);
}

function appendPreparedItems(state, preparedItems) {
  const remaining = exportQueueRemainingCapacity(state);
  if (remaining === 0 || !Array.isArray(preparedItems) || preparedItems.length === 0) return state;

  const nextItems = [];
  let nextItemId = state.nextItemId;
  for (const prepared of preparedItems) {
    if (nextItems.length >= remaining) break;
    const item = queueItemFromRequest(nextItemId, prepared?.request, prepared?.durationS ?? null);
    if (!item) continue;
    nextItems.push(item);
    nextItemId += 1;
  }

  if (nextItems.length === 0) return state;
  return {
    ...state,
    items: [...state.items, ...nextItems],
    nextItemId,
    pathRevision: state.pathRevision + 1,
  };
}

/**
 * Pure queue reducer. Backend attempt/job identity must accept an event before
 * the caller dispatches its matching queue settlement.
 */
export function reduceExportQueue(state, action) {
  if (!state || !action || typeof action.type !== "string") return state;

  switch (action.type) {
    case "enqueue-prepared":
      return appendPreparedItems(state, action.items);

    case "start-auto-run":
      if (state.autoRun || !state.items.some((item) => item.status === "queued")) return state;
      return { ...state, autoRun: true };

    case "stop-auto-run":
      return state.autoRun ? { ...state, autoRun: false } : state;

    case "claim-next": {
      if (!state.autoRun || state.active !== null) return state;
      const nextItem = getNextQueuedExportItem(state);
      if (!nextItem) return { ...state, autoRun: false };

      const runId = state.nextRunId;
      return {
        ...state,
        items: state.items.map((item) =>
          item.id === nextItem.id ? { ...item, status: "running" } : item
        ),
        nextRunId: runId + 1,
        active: { itemId: nextItem.id, runId },
      };
    }

    case "start-failed": {
      if (
        state.active?.itemId !== action.itemId ||
        state.active?.runId !== action.runId
      ) return state;
      const activeItem = getActiveExportQueueItem(state);
      if (!activeItem || activeItem.status !== "running") return state;

      const outcome = normalizeOutcome({
        kind: "failed",
        message: action.message,
        outputPath: action.outputPath,
        diagnostics: action.diagnostics,
        completedAtMs: action.completedAtMs,
      }, action.runId, activeItem.request);
      return {
        ...state,
        items: state.items.map((item) =>
          item.id === action.itemId
            ? {
                ...item,
                status: "failed",
                history: appendOutcome(item.history, outcome),
                lastOutcome: outcome,
              }
            : item
        ),
        active: null,
      };
    }

    case "settled": {
      if (
        state.active?.itemId !== action.itemId ||
        state.active?.runId !== action.runId
      ) return state;
      const activeItem = getActiveExportQueueItem(state);
      if (!activeItem || activeItem.status !== "running") return state;
      const outcome = normalizeOutcome(action.outcome, action.runId, activeItem.request);
      if (!outcome) return state;

      return {
        ...state,
        items: state.items.map((item) =>
          item.id === action.itemId
            ? {
                ...item,
                status: outcome.kind,
                history: appendOutcome(item.history, outcome),
                lastOutcome: outcome,
              }
            : item
        ),
        active: null,
      };
    }

    case "retry-prepared": {
      const item = state.items.find((candidate) => candidate.id === action.itemId);
      if (!item || (item.status !== "failed" && item.status !== "cancelled")) return state;
      const replacement = replaceItemRequest(
        item,
        action.request ?? item.request,
        action.durationS,
      );
      if (!replacement) return state;

      return {
        ...state,
        items: state.items.map((candidate) => candidate.id === item.id ? replacement : candidate),
        pathRevision: state.pathRevision + 1,
      };
    }

    case "duplicate-prepared": {
      const source = state.items.find((candidate) => candidate.id === action.sourceItemId);
      if (!source || source.status === "running") return state;
      return appendPreparedItems(state, [{
        request: action.request ?? source.request,
        durationS: action.durationS ?? source.durationS,
      }]);
    }

    case "remove": {
      const item = state.items.find((candidate) => candidate.id === action.itemId);
      if (!item || item.status === "running") return state;
      return {
        ...state,
        items: state.items.filter((candidate) => candidate.id !== action.itemId),
        pathRevision: state.pathRevision + 1,
      };
    }

    case "clear-terminal": {
      const items = state.items.filter((item) => !TERMINAL_STATUSES.has(item.status));
      if (items.length === state.items.length) return state;
      return { ...state, items, pathRevision: state.pathRevision + 1 };
    }

    case "reset":
      if (state.active !== null) return state;
      if (state.items.length === 0 && !state.autoRun) return state;
      return {
        ...state,
        items: [],
        autoRun: false,
        active: null,
        pathRevision: state.pathRevision + 1,
      };

    default:
      return state;
  }
}
