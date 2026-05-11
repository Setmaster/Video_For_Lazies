const VALID_FORMATS = new Set(["mp4", "webm", "mp3"]);
const VALID_VIDEO_CODECS = new Set(["auto", "h264", "mpeg4", "vp9", "vp8"]);
const VALID_AUDIO_BITRATES = new Set([96, 128, 192, 256, 320]);

export function parsePersistedSettings(raw) {
  if (!raw) return {};

  try {
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return {};

    const next = {};
    if (VALID_FORMATS.has(parsed.format)) {
      next.format = parsed.format;
    }
    if (parsed.advanced && typeof parsed.advanced === "object") {
      const advanced = {};
      if (VALID_VIDEO_CODECS.has(parsed.advanced.videoCodec) && parsed.advanced.videoCodec !== "auto") {
        advanced.videoCodec = parsed.advanced.videoCodec;
      }
      if (VALID_AUDIO_BITRATES.has(parsed.advanced.audioBitrateKbps)) {
        advanced.audioBitrateKbps = parsed.advanced.audioBitrateKbps;
      }
      if (Object.keys(advanced).length > 0) {
        next.advanced = advanced;
      }
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
  const advanced = {};
  if (VALID_VIDEO_CODECS.has(settings?.advanced?.videoCodec) && settings.advanced.videoCodec !== "auto") {
    advanced.videoCodec = settings.advanced.videoCodec;
  }
  if (VALID_AUDIO_BITRATES.has(settings?.advanced?.audioBitrateKbps)) {
    advanced.audioBitrateKbps = settings.advanced.audioBitrateKbps;
  }
  if (Object.keys(advanced).length > 0) {
    next.advanced = advanced;
  }
  return JSON.stringify(next);
}
