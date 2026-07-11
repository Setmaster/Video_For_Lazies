import { extname } from "./outputPath.mjs";
import {
  MAX_EXPORT_QUEUE_ITEMS,
  stableUniqueQueuePaths,
} from "./exportQueue.mjs";

export const SUPPORTED_INPUT_EXTENSIONS = ["mp4", "mov", "mkv", "avi", "webm", "m4v"];
export const DEFAULT_UNSUPPORTED_DROP_MESSAGE = "Unsupported file. Drop an mp4/mov/mkv/avi/webm/m4v.";
export const DEFAULT_QUEUE_FULL_DROP_MESSAGE = "The export queue is full. Clear finished items before adding more files.";

export function pickDroppedVideoPath(paths) {
  return paths.find((path) => SUPPORTED_INPUT_EXTENSIONS.includes(extname(path).toLowerCase())) ?? null;
}

export function inferDroppedFormat(path, currentFormat) {
  const droppedExt = extname(path).toLowerCase();
  return droppedExt === "mp4" || droppedExt === "webm" ? droppedExt : currentFormat;
}

export function resolveDroppedVideo(paths, currentFormat) {
  const firstSupported = pickDroppedVideoPath(paths);
  if (!firstSupported) return null;
  return {
    path: firstSupported,
    nextFormat: inferDroppedFormat(firstSupported, currentFormat),
  };
}

export function isFileDragTypeList(types) {
  return Array.from(types ?? []).includes("Files");
}

export function classifyDroppedVideoPaths(paths, { platform = "auto" } = {}) {
  const unique = stableUniqueQueuePaths(paths, platform);
  const supportedPaths = [];
  const unsupportedPaths = [];

  for (const path of unique.paths) {
    if (SUPPORTED_INPUT_EXTENSIONS.includes(extname(path).toLowerCase())) {
      supportedPaths.push(path);
    } else {
      unsupportedPaths.push(path);
    }
  }

  return {
    supportedPaths,
    unsupportedPaths,
    unsupportedCount: unsupportedPaths.length + unique.invalidCount,
    duplicateCount: unique.duplicateCount,
    invalidCount: unique.invalidCount,
  };
}

function normalizeQueueCapacity(value) {
  if (value === undefined || value === null) return MAX_EXPORT_QUEUE_ITEMS;
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(MAX_EXPORT_QUEUE_ITEMS, Math.floor(value)));
}

export function resolveDroppedVideoAction({
  paths,
  currentFormat,
  jobId = null,
  busy,
  queueCapacity,
  platform = "auto",
}) {
  const classified = classifyDroppedVideoPaths(paths, { platform });
  const {
    supportedPaths,
    unsupportedCount,
    duplicateCount,
    invalidCount,
  } = classified;
  const isBusy = typeof busy === "boolean" ? busy : jobId !== null;
  const base = {
    clearDragActive: true,
    unsupportedCount,
    duplicateCount,
    invalidCount,
    overflowCount: 0,
  };

  if (supportedPaths.length === 0) {
    if (unsupportedCount === 0 && duplicateCount === 0) {
      return { kind: "ignore", ...base };
    }

    return {
      kind: "status",
      ...base,
      message: DEFAULT_UNSUPPORTED_DROP_MESSAGE,
    };
  }

  if (!isBusy && supportedPaths.length === 1) {
    const path = supportedPaths[0];
    return {
      kind: "applyInput",
      ...base,
      path,
      nextFormat: inferDroppedFormat(path, currentFormat),
    };
  }

  const capacity = normalizeQueueCapacity(queueCapacity);
  const acceptedPaths = supportedPaths.slice(0, capacity);
  const overflowCount = supportedPaths.length - acceptedPaths.length;
  if (acceptedPaths.length === 0) {
    return {
      kind: "status",
      ...base,
      overflowCount,
      message: DEFAULT_QUEUE_FULL_DROP_MESSAGE,
    };
  }

  return {
    kind: "queueInputs",
    ...base,
    paths: acceptedPaths,
    format: currentFormat,
    reason: isBusy ? "busy" : "multiple",
    overflowCount,
  };
}

function fileUriToPath(uri) {
  try {
    const parsed = new URL(uri);
    if (parsed.protocol !== "file:") return null;

    const decodedPath = decodeURIComponent(parsed.pathname);
    if (parsed.host) return `//${parsed.host}${decodedPath}`;
    if (/^\/[A-Za-z]:/.test(decodedPath)) return decodedPath.slice(1);
    return decodedPath;
  } catch {
    return null;
  }
}

export function parseDroppedUriList(raw) {
  if (!raw) return [];

  return raw
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("#"))
    .map(fileUriToPath)
    .filter((value) => typeof value === "string" && value.length > 0);
}

export function extractDroppedPathsFromDataTransfer(dataTransfer) {
  if (!dataTransfer) return [];

  const filePaths = Array.from(dataTransfer.files ?? [])
    .map((file) => (file && typeof file.path === "string" ? file.path : ""))
    .filter(Boolean);

  const rawUriList = typeof dataTransfer.getData === "function"
    ? dataTransfer.getData("text/uri-list") || dataTransfer.getData("text/plain")
    : "";

  return stableUniqueQueuePaths(
    [...filePaths, ...parseDroppedUriList(rawUriList)],
  ).paths;
}

export function bindWindowFileDrop(target, { isDropAllowed, onDragActiveChange, onPathsDropped }) {
  const canDrop = () => (typeof isDropAllowed === "function" ? isDropAllowed() : true);
  const setDragActive = (active) => {
    if (typeof onDragActiveChange === "function") onDragActiveChange(active);
  };
  const isFileDrag = (event) => isFileDragTypeList(event?.dataTransfer?.types);

  const handleDragEnter = (event) => {
    event.preventDefault();
    if (isFileDrag(event) && canDrop()) {
      setDragActive(true);
    }
  };

  const handleDragLeave = (event) => {
    if (!isFileDrag(event)) return;
    if (event.relatedTarget != null) return;
    setDragActive(false);
  };

  const handleDrop = (event) => {
    event.preventDefault();
    setDragActive(false);
    if (!canDrop()) return;
    if (typeof onPathsDropped === "function") {
      onPathsDropped(extractDroppedPathsFromDataTransfer(event.dataTransfer));
    }
  };

  target.addEventListener("dragenter", handleDragEnter);
  target.addEventListener("dragover", handleDragEnter);
  target.addEventListener("dragleave", handleDragLeave);
  target.addEventListener("drop", handleDrop);

  return () => {
    target.removeEventListener("dragenter", handleDragEnter);
    target.removeEventListener("dragover", handleDragEnter);
    target.removeEventListener("dragleave", handleDragLeave);
    target.removeEventListener("drop", handleDrop);
  };
}
