export const EXPORT_RECIPES = [
  {
    id: "quick-share",
    label: "Quick share",
    description: "MP4, 720p, 30 fps cap, ready for general sharing.",
    settings: {
      format: "mp4",
      sizeLimitMb: "",
      maxEdgePx: "720",
      audioEnabled: true,
      advanced: {
        videoCodec: "h264",
        audioBitrateKbps: 128,
        videoQuality: "balanced",
        encodeSpeed: "faster",
        frameRateCapFps: 30,
        audioChannels: "auto",
      },
    },
  },
  {
    id: "discord-25mb",
    label: "Discord 25 MB",
    description: "MP4, 720p, size-targeted for common upload limits.",
    settings: {
      format: "mp4",
      sizeLimitMb: "25",
      maxEdgePx: "720",
      audioEnabled: true,
      advanced: {
        videoCodec: "h264",
        audioBitrateKbps: null,
        videoQuality: "auto",
        encodeSpeed: "balanced",
        frameRateCapFps: 30,
        audioChannels: "auto",
      },
    },
  },
  {
    id: "small-email",
    label: "Small email clip",
    description: "MP4, 540p, 10 MB target for short lightweight clips.",
    settings: {
      format: "mp4",
      sizeLimitMb: "10",
      maxEdgePx: "540",
      audioEnabled: true,
      advanced: {
        videoCodec: "h264",
        audioBitrateKbps: null,
        videoQuality: "auto",
        encodeSpeed: "balanced",
        frameRateCapFps: 30,
        audioChannels: "mono",
      },
    },
  },
  {
    id: "archive-quality",
    label: "Archive quality",
    description: "MP4 with higher-quality H.264 and no size target.",
    settings: {
      format: "mp4",
      sizeLimitMb: "",
      maxEdgePx: "",
      audioEnabled: true,
      advanced: {
        videoCodec: "h264",
        audioBitrateKbps: 192,
        videoQuality: "higher",
        encodeSpeed: "balanced",
        frameRateCapFps: null,
        audioChannels: "auto",
      },
    },
  },
  {
    id: "webm-smaller",
    label: "WebM smaller file",
    description: "WebM, VP9, 720p with a compact 25 MB target.",
    settings: {
      format: "webm",
      sizeLimitMb: "25",
      maxEdgePx: "720",
      audioEnabled: true,
      advanced: {
        videoCodec: "vp9",
        audioBitrateKbps: null,
        videoQuality: "auto",
        encodeSpeed: "smaller",
        frameRateCapFps: 30,
        audioChannels: "auto",
      },
    },
  },
  {
    id: "audio-mp3",
    label: "Audio only MP3",
    description: "Extract audio to MP3 with a 192 kbps target.",
    settings: {
      format: "mp3",
      sizeLimitMb: "",
      maxEdgePx: "",
      audioEnabled: true,
      advanced: {
        videoCodec: "auto",
        audioBitrateKbps: 192,
        videoQuality: "auto",
        encodeSpeed: "auto",
        frameRateCapFps: null,
        audioChannels: "auto",
      },
    },
  },
];

export function findExportRecipe(id) {
  return EXPORT_RECIPES.find((recipe) => recipe.id === id) ?? null;
}

function normalizeTextSetting(value) {
  return String(value ?? "").trim();
}

function normalizeAdvancedNumber(value) {
  if (value === null || value === undefined || value === "auto" || value === "") return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

export function recipeMatchesSettings(recipe, settings) {
  if (!recipe || !settings) return false;

  const recipeSettings = recipe.settings;
  const recipeAdvanced = recipeSettings.advanced ?? {};
  const currentAdvanced = settings.advanced ?? {};

  return (
    settings.format === recipeSettings.format &&
    normalizeTextSetting(settings.sizeLimitMb) === normalizeTextSetting(recipeSettings.sizeLimitMb) &&
    normalizeTextSetting(settings.maxEdgePx) === normalizeTextSetting(recipeSettings.maxEdgePx) &&
    Boolean(settings.audioEnabled) === Boolean(recipeSettings.audioEnabled) &&
    (currentAdvanced.videoCodec ?? "auto") === (recipeAdvanced.videoCodec ?? "auto") &&
    normalizeAdvancedNumber(currentAdvanced.audioBitrateKbps) === normalizeAdvancedNumber(recipeAdvanced.audioBitrateKbps) &&
    (currentAdvanced.videoQuality ?? "auto") === (recipeAdvanced.videoQuality ?? "auto") &&
    (currentAdvanced.encodeSpeed ?? "auto") === (recipeAdvanced.encodeSpeed ?? "auto") &&
    normalizeAdvancedNumber(currentAdvanced.frameRateCapFps) === normalizeAdvancedNumber(recipeAdvanced.frameRateCapFps) &&
    (currentAdvanced.audioChannels ?? "auto") === (recipeAdvanced.audioChannels ?? "auto")
  );
}

export function findMatchingExportRecipe(settings) {
  return EXPORT_RECIPES.find((recipe) => recipeMatchesSettings(recipe, settings)) ?? null;
}
