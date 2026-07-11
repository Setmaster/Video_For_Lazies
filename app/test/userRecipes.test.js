import test from "node:test";
import assert from "node:assert/strict";

import {
  USER_RECIPE_MAX_COUNT,
  USER_RECIPE_NAME_MAX_LENGTH,
  USER_RECIPE_SCHEMA_VERSION,
  USER_RECIPE_STORAGE_KEY,
  createEmptyUserRecipeStore,
  createUserRecipe,
  deleteUserRecipe,
  findMatchingUserRecipe,
  generateUserRecipeId,
  loadUserRecipeStore,
  normalizeUserRecipeSettings,
  parseUserRecipeStore,
  persistUserRecipeStore,
  renameUserRecipe,
  reusableSettingsFromEncodeRequest,
  serializeUserRecipeStore,
  userRecipeMatchesSettings,
} from "../src/lib/userRecipes.mjs";

function baseSettings(overrides = {}) {
  return {
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
    normalizeAudio: false,
    perturbFirstFrame: false,
    advanced: {
      videoCodec: "h264",
      audioBitrateKbps: 128,
      videoQuality: "balanced",
      encodeSpeed: "faster",
      frameRateCapFps: 30,
      audioChannels: "stereo",
    },
    ...overrides,
  };
}

function recipe(id = "user:one", name = "Share") {
  return { id, name, settings: baseSettings() };
}

function memoryStorage(initial = new Map()) {
  return {
    values: initial,
    getItem(key) {
      return this.values.has(key) ? this.values.get(key) : null;
    },
    setItem(key, value) {
      this.values.set(key, value);
    },
  };
}

test("recipe settings rebuild through the strict reusable allowlist", () => {
  const sentinelPath = "C:/Private/secret-source.mov";
  const normalized = normalizeUserRecipeSettings({
    ...baseSettings(),
    inputPath: sentinelPath,
    outputPath: "C:/Private/secret-output.mp4",
    title: "Private title",
    trim: { startS: 12, endS: 15 },
    crop: { x: 1, y: 2, width: 3, height: 4 },
    reverse: true,
    loopVideo: true,
    speed: 2,
    rotateDeg: 90,
    color: { brightness: 1, contrast: 2, saturation: 3 },
    colorPolicy: "standardSdr",
    stripMetadata: false,
    probe: { sourceFormat: "secret" },
    diagnostics: { commandPreview: sentinelPath },
    identity: { user: "secret" },
  });

  assert.deepEqual(normalized, baseSettings());
  const raw = JSON.stringify(normalized);
  for (const forbidden of [
    sentinelPath,
    "secret-output",
    "Private title",
    "trim",
    "crop",
    "reverse",
    "loopVideo",
    "speed",
    "rotateDeg",
    "colorPolicy",
    "stripMetadata",
    "probe",
    "diagnostics",
    "identity",
  ]) {
    assert.equal(raw.includes(forbidden), false, `serialized settings leaked ${forbidden}`);
  }
});

test("EncodeRequest conversion keeps output settings and excludes request identity and clip state", () => {
  const request = {
    inputPath: "/private/input.mov",
    outputPath: "/private/output.mp4",
    format: "mp4",
    title: "Secret",
    sizeLimitMb: 25,
    audioEnabled: true,
    normalizeAudio: true,
    stripMetadata: false,
    colorPolicy: "standardSdr",
    advanced: {
      videoCodec: "mpeg4",
      audioBitrateKbps: 192,
      videoQuality: "higher",
      encodeSpeed: "balanced",
      frameRateCapFps: 24,
      audioChannels: "mono",
    },
    trim: { startS: 4, endS: 9 },
    crop: { x: 2, y: 4, width: 100, height: 80 },
    reverse: true,
    speed: 0.5,
    rotateDeg: 270,
    resize: { mode: "custom", widthPx: 1280, heightPx: 720 },
    color: { brightness: 0.1, contrast: 1, saturation: 1 },
    perturbFirstFrame: true,
    loopVideo: true,
  };

  assert.deepEqual(reusableSettingsFromEncodeRequest(request), {
    format: "mp4",
    sizeLimitMb: "25",
    resize: {
      mode: "custom",
      maxEdgePx: "",
      widthPx: "1280",
      heightPx: "720",
      lockAspect: true,
    },
    audioEnabled: true,
    normalizeAudio: true,
    perturbFirstFrame: true,
    advanced: {
      videoCodec: "mpeg4",
      audioBitrateKbps: 192,
      videoQuality: "higher",
      encodeSpeed: "balanced",
      frameRateCapFps: 24,
      audioChannels: "mono",
    },
  });
});

test("MP3 recipes canonicalize video-only settings and always retain audio", () => {
  assert.deepEqual(
    normalizeUserRecipeSettings(baseSettings({
      format: "mp3",
      audioEnabled: false,
      perturbFirstFrame: true,
      resize: {
        mode: "custom",
        maxEdgePx: "",
        widthPx: "",
        heightPx: "",
        lockAspect: false,
      },
      advanced: {
        videoCodec: "h264",
        audioBitrateKbps: 192,
        videoQuality: "higher",
        encodeSpeed: "smaller",
        frameRateCapFps: 60,
        audioChannels: "mono",
      },
    })),
    {
      format: "mp3",
      sizeLimitMb: "10",
      resize: {
        mode: "source",
        maxEdgePx: "",
        widthPx: "",
        heightPx: "",
        lockAspect: true,
      },
      audioEnabled: true,
      normalizeAudio: false,
      perturbFirstFrame: false,
      advanced: {
        videoCodec: "auto",
        audioBitrateKbps: 192,
        videoQuality: "auto",
        encodeSpeed: "auto",
        frameRateCapFps: null,
        audioChannels: "mono",
      },
    },
  );
});

test("invalid reusable values fail closed instead of becoming auto defaults", () => {
  assert.equal(normalizeUserRecipeSettings(baseSettings({ format: "avi" })), null);
  assert.equal(normalizeUserRecipeSettings(baseSettings({ sizeLimitMb: "not-a-number" })), null);
  assert.equal(normalizeUserRecipeSettings(baseSettings({ audioEnabled: "yes" })), null);
  assert.equal(normalizeUserRecipeSettings(baseSettings({ advanced: "auto" })), null);
  assert.equal(
    normalizeUserRecipeSettings(baseSettings({
      advanced: { ...baseSettings().advanced, audioBitrateKbps: "garbage" },
    })),
    null,
  );
  assert.equal(
    normalizeUserRecipeSettings(baseSettings({
      format: "mp4",
      advanced: { ...baseSettings().advanced, videoCodec: "vp9" },
    })),
    null,
  );
});

test("schema v1 serialization has a stable exact shape and round trips", () => {
  const input = [recipe()];
  const raw = serializeUserRecipeStore(input);
  assert.equal(
    raw,
    JSON.stringify({
      schemaVersion: USER_RECIPE_SCHEMA_VERSION,
      recipes: input,
    }),
  );

  const parsed = parseUserRecipeStore(raw);
  assert.deepEqual(parsed, {
    schemaVersion: 1,
    recipes: input,
    warnings: [],
    migrated: false,
    readOnly: false,
    sourceSchemaVersion: 1,
  });
});

test("unversioned label/maxEdge records migrate to schema v1", () => {
  const parsed = parseUserRecipeStore(JSON.stringify([
    {
      id: "legacy-one",
      label: "Legacy 720p",
      settings: {
        format: "mp4",
        sizeLimitMb: "10",
        maxEdgePx: "720",
        audioEnabled: true,
        advanced: baseSettings().advanced,
      },
    },
    {
      label: "Generated ID",
      settings: {
        ...baseSettings(),
        maxEdgePx: "",
        resize: undefined,
      },
    },
  ]));

  assert.equal(parsed.migrated, true);
  assert.equal(parsed.sourceSchemaVersion, 0);
  assert.match(parsed.warnings[0], /migrated/);
  assert.deepEqual(parsed.recipes[0], {
    id: "legacy-one",
    name: "Legacy 720p",
    settings: baseSettings(),
  });
  assert.equal(parsed.recipes[1].id, "user:migrated-2");
  assert.equal(parsed.recipes[1].name, "Generated ID");
});

test("explicit schema zero objects migrate label and record-level maxEdge fields", () => {
  const parsed = parseUserRecipeStore(JSON.stringify({
    schemaVersion: 0,
    recipes: [
      {
        id: "old",
        label: "Old compact",
        maxEdgePx: "540",
        settings: {
          ...baseSettings(),
          resize: undefined,
          maxEdgePx: undefined,
        },
      },
    ],
  }));

  assert.equal(parsed.migrated, true);
  assert.equal(parsed.recipes[0].settings.resize.mode, "maxEdge");
  assert.equal(parsed.recipes[0].settings.resize.maxEdgePx, "540");
});

test("supported stores salvage valid records and report invalid and duplicate records", () => {
  const parsed = parseUserRecipeStore(JSON.stringify({
    schemaVersion: 1,
    recipes: [
      recipe("User:One", "Share"),
      { ...recipe("user:one", "Different"), name: "Different" },
      recipe("user:two", "SHARE"),
      { id: "broken", name: "Broken", settings: { format: "avi" } },
      recipe("user:three", "Archive"),
    ],
  }));

  assert.deepEqual(parsed.recipes.map(({ id }) => id), ["User:One", "user:three"]);
  assert.equal(parsed.warnings.length, 3);
  assert.match(parsed.warnings[0], /identifier/);
  assert.match(parsed.warnings[1], /name/);
  assert.match(parsed.warnings[2], /invalid/);
  assert.equal(parsed.readOnly, false);
});

test("malformed and structurally corrupt stores recover without throwing", () => {
  const malformed = parseUserRecipeStore("{bad-json");
  assert.deepEqual(malformed.recipes, []);
  assert.match(malformed.warnings[0], /could not be read/);
  assert.equal(malformed.readOnly, false);

  const missingList = parseUserRecipeStore(JSON.stringify({ schemaVersion: 1, recipes: {} }));
  assert.deepEqual(missingList.recipes, []);
  assert.match(missingList.warnings[0], /did not contain a recipe list/);

  const unavailable = loadUserRecipeStore({
    getItem() {
      throw new Error("unavailable");
    },
  });
  assert.match(unavailable.warnings[0], /could not be accessed/);
});

test("future schemas are read-only and persist never overwrites them", () => {
  const raw = JSON.stringify({ schemaVersion: 2, recipes: [recipe()] });
  const current = parseUserRecipeStore(raw);
  const storage = memoryStorage(new Map([[USER_RECIPE_STORAGE_KEY, raw]]));

  assert.equal(current.readOnly, true);
  assert.equal(current.sourceSchemaVersion, 2);
  assert.match(current.warnings[0], /newer app version/);

  const result = persistUserRecipeStore(storage, current, []);
  assert.equal(result.ok, false);
  assert.match(result.error, /newer app version/);
  assert.equal(storage.getItem(USER_RECIPE_STORAGE_KEY), raw);
});

test("persist writes first, returns canonical state, and reports storage failure", () => {
  const storage = memoryStorage();
  const current = createEmptyUserRecipeStore();
  const next = [recipe()];
  const result = persistUserRecipeStore(storage, current, next);

  assert.equal(result.ok, true);
  assert.equal(storage.getItem(USER_RECIPE_STORAGE_KEY), result.raw);
  assert.deepEqual(result.store.recipes, next);
  assert.deepEqual(result.store.warnings, []);

  const failing = {
    setItem() {
      throw new Error("quota exceeded");
    },
  };
  const failed = persistUserRecipeStore(failing, current, next);
  assert.deepEqual(failed, { ok: false, error: "quota exceeded" });
});

test("serializer rejects invalid, duplicate, and oversized stores", () => {
  assert.throws(
    () => serializeUserRecipeStore([recipe("USER:ONE", "One"), recipe("user:one", "Two")]),
    /identifiers must be unique/,
  );
  assert.throws(
    () => serializeUserRecipeStore([recipe("user:one", "One"), recipe("user:two", "oNe")]),
    /names must be unique/,
  );
  assert.throws(
    () => serializeUserRecipeStore(Array.from({ length: USER_RECIPE_MAX_COUNT + 1 }, (_, index) => recipe(`user:${index}`, `R ${index}`))),
    /No more than 50/,
  );
});

test("parser caps supported stores at 50 valid records with a warning", () => {
  const recipes = Array.from({ length: USER_RECIPE_MAX_COUNT + 2 }, (_, index) => recipe(`user:${index}`, `Recipe ${index}`));
  const parsed = parseUserRecipeStore(JSON.stringify({ schemaVersion: 1, recipes }));

  assert.equal(parsed.recipes.length, USER_RECIPE_MAX_COUNT);
  assert.match(parsed.warnings.at(-1), /first 50/);
});

test("create, rename, and delete enforce normalized case-insensitive identity", () => {
  const created = createUserRecipe([], "  My\nshare   recipe  ", baseSettings(), {
    id: "User:One",
  });
  assert.equal(created.ok, true);
  assert.equal(created.recipe.name, "My share recipe");

  const duplicateName = createUserRecipe(created.recipes, "my SHARE recipe", baseSettings(), {
    id: "user:two",
  });
  assert.equal(duplicateName.ok, false);
  assert.match(duplicateName.error, /names must be unique/);

  const duplicateId = createUserRecipe(created.recipes, "Another", baseSettings(), {
    id: "user:one",
  });
  assert.equal(duplicateId.ok, false);
  assert.match(duplicateId.error, /identifiers must be unique/);

  const second = createUserRecipe(created.recipes, "Archive", baseSettings({ sizeLimitMb: "" }), {
    id: "user:two",
  });
  assert.equal(second.ok, true);

  const duplicateRename = renameUserRecipe(second.recipes, "USER:TWO", "my share recipe");
  assert.equal(duplicateRename.ok, false);
  assert.match(duplicateRename.error, /names must be unique/);

  const renamed = renameUserRecipe(second.recipes, "USER:TWO", "  Archive master  ");
  assert.equal(renamed.ok, true);
  assert.equal(renamed.recipe.name, "Archive master");

  const deleted = deleteUserRecipe(renamed.recipes, "USER:ONE");
  assert.equal(deleted.ok, true);
  assert.deepEqual(deleted.recipes.map(({ id }) => id), ["user:two"]);
});

test("name and count bounds are enforced by mutations", () => {
  const validMaxName = "x".repeat(USER_RECIPE_NAME_MAX_LENGTH);
  assert.equal(createUserRecipe([], validMaxName, baseSettings(), { id: "user:max" }).ok, true);
  assert.equal(createUserRecipe([], `${validMaxName}x`, baseSettings(), { id: "user:too-long" }).ok, false);

  const full = Array.from({ length: USER_RECIPE_MAX_COUNT }, (_, index) => recipe(`user:${index}`, `Recipe ${index}`));
  const result = createUserRecipe(full, "Overflow", baseSettings(), { id: "user:overflow" });
  assert.equal(result.ok, false);
  assert.match(result.error, /No more than 50/);
});

test("generated identifiers remain unique within the same millisecond", () => {
  const first = generateUserRecipeId([], 12345);
  const second = generateUserRecipeId([{ ...recipe(), id: first }], 12345);
  assert.equal(first, "user:9ix");
  assert.equal(second, "user:9ix:2");
});

test("exact matching uses normalized reusable settings only", () => {
  const recipes = [recipe()];
  const withForbiddenExtras = {
    ...baseSettings(),
    inputPath: "/private/input.mp4",
    trim: { startS: 10 },
    stripMetadata: false,
  };

  assert.equal(userRecipeMatchesSettings(recipes[0], withForbiddenExtras), true);
  assert.equal(findMatchingUserRecipe(recipes, withForbiddenExtras)?.id, "user:one");
  assert.equal(findMatchingUserRecipe(recipes, baseSettings({ sizeLimitMb: "25" })), null);
});

test("exact serialized recipe JSON cannot contain forbidden request sentinels", () => {
  const sentinel = "DO_NOT_PERSIST_C:/People/Alice/private.mov";
  const created = createUserRecipe([], "Safe", {
    ...baseSettings(),
    inputPath: sentinel,
    outputPath: `${sentinel}.mp4`,
    title: sentinel,
    trim: { sentinel },
    crop: { sentinel },
    colorPolicy: sentinel,
    stripMetadata: false,
    diagnostics: { commandPreview: sentinel },
  }, { id: "user:safe" });
  assert.equal(created.ok, true);

  const raw = serializeUserRecipeStore(created.recipes);
  assert.equal(raw.includes(sentinel), false);
  assert.equal(raw.includes("stripMetadata"), false);
  assert.deepEqual(Object.keys(JSON.parse(raw)), ["schemaVersion", "recipes"]);
});
