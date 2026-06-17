import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));
const read = (rel) => fs.readFile(path.resolve(__dirname, rel), "utf8");

test("backend builds a timeline-gated noise filter for the first frame", async () => {
  const video = await read("../src-tauri/src/video.rs");

  // The request carries the toggle and an optional deterministic seed.
  assert.match(video, /pub perturb_first_frame: bool/);
  assert.match(video, /pub perturb_seed: Option<u32>/);

  // The filter is uniform noise on frame 0, with the comma in eq(n,0) escaped
  // so it survives the comma-join with the rest of the filterchain.
  assert.match(video, /fn first_frame_perturb_filter/);
  assert.match(video, /noise=alls=\{FIRST_FRAME_PERTURB_STRENGTH\}:allf=u:all_seed=\{seed\}:enable='eq\(n\\\\,0\)'/);

  // Strength must stay >= 2 or the re-encode quantizes it away (see research).
  assert.match(video, /const FIRST_FRAME_PERTURB_STRENGTH: u32 = 3;/);

  // It must be appended last and must disable the stream-copy fast path.
  assert.match(video, /Must be LAST[\s\S]*first_frame_perturb_filter\(req\)/);
  assert.match(video, /&& !request\.perturb_first_frame\b/);
});

test("frontend plumbs perturbFirstFrame through requests, recipes, and reset", async () => {
  const app = await read("../src/App.tsx");

  assert.match(app, /const \[perturbFirstFrame, setPerturbFirstFrame\] = useState\(false\);/);
  // Both request builders forward it (and force it off for audio-only mp3).
  const forwards = app.match(/perturbFirstFrame: format === "mp3" \? false : perturbFirstFrame,/g) ?? [];
  assert.ok(forwards.length >= 2, "both buildRequest and buildSettingsOnlyRequest forward the flag");
  // Recipe apply: partial branch sets it, full branch resets it.
  assert.match(app, /if \(partialSettings\.perturbFirstFrame !== undefined\) setPerturbFirstFrame\(partialSettings\.perturbFirstFrame\);/);
  assert.match(app, /setPerturbFirstFrame\(Boolean\(recipeSettings\.perturbFirstFrame\)\);/);
  // Reset returns it to off, and it participates in recipe matching.
  assert.match(app, /resetAllSettings[\s\S]*setPerturbFirstFrame\(false\);/);
  // A user-facing control exists.
  assert.match(app, /Make each export unique/);
});

test("types and recipe metadata expose the flag", async () => {
  const types = await read("../src/lib/types.ts");
  assert.match(types, /perturbFirstFrame: boolean;/);
  assert.match(types, /perturbFirstFrame\?: boolean \| null;/); // smoke config

  const recipesDts = await read("../src/lib/exportRecipes.d.ts");
  assert.match(recipesDts, /perturbFirstFrame\?: boolean;/);
  assert.match(recipesDts, /"normalizeAudio" \| "perturbFirstFrame"/);
});
