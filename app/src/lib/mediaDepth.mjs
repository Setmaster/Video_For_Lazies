export const TRANSFORM_MEMORY_WARN_BYTES = 512 * 1024 * 1024;
export const TRANSFORM_MEMORY_BLOCK_BYTES = 2 * 1024 * 1024 * 1024;
export const TRANSFORM_MEMORY_OVERHEAD = 1.5;
export const TRANSFORM_VIDEO_FRAME_OVERHEAD_BYTES = 8 * 1024;
export const TRANSFORM_AUDIO_FRAME_OVERHEAD_BYTES = 8 * 1024;
export const TRANSFORM_AUDIO_FRAME_SAMPLES = 1024;

function finitePositive(value) {
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? number : null;
}

function clamp(value, min, max) {
  return Math.max(min, Math.min(max, value));
}

function evenAtLeastTwo(value) {
  return Math.max(2, Math.floor(Number(value) / 2) * 2);
}

export function hasNonSquarePixels(probe) {
  const numerator = finitePositive(probe?.sampleAspectRatio?.numerator);
  const denominator = finitePositive(probe?.sampleAspectRatio?.denominator);
  if (numerator === null || denominator === null) return false;
  return numerator !== denominator;
}

export function effectiveSampleAspectRatio(probe, manualRotateDeg = 0) {
  const numerator = finitePositive(probe?.sampleAspectRatio?.numerator) ?? 1;
  const denominator = finitePositive(probe?.sampleAspectRatio?.denominator) ?? 1;
  let ratio = numerator / denominator;
  const sourceRotation = Math.abs(Number(probe?.rotationDeg ?? 0)) % 180;
  if (sourceRotation === 90) ratio = 1 / ratio;
  if (Math.abs(Number(manualRotateDeg ?? 0)) % 180 === 90) ratio = 1 / ratio;
  return ratio;
}

export function squarePixelDimensions({ probe, width, height, manualRotateDeg = 0 }) {
  const safeWidth = Math.max(2, Math.floor(Number(width) || 0));
  const safeHeight = Math.max(2, Math.floor(Number(height) || 0));
  const ratio = effectiveSampleAspectRatio(probe, manualRotateDeg);
  const normalizedWidth = evenAtLeastTwo(Math.round(safeWidth * ratio));
  const normalizedHeight = evenAtLeastTwo(safeHeight);
  return { width: normalizedWidth, height: normalizedHeight };
}

export function fitMaxEdgeDimensions(width, height, maxEdge) {
  const safeWidth = Math.max(2, Number(width));
  const safeHeight = Math.max(2, Number(height));
  const safeMaxEdge = Number(maxEdge);
  const longEdge = Math.max(safeWidth, safeHeight);
  if (!Number.isFinite(safeMaxEdge) || safeMaxEdge <= 0 || longEdge <= safeMaxEdge) {
    return { width: evenAtLeastTwo(safeWidth), height: evenAtLeastTwo(safeHeight) };
  }
  const scale = safeMaxEdge / longEdge;
  return {
    width: evenAtLeastTwo(Math.round(safeWidth * scale)),
    height: evenAtLeastTwo(Math.round(safeHeight * scale)),
  };
}

export function fitMaxEdgeDisplayDimensions({ probe, width, height, maxEdge, manualRotateDeg = 0 }) {
  const sourceWidth = finitePositive(width);
  const sourceHeight = finitePositive(height);
  const safeMaxEdge = finitePositive(maxEdge);
  if (sourceWidth === null || sourceHeight === null || safeMaxEdge === null) {
    return { width: 2, height: 2 };
  }
  const logicalWidth = sourceWidth * effectiveSampleAspectRatio(probe, manualRotateDeg);
  const logicalHeight = sourceHeight;
  const longEdge = Math.max(logicalWidth, logicalHeight);
  const scale = longEdge > safeMaxEdge ? safeMaxEdge / longEdge : 1;
  return {
    width: evenAtLeastTwo(Math.round(logicalWidth * scale)),
    height: evenAtLeastTwo(Math.round(logicalHeight * scale)),
  };
}

export function sourceRotationBlockingReason(probe) {
  const rawRotation = probe?.unsupportedRotationDeg;
  if (rawRotation === null || rawRotation === undefined) return null;
  const rotation = Number(rawRotation);
  if (!Number.isFinite(rotation)) return null;
  return `The source uses a ${rotation.toFixed(3)}° display rotation. Video export, crop detection, and frame export support only exact quarter-turn source rotations.`;
}

export function plannedDimensionBlockingReason(width, height, maxDimension = 32_768) {
  const safeWidth = finitePositive(width);
  const safeHeight = finitePositive(height);
  const safeLimit = finitePositive(maxDimension);
  if (safeWidth === null || safeHeight === null || safeLimit === null) return null;
  if (safeWidth <= safeLimit && safeHeight <= safeLimit) return null;
  return `Square-pixel source dimensions exceed the ${Math.round(safeLimit)} px per-dimension safety limit. Choose a bounded resize or correct the source aspect metadata.`;
}

const VIDEO_CODEC_DIMENSION_LIMITS = Object.freeze({
  h264: 16_384,
  mpeg4: 8_190,
  vp8: 16_382,
  vp9: 32_768,
});

export function videoCodecDimensionLimit(codec) {
  return VIDEO_CODEC_DIMENSION_LIMITS[String(codec ?? "").toLowerCase()] ?? 32_768;
}

export function codecOutputDimensionBlockingReason(codec, width, height) {
  const safeWidth = finitePositive(width);
  const safeHeight = finitePositive(height);
  if (safeWidth === null || safeHeight === null) return null;
  const normalizedCodec = String(codec ?? "video").toLowerCase();
  const limit = videoCodecDimensionLimit(normalizedCodec);
  if (safeWidth <= limit && safeHeight <= limit) return null;
  return `${normalizedCodec.toUpperCase()} output supports at most ${limit} pixels per dimension; planned output is ${Math.round(safeWidth)}x${Math.round(safeHeight)}. Choose a bounded resize or another codec.`;
}

export function trimRequestIsActive({ startS = 0, endS = null, durationS = null }) {
  const start = Number(startS);
  const end = endS === null || endS === undefined ? null : Number(endS);
  const duration = finitePositive(durationS);
  const activeStart = Number.isFinite(start) && start > 0;
  const activeEnd = end !== null && Number.isFinite(end) && end > 0 &&
    (duration === null || end < duration);
  return activeStart || activeEnd;
}

export function encodedOutputDimensions(width, height, requiresEncode) {
  const sourceWidth = Math.max(1, Math.floor(Number(width) || 0));
  const sourceHeight = Math.max(1, Math.floor(Number(height) || 0));
  if (!requiresEncode) return { width: sourceWidth, height: sourceHeight };
  return {
    width: evenAtLeastTwo(sourceWidth),
    height: evenAtLeastTwo(sourceHeight),
  };
}

export function timelineSpeedChanges(value) {
  const speed = Number(value);
  return Number.isFinite(speed) && Math.abs(speed - 1) > 1e-9;
}

export function minimumVideoBitrateKbps({ codec, width, height, sourceFrameRate = null, speed = 1, frameRateCapFps = null }) {
  if (codec !== "mpeg4") return 50;
  const sourceFps = finitePositive(sourceFrameRate);
  const safeSpeed = finitePositive(speed) ?? 1;
  const cap = finitePositive(frameRateCapFps);
  const effectiveSourceFps = sourceFps === null ? null : sourceFps * safeSpeed;
  const plannedFps = cap !== null && (effectiveSourceFps === null || effectiveSourceFps > cap + 0.01)
    ? cap
    : effectiveSourceFps ?? 30;
  const fps = clamp(plannedFps, 1, 120);
  const safeWidth = finitePositive(width);
  const safeHeight = finitePositive(height);
  if (safeWidth === null || safeHeight === null) return Number.POSITIVE_INFINITY;
  const macroblocksPerFrame = Math.ceil(safeWidth / 16) * Math.ceil(safeHeight / 16);
  return Math.max(96, Math.ceil((macroblocksPerFrame * fps * 3.6) / 1000));
}

export function sizeTargetRetainsAudio({
  audioRequested,
  hasAudio,
  sizeLimitEnabled,
  totalKbps,
  codec,
  width,
  height,
  sourceFrameRate = null,
  speed = 1,
  frameRateCapFps = null,
}) {
  if (!audioRequested || !hasAudio) return false;
  if (!sizeLimitEnabled) return true;
  const budget = finitePositive(totalKbps);
  if (budget === null) return false;
  const minimumVideoKbps = minimumVideoBitrateKbps({
    codec,
    width,
    height,
    sourceFrameRate,
    speed,
    frameRateCapFps,
  });
  return budget - minimumVideoKbps >= 32;
}

export function classifyColorSource(probe) {
  const parsedBitDepth = finitePositive(probe?.bitDepth);
  if (probe && parsedBitDepth === null) {
    return { kind: "unsupported", reason: "Pixel-component depth could not be determined, so a safe video color policy cannot be planned." };
  }
  const bitDepth = parsedBitDepth ?? 8;
  const dynamicRange = probe?.dynamicRange ?? "sdr";
  if (dynamicRange === "dolbyVision") return { kind: "unsupported", reason: "Dolby Vision is not supported for video export." };
  if (dynamicRange === "hlg") return { kind: "unsupported", reason: "HLG video conversion is not supported yet." };
  if (dynamicRange === "unknown" && probe?.colorTransfer === "smpte2084") {
    return { kind: "unsupported", reason: "PQ/HDR metadata is incomplete or contradictory, so a safe SDR conversion cannot be planned." };
  }
  if (dynamicRange === "hdr10") {
    const complete = Boolean(probe?.colorPrimaries && probe?.colorTransfer && probe?.colorSpace);
    return complete
      ? { kind: "convertible", label: "HDR10" }
      : { kind: "unsupported", reason: "HDR metadata is incomplete, so a safe SDR conversion cannot be planned." };
  }
  if (bitDepth > 8) {
    const complete = dynamicRange === "sdr" && Boolean(probe?.colorPrimaries && probe?.colorTransfer && probe?.colorSpace);
    return complete
      ? { kind: "convertible", label: `${Math.round(bitDepth)}-bit SDR` }
      : { kind: "unsupported", reason: "High-bit-depth color metadata is incomplete or unsupported." };
  }
  return { kind: "standard", label: "Standard SDR" };
}

export function estimateTransformMemory({
  probe,
  reverse,
  loopVideo,
  trimStartS = 0,
  trimEndS = null,
  speed = 1,
  frameRateCapFps = null,
  width,
  height,
  decodedVideoBytesPerPixel = null,
  normalizeAudio = false,
  audioEnabled = true,
  videoEnabled = true,
}) {
  if (!reverse && !loopVideo) {
    return { bytes: 0, severity: "ok", reason: null, videoBytes: 0, audioBytes: 0 };
  }

  const sourceDuration = finitePositive(probe?.durationS);
  const safeSpeed = finitePositive(speed);
  const fps = videoEnabled ? finitePositive(probe?.frameRate) : 1;
  const bytesPerPixel = videoEnabled
    ? finitePositive(decodedVideoBytesPerPixel) ?? finitePositive(probe?.decodedVideoBytesPerPixel)
    : 1;
  const safeWidth = videoEnabled ? finitePositive(width) : 1;
  const safeHeight = videoEnabled ? finitePositive(height) : 1;
  if (fps === null || bytesPerPixel === null || safeWidth === null || safeHeight === null || sourceDuration === null || safeSpeed === null) {
    return {
      bytes: null,
      severity: "blocked",
      reason: "Reverse and Loop need known frame rate and decoded pixel storage before export can start.",
      videoBytes: null,
      audioBytes: null,
    };
  }

  const start = clamp(Number(trimStartS) || 0, 0, sourceDuration);
  const parsedEnd = trimEndS === null || trimEndS === undefined ? sourceDuration : Number(trimEndS);
  const end = clamp(Number.isFinite(parsedEnd) ? parsedEnd : sourceDuration, start, sourceDuration);
  const retainedDuration = Math.max(0, end - start);
  const reverseFrames = Math.ceil(retainedDuration * fps);
  const cap = finitePositive(frameRateCapFps);
  const loopFrames = cap !== null && fps * safeSpeed > cap + 0.01
    ? Math.ceil((retainedDuration / safeSpeed) * cap)
    : reverseFrames;
  const frameBytes = safeWidth * safeHeight * bytesPerPixel + TRANSFORM_VIDEO_FRAME_OVERHEAD_BYTES;
  const videoBytes = videoEnabled
    ? frameBytes * ((reverse ? reverseFrames : 0) + (loopVideo ? loopFrames : 0))
    : 0;

  let audioBytes = 0;
  if (audioEnabled && probe?.hasAudio) {
    const sampleRate = normalizeAudio ? 48_000 : finitePositive(probe?.audioSampleRate);
    const channels = finitePositive(probe?.audioChannels);
    const bytesPerSample = normalizeAudio ? 8 : finitePositive(probe?.decodedAudioBytesPerSample);
    if (sampleRate === null || channels === null || bytesPerSample === null) {
      return {
        bytes: null,
        severity: "blocked",
        reason: "Reverse and Loop need known retained-audio sample rate, channel count, and decoded sample format before export can start.",
        videoBytes: Math.ceil(videoBytes * TRANSFORM_MEMORY_OVERHEAD),
        audioBytes: null,
      };
    }
    const reverseSamples = Math.ceil(retainedDuration * sampleRate);
    const loopSamples = Math.ceil((retainedDuration / safeSpeed) * sampleRate);
    const retainedSamples = (reverse ? reverseSamples : 0) + (loopVideo ? loopSamples : 0);
    const retainedFrames =
      (reverse ? Math.ceil(reverseSamples / TRANSFORM_AUDIO_FRAME_SAMPLES) : 0) +
      (loopVideo ? Math.ceil(loopSamples / TRANSFORM_AUDIO_FRAME_SAMPLES) : 0);
    audioBytes =
      channels * bytesPerSample * retainedSamples +
      retainedFrames * TRANSFORM_AUDIO_FRAME_OVERHEAD_BYTES;
  }

  const bytes = Math.ceil((videoBytes + audioBytes) * TRANSFORM_MEMORY_OVERHEAD);
  const severity = bytes >= TRANSFORM_MEMORY_BLOCK_BYTES
    ? "blocked"
    : bytes >= TRANSFORM_MEMORY_WARN_BYTES
      ? "warning"
      : "ok";
  return {
    bytes,
    severity,
    reason: severity === "blocked" ? "The decoded Reverse/Loop buffer is above the 2 GiB safety limit." : null,
    videoBytes: Math.ceil(videoBytes * TRANSFORM_MEMORY_OVERHEAD),
    audioBytes: Math.ceil(audioBytes * TRANSFORM_MEMORY_OVERHEAD),
  };
}
