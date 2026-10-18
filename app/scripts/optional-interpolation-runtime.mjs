// Maintainer-only pinned fetch/assembly utility. The desktop installer uses Rust
// and the bundled offline Python helper; end users do not need Node or pip.
import fs from 'node:fs/promises';
import { createReadStream } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
export const runtimeLockPath = path.join(appRoot, 'optional-interpolation-runtime.json');
const helper = path.join(appRoot, 'optional-interpolation', 'install_runtime.py');
const allowedHosts = new Set(['files.pythonhosted.org', 'download.pytorch.org', 'download-r2.pytorch.org', 'github.com', 'release-assets.githubusercontent.com']);
const shaPattern = /^[a-f0-9]{64}$/;
const filenamePattern = /^[A-Za-z0-9_.+-]+$/;

export async function hashFile(file) {
  const hash = createHash('sha256');
  for await (const block of createReadStream(file)) hash.update(block);
  return hash.digest('hex');
}

function approvedUrl(value) {
  const url = new URL(value);
  if (url.protocol !== 'https:' || url.username || url.password || url.hash || !allowedHosts.has(url.hostname)) throw new Error('Unapproved artifact URL');
  if (url.hostname === 'github.com' && !url.pathname.startsWith('/astral-sh/python-build-standalone/releases/download/')) throw new Error('Unapproved GitHub artifact');
  return url;
}

export async function loadRuntimeLock(platform, file = runtimeLockPath) {
  const lock = JSON.parse(await fs.readFile(file, 'utf8'));
  if (lock.schema !== 1 || !/^[a-z0-9-]{1,96}$/.test(lock.id) || !['windows-x86_64', 'linux-x86_64'].includes(platform)) throw new Error('Unsupported runtime lock/platform');
  const config = lock.platforms[platform];
  if (!config || !Array.isArray(config.wheels) || config.wheels.length > 100) throw new Error('Invalid runtime artifacts');
  const artifacts = [config.python, ...config.wheels];
  const keys = new Set();
  for (const item of artifacts) {
    if (!shaPattern.test(item.sha256) || !filenamePattern.test(item.filename) || !Number.isSafeInteger(item.bytes) || item.bytes <= 0 || item.bytes > 5 * 1024 ** 3) throw new Error('Invalid artifact pin');
    approvedUrl(item.url);
    const key = `${item.sha256}-${item.filename}`;
    if (keys.has(key)) throw new Error('Duplicate runtime artifact');
    keys.add(key);
  }
  return { lock, config, artifacts };
}

async function regularDirectory(dir) {
  const info = await fs.lstat(dir);
  if (!info.isDirectory() || info.isSymbolicLink()) throw new Error('Expected a regular owned directory');
}

async function regularFile(file, allowMissing = false) {
  try {
    const info = await fs.lstat(file);
    if (!info.isFile() || info.isSymbolicLink()) throw new Error('Expected a regular artifact file');
    return info;
  } catch (error) {
    if (allowMissing && error.code === 'ENOENT') return null;
    throw error;
  }
}

async function boundedResponse(url, headers, signal) {
  for (let redirects = 0; redirects < 6; redirects += 1) {
    const response = await fetch(approvedUrl(url), { headers, redirect: 'manual', signal });
    if ([301, 302, 303, 307, 308].includes(response.status)) {
      const next = response.headers.get('location');
      await response.body?.cancel();
      if (!next) throw new Error('Artifact redirect is missing');
      url = new URL(next, url).href;
      continue;
    }
    return response;
  }
  throw new Error('Artifact redirect limit exceeded');
}

export async function fetchArtifact(item, cache, { signal, onProgress = () => {} } = {}) {
  await regularDirectory(cache);
  const target = path.join(cache, `${item.sha256}-${item.filename}`);
  const existing = await regularFile(target, true);
  if (existing) {
    if (existing.size !== item.bytes || await hashFile(target) !== item.sha256) throw new Error('Existing cached artifact differs from its pin');
    onProgress(item.bytes);
    return target;
  }
  const partial = `${target}.part`;
  const previous = await regularFile(partial, true);
  let completed = previous?.size ?? 0;
  if (completed > item.bytes) throw new Error('Partial artifact exceeds pinned size');
  if (completed < item.bytes) {
    const response = await boundedResponse(item.url, completed ? { Range: `bytes=${completed}-` } : {}, signal);
    const validRange = completed > 0 && response.status === 206 && response.headers.get('content-range') === `bytes ${completed}-${item.bytes - 1}/${item.bytes}`;
    if ((!completed && response.status !== 200) || (completed && !validRange) || !response.body) {
      await response.body?.cancel();
      throw new Error('Artifact server did not honor the bounded download');
    }
    const length = Number(response.headers.get('content-length'));
    if (length !== item.bytes - completed) { await response.body.cancel(); throw new Error('Artifact response length differs from its pin'); }
    const output = await fs.open(partial, completed ? 'a' : 'wx', 0o600);
    try {
      for await (const chunk of response.body) {
        if (signal?.aborted) throw signal.reason;
        if (completed + chunk.byteLength > item.bytes) throw new Error('Artifact download exceeded pinned size');
        let offset = 0;
        while (offset < chunk.byteLength) {
          const { bytesWritten } = await output.write(chunk, offset, chunk.byteLength - offset);
          if (!bytesWritten) throw new Error('Artifact write made no progress');
          offset += bytesWritten;
        }
        completed += chunk.byteLength;
        onProgress(completed);
      }
      await output.sync();
    } finally { await output.close(); }
  }
  if (completed !== item.bytes || await hashFile(partial) !== item.sha256) throw new Error('Artifact bytes/hash differ from their pin');
  // Link with exclusive destination semantics, then remove only our .part name.
  await fs.link(partial, target);
  await fs.unlink(partial);
  return target;
}

export async function assembleRuntime(platform, cache, root, lockFile = runtimeLockPath) {
  const { config } = await loadRuntimeLock(platform, lockFile);
  await regularDirectory(root);
  const relative = config.pythonExecutable;
  if (!relative || relative.includes('\\') || relative.startsWith('/') || relative.split('/').some(p => !p || p === '.' || p === '..')) throw new Error('Invalid Python executable path');
  const python = path.join(root, ...relative.split('/'));
  await regularFile(python);
  const privateState = await fs.mkdtemp(path.join(cache, '.assembly-state-'));
  try {
    const env = {};
    for (const name of ['SystemRoot', 'WINDIR', 'COMSPEC', 'PATHEXT']) if (process.env[name]) env[name] = process.env[name];
    Object.assign(env, { HOME: privateState, USERPROFILE: privateState, TEMP: privateState, TMP: privateState, PYTHONDONTWRITEBYTECODE: '1', HF_HUB_OFFLINE: '1', TRANSFORMERS_OFFLINE: '1' });
    const child = spawn(python, ['-I', '-B', helper, '--lock', path.resolve(lockFile), '--platform', platform, '--cache', path.resolve(cache), '--root', path.resolve(root)], { cwd: cache, env, stdio: ['ignore', 'inherit', 'inherit'], windowsHide: true });
    return await new Promise((resolve, reject) => { child.on('error', reject); child.on('exit', code => code === 0 ? resolve() : reject(new Error(`Offline assembly exited ${code}`))); });
  } finally { await fs.rmdir(privateState).catch(error => { if (error.code !== 'ENOTEMPTY') throw error; }); }
}

async function main() {
  const args = process.argv.slice(2);
  const values = new Map();
  const switches = new Set();
  for (let i = 0; i < args.length; i += 1) {
    const argument = args[i];
    if (values.has(argument) || switches.has(argument)) throw new Error(`Duplicate argument: ${argument}`);
    if (['--help', '--fetch', '--assemble'].includes(argument)) switches.add(argument);
    else if (['--platform', '--cache', '--root'].includes(argument)) {
      const value = args[++i];
      if (!value || value.startsWith('--')) throw new Error(`Missing value for ${argument}`);
      values.set(argument, value);
    } else throw new Error(`Unknown argument: ${argument}`);
  }
  if (switches.has('--help')) {
    if (args.length !== 1) throw new Error('--help must be used alone');
    console.log(`Portable optional-model runtime maintainer utility

Usage:
  node scripts/optional-interpolation-runtime.mjs --platform <platform>
  node scripts/optional-interpolation-runtime.mjs --platform <platform> --fetch --cache <directory>
  node scripts/optional-interpolation-runtime.mjs --platform <platform> --assemble --cache <directory> --root <python-prefix>

Platforms: windows-x86_64, linux-x86_64
Default: print the pinned plan without network access.
--fetch: download/resume exact pinned artifacts into an existing owned cache.
--assemble: assemble wheels offline using a pre-extracted private Python prefix.
--fetch and --assemble may be combined. No pip or system installation runs.`);
    return;
  }
  if (values.has('--root') && !switches.has('--assemble')) throw new Error('--root requires --assemble');
  if (values.has('--cache') && !switches.has('--fetch') && !switches.has('--assemble')) throw new Error('--cache requires --fetch or --assemble');
  const platform = values.get('--platform');
  const { lock, config, artifacts } = await loadRuntimeLock(platform);
  if (!switches.has('--fetch') && !switches.has('--assemble')) {
    console.log(JSON.stringify({ schema: 1, status: 'plan', runtimeId: lock.id, platform, downloadBytes: config.downloadBytes, artifacts }, null, 2));
    return;
  }
  const cache = values.get('--cache');
  if (!cache) throw new Error('--cache must name an existing owned directory');
  await regularDirectory(cache);
  if (switches.has('--fetch')) {
    const controller = new AbortController();
    process.once('SIGINT', () => controller.abort(new Error('Runtime fetch canceled')));
    let before = 0, last = 0;
    for (const item of artifacts) {
      await fetchArtifact(item, cache, { signal: controller.signal, onProgress: completed => {
        const now = Date.now();
        if (now - last > 1000 || completed === item.bytes) { console.log(JSON.stringify({ schema: 1, status: 'downloading', artifact: item.id, completedBytes: before + completed, totalBytes: config.downloadBytes })); last = now; }
      } });
      before += item.bytes;
    }
    console.log(JSON.stringify({ schema: 1, status: 'downloaded', runtimeId: lock.id, bytes: before }));
  }
  if (switches.has('--assemble')) {
    const root = values.get('--root');
    if (!root) throw new Error('--root must name the pre-extracted private Python prefix');
    await assembleRuntime(platform, cache, root);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(JSON.stringify({ schema: 1, status: 'error', message: error.message })); process.exitCode = 1; });
}
