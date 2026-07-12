import assert from "node:assert/strict";
import test from "node:test";

import { formatClock } from "../src/lib/timeFormat.mjs";

test("clock formatting rounds to centiseconds before decomposing the time", () => {
  assert.equal(formatClock(0), "0:00.00");
  assert.equal(formatClock(1.234), "0:01.23");
  assert.equal(formatClock(61.234), "1:01.23");
  assert.equal(formatClock(3_661.234), "1:01:01.23");
});

test("clock formatting carries rounded centiseconds through minutes and hours", () => {
  assert.equal(formatClock(59.994), "0:59.99");
  assert.equal(formatClock(59.999), "1:00.00");
  assert.equal(formatClock(3_599.999), "1:00:00.00");
  assert.equal(formatClock(86_399.999), "24:00:00.00");
});

test("clock formatting returns the fixed fallback for invalid or negative input", () => {
  assert.equal(formatClock(-0.001), "0:00");
  assert.equal(formatClock(Number.NaN), "0:00");
  assert.equal(formatClock(Number.POSITIVE_INFINITY), "0:00");
  assert.equal(formatClock(Number.MAX_VALUE), "0:00");
});
