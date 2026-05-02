import { extname } from "./outputPath.mjs";

export const SUPPORTED_INPUT_EXTENSIONS = ["mp4", "mov", "mkv", "avi", "webm", "m4v"];
export const DEFAULT_UNSUPPORTED_DROP_MESSAGE = "Unsupported file. Drop an mp4/mov/mkv/avi/webm/m4v.";

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

export function resolveDroppedVideoAction({ paths, currentFormat, jobId }) {
  if (jobId !== null) {
    return {
      kind: "ignore",
      clearDragActive: true,
    };
  }

  const dropped = resolveDroppedVideo(paths, currentFormat);
  if (!dropped) {
    if (!paths.length) {
      return {
        kind: "ignore",
        clearDragActive: true,
      };
    }

    return {
      kind: "status",
      clearDragActive: true,
      message: DEFAULT_UNSUPPORTED_DROP_MESSAGE,
    };
  }

  return {
    kind: "applyInput",
    clearDragActive: true,
    path: dropped.path,
    nextFormat: dropped.nextFormat,
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

  return [...new Set([...filePaths, ...parseDroppedUriList(rawUriList)])];
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
