import test from 'node:test';
import assert from 'node:assert/strict';
import { validateReport, issueBody, validIssueUrl } from '../src/report.js';
import { session, verifySession } from '../src/crypto.js';
import { sample, secret } from './helpers.js';

test('only bounded explicitly consented fields are accepted', () => {
  assert.equal(validateReport(sample).version, '2.2.1');
  for (const patch of [{ consent: false }, { title: '' }, { title: 'x'.repeat(161) }, { steps: null }, { platform: 'C:\\private' }, { version: '/home/user' }, { filename: 'secret.mp4' }, { website: 'bot' }, { actual: '\u0000' }]) assert.throws(() => validateReport({ ...sample, ...patch }));
});
test('public issue prose cannot create markup, embedded media or mentions', () => {
  const body = issueBody({ ...sample, steps: '</pre><img src="https://tracker.test"> @team ![x](https://tracker.test) `code` &amp;' }, 'reference');
  assert.ok(body.includes('&lt;/pre&gt;&lt;img'));
  assert.ok(body.includes('&#64;team'));
  assert.ok(!body.includes('<img'));
  assert.ok(!body.includes('@team'));
  assert.ok(body.includes('<pre>'));
});
test('issue links must identify this exact repository and an issue number', () => {
  assert.ok(validIssueUrl('https://github.com/Setmaster/Video_For_Lazies/issues/123'));
  for (const url of ['javascript:alert(1)', 'https://github.com/Other/Repo/issues/1', 'https://github.com/Setmaster/Video_For_Lazies/issues/1?x=2', 'https://github.com/Setmaster/Video_For_Lazies/issues/0']) assert.ok(!validIssueUrl(url));
});
test('nonce signature, minimum age and maximum recovery age are enforced', async () => {
  const now = Date.now(); const token = await session(secret, now);
  assert.equal(await verifySession(secret, token, now), null);
  assert.ok(await verifySession(secret, token, now + 3000));
  assert.equal(await verifySession(secret, token, now + 7 * 86400000), null);
  assert.equal(await verifySession(secret, `${token.slice(0, -1)}!`, now + 3000), null);
  assert.equal(await verifySession('different secret', token, now + 3000), null);
});
