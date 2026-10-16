import { DatabaseSync } from 'node:sqlite';
import { readFileSync } from 'node:fs';
import { session } from '../src/crypto.js';

export const secret = 'local-test-only-not-a-real-secret-0123456789';
export const sample = { title: 'Export stopped', version: '2.2.1', platform: 'Windows', steps: 'Open a synthetic clip, export', expected: 'A completed export', actual: 'Export stops', diagnostics: '', consent: true, website: '' };

export function database() {
  const sqlite = new DatabaseSync(':memory:');
  sqlite.exec(readFileSync(new URL('../migrations/0001_reports.sql', import.meta.url), 'utf8'));
  return {
    sqlite,
    prepare(sql) {
      return { bind(...args) {
        return {
          async first() { return sqlite.prepare(sql).get(...args) || null; },
          async run() { const result = sqlite.prepare(sql).run(...args); return { meta: { changes: Number(result.changes) } }; }
        };
      } };
    }
  };
}

export function environment(DB = database()) { return { DB, REPORTING_ENABLED: 'true', GITHUB_APP_ID: '1', GITHUB_INSTALLATION_ID: '2', GITHUB_PRIVATE_KEY: '-----BEGIN PRIVATE KEY-----\nTEST_ONLY\n-----END PRIVATE KEY-----', REPORT_SECRET: secret }; }
export function request(payload, options = {}) { return new Request('https://report.example/api/reports', { method: 'POST', headers: { origin: 'https://report.example', 'content-type': 'application/json', 'CF-Connecting-IP': '192.0.2.10', ...options }, body: JSON.stringify(payload) }); }
export async function submission() { return { ...sample, token: await session(secret, Date.now() - 5000) }; }
