export const EXPORT_RECIPES = [
  {
    id: "quick-share",
    label: "Quick share",
    description: "MP4, 720p, 30 fps cap, ready for general sharing.",
    settings: {
      format: "mp4",
      sizeLimitMb: "",
      resize: {
        mode: "maxEdge",
        maxEdgePx: "720",
        widthPx: "",
        heightPx: "",
        lockAspect: true,
      },
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
    id: "discord-10mb",
    label: "Discord 10 MB",
    description: "MP4, 720p, sized for Discord's 10 MB free upload limit.",
    settings: {
      format: "mp4",
      sizeLimitMb: "10",
      resize: {
        mode: "maxEdge",
        maxEdgePx: "720",
        widthPx: "",
        heightPx: "",
        lockAspect: true,
      },
      audioEnabled: true,
      advanced: {
        videoCodec: "h264",
        audioBitrateKbps: 96,
        videoQuality: "auto",
        encodeSpeed: "smaller",
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
      resize: {
        mode: "maxEdge",
        maxEdgePx: "540",
        widthPx: "",
        heightPx: "",
        lockAspect: true,
      },
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
    id: "forum-4mb",
    label: "Forum 4 MB",
    description: "Caps the export at 4 MB, removes audio, and makes each export unique. Leaves every other setting unchanged.",
    // Partial recipes only set (and only match) the settings they list.
    partial: true,
    settings: {
      sizeLimitMb: "4",
      audioEnabled: false,
      perturbFirstFrame: true,
    },
  },
  {
    id: "archive-quality",
    label: "Archive quality",
    description: "MP4 with higher-quality H.264 and no size target.",
    settings: {
      format: "mp4",
      sizeLimitMb: "",
      resize: {
        mode: "source",
        maxEdgePx: "",
        widthPx: "",
        heightPx: "",
        lockAspect: true,
      },
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
      resize: {
        mode: "maxEdge",
        maxEdgePx: "720",
        widthPx: "",
        heightPx: "",
        lockAspect: true,
      },
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
      resize: {
        mode: "source",
        maxEdgePx: "",
        widthPx: "",
        heightPx: "",
        lockAspect: true,
      },
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

function normalizeResizeMode(value, legacyMaxEdgePx) {
  if (value === "source" || value === "maxEdge" || value === "custom") return value;
  return legacyMaxEdgePx ? "maxEdge" : "source";
}

export function normalizeRecipeResizeSettings(settings) {
  const legacyMaxEdgePx = normalizeTextSetting(settings?.maxEdgePx);
  const raw = settings?.resize && typeof settings.resize === "object" ? settings.resize : {};
  const mode = normalizeResizeMode(raw.mode, legacyMaxEdgePx);

  return {
    mode,
    maxEdgePx: normalizeTextSetting(raw.maxEdgePx ?? legacyMaxEdgePx),
    widthPx: normalizeTextSetting(raw.widthPx),
    heightPx: normalizeTextSetting(raw.heightPx),
    lockAspect: raw.lockAspect !== false,
  };
}

function normalizeAdvancedNumber(value) {
  if (value === null || value === undefined || value === "auto" || value === "") return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

export function recipeMatchesSettings(recipe, settings) {
  if (!recipe || !settings) return false;

  if (recipe.partial) {
    const partialSettings = recipe.settings ?? {};
    if ("format" in partialSettings && settings.format !== partialSettings.format) return false;
    if (
      "sizeLimitMb" in partialSettings &&
      normalizeTextSetting(settings.sizeLimitMb) !== normalizeTextSetting(partialSettings.sizeLimitMb)
    ) {
      return false;
    }
    if (
      "audioEnabled" in partialSettings &&
      Boolean(settings.audioEnabled) !== Boolean(partialSettings.audioEnabled)
    ) {
      return false;
    }
    if (
      "normalizeAudio" in partialSettings &&
      Boolean(settings.normalizeAudio) !== Boolean(partialSettings.normalizeAudio)
    ) {
      return false;
    }
    if (
      "perturbFirstFrame" in partialSettings &&
      Boolean(settings.perturbFirstFrame) !== Boolean(partialSettings.perturbFirstFrame)
    ) {
      return false;
    }
    return true;
  }

  const recipeSettings = recipe.settings;
  const recipeAdvanced = recipeSettings.advanced ?? {};
  const currentAdvanced = settings.advanced ?? {};
  const recipeResize = normalizeRecipeResizeSettings(recipeSettings);
  const currentResize = normalizeRecipeResizeSettings(settings);

  const resizeMatches =
    recipeResize.mode === currentResize.mode &&
    (recipeResize.mode === "source" ||
      (recipeResize.mode === "maxEdge" && recipeResize.maxEdgePx === currentResize.maxEdgePx) ||
      (recipeResize.mode === "custom" &&
        recipeResize.widthPx === currentResize.widthPx &&
        recipeResize.heightPx === currentResize.heightPx &&
        Boolean(recipeResize.lockAspect) === Boolean(currentResize.lockAspect)));

  return (
    settings.format === recipeSettings.format &&
    normalizeTextSetting(settings.sizeLimitMb) === normalizeTextSetting(recipeSettings.sizeLimitMb) &&
    resizeMatches &&
    Boolean(settings.audioEnabled) === Boolean(recipeSettings.audioEnabled) &&
    Boolean(settings.normalizeAudio) === Boolean(recipeSettings.normalizeAudio) &&
    Boolean(settings.perturbFirstFrame) === Boolean(recipeSettings.perturbFirstFrame) &&
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
