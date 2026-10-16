const form = document.querySelector('#report-form');
const status = document.querySelector('#status');
const localStatus = document.querySelector('#local-status');
const submit = document.querySelector('#submit');
const refresh = document.querySelector('#refresh');
const keys = ['title', 'version', 'platform', 'steps', 'expected', 'actual', 'diagnostics'];
let token = null;
let inFlight = false;
let submitted = null;
let completed = false;

const params = new URLSearchParams(location.search);
const version = params.get('version');
if (version && /^[a-zA-Z0-9.+_-]{1,80}$/.test(version)) form.elements.version.value = version;
if (['Windows', 'Linux', 'macOS', 'Other'].includes(params.get('platform'))) form.elements.platform.value = params.get('platform');
// Remove all incoming query parameters from browser history after the allowlisted prefill.
history.replaceState(null, '', location.pathname);

function message(text) { status.replaceChildren(document.createTextNode(text)); }
function values() { return Object.fromEntries(keys.map(key => [key, form.elements[key].value])); }
function draft() { const data = values(); const reference = submitted ? `REPORT REFERENCE\n${submitted.token.split('.')[0]}\n\n` : ''; return reference + keys.map(key => `${key.toUpperCase()}\n${data[key]}`).join('\n\n'); }
function freezeReport(frozen) { for (const key of [...keys, 'consent', 'website']) form.elements[key].disabled = frozen; }
async function newSession() {
  refresh.hidden = true;
  try {
    const response = await fetch('/api/session', { cache: 'no-store' });
    const body = await response.json();
    if (!response.ok || typeof body.token !== 'string') throw new Error();
    token = body.token;
    message('Ready. Review your report before publishing.');
  } catch { token = null; message('Reporting is unavailable. You can keep writing, then copy or save your report.'); refresh.hidden = false; }
}

refresh.addEventListener('click', () => { if (!submitted && !inFlight) void newSession(); });
document.querySelector('#copy').addEventListener('click', async () => {
  try { await navigator.clipboard.writeText(draft()); localStatus.textContent = 'Report copied.'; }
  catch { localStatus.textContent = 'Copy is unavailable in this browser. Use Save report instead.'; }
});
document.querySelector('#save').addEventListener('click', () => {
  const url = URL.createObjectURL(new Blob([draft()], { type: 'text/plain;charset=utf-8' }));
  const link = document.createElement('a'); link.href = url; link.download = 'video-for-lazies-bug-report.txt'; link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
  localStatus.textContent = 'Report saved using your browser download settings.';
});

form.addEventListener('submit', async event => {
  event.preventDefault();
  if (inFlight || completed) return;
  if (!token) { message('Reporting is unavailable. Refresh the form session, or copy or save your draft.'); refresh.hidden = false; return; }
  inFlight = true; submit.disabled = true; refresh.hidden = true;
  // Rechecks use the exact submitted snapshot and original nonce; unresolved fields remain locked.
  const payload = submitted || { ...values(), consent: form.elements.consent.checked, website: form.elements.website.value, token };
  submitted = payload;
  freezeReport(true);
  message('Publishing your report. Please keep this tab open.');
  try {
    const response = await fetch('/api/reports', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(payload), signal: AbortSignal.timeout(35000) });
    const result = await response.json();
    if (response.ok && result.state === 'created' && /^https:\/\/github\.com\/Setmaster\/Video_For_Lazies\/issues\/[1-9][0-9]*$/.test(result.issueUrl)) {
      completed = true;
      message('Your public report was published. ');
      const link = document.createElement('a'); link.href = result.issueUrl; link.textContent = 'View your GitHub issue'; status.append(link);
      submit.textContent = 'Report published';
    } else {
      message(result.message || 'Publication could not be confirmed. Save your draft and check existing reports before trying again.');
      if (result.reference) status.append(document.createTextNode(` Reference: ${result.reference}`));
      if (['expired', 'limited'].includes(result.state) || (response.status === 400 && !result.state)) {
        submitted = null;
        freezeReport(false);
        refresh.hidden = result.state !== 'expired';
        submit.textContent = 'Publish public report';
      } else if (result.state === 'rejected') {
        submitted = null; token = null; freezeReport(false); refresh.hidden = false; submit.textContent = 'Publish public report';
      } else { status.append(document.createTextNode(' The submitted fields are locked while the outcome is unresolved; Copy and Save remain available.')); submit.textContent = 'Check report status'; }
    }
  } catch {
    message('The connection failed and publication could not be confirmed. Your submitted fields are locked; Copy and Save remain available. Check report status using the same button; do not submit a second report.');
    submit.textContent = 'Check report status';
  } finally { inFlight = false; submit.disabled = completed; status.focus(); }
});

window.addEventListener('beforeunload', event => {
  if (!completed && ['title', 'steps', 'expected', 'actual', 'diagnostics'].some(key => form.elements[key].value)) { event.preventDefault(); event.returnValue = ''; }
});
void newSession();
