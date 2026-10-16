import test from 'node:test';
import assert from 'node:assert/strict';
import worker, { handle } from '../src/worker.js';
import { reserve, cleanup } from '../src/store.js';
import { session } from '../src/crypto.js';
import { database, environment, request, submission, secret } from './helpers.js';
const created = { state: 'created', issue_url: 'https://github.com/Setmaster/Video_For_Lazies/issues/123' };

test('unconfigured service fails closed and nonsensitive health remains available', async () => {
  let response = await worker.fetch(new Request('https://report.example/api/session'), {});
  assert.equal(response.status, 503);
  response = await worker.fetch(new Request('https://report.example/api/health'), {});
  assert.deepEqual(await response.json(), { ready: false });
});
test('success, exact retry and conflicting edit create only one issue', async () => {
  const env = environment(); const payload = await submission(); let writes = 0;
  const dependencies = { createIssue: async () => { writes++; return created; } };
  const first = await handle(request(payload), env, dependencies);
  assert.equal((await first.json()).issueUrl, created.issue_url);
  assert.equal((await handle(request(payload), env, dependencies)).status, 200);
  assert.equal((await handle(request({ ...payload, title: 'Changed' }), env, dependencies)).status, 409);
  assert.equal(writes, 1);
  const row = env.DB.sqlite.prepare('SELECT * FROM reports').get();
  assert.ok(!JSON.stringify(row).includes(payload.title));
  assert.ok(!JSON.stringify(row).includes('192.0.2.10'));
});
test('simultaneous retries preserve the pending reservation and create once', async () => {
  const env = environment(); const payload = await submission(); let writes = 0; let release;
  const gate = new Promise(resolve => { release = resolve; });
  const dependencies = { createIssue: async () => { writes++; await gate; return created; } };
  const first = handle(request(payload), env, dependencies);
  await new Promise(resolve => setTimeout(resolve, 20));
  const retry = await handle(request(payload), env, dependencies);
  assert.equal(retry.status, 202);
  release(); await first; assert.equal(writes, 1);
});
test('uncertain GitHub outcome and failed final persistence never resend', async () => {
  for (const persistFails of [false, true]) {
    const env = environment(); const payload = await submission(); let writes = 0;
    if (persistFails) {
      const original = env.DB.prepare.bind(env.DB);
      env.DB.prepare = sql => { if (sql.startsWith('UPDATE')) throw new Error('DB outage'); return original(sql); };
    }
    const dependencies = { createIssue: async () => { writes++; return persistFails ? created : { state: 'uncertain' }; } };
    assert.equal((await handle(request(payload), env, dependencies)).status, 202);
    assert.equal((await handle(request(payload), env, dependencies)).status, 202);
    assert.equal(writes, 1);
  }
});
test('definite rejection cannot republish with the same nonce', async () => {
  const env = environment(); const payload = await submission(); let writes = 0;
  const dependencies = { createIssue: async () => { writes++; return { state: 'rejected' }; } };
  assert.equal((await handle(request(payload), env, dependencies)).status, 503);
  assert.equal((await handle(request(payload), env, dependencies)).status, 503);
  assert.equal(writes, 1);
});
test('quota admission caps concurrent attempts at three per address and twenty globally', async () => {
  const db = database(); const now = 1800000000;
  const local = await Promise.all(Array.from({ length: 12 }, (_, index) => reserve(db, `local-${index}`, 'fp', 'ip', now)));
  assert.equal(local.filter(Boolean).length, 3);
  const global = await Promise.all(Array.from({ length: 30 }, (_, index) => reserve(db, `global-${index}`, 'fp', `ip-${index}`, now)));
  assert.equal(global.filter(Boolean).length, 17);
  assert.equal(db.sqlite.prepare('SELECT COUNT(*) AS count FROM reports').get().count, 20);
});
test('expired admission cannot write but an existing receipt can be recovered', async () => {
  const env = environment(); const payload = { ...await submission(), token: await session(secret, Date.now() - 3601000) };
  assert.equal((await handle(request(payload), env)).status, 400);
  assert.equal(env.DB.sqlite.prepare('SELECT COUNT(*) AS count FROM reports').get().count, 0);
});
test('origin, media type, body size, missing edge IP and validation fail before GitHub', async () => {
  const env = environment(); const payload = await submission();
  const cases = [request(payload, { origin: 'https://evil.test' }), request(payload, { 'content-type': 'text/plain' }), request({ ...payload, actual: 'x'.repeat(70001) }), request({ ...payload, consent: false })];
  for (const req of cases) assert.ok((await handle(req, env)).status >= 400);
  const noIp = request(payload); noIp.headers.delete('CF-Connecting-IP');
  assert.equal((await handle(noIp, env)).status, 503);
  assert.equal(env.DB.sqlite.prepare('SELECT COUNT(*) AS count FROM reports').get().count, 0);
});
test('cleanup removes expired metadata and retains recent reservations', async () => {
  const db = database(); const now = 1800000000;
  await reserve(db, 'old', 'fp', 'old-ip', now - 8 * 86400);
  await reserve(db, 'new', 'fp', 'new-ip', now);
  await cleanup(db, now);
  assert.deepEqual(db.sqlite.prepare('SELECT id FROM reports').all().map(row => row.id), ['new']);
});
