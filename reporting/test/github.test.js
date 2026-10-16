import test from 'node:test';
import assert from 'node:assert/strict';
import { generateKeyPairSync, createPublicKey, verify } from 'node:crypto';
import { createIssue } from '../src/github.js';
import { appJwt } from '../src/crypto.js';
import { environment, sample } from './helpers.js';
const { privateKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
const pem = privateKey.export({ type: 'pkcs8', format: 'pem' });
const env = { ...environment(), GITHUB_PRIVATE_KEY: pem };

test('GitHub App JWT uses a verified RS256 signature and a short expiry', async () => {
  const token = await appJwt('1234', pem); const [head, body, signature] = token.split('.');
  assert.ok(verify('RSA-SHA256', Buffer.from(`${head}.${body}`), createPublicKey(privateKey), Buffer.from(signature, 'base64url')));
  const payload = JSON.parse(Buffer.from(body, 'base64url'));
  assert.equal(payload.iss, '1234'); assert.equal(payload.exp - payload.iat, 600);
});
test('token is narrowed to repository/issues and never returned to the user', async () => {
  const calls = [];
  const result = await createIssue(env, sample, 'reference', async (url, options) => {
    calls.push({ url, options });
    return calls.length === 1 ? Response.json({ token: 'test-installation-token' }) : Response.json({ number: 8, html_url: 'https://github.com/Setmaster/Video_For_Lazies/issues/8' }, { status: 201 });
  });
  assert.deepEqual(JSON.parse(calls[0].options.body), { repositories: ['Video_For_Lazies'], permissions: { issues: 'write' } });
  assert.equal(calls[1].url, 'https://api.github.com/repos/Setmaster/Video_For_Lazies/issues');
  assert.ok(calls.every(call => call.options.redirect === 'manual'));
  assert.equal(result.state, 'created'); assert.ok(!JSON.stringify(result).includes('token'));
});
test('GitHub redirects are never followed with credentials', async () => {
  for (const redirectAt of [1, 2]) {
    let calls = 0;
    const result = await createIssue(env, sample, 'reference', async (_url, options) => {
      assert.equal(options.redirect, 'manual');
      return ++calls === redirectAt
        ? new Response(null, { status: 302, headers: { Location: 'https://untrusted.example/' } })
        : Response.json({ token: 'test' });
    });
    assert.equal(calls, redirectAt);
    assert.equal(result.state, redirectAt === 1 ? 'rejected' : 'uncertain');
  }
});
test('GitHub failures distinguish safe rejection from uncertain publication', async () => {
  for (const [response, expected] of [[new Response('', { status: 401 }), 'rejected'], [new Response('', { status: 403 }), 'rejected'], [new Response('', { status: 422 }), 'rejected'], [new Response('', { status: 429 }), 'rejected'], [new Response('', { status: 500 }), 'uncertain'], [Response.json({ number: 1, html_url: 'https://evil.test/' }, { status: 201 }), 'uncertain'], [new Response('not JSON', { status: 201 }), 'uncertain']]) {
    let count = 0;
    const result = await createIssue(env, sample, 'reference', async () => ++count === 1 ? Response.json({ token: 'test' }) : response);
    assert.equal(result.state, expected);
  }
  let count = 0;
  assert.equal((await createIssue(env, sample, 'reference', async () => { if (++count === 1) return Response.json({ token: 'test' }); throw new Error('lost response'); })).state, 'uncertain');
  assert.equal((await createIssue(env, sample, 'reference', async () => { throw new Error('token outage'); })).state, 'rejected');
});
