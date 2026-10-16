// All fixture endpoints and fake GitHub behavior live in a disposable entrypoint, never the deploy artifact.
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { session } from '../src/crypto.js';

// Wrangler can finish asynchronous debug-log writes after runtime.stop(). Keep
// its whole process inside the fixture, then remove state only after it exits.
if (!process.env.VFL_REPORT_RUNTIME_CHILD) {
  const temporary = await mkdtemp(join(tmpdir(), 'vfl-report-runtime-'));
  try {
    const child = spawnSync(process.execPath, [fileURLToPath(import.meta.url)], {
      env: { PATH: process.env.PATH, HOME: temporary, XDG_CONFIG_HOME: temporary,
        CI: 'true', WRANGLER_SEND_METRICS: 'false', VFL_REPORT_RUNTIME_CHILD: temporary },
      stdio: 'inherit'
    });
    if (child.error) throw child.error;
    process.exitCode = child.status ?? 1;
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
} else {
  const temporary = process.env.VFL_REPORT_RUNTIME_CHILD;
  const keep = { PATH: process.env.PATH, HOME: temporary, XDG_CONFIG_HOME: temporary, CI: 'true', WRANGLER_SEND_METRICS: 'false' };
  for (const key of Object.keys(process.env)) delete process.env[key];
  Object.assign(process.env, keep);
  let runtime;
  try {
    const { unstable_dev } = await import('wrangler');
    const migration = await readFile(new URL('../migrations/0001_reports.sql', import.meta.url), 'utf8');
    const secret = 'runtime-fixture-only-not-a-real-secret';
    const entry = join(temporary, 'fixture.js');
    await writeFile(entry, `import { handle } from ${JSON.stringify(resolve('src/worker.js'))};
  let writes = 0;
  export default { async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === '/__fixture-init') {
      for (const sql of ${JSON.stringify(migration)}.split(';').filter(s => s.trim())) await env.DB.prepare(sql).run();
      return Response.json({ ready: true });
    }
    if (url.pathname === '/__fixture-state') return Response.json({ writes, rows: (await env.DB.prepare('SELECT * FROM reports').all()).results });
    return handle(request, env, { createIssue: async () => { writes++; return { state: 'created', issue_url: 'https://github.com/Setmaster/Video_For_Lazies/issues/123' }; } });
  } };`);
    const config = join(temporary, 'wrangler.json');
    await writeFile(config, JSON.stringify({
      name: 'vfl-local-fixture', main: entry, compatibility_date: '2026-10-16', workers_dev: false,
      assets: { directory: resolve('public'), binding: 'ASSETS', run_worker_first: true },
      vars: { REPORTING_ENABLED: 'true', REPORT_SECRET: secret, GITHUB_APP_ID: '1', GITHUB_INSTALLATION_ID: '2', GITHUB_PRIVATE_KEY: '-----BEGIN PRIVATE KEY-----\nTEST_ONLY\n-----END PRIVATE KEY-----' },
      d1_databases: [{ binding: 'DB', database_name: 'fixture', database_id: '00000000-0000-0000-0000-000000000001' }]
    }));
    runtime = await unstable_dev(entry, { config, local: true, port: 0, inspectorPort: 0, persist: false, logLevel: 'error', experimental: { disableExperimentalWarning: true, disableDevRegistry: true, watch: false } });
    const base = `http://${runtime.address}:${runtime.port}`;
    const send = (path, options) => fetch(`${base}${path}`, options);
    assert.equal((await send('/__fixture-init')).status, 200);
    const page = await send('/');
    assert.equal(page.status, 200);
    assert.ok(page.headers.get('content-security-policy').includes("frame-ancestors 'none'"));
    assert.ok((await page.text()).includes('Publish public report'));
    assert.deepEqual(await (await send('/api/health')).json(), { ready: true });
    const payload = { title: 'Synthetic local runtime report', version: '2.2.1', platform: 'Linux', steps: 'Local runtime test', expected: 'One issue', actual: 'Fixture issue', diagnostics: '', website: '', consent: true, token: await session(secret, Date.now() - 5000) };
    const submit = body => send('/api/reports', { method: 'POST', headers: { origin: base, 'content-type': 'application/json', 'CF-Connecting-IP': '192.0.2.10' }, body: JSON.stringify(body) });
    const first = await submit(payload);
    assert.equal(first.status, 200, await first.clone().text());
    assert.equal((await first.json()).state, 'created');
    assert.equal((await submit(payload)).status, 200);
    const concurrency = await Promise.all(Array.from({ length: 8 }, async (_, index) => submit({ ...payload, title: `Quota probe ${index}`, token: await session(secret, Date.now() - 5000) })));
    assert.equal(concurrency.filter(response => response.status === 200).length, 2);
    assert.equal(concurrency.filter(response => response.status === 429).length, 6);
    const state = await (await send('/__fixture-state')).json();
    assert.equal(state.writes, 3); assert.equal(state.rows.length, 3);
    assert.ok(!JSON.stringify(state.rows).includes('192.0.2.10'));
    assert.ok(!JSON.stringify(state.rows).includes(payload.title));
    console.log('PASS: real local workerd, static assets/CSP, health, D1 migration, one-write replay, concurrent quota admission and metadata privacy. No GitHub/cloud writes.');
  } finally {
    if (runtime) await runtime.stop();
  }
}
