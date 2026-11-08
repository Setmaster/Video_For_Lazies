import test from 'node:test';
import assert from 'node:assert/strict';
import { sourceFrameNumber, validFrameIndex } from '../src/lib/frameCounter.mjs';

test('one-based CFR rank, boundaries, and exclusive duration', () => {
  const times = [0, 1 / 30, 2 / 30, 3 / 30];
  assert.equal(sourceFrameNumber(times, 0, 0.1), 1);
  assert.equal(sourceFrameNumber(times, 1 / 30, 0.1), 2);
  assert.equal(sourceFrameNumber(times, 0.05, 0.1), 2);
  assert.equal(sourceFrameNumber(times, 0.1, 0.1), 3);
  assert.equal(sourceFrameNumber(times, 50, 0.1), 3);
});

test('VFR holds use source rank rather than timestamp times average FPS', () => {
  const times = [0, 0.01, 0.02, 0.5, 0.6, 0.9];
  assert.equal(sourceFrameNumber(times, 0.49, 1), 3);
  assert.equal(sourceFrameNumber(times, 0.5, 1), 4);
  assert.equal(sourceFrameNumber(times, 1, 1), 6);
});

test('delayed source start has no frame yet; negative PTS retain source ranks', () => {
  assert.equal(sourceFrameNumber([0.2, 0.3], 0, 1), null);
  assert.equal(sourceFrameNumber([0.2, 0.3], 0.2, 1), 1);
  assert.equal(sourceFrameNumber([-0.1, 0, 0.1], 0, 1), 2);
});

test('rounded presented timestamp tolerance never affects an ordinary seek', () => {
  const times = [0, 1 / 30, 2 / 30];
  assert.equal(sourceFrameNumber(times, 0.033333, 1, true), 2);
  assert.equal(sourceFrameNumber(times, 0.033333, 1), 1);
  assert.equal(sourceFrameNumber(times, 0.0333, 1, true), 1);
  assert.equal(sourceFrameNumber([0, 0.000001, 0.000002], 0.0000015, 1, true), 2);
  assert.equal(sourceFrameNumber([0, 0.1], 0.0999999, 0.1, true), 1);
});

test('unavailable inputs never invent frame 1', () => {
  for (const t of [NaN, Infinity, -Infinity]) assert.equal(sourceFrameNumber([0], t, 1), null);
  assert.equal(sourceFrameNumber([], 0, 1), null);
  assert.equal(sourceFrameNumber([0], 0, 0), null);
});

test('IPC index requires matching source identity and complete ordered finite entries', () => {
  assert.equal(validFrameIndex({sourceIdentity:'a',timesS:[0, 0.01, 0.5]}, 'a'), true);
  for (const timesS of [[], [0, 0], [1, 0], [0, NaN], [0, Infinity], ['0'], new Array(100001).fill(0)]) {
    assert.equal(validFrameIndex({sourceIdentity:'a', timesS}, 'a'), false);
  }
  assert.equal(validFrameIndex({sourceIdentity:'old',timesS:[0]}, 'new'), false);
  assert.equal(validFrameIndex(null, 'a'), false);
});
