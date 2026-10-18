import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

test('optional worker framing and runtime assembly checks use no model runtime', (context) => {
  const sandbox = fs.mkdtempSync(path.join(os.tmpdir(), 'vfl-worker-test-'));
  try {
    const env = { PATH: process.env.PATH ?? process.env.Path ?? '', HOME: sandbox,
      USERPROFILE: sandbox, TMPDIR: sandbox, TMP: sandbox, TEMP: sandbox };
    for (const name of ['SystemRoot', 'WINDIR', 'COMSPEC']) {
      if (process.env[name]) env[name] = process.env[name];
    }
    const candidates = process.platform === 'win32'
      ? [['py', ['-3']], ['python3', []], ['python', []]]
      : [['python3', []], ['python', []]];
    let selected;
    for (const [executable, prefix] of candidates) {
      const probe = spawnSync(executable, [...prefix, '-I', '-B', '-c',
        'import sys; print("vfl-python-ok" if sys.version_info >= (3, 11) else "vfl-python-old")'],
      { cwd: sandbox, env, encoding: 'utf8', timeout: 10_000, windowsHide: true });
      if (probe.error?.code === 'ENOENT') continue;
      assert.ifError(probe.error);
      if (probe.status !== 0 && /Python was not found|No suitable Python|No installed Python/i.test(probe.stderr)) continue;
      assert.equal(probe.status, 0, `Python availability check failed: ${probe.stderr}`);
      assert.ok(['vfl-python-ok', 'vfl-python-old'].includes(probe.stdout.trim()), 'Unexpected Python availability response');
      if (probe.status === 0 && probe.stdout.trim() === 'vfl-python-ok') {
        selected = { executable, prefix };
        break;
      }
    }
    if (!selected) {
      context.skip('Python 3.11+ is unavailable; stdlib worker protocol checks were not run.');
      return;
    }
    const tests = path.dirname(fileURLToPath(import.meta.url));
    const result = spawnSync(selected.executable, [...selected.prefix, '-I', '-B', '-m', 'unittest',
      'discover', '-s', tests, '-p', 'test_optional_*.py', '-v'],
    { cwd: sandbox, env, encoding: 'utf8', timeout: 30_000, maxBuffer: 1024 * 1024, windowsHide: true });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stdout + result.stderr);
    context.diagnostic(result.stderr.trim());
  } finally {
    fs.rmSync(sandbox, { recursive: true, force: true });
  }
});
