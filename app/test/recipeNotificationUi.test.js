import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

async function read(relativePath) {
  return fs.readFile(path.resolve(__dirname, relativePath), "utf8");
}

function between(source, startMarker, endMarker) {
  const start = source.indexOf(startMarker);
  const end = source.indexOf(endMarker, start + startMarker.length);
  assert.ok(start >= 0, `Missing start marker: ${startMarker}`);
  assert.ok(end > start, `Missing end marker after ${startMarker}: ${endMarker}`);
  return source.slice(start, end);
}

test("successful recipe notices hold for 3 seconds before fading", async () => {
  const [app, css] = await Promise.all([
    read("../src/App.tsx"),
    read("../src/App.css"),
  ]);

  assert.match(app, /const RECIPE_NOTIFICATION_FADE_START_MS = 3000;/);
  assert.match(app, /const RECIPE_NOTIFICATION_CLEAR_MS = 3200;/);

  const transientHelper = between(
    app,
    "function showRecipeNotification(message: string)",
    "useEffect(() => () => clearRecipeNotificationTimers(), []);",
  );
  assert.match(transientHelper, /clearRecipeNotificationTimers\(\)/);
  assert.match(transientHelper, /setRecipeStatus\(null\)/);
  assert.match(transientHelper, /setRecipeNotification\(\{ message, isFading: false \}\)/);
  assert.match(transientHelper, /isFading: true/);
  assert.match(transientHelper, /RECIPE_NOTIFICATION_FADE_START_MS/);
  assert.match(transientHelper, /RECIPE_NOTIFICATION_CLEAR_MS/);
  assert.match(transientHelper, /current\?\.message === message/);

  assert.match(app, /useEffect\(\(\) => \(\) => clearRecipeNotificationTimers\(\), \[\]\);/);
  assert.match(app, /const visibleRecipeStatus = recipeStatus \?\? recipeNotification\?\.message \?\? null;/);
  assert.match(app, /const recipeStatusIsError = recipeStatus !== null && userRecipeStore\.readOnly;/);
  assert.match(app, /recipeNotification\?\.isFading === true/);
  assert.match(app, /recipeStatusIsFading \? " is-fading" : ""/);

  assert.match(css, /\.vfl-recipe-status \{[\s\S]*?opacity: 1;[\s\S]*?transition: opacity 200ms ease;/);
  assert.match(css, /\.vfl-recipe-status\.is-fading \{\s*opacity: 0;\s*\}/);
  assert.match(css, /@media \(prefers-reduced-motion: reduce\)[\s\S]*?transition: none !important;/);
});

test("recipe warnings stay persistent while successes avoid the global footer", async () => {
  const app = await read("../src/App.tsx");

  const persistentHelper = between(
    app,
    "function showPersistentRecipeStatus(message: string | null)",
    "function showRecipeNotification(message: string)",
  );
  assert.match(persistentHelper, /clearRecipeNotificationTimers\(\)/);
  assert.match(persistentHelper, /setRecipeNotification\(null\)/);
  assert.match(persistentHelper, /setRecipeStatus\(message\)/);

  const applyRecipe = between(app, "function applyFullRecipeSettings", "function applyExportRecipe");
  assert.match(applyRecipe, /showRecipeNotification\(/);
  assert.doesNotMatch(applyRecipe, /setStatus\(|setRecipeStatus\(/);

  const persistRecipe = between(app, "function persistRecipeMutation", "function confirmRecipeDialog");
  assert.match(persistRecipe, /showPersistentRecipeStatus\(persisted\.error\)/);
  assert.match(persistRecipe, /showRecipeNotification\(successMessage\)/);
  assert.doesNotMatch(persistRecipe, /setStatus\(|setRecipeStatus\(/);

  assert.match(app, /showPersistentRecipeStatus\(loaded\.warnings\.join\(" "\)\)/);
  assert.match(app, /showPersistentRecipeStatus\("Saved recipes were created by a newer app version/);
  assert.match(app, /showPersistentRecipeStatus\(originalRecipeStatus\)/);
});
