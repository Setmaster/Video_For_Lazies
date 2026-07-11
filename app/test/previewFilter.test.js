import test from "node:test";
import assert from "node:assert/strict";

import { buildPreviewColorFilter } from "../src/lib/previewFilter.mjs";

test("default color adjustments produce no preview filter", () => {
  assert.equal(buildPreviewColorFilter("0", "1", "1"), null);
  assert.equal(buildPreviewColorFilter(0, 1, 1), null);
  // Empty inputs fall back to the per-channel defaults, like the export path.
  assert.equal(buildPreviewColorFilter("", "", ""), null);
  assert.equal(buildPreviewColorFilter(null, undefined, ""), null);
  assert.equal(buildPreviewColorFilter("abc", "xyz", " "), null);
});

test("each adjustment maps onto its CSS filter function", () => {
  assert.equal(buildPreviewColorFilter("0.25", "1", "1"), "brightness(1.25)");
  assert.equal(buildPreviewColorFilter("-0.4", "1", "1"), "brightness(0.6)");
  assert.equal(buildPreviewColorFilter("0", "1.5", "1"), "contrast(1.5)");
  assert.equal(buildPreviewColorFilter("0", "1", "2"), "saturate(2)");
  assert.equal(buildPreviewColorFilter("0.0005", "1", "1"), "brightness(1.0005)");
  assert.equal(
    buildPreviewColorFilter("0.1", "0.8", "1.3"),
    "brightness(1.1) contrast(0.8) saturate(1.3)",
  );
});

test("filter values clamp to the supported adjustment ranges", () => {
  assert.equal(buildPreviewColorFilter("-2", "1", "1"), "brightness(0)");
  assert.equal(buildPreviewColorFilter("5", "1", "1"), "brightness(2)");
  assert.equal(buildPreviewColorFilter("0", "9", "1"), "contrast(2)");
  assert.equal(buildPreviewColorFilter("0", "1", "9"), "saturate(3)");
});
