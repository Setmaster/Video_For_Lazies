import { queuePathIdentity } from "./exportQueue.mjs";

function lastSepIndex(p) {
  return Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
}

export function splitPath(p) {
  const idx = lastSepIndex(p);
  if (idx < 0) return { dir: "", base: p };
  return { dir: p.slice(0, idx + 1), base: p.slice(idx + 1) };
}

export function basename(p) {
  return splitPath(p).base;
}

export function dirname(p) {
  return splitPath(p).dir;
}

export function extname(p) {
  const { base } = splitPath(p);
  const dot = base.lastIndexOf(".");
  if (dot <= 0) return "";
  return base.slice(dot + 1);
}

export function stem(p) {
  const { base } = splitPath(p);
  const dot = base.lastIndexOf(".");
  if (dot <= 0) return base;
  return base.slice(0, dot);
}

export function replaceExtension(p, newExt) {
  const { dir, base } = splitPath(p);
  const dot = base.lastIndexOf(".");
  const baseNoExt = dot <= 0 ? base : base.slice(0, dot);
  const ext = newExt.startsWith(".") ? newExt.slice(1) : newExt;
  return `${dir}${baseNoExt}.${ext}`;
}

function incrementSuffixNumber(s) {
  // Only treat "-N" as an incrementable suffix when N is short (1..=3 digits),
  // otherwise append "-2" (ex: "movie-2024" -> "movie-2024-2").
  const m = /^(.*?)-(\d{1,3})$/.exec(s);
  if (!m) return `${s}-2`;
  const base = m[1];
  const n = Number.parseInt(m[2], 10);
  if (!Number.isFinite(n)) return `${s}-2`;
  return `${base}-${n + 1}`;
}

export function suggestOutputPath(inputPath, formatExt) {
  const { dir } = splitPath(inputPath);
  const inputStem = stem(inputPath);
  const nextStem = incrementSuffixNumber(inputStem);
  const ext = formatExt.startsWith(".") ? formatExt.slice(1) : formatExt;
  return `${dir}${nextStem}.${ext}`;
}

export function ensureUniqueOutputPath(candidate, takenPaths, platform = "auto") {
  // Disk existence is the backend's job; this guards against paths already
  // claimed by queued export snapshots that have not been written yet.
  const taken = new Set();
  for (const path of takenPaths ?? []) {
    const identity = queuePathIdentity(path, platform);
    if (identity !== null) taken.add(identity);
  }

  let next = candidate;
  let guard = 0;
  while (taken.has(queuePathIdentity(next, platform)) && guard < 10_000) {
    const { dir } = splitPath(next);
    const bumpedStem = incrementSuffixNumber(stem(next));
    const ext = extname(next);
    next = `${dir}${bumpedStem}${ext ? `.${ext}` : ""}`;
    guard += 1;
  }
  return next;
}
