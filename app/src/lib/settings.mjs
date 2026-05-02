const VALID_FORMATS = new Set(["mp4", "webm", "mp3"]);

export function parsePersistedSettings(raw) {
  if (!raw) return {};

  try {
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return {};

    const next = {};
    if (VALID_FORMATS.has(parsed.format)) {
      next.format = parsed.format;
    }
    return next;
  } catch {
    return {};
  }
}

export function serializePersistedSettings(settings) {
  const next = {};
  if (VALID_FORMATS.has(settings?.format)) {
    next.format = settings.format;
  }
  return JSON.stringify(next);
}
