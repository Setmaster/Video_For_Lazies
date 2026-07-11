export const USER_RECIPE_STORAGE_KEY = "vfl:user-recipes";
export const USER_RECIPE_SCHEMA_VERSION = 2;
export const USER_RECIPE_MAX_COUNT = 50;
export const USER_RECIPE_NAME_MAX_LENGTH = 64;

const USER_RECIPE_ID_MAX_LENGTH = 128;
const OUTPUT_DIMENSION_MIN_PX = 16;
const OUTPUT_DIMENSION_MAX_PX = 32768;
const VALID_FORMATS = new Set(["mp4", "webm", "mp3"]);
const VALID_RESIZE_MODES = new Set(["source", "maxEdge", "custom"]);
const VALID_VIDEO_CODECS = new Set(["auto", "h264", "mpeg4", "vp9", "vp8"]);
const VALID_AUDIO_BITRATES = new Set([96, 128, 192, 256, 320]);
const VALID_VIDEO_QUALITIES = new Set(["auto", "smaller", "balanced", "higher"]);
const VALID_ENCODE_SPEEDS = new Set(["auto", "faster", "balanced", "smaller"]);
const VALID_FRAME_RATE_CAPS = new Set([24, 30, 60]);
const VALID_AUDIO_CHANNELS = new Set(["auto", "stereo", "mono"]);

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function cloneJson(value) {
  return JSON.parse(JSON.stringify(value));
}

function normalizeName(value) {
  if (typeof value !== "string") return null;
  const normalized = value.trim().replace(/\s+/g, " ");
  if (!normalized || normalized.length > USER_RECIPE_NAME_MAX_LENGTH) return null;
  return normalized;
}

function normalizeId(value) {
  if (typeof value !== "string") return null;
  const normalized = value.trim();
  if (!normalized || normalized.length > USER_RECIPE_ID_MAX_LENGTH) return null;
  if (!/^[A-Za-z0-9._:-]+$/.test(normalized)) return null;
  return normalized;
}

function normalizeOptionalFiniteNumber(value) {
  if (value === null || value === undefined || value === "" || value === "auto") {
    return { valid: true, value: null };
  }
  const parsed = Number(value);
  return Number.isFinite(parsed)
    ? { valid: true, value: parsed }
    : { valid: false, value: null };
}

function normalizeSizeLimitMb(value) {
  if (value === null || value === undefined) return "";
  if (typeof value !== "string" && typeof value !== "number") return null;
  if (String(value).trim() === "") return "";
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed < 0 || (parsed > 0 && parsed < 0.1)) return null;
  return parsed === 0 ? "" : String(parsed);
}

function normalizeDimension(value) {
  const parsed = Number(value);
  if (
    !Number.isInteger(parsed) ||
    parsed < OUTPUT_DIMENSION_MIN_PX ||
    parsed > OUTPUT_DIMENSION_MAX_PX
  ) {
    return null;
  }
  return String(parsed);
}

function normalizeResizeSettings(settings, legacyMaxEdgePx) {
  const rawResize = isRecord(settings?.resize) ? settings.resize : {};
  const legacyValue = legacyMaxEdgePx ?? settings?.maxEdgePx ?? "";
  const inferredMode = String(legacyValue ?? "").trim() ? "maxEdge" : "source";
  const mode = rawResize.mode ?? inferredMode;
  if (!VALID_RESIZE_MODES.has(mode)) return null;

  if (mode === "source") {
    return {
      mode: "source",
      maxEdgePx: "",
      widthPx: "",
      heightPx: "",
      lockAspect: true,
    };
  }

  if (mode === "maxEdge") {
    const maxEdgePx = normalizeDimension(rawResize.maxEdgePx ?? legacyValue);
    if (maxEdgePx === null) return null;
    return {
      mode: "maxEdge",
      maxEdgePx,
      widthPx: "",
      heightPx: "",
      lockAspect: true,
    };
  }

  const widthPx = normalizeDimension(rawResize.widthPx);
  const heightPx = normalizeDimension(rawResize.heightPx);
  if (widthPx === null || heightPx === null) return null;
  if (rawResize.lockAspect !== undefined && typeof rawResize.lockAspect !== "boolean") return null;
  return {
    mode: "custom",
    maxEdgePx: "",
    widthPx,
    heightPx,
    lockAspect: rawResize.lockAspect !== false,
  };
}

function videoCodecIsCompatible(format, codec) {
  if (codec === "auto") return true;
  if (format === "mp4") return codec === "h264" || codec === "mpeg4";
  if (format === "webm") return codec === "vp9" || codec === "vp8";
  return false;
}

function normalizeAdvancedSettings(value, format) {
  if (value !== undefined && value !== null && !isRecord(value)) return null;
  const advanced = isRecord(value) ? value : {};
  const videoCodec = advanced.videoCodec ?? "auto";
  const videoQuality = advanced.videoQuality ?? "auto";
  const encodeSpeed = advanced.encodeSpeed ?? "auto";
  const audioChannels = advanced.audioChannels ?? "auto";
  const audioBitrate = normalizeOptionalFiniteNumber(advanced.audioBitrateKbps);
  const frameRateCap = normalizeOptionalFiniteNumber(advanced.frameRateCapFps);
  if (!audioBitrate.valid || !frameRateCap.valid) return null;
  const audioBitrateKbps = audioBitrate.value;
  const frameRateCapFps = frameRateCap.value;

  if (!VALID_VIDEO_CODECS.has(videoCodec)) return null;
  if (format !== "mp3" && !videoCodecIsCompatible(format, videoCodec)) return null;
  if (audioBitrateKbps !== null && !VALID_AUDIO_BITRATES.has(audioBitrateKbps)) return null;
  if (!VALID_VIDEO_QUALITIES.has(videoQuality)) return null;
  if (!VALID_ENCODE_SPEEDS.has(encodeSpeed)) return null;
  if (frameRateCapFps !== null && !VALID_FRAME_RATE_CAPS.has(frameRateCapFps)) return null;
  if (!VALID_AUDIO_CHANNELS.has(audioChannels)) return null;

  if (format === "mp3") {
    return {
      videoCodec: "auto",
      audioBitrateKbps,
      videoQuality: "auto",
      encodeSpeed: "auto",
      frameRateCapFps: null,
      audioChannels,
    };
  }

  return {
    videoCodec,
    audioBitrateKbps,
    videoQuality,
    encodeSpeed,
    frameRateCapFps,
    audioChannels,
  };
}

/**
 * Rebuild a reusable settings snapshot from an explicit allowlist. Extra
 * properties are intentionally ignored so paths, clip edits, diagnostics,
 * metadata policy, and source-specific color consent cannot enter a recipe.
 */
export function normalizeUserRecipeSettings(value, options = {}) {
  if (!isRecord(value)) return null;
  const format = value.format;
  if (!VALID_FORMATS.has(format)) return null;

  const sizeLimitMb = normalizeSizeLimitMb(value.sizeLimitMb);
  const resize = format === "mp3"
    ? {
        mode: "source",
        maxEdgePx: "",
        widthPx: "",
        heightPx: "",
        lockAspect: true,
      }
    : normalizeResizeSettings(value, options.legacyMaxEdgePx);
  const advanced = normalizeAdvancedSettings(value.advanced, format);
  if (sizeLimitMb === null || resize === null || advanced === null) return null;
  if (typeof value.audioEnabled !== "boolean") return null;
  if (value.normalizeAudio !== undefined && typeof value.normalizeAudio !== "boolean") return null;
  if (value.perturbFirstFrame !== undefined && typeof value.perturbFirstFrame !== "boolean") return null;
  if (value.strictFit !== undefined && typeof value.strictFit !== "boolean") return null;
  if (
    value.strictFitAllowAudioRemoval !== undefined &&
    typeof value.strictFitAllowAudioRemoval !== "boolean"
  ) {
    return null;
  }

  const audioEnabled = format === "mp3" ? true : value.audioEnabled;
  const hasPositiveSizeTarget = Number(sizeLimitMb) > 0;
  const strictFit = format !== "mp3" && hasPositiveSizeTarget && value.strictFit === true;
  const strictFitAllowAudioRemoval =
    strictFit && audioEnabled && value.strictFitAllowAudioRemoval === true;

  return {
    format,
    sizeLimitMb,
    resize,
    audioEnabled,
    normalizeAudio: value.normalizeAudio === true,
    perturbFirstFrame: format === "mp3" ? false : value.perturbFirstFrame === true,
    strictFit,
    strictFitAllowAudioRemoval,
    advanced,
  };
}

/**
 * Convert a full backend request into the same privacy-safe recipe allowlist.
 * subtitlePath and all other path, clip, metadata, and diagnostic fields are
 * deliberately not read here.
 */
export function reusableSettingsFromEncodeRequest(request) {
  if (!isRecord(request)) return null;
  const rawResize = isRecord(request.resize) ? request.resize : {};
  const resizeMode = rawResize.mode ?? (request.maxEdgePx ? "maxEdge" : "source");
  return normalizeUserRecipeSettings({
    format: request.format,
    sizeLimitMb: request.sizeLimitMb,
    resize: {
      mode: resizeMode,
      maxEdgePx: rawResize.maxEdgePx ?? request.maxEdgePx ?? "",
      widthPx: rawResize.widthPx ?? "",
      heightPx: rawResize.heightPx ?? "",
      lockAspect: true,
    },
    audioEnabled: request.audioEnabled,
    normalizeAudio: request.normalizeAudio,
    perturbFirstFrame: request.perturbFirstFrame,
    strictFit: request.strictFit,
    strictFitAllowAudioRemoval: request.strictFitAllowAudioRemoval,
    advanced: request.advanced,
  });
}

function makeStore(recipes = [], options = {}) {
  return {
    schemaVersion: USER_RECIPE_SCHEMA_VERSION,
    recipes,
    warnings: options.warnings ?? [],
    migrated: options.migrated === true,
    readOnly: options.readOnly === true,
    sourceSchemaVersion: options.sourceSchemaVersion ?? null,
  };
}

export function createEmptyUserRecipeStore() {
  return makeStore();
}

function migrateSchemaZeroRecipe(value, index) {
  if (!isRecord(value)) return null;
  const id = normalizeId(value.id) ?? `user:migrated-${index + 1}`;
  const name = normalizeName(value.label ?? value.name);
  const rawSettings = isRecord(value.settings) ? value.settings : value;
  const legacyMaxEdgePx = rawSettings.maxEdgePx ?? value.maxEdgePx;
  const settings = normalizeUserRecipeSettings(
    {
      ...rawSettings,
      strictFit: false,
      strictFitAllowAudioRemoval: false,
    },
    { legacyMaxEdgePx },
  );
  if (!name || !settings) return null;
  return { id, name, settings };
}

function migrateSchemaOneRecipe(value) {
  if (!isRecord(value) || !isRecord(value.settings)) return null;
  const id = normalizeId(value.id);
  const name = normalizeName(value.name);
  const settings = normalizeUserRecipeSettings({
    ...value.settings,
    strictFit: false,
    strictFitAllowAudioRemoval: false,
  });
  if (!id || !name || !settings) return null;
  return { id, name, settings };
}

function normalizeSchemaTwoRecipe(value) {
  if (!isRecord(value)) return null;
  const id = normalizeId(value.id);
  const name = normalizeName(value.name);
  const settings = normalizeUserRecipeSettings(value.settings);
  if (!id || !name || !settings) return null;
  return { id, name, settings };
}

function salvageRecipes(values, normalizeRecipe) {
  const recipes = [];
  const warnings = [];
  const ids = new Set();
  const names = new Set();

  if (!Array.isArray(values)) {
    return {
      recipes,
      warnings: ["Saved recipe data did not contain a recipe list and was ignored."],
    };
  }

  const recordCount = Math.min(values.length, USER_RECIPE_MAX_COUNT);
  for (let index = 0; index < recordCount; index += 1) {
    const recipe = normalizeRecipe(values[index], index);
    if (!recipe) {
      warnings.push(`Saved recipe ${index + 1} was invalid and was ignored.`);
      continue;
    }

    const idKey = recipe.id.toLowerCase();
    const nameKey = recipe.name.toLowerCase();
    if (ids.has(idKey)) {
      warnings.push(`Saved recipe ${index + 1} reused an existing identifier and was ignored.`);
      continue;
    }
    if (names.has(nameKey)) {
      warnings.push(`Saved recipe ${index + 1} reused an existing name and was ignored.`);
      continue;
    }

    ids.add(idKey);
    names.add(nameKey);
    recipes.push(recipe);
  }

  if (values.length > recordCount) {
    warnings.push(`Only the first ${USER_RECIPE_MAX_COUNT} saved recipe records were examined.`);
  }

  return { recipes, warnings };
}

export function parseUserRecipeStore(raw) {
  if (raw === null || raw === undefined || raw === "") return createEmptyUserRecipeStore();

  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return makeStore([], {
      warnings: ["Saved recipes could not be read and were ignored."],
    });
  }

  const isUnversionedArray = Array.isArray(parsed);
  const isUnversionedObject = isRecord(parsed) && parsed.schemaVersion === undefined;
  const sourceSchemaVersion = isUnversionedArray || isUnversionedObject ? 0 : parsed?.schemaVersion;

  if (Number.isInteger(sourceSchemaVersion) && sourceSchemaVersion > USER_RECIPE_SCHEMA_VERSION) {
    return makeStore([], {
      warnings: ["Saved recipes were created by a newer app version and were left unchanged."],
      readOnly: true,
      sourceSchemaVersion,
    });
  }

  if (
    sourceSchemaVersion !== 0 &&
    sourceSchemaVersion !== 1 &&
    sourceSchemaVersion !== USER_RECIPE_SCHEMA_VERSION
  ) {
    return makeStore([], {
      warnings: ["Saved recipe data used an unsupported schema and was ignored."],
      sourceSchemaVersion: Number.isInteger(sourceSchemaVersion) ? sourceSchemaVersion : null,
    });
  }

  const values = isUnversionedArray ? parsed : parsed.recipes;
  const migrating = sourceSchemaVersion === 0 || sourceSchemaVersion === 1;
  const normalizeRecipe = sourceSchemaVersion === 0
    ? migrateSchemaZeroRecipe
    : sourceSchemaVersion === 1
      ? migrateSchemaOneRecipe
      : normalizeSchemaTwoRecipe;
  const salvaged = salvageRecipes(values, normalizeRecipe);
  if (migrating) {
    salvaged.warnings.unshift("Saved recipes were migrated to the current format.");
  }
  return makeStore(salvaged.recipes, {
    warnings: salvaged.warnings,
    migrated: migrating,
    sourceSchemaVersion,
  });
}

export function loadUserRecipeStore(storage, key = USER_RECIPE_STORAGE_KEY) {
  try {
    return parseUserRecipeStore(storage?.getItem?.(key) ?? null);
  } catch {
    return makeStore([], {
      warnings: ["Saved recipes could not be accessed."],
    });
  }
}

function normalizeRecipeListForWrite(recipes) {
  if (!Array.isArray(recipes)) throw new TypeError("Recipes must be an array.");
  if (recipes.length > USER_RECIPE_MAX_COUNT) {
    throw new TypeError(`No more than ${USER_RECIPE_MAX_COUNT} recipes can be saved.`);
  }

  const normalized = [];
  const ids = new Set();
  const names = new Set();
  for (const value of recipes) {
    const recipe = normalizeSchemaTwoRecipe(value);
    if (!recipe) throw new TypeError("A recipe contains invalid data.");
    const idKey = recipe.id.toLowerCase();
    const nameKey = recipe.name.toLowerCase();
    if (ids.has(idKey)) throw new TypeError("Recipe identifiers must be unique.");
    if (names.has(nameKey)) throw new TypeError("Recipe names must be unique.");
    ids.add(idKey);
    names.add(nameKey);
    normalized.push(recipe);
  }
  return normalized;
}

export function serializeUserRecipeStore(recipes) {
  const normalized = normalizeRecipeListForWrite(recipes);
  return JSON.stringify({
    schemaVersion: USER_RECIPE_SCHEMA_VERSION,
    recipes: normalized,
  });
}

/**
 * Write before returning the next in-memory store. Callers should update React
 * state only when ok is true. A future-version store is never overwritten.
 */
export function persistUserRecipeStore(storage, currentStore, nextRecipes, key = USER_RECIPE_STORAGE_KEY) {
  if (currentStore?.readOnly) {
    return {
      ok: false,
      error: "Saved recipes belong to a newer app version and cannot be changed here.",
    };
  }

  try {
    const normalized = normalizeRecipeListForWrite(nextRecipes);
    const raw = serializeUserRecipeStore(normalized);
    storage.setItem(key, raw);
    return {
      ok: true,
      raw,
      store: makeStore(normalized, { sourceSchemaVersion: USER_RECIPE_SCHEMA_VERSION }),
    };
  } catch (error) {
    return {
      ok: false,
      error: error instanceof Error ? error.message : "Saved recipes could not be written.",
    };
  }
}

export function generateUserRecipeId(recipes, nowMs = Date.now()) {
  const used = new Set((Array.isArray(recipes) ? recipes : []).map((recipe) => String(recipe?.id ?? "").toLowerCase()));
  const parsedNowMs = Number(nowMs);
  const safeNowMs = Number.isFinite(parsedNowMs) ? Math.max(0, Math.floor(parsedNowMs)) : 0;
  const base = `user:${safeNowMs.toString(36)}`;
  if (!used.has(base.toLowerCase())) return base;
  for (let suffix = 2; suffix <= USER_RECIPE_MAX_COUNT + 1; suffix += 1) {
    const candidate = `${base}:${suffix}`;
    if (!used.has(candidate.toLowerCase())) return candidate;
  }
  throw new Error("Could not allocate a recipe identifier.");
}

function mutationError(error) {
  return { ok: false, error };
}

export function createUserRecipe(recipes, name, settings, options = {}) {
  if (!Array.isArray(recipes)) return mutationError("Recipes are unavailable.");
  if (recipes.length >= USER_RECIPE_MAX_COUNT) {
    return mutationError(`No more than ${USER_RECIPE_MAX_COUNT} recipes can be saved.`);
  }

  const normalizedName = normalizeName(name);
  if (!normalizedName) {
    return mutationError(`Recipe names must contain 1 to ${USER_RECIPE_NAME_MAX_LENGTH} characters.`);
  }
  if (recipes.some((recipe) => recipe.name.toLowerCase() === normalizedName.toLowerCase())) {
    return mutationError("Recipe names must be unique.");
  }

  const normalizedSettings = normalizeUserRecipeSettings(settings);
  if (!normalizedSettings) return mutationError("The current settings cannot be saved as a recipe.");
  const id = normalizeId(options.id ?? generateUserRecipeId(recipes, options.nowMs));
  if (!id) return mutationError("The recipe identifier is invalid.");
  if (recipes.some((recipe) => recipe.id.toLowerCase() === id.toLowerCase())) {
    return mutationError("Recipe identifiers must be unique.");
  }

  const recipe = { id, name: normalizedName, settings: normalizedSettings };
  return { ok: true, recipe, recipes: [...recipes, recipe] };
}

export function renameUserRecipe(recipes, id, name) {
  if (!Array.isArray(recipes)) return mutationError("Recipes are unavailable.");
  const idKey = String(id ?? "").toLowerCase();
  const index = recipes.findIndex((recipe) => recipe.id.toLowerCase() === idKey);
  if (index < 0) return mutationError("Recipe not found.");

  const normalizedName = normalizeName(name);
  if (!normalizedName) {
    return mutationError(`Recipe names must contain 1 to ${USER_RECIPE_NAME_MAX_LENGTH} characters.`);
  }
  if (
    recipes.some(
      (recipe, recipeIndex) =>
        recipeIndex !== index && recipe.name.toLowerCase() === normalizedName.toLowerCase(),
    )
  ) {
    return mutationError("Recipe names must be unique.");
  }

  const recipe = { ...recipes[index], name: normalizedName };
  const next = [...recipes];
  next[index] = recipe;
  return { ok: true, recipe, recipes: next };
}

export function deleteUserRecipe(recipes, id) {
  if (!Array.isArray(recipes)) return mutationError("Recipes are unavailable.");
  const idKey = String(id ?? "").toLowerCase();
  const index = recipes.findIndex((recipe) => recipe.id.toLowerCase() === idKey);
  if (index < 0) return mutationError("Recipe not found.");
  return {
    ok: true,
    recipe: recipes[index],
    recipes: recipes.filter((_, recipeIndex) => recipeIndex !== index),
  };
}

function settingsFingerprint(settings) {
  const normalized = normalizeUserRecipeSettings(settings);
  return normalized ? JSON.stringify(normalized) : null;
}

export function userRecipeMatchesSettings(recipe, settings) {
  if (!recipe || !isRecord(recipe)) return false;
  const expected = settingsFingerprint(recipe.settings);
  const actual = settingsFingerprint(settings);
  return expected !== null && expected === actual;
}

export function findMatchingUserRecipe(recipes, settings) {
  if (!Array.isArray(recipes)) return null;
  return recipes.find((recipe) => userRecipeMatchesSettings(recipe, settings)) ?? null;
}

export function cloneUserRecipeSettings(settings) {
  const normalized = normalizeUserRecipeSettings(settings);
  return normalized ? cloneJson(normalized) : null;
}
