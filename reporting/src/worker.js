import { hmac, session, verifySession } from './crypto.js';
import { validateReport, validIssueUrl } from './report.js';
import { createIssue } from './github.js';
import { getReport, reserve, finish, cleanup } from './store.js';

const headers = {
  'Cache-Control': 'no-store', 'Referrer-Policy': 'no-referrer', 'X-Content-Type-Options': 'nosniff',
  'Content-Security-Policy': "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
  'Permissions-Policy': 'camera=(), microphone=(), geolocation=()', 'X-Frame-Options': 'DENY'
};
function json(body, status = 200) { return Response.json(body, { status, headers }); }
function configured(env) { return env.REPORTING_ENABLED === 'true' && env.DB && /^[0-9]+$/.test(env.GITHUB_APP_ID || '') && /^[0-9]+$/.test(env.GITHUB_INSTALLATION_ID || '') && env.GITHUB_PRIVATE_KEY?.includes('-----BEGIN PRIVATE KEY-----') && env.REPORT_SECRET?.length >= 32; }
function outcome(row) {
  if (row.state === 'created' && validIssueUrl(row.issue_url)) return json({ state: 'created', issueUrl: row.issue_url, reference: row.id });
  if (row.state === 'rejected') return json({ state: 'rejected', message: 'The report was not published. Save or copy it, then try again later with a new form session.', reference: row.id }, 503);
  return json({ state: 'uncertain', message: 'Publication could not be confirmed. Do not submit a second report. Save your draft and check the issue list for the report reference.', reference: row.id }, 202);
}
async function boundedJson(request) {
  if (!request.headers.get('content-type')?.toLowerCase().startsWith('application/json')) throw new Error('Use JSON.');
  if (Number(request.headers.get('content-length')) > 70000) throw new Error('Report is too large.');
  const reader = request.body?.getReader();
  if (!reader) throw new Error('Missing report.');
  let size = 0; const parts = [];
  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > 70000) { await reader.cancel(); throw new Error('Report is too large.'); }
    parts.push(value);
  }
  const bytes = new Uint8Array(size); let offset = 0;
  for (const part of parts) { bytes.set(part, offset); offset += part.length; }
  return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
}

export async function handle(request, env, dependencies = {}) {
  const url = new URL(request.url);
  if (url.pathname === '/api/health' && request.method === 'GET') return json({ ready: Boolean(configured(env)) });
  if (!url.pathname.startsWith('/api/')) {
    if (!['GET', 'HEAD'].includes(request.method)) return json({ message: 'Method not allowed.' }, 405);
    // Only the form and its two assets are public routes. No SPA fallback for arbitrary paths.
    if (!['/', '/index.html', '/form.js', '/style.css'].includes(url.pathname)) return json({ message: 'Not found.' }, 404);
    const response = await env.ASSETS.fetch(request);
    const secured = new Response(response.body, response);
    for (const [key, value] of Object.entries(headers)) secured.headers.set(key, value);
    return secured;
  }
  if (!configured(env)) return json({ message: 'Reporting is temporarily unavailable. Your draft stays in this tab; use Copy or Save.' }, 503);
  if (url.pathname === '/api/session' && request.method === 'GET') return json({ token: await session(env.REPORT_SECRET) });
  if (url.pathname !== '/api/reports' || request.method !== 'POST') return json({ message: 'Not found.' }, 404);
  if (request.headers.get('origin') !== url.origin) return json({ message: 'Open the reporting form in your browser to submit.' }, 403);
  let input, report;
  try { input = await boundedJson(request); report = validateReport(input); }
  catch (error) { return json({ message: error instanceof SyntaxError ? 'Invalid report.' : error.message }, 400); }
  const nonce = await verifySession(env.REPORT_SECRET, input.token);
  if (!nonce) return json({ state: 'invalid-session', message: 'Wait a few seconds, then check again. If this persists, save your draft and check existing reports before opening a new form.' }, 400);
  const fingerprint = await hmac(env.REPORT_SECRET, `report:${JSON.stringify(report)}`);
  const previous = await getReport(env.DB, nonce.id);
  if (previous) {
    if (previous.fingerprint !== fingerprint) return json({ state: 'conflict', message: 'This report reference is already in use. Save your edited draft and check the original report before submitting again.' }, 409);
    return outcome(previous);
  }
  if (nonce.age > 3600) return json({ state: 'expired', message: 'The form session expired. Your draft is still here; refresh the form session and try again.' }, 400);
  // Only Cloudflare's connecting address is accepted, never X-Forwarded-For or user fields.
  const ip = request.headers.get('CF-Connecting-IP');
  if (!ip) return json({ message: 'Reporting is temporarily unavailable. Use Copy or Save.' }, 503);
  const ipKey = await hmac(env.REPORT_SECRET, `ip:${ip}`);
  const accepted = await reserve(env.DB, nonce.id, fingerprint, ipKey, Math.floor(Date.now() / 1000));
  if (!accepted) {
    const raced = await getReport(env.DB, nonce.id);
    if (raced) return raced.fingerprint === fingerprint ? outcome(raced) : json({ state: 'conflict', message: 'This reference is already in use.' }, 409);
    return json({ state: 'limited', message: 'The report limit has been reached. Save or copy your draft and try later.' }, 429);
  }
  const result = await (dependencies.createIssue || createIssue)(env, report, nonce.id);
  // If persistence fails after GitHub accepted, the pending row still prevents duplicate writes.
  try { await finish(env.DB, nonce.id, result); } catch { return outcome({ id: nonce.id, state: 'uncertain' }); }
  return outcome({ id: nonce.id, ...result });
}

export default {
  async fetch(request, env) {
    try { return await handle(request, env); }
    catch { return json({ message: 'Reporting is temporarily unavailable. Your draft stays in this tab; use Copy or Save.' }, 503); }
  },
  async scheduled(_event, env) { if (env.DB) await cleanup(env.DB); }
};
