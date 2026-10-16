export const REPOSITORY = 'Setmaster/Video_For_Lazies';
export const ISSUE_PREFIX = `https://github.com/${REPOSITORY}/issues/`;
export const FIELDS = { title: 160, steps: 4000, expected: 2000, actual: 4000, version: 80, platform: 20, diagnostics: 8000 };
export const PLATFORMS = ['Windows', 'Linux', 'macOS', 'Other'];

export function validateReport(input) {
  if (!input || typeof input !== 'object' || Array.isArray(input) || input.consent !== true) throw new Error('Confirm that you agree to publish this report.');
  if (Object.keys(input).some(key => ![...Object.keys(FIELDS), 'consent', 'website', 'token'].includes(key))) throw new Error('Unexpected report field.');
  if (typeof input.website !== 'string' || input.website !== '') throw new Error('Unable to accept this report.');
  const report = {};
  for (const [key, max] of Object.entries(FIELDS)) {
    if (typeof input[key] !== 'string' || input[key].length > max || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/u.test(input[key])) throw new Error(`Check the ${key} field (maximum ${max} characters).`);
    report[key] = input[key].trim();
    if (key !== 'diagnostics' && !report[key]) throw new Error(`Fill in the ${key} field.`);
  }
  if (!PLATFORMS.includes(report.platform)) throw new Error('Select an operating system.');
  if (!/^[a-zA-Z0-9.+_-]{1,80}$/.test(report.version)) throw new Error('Use an app version such as 2.2.1, or unknown.');
  if (/[\r\n]/.test(report.title)) throw new Error('Keep the title on one line.');
  return report;
}

// Keep user prose literal, including HTML, Markdown links, images and mentions.
export function literal(value) {
  return value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/@/g, '&#64;');
}

export function issueBody(report, id) {
  const fields = [['App version', report.version], ['Operating system', report.platform], ['Steps to reproduce', report.steps], ['Expected result', report.expected], ['Actual result', report.actual], ['Optional diagnostics (reviewed by reporter)', report.diagnostics || 'Not included']];
  return `Submitted anonymously through the Video For Lazies public bug form.\n\n${fields.map(([name, value]) => `### ${name}\n\n<pre>${literal(value)}</pre>`).join('\n\n')}\n\nReport reference: \`${id}\`\n`;
}

export function validIssueUrl(value) {
  return typeof value === 'string' && new RegExp(`^${ISSUE_PREFIX.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}[1-9][0-9]*$`).test(value);
}
