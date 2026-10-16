import { appJwt } from './crypto.js';
import { REPOSITORY, issueBody, validIssueUrl } from './report.js';

export async function createIssue(env, report, id, fetcher = fetch) {
  // Workers supports manual redirects, not redirect: 'error'. Never forward credentials.
  const headers = { Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28', 'User-Agent': 'Video-For-Lazies-Bug-Reports', 'Content-Type': 'application/json' };
  // Everything before the issue POST is safe to fail without an ambiguous issue.
  let token;
  try {
    const jwt = await appJwt(env.GITHUB_APP_ID, env.GITHUB_PRIVATE_KEY);
    const response = await fetcher(`https://api.github.com/app/installations/${env.GITHUB_INSTALLATION_ID}/access_tokens`, {
      method: 'POST', headers: { ...headers, Authorization: `Bearer ${jwt}` },
      body: JSON.stringify({ repositories: ['Video_For_Lazies'], permissions: { issues: 'write' } }), signal: AbortSignal.timeout(10000), redirect: 'manual'
    });
    if (!response.ok) return { state: 'rejected' };
    token = (await response.json()).token;
    if (typeof token !== 'string' || !token) return { state: 'rejected' };
  } catch { return { state: 'rejected' }; }
  try {
    const response = await fetcher(`https://api.github.com/repos/${REPOSITORY}/issues`, {
      method: 'POST', headers: { ...headers, Authorization: `Bearer ${token}` },
      body: JSON.stringify({ title: `[Bug] ${report.title.replace(/@/g, '＠')}`, body: issueBody(report, id) }), signal: AbortSignal.timeout(15000), redirect: 'manual'
    });
    if (response.status >= 400 && response.status < 500) return { state: 'rejected' };
    if (response.status !== 201) return { state: 'uncertain' };
    const issue = await response.json();
    if (!Number.isSafeInteger(issue.number) || issue.number < 1 || !validIssueUrl(issue.html_url) || issue.html_url !== `https://github.com/${REPOSITORY}/issues/${issue.number}`) return { state: 'uncertain' };
    return { state: 'created', issue_url: issue.html_url };
  } catch { return { state: 'uncertain' }; }
}
