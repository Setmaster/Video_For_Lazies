export function isAudioSelection(selection) {
  return !!selection && (selection.audioStreamIndex === null || Number.isSafeInteger(selection.audioStreamIndex) && selection.audioStreamIndex >= 0 && selection.audioStreamIndex <= 0xffffffff) &&
    (selection.audioSourceIdentity === null || typeof selection.audioSourceIdentity === "string" && /^[a-f0-9]{64}$/.test(selection.audioSourceIdentity)) &&
    (selection.audioStreamIndex === null || selection.audioSourceIdentity !== null);
}
export function validateAudioPreviewAsset(asset, request) {
  if (!asset || asset.requestId !== request.requestId || asset.sourceIdentity !== request.sourceIdentity || asset.audioStreamIndex !== request.streamIndex ||
      typeof asset.path !== "string" || !asset.path || !Number.isFinite(asset.timelineOffsetS) ||
      !(asset.kind === "original" && asset.proxyId === null && asset.timelineOffsetS === 0 || asset.kind === "proxy" && typeof asset.proxyId === "string" && asset.proxyId.length > 0 && asset.proxyId.length <= 128)) {
    throw new Error("Audio preview returned a different source, track or request identity.");
  }
  return asset;
}
export function audioTrackLabel(track) {
  return `${track.title || "Audio"} · stream ${track.streamIndex}${track.language ? ` · ${track.language}` : ""}${track.isDefault ? " · default" : ""}`;
}
