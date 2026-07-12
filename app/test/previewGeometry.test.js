import assert from "node:assert/strict";
import test from "node:test";

import {
  displayPointToSourcePoint,
  normalizeQuarterTurn,
  quarterTurnSwapsAxes,
  rotatedAspectRatio,
  sourcePointToDisplayPoint,
  sourceRectToDisplayRect,
} from "../src/lib/previewGeometry.mjs";
import {
  cropRectToPixels,
  pixelAspectToNormalizedRatio,
} from "../src/lib/accessibility.mjs";

function assertPointAlmostEqual(actual, expected) {
  assert.ok(Math.abs(actual.x - expected.x) < 1e-12, `expected x=${expected.x}, saw ${actual.x}`);
  assert.ok(Math.abs(actual.y - expected.y) < 1e-12, `expected y=${expected.y}, saw ${actual.y}`);
}

test("quarter-turn rotation normalization wraps exact positive and negative turns", () => {
  for (const [input, expected] of [
    [0, 0],
    [90, 90],
    [180, 180],
    [270, 270],
    [360, 0],
    [450, 90],
    [-90, 270],
    [-450, 270],
    [45, 0],
    [Number.NaN, 0],
  ]) {
    assert.equal(normalizeQuarterTurn(input), expected, String(input));
  }
});

test("quarter turns swap the displayed aspect only for 90 and 270 degrees", () => {
  assert.equal(quarterTurnSwapsAxes(0), false);
  assert.equal(quarterTurnSwapsAxes(90), true);
  assert.equal(quarterTurnSwapsAxes(180), false);
  assert.equal(quarterTurnSwapsAxes(270), true);
  assert.equal(rotatedAspectRatio(1_920, 1_080, 0), 16 / 9);
  assert.equal(rotatedAspectRatio(1_920, 1_080, 90), 9 / 16);
  assert.equal(rotatedAspectRatio(1_920, 1_080, 180), 16 / 9);
  assert.equal(rotatedAspectRatio(1_920, 1_080, 270), 9 / 16);
  assert.equal(rotatedAspectRatio(0, 1_080, 90), null);
});

test("source rectangles map to the correct displayed quarter-turn rectangle", () => {
  const source = { x: 0.125, y: 0.25, w: 0.375, h: 0.5 };
  assert.deepEqual(sourceRectToDisplayRect(source, 0), source);
  assert.deepEqual(sourceRectToDisplayRect(source, 90), { x: 0.25, y: 0.125, w: 0.5, h: 0.375 });
  assert.deepEqual(sourceRectToDisplayRect(source, 180), { x: 0.5, y: 0.25, w: 0.375, h: 0.5 });
  assert.deepEqual(sourceRectToDisplayRect(source, 270), { x: 0.25, y: 0.5, w: 0.5, h: 0.375 });
});

test("display pointer mapping is the inverse of source mapping for every quarter turn", () => {
  const points = [
    { x: 0, y: 0 },
    { x: 1, y: 0 },
    { x: 0, y: 1 },
    { x: 1, y: 1 },
    { x: 0.2, y: 0.7 },
  ];
  for (const rotation of [0, 90, 180, 270]) {
    for (const source of points) {
      const display = sourcePointToDisplayPoint(source, rotation);
      assertPointAlmostEqual(displayPointToSourcePoint(display, rotation), source);
    }
  }
});

test("pointer mapping clamps positions to the normalized display bounds", () => {
  assert.deepEqual(displayPointToSourcePoint({ x: -2, y: 3 }, 0), { x: 0, y: 1 });
  assert.deepEqual(displayPointToSourcePoint({ x: -2, y: 3 }, 90), { x: 1, y: 1 });
});

test("rotated preview geometry preserves source-pixel crop truth and swaps the visible aspect", () => {
  const sourceRect = { x: 0.125, y: 0.25, w: 0.5, h: 0.25 };
  assert.deepEqual(cropRectToPixels(sourceRect, 1_920, 1_080), {
    x: 240,
    y: 270,
    width: 960,
    height: 270,
  });

  const displayed = sourceRectToDisplayRect(sourceRect, 90);
  assert.deepEqual(displayed, { x: 0.5, y: 0.125, w: 0.25, h: 0.5 });
  assert.deepEqual(
    { width: displayed.w * 1_080, height: displayed.h * 1_920 },
    { width: 270, height: 960 },
  );

  const lockedSourceRatio = pixelAspectToNormalizedRatio(16 / 9, 1_920, 1_080);
  assert.equal(lockedSourceRatio, 1);
  const lockedSourceRect = { x: 0.25, y: 0.25, w: 0.5, h: 0.5 };
  const sourcePixels = cropRectToPixels(lockedSourceRect, 1_920, 1_080);
  assert.equal(sourcePixels.width / sourcePixels.height, 16 / 9);
  const rotatedLockedRect = sourceRectToDisplayRect(lockedSourceRect, 90);
  assert.equal(
    (rotatedLockedRect.w * 1_080) / (rotatedLockedRect.h * 1_920),
    9 / 16,
  );
});
