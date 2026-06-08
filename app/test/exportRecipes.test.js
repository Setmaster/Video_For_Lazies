import test from "node:test";
import assert from "node:assert/strict";

import {
  EXPORT_RECIPES,
  findExportRecipe,
  findMatchingExportRecipe,
  normalizeRecipeResizeSettings,
} from "../src/lib/exportRecipes.mjs";

test("built-in export recipes cover the supported output formats", () => {
  const formats = new Set(EXPORT_RECIPES.map((recipe) => recipe.settings.format));

  assert.ok(formats.has("mp4"));
  assert.ok(formats.has("webm"));
  assert.ok(formats.has("mp3"));
  assert.ok(EXPORT_RECIPES.length >= 6);
});

test("findMatchingExportRecipe matches a full recipe settings snapshot", () => {
  const recipe = findExportRecipe("discord-25mb");

  assert.equal(findMatchingExportRecipe(recipe.settings)?.id, "discord-25mb");
});

test("recipe matching treats edited settings as custom", () => {
  const recipe = findExportRecipe("quick-share");
  const changed = {
    ...recipe.settings,
    sizeLimitMb: "25",
  };

  assert.equal(findMatchingExportRecipe(changed), null);
});

test("recipe matching treats edited resize settings as custom", () => {
  const recipe = findExportRecipe("quick-share");
  const changed = {
    ...recipe.settings,
    resize: {
      ...recipe.settings.resize,
      mode: "custom",
      widthPx: "1280",
      heightPx: "720",
    },
  };

  assert.equal(findMatchingExportRecipe(changed), null);
});

test("normalizeRecipeResizeSettings migrates legacy max edge snapshots", () => {
  assert.deepEqual(
    normalizeRecipeResizeSettings({
      maxEdgePx: "720",
    }),
    {
      mode: "maxEdge",
      maxEdgePx: "720",
      widthPx: "",
      heightPx: "",
      lockAspect: true,
    },
  );
});
