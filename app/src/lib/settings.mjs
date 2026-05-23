const VALID_FORMATS = new Set(["mp4", "webm", "mp3"]);
const VALID_VIDEO_CODECS = new Set(["auto", "h264", "mpeg4", "vp9", "vp8"]);
const VALID_AUDIO_BITRATES = new Set([96, 128, 192, 256, 320]);
const VALID_VIDEO_QUALITIES = new Set(["auto", "smaller", "balanced", "higher"]);
const VALID_ENCODE_SPEEDS = new Set(["auto", "faster", "balanced", "smaller"]);
const VALID_FRAME_RATE_CAPS = new Set([24, 30, 60]);
const VALID_AUDIO_CHANNELS = new Set(["auto", "stereo", "mono"]);

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
      if (VALID_VIDEO_QUALITIES.has(parsed.advanced.videoQuality) && parsed.advanced.videoQuality !== "auto") {
        advanced.videoQuality = parsed.advanced.videoQuality;
      }
      if (VALID_ENCODE_SPEEDS.has(parsed.advanced.encodeSpeed) && parsed.advanced.encodeSpeed !== "auto") {
        advanced.encodeSpeed = parsed.advanced.encodeSpeed;
      }
      if (VALID_FRAME_RATE_CAPS.has(parsed.advanced.frameRateCapFps)) {
        advanced.frameRateCapFps = parsed.advanced.frameRateCapFps;
      }
      if (VALID_AUDIO_CHANNELS.has(parsed.advanced.audioChannels) && parsed.advanced.audioChannels !== "auto") {
        advanced.audioChannels = parsed.advanced.audioChannels;
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
  if (VALID_VIDEO_QUALITIES.has(settings?.advanced?.videoQuality) && settings.advanced.videoQuality !== "auto") {
    advanced.videoQuality = settings.advanced.videoQuality;
  }
  if (VALID_ENCODE_SPEEDS.has(settings?.advanced?.encodeSpeed) && settings.advanced.encodeSpeed !== "auto") {
    advanced.encodeSpeed = settings.advanced.encodeSpeed;
  }
  if (VALID_FRAME_RATE_CAPS.has(settings?.advanced?.frameRateCapFps)) {
    advanced.frameRateCapFps = settings.advanced.frameRateCapFps;
  }
  if (VALID_AUDIO_CHANNELS.has(settings?.advanced?.audioChannels) && settings.advanced.audioChannels !== "auto") {
    advanced.audioChannels = settings.advanced.audioChannels;
  }
  if (Object.keys(advanced).length > 0) {
    next.advanced = advanced;
  }
  return JSON.stringify(next);
}
