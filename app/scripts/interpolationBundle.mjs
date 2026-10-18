import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';

export const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
export const interpolationSidecarResourceTarget = 'interpolation-sidecar';
export const interpolationBundleRoot = path.join(appRoot, '.interpolation-bundle');
export const interpolationNativeRoot = path.join(appRoot, 'native', 'interpolation');
export const optionalInterpolationRoot = path.join(appRoot, 'optional-interpolation');
export const optionalInterpolationFiles = Object.freeze([
  'worker.py', 'gmfss.py', 'gimm.py', 'ldf.py', 'install_runtime.py',
  'gmfss-files.json', 'gimm-files.json', 'ldf-files.json',
]);
export const interpolationSidecarDir = path.join(interpolationBundleRoot, 'current', interpolationSidecarResourceTarget);
export const tauriInterpolationResourceSource = '../.interpolation-bundle/current/interpolation-sidecar';
export const INTERPOLATION_NATIVE = Object.freeze({
  protocol: 1,
  rifeSourceCommit: 'f1de3a97c49e1bed4a625aa8eef92586a4871e74',
  ncnn: {
    commit: 'c4193aadbbb56582aa87b1850dd3d98fb8fd936d',
    url: 'https://codeload.github.com/Tencent/ncnn/tar.gz/c4193aadbbb56582aa87b1850dd3d98fb8fd936d',
    sha256: '6005bbdefb83ad6da63b2fa200d04c59882d09c9c62e977aa6325a0a6a4822ab',
  },
  glslang: {
    commit: '8cd77a808d0bffa442ae9462d5e3a8141892ba5a',
    url: 'https://codeload.github.com/KhronosGroup/glslang/tar.gz/8cd77a808d0bffa442ae9462d5e3a8141892ba5a',
    sha256: '2ce175c900a8a531839ff88f5f311f1d981ffb9ae4022d657b187468a084d190',
  },
});

export function getInterpolationSidecarDir() { return interpolationSidecarDir; }
export function getInterpolationExecutableName(platform = process.platform) {
  if (!['linux', 'win32'].includes(platform)) throw new Error(`Unsupported interpolation platform: ${platform}`);
  return platform === 'win32' ? 'vfl-rife.exe' : 'vfl-rife';
}
export async function sha256File(file) {
  const hash = createHash('sha256');
  const handle = await fs.open(file, 'r');
  try { for await (const chunk of handle.createReadStream()) hash.update(chunk); }
  finally { await handle.close(); }
  return hash.digest('hex');
}
export async function readInterpolationCatalog() {
  return JSON.parse(await fs.readFile(path.join(appRoot, 'interpolation-models.json'), 'utf8'));
}
export async function getInterpolationSourceDigest() {
  const hash = createHash('sha256');
  async function add(dir, relative = '') {
    for (const entry of (await fs.readdir(dir, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
      const next = path.join(dir, entry.name), label = path.posix.join(relative, entry.name);
      if (entry.isDirectory()) await add(next, label);
      else if (entry.isFile()) { hash.update(label); hash.update(await fs.readFile(next)); }
      else throw new Error('Native source tree contains a nonregular entry');
    }
  }
  await add(interpolationNativeRoot);
  for (const name of optionalInterpolationFiles) {
    hash.update(`optional/${name}`);
    hash.update(await fs.readFile(path.join(optionalInterpolationRoot, name)));
  }
  hash.update(await fs.readFile(path.join(appRoot, 'optional-interpolation-runtime.json')));
  hash.update(await fs.readFile(fileURLToPath(import.meta.url)));
  hash.update(await fs.readFile(path.join(appRoot, 'scripts', 'build-interpolation-sidecar.mjs')));
  hash.update(await fs.readFile(path.join(appRoot, 'interpolation-models.json')));
  return hash.digest('hex');
}
// Import lazily so packaging can import the layout without invoking a build.
export async function ensureInterpolationBundle(options = {}) {
  return (await import('./build-interpolation-sidecar.mjs')).ensureInterpolationBundle(options);
}
