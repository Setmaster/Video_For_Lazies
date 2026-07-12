import test from "node:test";
import assert from "node:assert/strict";

import {
  EXPORT_RECIPES,
  findExportRecipe,
  findMatchingExportRecipe,
  normalizeRecipeResizeSettings,
  recipeMatchesSettings,
} from "../src/lib/exportRecipes.mjs";

test("built-in export recipes cover the supported output formats", () => {
  const formats = new Set(EXPORT_RECIPES.map((recipe) => recipe.settings.format));

  assert.ok(formats.has("mp4"));
  assert.ok(formats.has("webm"));
  assert.ok(formats.has("mp3"));
  assert.ok(EXPORT_RECIPES.length >= 6);
});

test("findMatchingExportRecipe matches a full recipe settings snapshot", () => {
  const recipe = findExportRecipe("discord-10mb");

  assert.equal(findMatchingExportRecipe(recipe.settings)?.id, "discord-10mb");
});

test("full built-in recipes default Strict Fit off and omit the retired permission", () => {
  for (const recipe of EXPORT_RECIPES.filter(({ partial }) => !partial)) {
    assert.equal(recipe.settings.strictFit, false, `${recipe.id} must default Strict Fit off`);
    assert.equal("strictFitAllowAudioRemoval" in recipe.settings, false);
  }
});

test("discord recipe targets the current 10 MB free upload limit", () => {
  const recipe = findExportRecipe("discord-10mb");

  assert.equal(recipe.settings.sizeLimitMb, "10");
  assert.equal(findExportRecipe("discord-25mb"), null);
});

test("forum recipe caps size at 4 MB, drops audio, and uniquifies the first frame", () => {
  const recipe = findExportRecipe("forum-4mb");

  assert.equal(recipe.label, "Forum 4 MB");
  assert.equal(recipe.partial, true);
  // The partial recipe must not touch anything beyond these three settings.
  assert.deepEqual(recipe.settings, {
    sizeLimitMb: "4",
    audioEnabled: false,
    perturbFirstFrame: true,
  });
});

test("partial forum recipe matches a 4 MB cap with audio off and frame uniquify on", () => {
  const base = findExportRecipe("quick-share").settings;

  const forumFromQuickShare = { ...base, sizeLimitMb: "4", audioEnabled: false, perturbFirstFrame: true };
  assert.equal(findMatchingExportRecipe(forumFromQuickShare)?.id, "forum-4mb");

  const archive = findExportRecipe("archive-quality").settings;
  const forumFromArchive = { ...archive, sizeLimitMb: "4", audioEnabled: false, perturbFirstFrame: true };
  assert.equal(findMatchingExportRecipe(forumFromArchive)?.id, "forum-4mb");

  // Any of the three required edits missing means it is not the forum recipe.
  assert.equal(findMatchingExportRecipe({ ...base, sizeLimitMb: "4", perturbFirstFrame: true }), null);
  assert.equal(findMatchingExportRecipe({ ...base, audioEnabled: false, perturbFirstFrame: true }), null);
  assert.equal(findMatchingExportRecipe({ ...base, sizeLimitMb: "4", audioEnabled: false }), null);
});

test("partial recipes only match Strict Fit when they explicitly list it", () => {
  const base = findExportRecipe("quick-share").settings;
  const forumSettings = {
    ...base,
    sizeLimitMb: "4",
    audioEnabled: false,
    perturbFirstFrame: true,
    strictFit: true,
  };
  assert.equal(findMatchingExportRecipe(forumSettings)?.id, "forum-4mb");

  const strictOnly = { partial: true, settings: { strictFit: true } };
  assert.equal(recipeMatchesSettings(strictOnly, forumSettings), true);
  assert.equal(recipeMatchesSettings(strictOnly, { ...forumSettings, strictFit: false }), false);

});

test("full recipe matching canonicalizes Strict Fit defaults and applicability", () => {
  const quickShare = findExportRecipe("quick-share");
  const withoutStrictFit = { ...quickShare.settings };
  delete withoutStrictFit.strictFit;

  assert.equal(findMatchingExportRecipe(withoutStrictFit)?.id, "quick-share");
  assert.equal(
    findMatchingExportRecipe({
      ...quickShare.settings,
      strictFit: true,
    })?.id,
    "quick-share",
  );

  const discord = findExportRecipe("discord-10mb");
  assert.equal(
    findMatchingExportRecipe({
      ...discord.settings,
      strictFit: true,
    }),
    null,
  );
});

test("enabling frame uniquify makes a full recipe snapshot read as custom", () => {
  const discord = findExportRecipe("discord-10mb");
  assert.equal(findMatchingExportRecipe(discord.settings)?.id, "discord-10mb");
  // Non-partial recipes never request perturbation, so turning it on is custom.
  assert.equal(findMatchingExportRecipe({ ...discord.settings, perturbFirstFrame: true }), null);
});

test("normalize-audio edits break recipe matching", () => {
  const recipe = findExportRecipe("quick-share");
  const changed = {
    ...recipe.settings,
    normalizeAudio: true,
  };

  assert.equal(findMatchingExportRecipe(changed), null);
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
