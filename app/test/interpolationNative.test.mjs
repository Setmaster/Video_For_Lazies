import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const worker = process.env.VFL_TEST_RIFE_WORKER;
const model = process.env.VFL_TEST_RIFE_MODEL;
const enabled = Boolean(worker && model);
const qualifiedGpu = process.env.VFL_TEST_RIFE_GPU;
function run(args, input = undefined) {
  const result = spawnSync(worker, args, { input, timeout: 180_000, maxBuffer: 4 * 1024 * 1024, windowsHide: true });
  assert.ifError(result.error);
  return result;
}
function streamArgs(dir = model) { return ['--stream', '--model', dir, '--width', '65', '--height', '37', '--gpu', '-1']; }
function frame(red, green, blue) { return Buffer.from(Array.from({ length: 65 * 37 }, () => [red, green, blue]).flat()); }
function record(alpha, left, right) { const header = Buffer.alloc(4); header.writeFloatLE(alpha); return Buffer.concat([header, left, right]); }

test('native capability and disk queries are machine JSON interfaces', { skip: !enabled }, () => {
  const result = run(['--capabilities']);
  assert.equal(result.status, 0);
  const caps = JSON.parse(result.stdout.toString());
  assert.equal(caps.protocol, 1); assert.equal(caps.cpu, true); assert.ok(Array.isArray(caps.devices));
  if (caps.devices.length === 0) {
    const automatic = run(['--self-test', '--model', model, '--gpu', 'auto']);
    assert.equal(automatic.status, 2);
    assert.match(automatic.stderr.toString(), /vfl-rife: vulkan-(hardware-)?unavailable/);
  }
  const space = run(['--available-space', os.tmpdir()]);
  assert.equal(space.status, 0);
  assert.ok(JSON.parse(space.stdout.toString()).availableBytes > 0);
});
test('native self-test performs non-midpoint inference on odd dimensions', { skip: !enabled }, () => {
  const result = run(['--self-test', '--model', model, '--gpu', '-1']);
  assert.equal(result.status, 0, result.stderr.toString());
  assert.deepEqual(JSON.parse(result.stdout.toString()), { protocol: 1, ok: true, deviceName: 'CPU' });
});
test('persistent records preserve RGB endpoints and produce timestep-dependent inference', { skip: !enabled }, () => {
  const a = frame(180, 70, 25), b = frame(25, 70, 180);
  const result = run(streamArgs(), Buffer.concat([record(0, a, b), record(0.25, a, b), record(0.75, a, b), record(1, a, b)]));
  assert.equal(result.status, 0, result.stderr.toString());
  assert.equal(result.stdout.length, a.length * 4);
  assert.deepEqual(result.stdout.subarray(0, a.length), a);
  assert.deepEqual(result.stdout.subarray(a.length * 3), b);
  const p = result.stdout.subarray(a.length, a.length * 2), q = result.stdout.subarray(a.length * 2, a.length * 3);
  assert.notDeepEqual(p, a); assert.notDeepEqual(p, b); assert.notDeepEqual(p, q);
  const center = (18 * 65 + 32) * 3;
  assert.ok(p[center] > p[center + 2], 'red should remain dominant near the left frame');
  assert.ok(q[center + 2] > q[center], 'blue should become dominant near the right frame');
  assert.ok(Math.abs(p[center + 1] - 70) < 8, 'green channel must not be swapped');
});
test('selected Vulkan device performs RGB inference with fractional timesteps', { skip: !enabled || qualifiedGpu === undefined }, () => {
  const self = run(['--self-test', '--model', model, '--gpu', qualifiedGpu]);
  assert.equal(self.status, 0, self.stderr.toString());
  assert.notEqual(JSON.parse(self.stdout.toString()).deviceName, 'CPU');
  const a = frame(180, 70, 25), b = frame(25, 70, 180);
  const args = streamArgs(); args[args.length - 1] = qualifiedGpu;
  const result = run(args, Buffer.concat([record(0.25, a, b), record(0.75, a, b)]));
  assert.equal(result.status, 0, result.stderr.toString());
  assert.equal(result.stdout.length, a.length * 2);
  const center = (18 * 65 + 32) * 3;
  assert.ok(result.stdout[center] > result.stdout[center + 2]);
  assert.ok(result.stdout[a.length + center + 2] > result.stdout[a.length + center]);
  const moving = offset => {
    const pixels = Buffer.alloc(a.length);
    for (let y = 0; y < 37; y++) for (let x = 0; x < 65; x++) {
      const i = (y * 65 + x) * 3, square = x >= offset && x < offset + 18 && y >= 9 && y < 28;
      pixels[i] = square ? 220 : (x * 3 + y) % 80;
      pixels[i + 1] = square ? 40 + (x + y) % 35 : y * 2;
      pixels[i + 2] = square ? 20 : 40 + x;
    }
    return pixels;
  };
  const input = record(0.5, moving(8), moving(23));
  const gpu = run(args, input), cpu = run(streamArgs(), input);
  assert.equal(gpu.status, 0, gpu.stderr.toString()); assert.equal(cpu.status, 0, cpu.stderr.toString());
  assert.equal(gpu.stdout.length, a.length);
  const meanDifference = gpu.stdout.reduce((sum, value, i) => sum + Math.abs(value - cpu.stdout[i]), 0) / a.length;
  assert.ok(meanDifference < 3, `moving fixture CPU/Vulkan mean byte difference ${meanDifference}`);
  assert.notDeepEqual(gpu.stdout, moving(8)); assert.notDeepEqual(gpu.stdout, moving(23));
});
test('truncated frames and invalid alpha fail without partial output', { skip: !enabled }, () => {
  for (const input of [Buffer.from([0, 0]), Buffer.from([0, 0, 0, 63, 1, 2])]) {
    const result = run(streamArgs(), input);
    assert.equal(result.status, 2); assert.equal(result.signal, null); assert.equal(result.stdout.length, 0);
    assert.match(result.stderr.toString(), /vfl-rife: truncated-record/);
  }
  for (const alpha of [NaN, Infinity, -0.1, 1.1]) {
    const header = Buffer.alloc(4); header.writeFloatLE(alpha);
    const result = run(streamArgs(), header);
    assert.equal(result.status, 2); assert.equal(result.stdout.length, 0);
    assert.match(result.stderr.toString(), /vfl-rife: invalid-alpha/);
  }
});
test('empty stream exits cleanly; malformed options and bounded dimensions fail', { skip: !enabled }, () => {
  assert.equal(run(streamArgs(), Buffer.alloc(0)).status, 0);
  for (const args of [['--stream', '--model', model, '--width', '8192', '--height', '8192'], ['--capabilities', '--width', '1'], ['--capabilities', '--capabilities'], ['--unknown'], ['--stream', '--model', model, '--width', '-1', '--height', '5']]) {
    const result = run(args);
    assert.equal(result.status, 2); assert.equal(result.stdout.length, 0);
  }
});
test('long Unicode model paths load, missing models fail without exposing the path', { skip: !enabled }, async () => {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'vfl-rife-native-'));
  try {
    const long = path.join(temporary, 'é'.repeat(70), '中'.repeat(70), 'model'.repeat(24));
    await fs.mkdir(long, { recursive: true });
    for (const name of ['flownet.bin', 'flownet.param']) await fs.copyFile(path.join(model, name), path.join(long, name));
    const result = run(['--self-test', '--model', long, '--gpu', '-1']);
    assert.equal(result.status, 0, result.stderr.toString());
    const sensitive = path.join(temporary, 'private-source-path-do-not-print');
    const missing = run(['--self-test', '--model', sensitive, '--gpu', '-1']);
    assert.equal(missing.status, 2); assert.equal(missing.signal, null);
    assert.match(missing.stderr.toString(), /vfl-rife: model-file-unavailable/);
    assert.ok(!missing.stderr.toString().includes('private-source-path-do-not-print'));
  } finally { await fs.rm(temporary, { recursive: true, force: true }); }
});
