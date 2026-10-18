import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import {
  appRoot, interpolationBundleRoot, interpolationNativeRoot, interpolationSidecarDir,
  INTERPOLATION_NATIVE, getInterpolationExecutableName, readInterpolationCatalog,
  getInterpolationSourceDigest, sha256File,
  optionalInterpolationRoot, optionalInterpolationFiles,
} from './interpolationBundle.mjs';

const execFileAsync = promisify(execFile);
async function cmakeExecutable(platform) {
  if (platform !== 'win32') return 'cmake';
  try { await execFileAsync('cmake', ['--version'], { windowsHide: true }); return 'cmake'; } catch { /* Try Visual Studio's installed CMake. */ }
  const installer = path.join(process.env['ProgramFiles(x86)'] || 'C:\\Program Files (x86)', 'Microsoft Visual Studio', 'Installer', 'vswhere.exe');
  const { stdout } = await execFileAsync(installer, ['-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath'], { windowsHide: true });
  const cmake = path.join(stdout.trim(), 'Common7', 'IDE', 'CommonExtensions', 'Microsoft', 'CMake', 'CMake', 'bin', 'cmake.exe');
  await fs.access(cmake);
  return cmake;
}

function run(executable, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(executable, args, { stdio: ['ignore', 'inherit', 'inherit'], windowsHide: true, ...options });
    child.on('error', reject);
    child.on('exit', (code, signal) => code === 0 ? resolve() : reject(new Error(`${executable} failed (${signal || code})`)));
  });
}
async function downloadPinned(url, destination, expectedHash, expectedBytes) {
  try {
    const st = await fs.stat(destination);
    if ((!expectedBytes || st.size === expectedBytes) && await sha256File(destination) === expectedHash) return;
  } catch (e) { if (e.code !== 'ENOENT') throw e; }
  await fs.mkdir(path.dirname(destination), { recursive: true });
  const temporary = `${destination}.${process.pid}.part`;
  try {
    const response = await fetch(url, { headers: { 'Accept-Encoding': 'identity' }, signal: AbortSignal.timeout(180_000) });
    if (!response.ok || !response.body) throw new Error(`Pinned source/model download failed: HTTP ${response.status}`);
    const declared = Number(response.headers.get('content-length') || 0);
    const limit = expectedBytes || 64 * 1024 * 1024;
    const compressed = response.headers.get('content-encoding');
    if ((!compressed || compressed === 'identity') && (declared > limit || (expectedBytes && declared && declared !== expectedBytes))) throw new Error('Pinned download length mismatch');
    const handle = await fs.open(temporary, 'wx');
    let total = 0;
    try {
      for await (const chunk of response.body) {
        total += chunk.length;
        if (total > limit) throw new Error('Pinned download exceeds byte limit');
        await handle.writeFile(chunk);
      }
    } finally { await handle.close(); }
    if ((expectedBytes && total !== expectedBytes) || await sha256File(temporary) !== expectedHash) throw new Error('Pinned download verification failed');
    await fs.rename(temporary, destination);
  } finally { await fs.rm(temporary, { force: true }); }
}
async function verifiedCurrent(platform, sourceDigest, catalog) {
  try {
    const manifest = JSON.parse(await fs.readFile(path.join(interpolationSidecarDir, 'bundle-provenance.json'), 'utf8'));
    if (!manifest || !Array.isArray(manifest.files) || manifest.sourceDigest !== sourceDigest || manifest.platform !== platform || manifest.arch !== 'x64' || manifest.protocol !== 1) return false;
    for (const file of manifest.files) {
      if (!file || typeof file.path !== 'string' || file.path.includes('..') || path.isAbsolute(file.path) || !Number.isSafeInteger(file.bytes) || !/^[a-f0-9]{64}$/.test(file.sha256)) return false;
      const actual = path.join(interpolationSidecarDir, file.path);
      const st = await fs.lstat(actual);
      if (!st.isFile() || st.size !== file.bytes || await sha256File(actual) !== file.sha256) return false;
    }
    const defaultModel = catalog.models.find(m => m.id === catalog.defaultModelId);
    for (const file of defaultModel.files) {
      const actual = path.join(interpolationSidecarDir, 'models', defaultModel.id, file.name);
      if ((await fs.stat(actual)).size !== file.bytes || await sha256File(actual) !== file.sha256) return false;
    }
    return manifest.files.some(f => f.path === getInterpolationExecutableName(platform));
  } catch (e) { if (e.code === 'ENOENT' || e instanceof SyntaxError) return false; throw e; }
}
async function fileManifest(dir, relative = '') {
  const files = [];
  for (const entry of (await fs.readdir(dir, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
    const label = path.posix.join(relative, entry.name), full = path.join(dir, entry.name);
    if (entry.isDirectory()) files.push(...await fileManifest(full, label));
    else if (entry.isFile()) files.push({ path: label, bytes: (await fs.stat(full)).size, sha256: await sha256File(full) });
    else throw new Error('Unexpected nonregular interpolation bundle entry');
  }
  return files;
}
async function buildInterpolationBundle({ force = false, jobs = 4, platform = process.platform } = {}) {
  if (platform !== process.platform || process.arch !== 'x64') throw new Error('Build the interpolation worker natively on Windows/Linux x64');
  const exe = getInterpolationExecutableName(platform), sourceDigest = await getInterpolationSourceDigest(), catalog = await readInterpolationCatalog();
  if (!force && await verifiedCurrent(platform, sourceDigest, catalog)) return interpolationSidecarDir;
  if (!Number.isInteger(jobs) || jobs < 1 || jobs > 4) throw new Error('Interpolation build jobs must be 1..4');
  const downloads = path.join(interpolationBundleRoot, 'downloads'), deps = path.join(interpolationBundleRoot, 'deps');
  const ncnnDir = path.join(deps, `ncnn-${INTERPOLATION_NATIVE.ncnn.commit}`);
  await fs.mkdir(ncnnDir, { recursive: true });
  const sourceArchives = [];
  for (const [name, pin] of Object.entries(INTERPOLATION_NATIVE).filter(([name]) => ['ncnn', 'glslang'].includes(name))) {
    const archiveName = `${name}-${pin.commit}.tar.gz`, archive = path.join(downloads, archiveName);
    await downloadPinned(pin.url, archive, pin.sha256);
    sourceArchives.push({ name, pin, archiveName, archive });
    const destination = name === 'ncnn' ? ncnnDir : path.join(ncnnDir, 'glslang');
    await fs.mkdir(destination, { recursive: true });
    // Always re-extract hash-verified source before a build, discarding stale source edits.
    await run('tar', ['-xzf', archive, '-C', destination, '--strip-components=1']);
  }
  const build = path.join(interpolationBundleRoot, `build-${platform}-x64`);
  const configure = ['-S', interpolationNativeRoot, '-B', build, '-DCMAKE_BUILD_TYPE=Release', `-DVFL_NCNN_SOURCE=${ncnnDir}`];
  if (platform === 'win32') configure.push('-A', 'x64');
  else configure.push('-G', 'Ninja');
  const cmake = await cmakeExecutable(platform);
  await run(cmake, configure);
  await run(cmake, ['--build', build, '--config', 'Release', '-j', String(jobs)]);
  const staging = await fs.mkdtemp(path.join(interpolationBundleRoot, 'stage-'));
  const target = path.join(staging, 'interpolation-sidecar');
  let preserveStaging = false;
  try {
    await fs.mkdir(target);
    const binary = path.join(build, ...(platform === 'win32' ? ['Release', exe] : [exe]));
    await fs.copyFile(binary, path.join(target, exe));
    if (platform === 'linux') await fs.chmod(path.join(target, exe), 0o755);
    const source = path.join(target, 'source');
    await fs.mkdir(source);
    for (const item of sourceArchives) await fs.copyFile(item.archive, path.join(source, item.archiveName));
    await fs.cp(interpolationNativeRoot, path.join(source, 'vfl-rife'), { recursive: true });
    await fs.mkdir(path.join(target, 'optional'));
    await fs.mkdir(path.join(source, 'vfl-optional'));
    for (const name of optionalInterpolationFiles) {
      await fs.copyFile(path.join(optionalInterpolationRoot, name), path.join(target, 'optional', name));
      await fs.copyFile(path.join(optionalInterpolationRoot, name), path.join(source, 'vfl-optional', name));
    }
    await fs.copyFile(path.join(appRoot, 'optional-interpolation-runtime.json'), path.join(source, 'optional-interpolation-runtime.json'));
    await fs.copyFile(path.join(appRoot, 'scripts', 'interpolationBundle.mjs'), path.join(source, 'interpolationBundle.mjs'));
    await fs.copyFile(fileURLToPath(import.meta.url), path.join(source, 'build-interpolation-sidecar.mjs'));
    await fs.copyFile(path.join(appRoot, 'interpolation-models.json'), path.join(target, 'model-catalog.json'));
    await fs.copyFile(path.join(appRoot, '..', 'LICENSE'), path.join(target, 'LICENSE.txt'));
    const licenseDir = path.join(target, 'licenses');
    await fs.mkdir(licenseDir);
    await fs.copyFile(path.join(ncnnDir, 'LICENSE.txt'), path.join(licenseDir, 'ncnn-LICENSE.txt'));
    await fs.copyFile(path.join(ncnnDir, 'glslang', 'LICENSE.txt'), path.join(licenseDir, 'glslang-LICENSE.txt'));
    await fs.copyFile(path.join(interpolationNativeRoot, 'vendor', 'rife', 'LICENSE'), path.join(licenseDir, 'rife-MIT.txt'));
    await fs.copyFile(path.join(interpolationNativeRoot, 'model-license.txt'), path.join(licenseDir, 'model-license.txt'));
    const model = catalog.models.find(m => m.id === catalog.defaultModelId);
    if (!model) throw new Error('Default interpolation model missing from catalog');
    if (!/^rife-v4\.(25|26)$/.test(model.id) || !/^[a-zA-Z0-9.-]+$/.test(catalog.catalogVersion)) throw new Error('Unsupported catalog model identity');
    for (const file of model.files) {
      if (!['flownet.bin', 'flownet.param'].includes(file.name)) throw new Error('Invalid catalog model filename');
      const cached = path.join(downloads, 'models', catalog.catalogVersion, model.id, file.name);
      await downloadPinned(file.url, cached, file.sha256, file.bytes);
      const output = path.join(target, 'models', model.id, file.name);
      await fs.mkdir(path.dirname(output), { recursive: true });
      await fs.copyFile(cached, output);
    }
    await fs.writeFile(path.join(target, 'INTERPOLATION_NOTICES.txt'),
      `Video For Lazies interpolation sidecar, protocol 1\nNative wrapper and optional worker adapters: GPL-3.0-or-later; see LICENSE.txt, source/vfl-rife and source/vfl-optional.\nRIFE Warp layer derives from k4yt3x/librife-ncnn-vulkan ${INTERPOLATION_NATIVE.rifeSourceCommit} (MIT), with validation, border and error handling changes documented in its README.\nncnn ${INTERPOLATION_NATIVE.ncnn.commit} (BSD-3-Clause), glslang ${INTERPOLATION_NATIVE.glslang.commit} (see complete multi-party license).\nOriginal model: Practical-RIFE 4.25, linked weights MIT per upstream declaration in licenses/model-license.txt. Converted model catalog/source and file hashes are recorded in model-catalog.json and bundle-provenance.json. This build consumes the published model conversion; it does not regenerate it. See source/vfl-rife/README.md.\nGPU drivers including Vulkan are provided by the operating system/vendor. Bundled RIFE requires no Python or CUDA setup. Optional GMFSS, GIMM and LDF use separately downloaded, app-local pinned runtimes, sources and weights. Their per-file manifests and runtime lock are included under source/. Downloaded components retain their own licenses and notices; GIMM is noncommercial. No optional model weights or Torch runtime are included in this portable bundle.\n`);
    const provenance = {
      protocol: 1, platform, arch: 'x64', sourceDigest,
      native: INTERPOLATION_NATIVE, modelCatalogVersion: catalog.catalogVersion,
      modelSourceCommit: catalog.sourceCommit, defaultModelId: model.id,
      build: { ...JSON.parse(await fs.readFile(path.join(build, 'build-info.json'), 'utf8')), compilerHost: os.platform(), ncnnSimpleVk: true, openmp: false, padding: 64, input: 'rgb8', fp16Storage: true, fp16Arithmetic: false },
      files: await fileManifest(target),
    };
    await fs.writeFile(path.join(target, 'bundle-provenance.json'), JSON.stringify(provenance, null, 2) + '\n');
    // Check actual model load and non-midpoint inference on CPU, independent of GPU availability on build hosts.
    await run(path.join(target, exe), ['--self-test', '--model', path.join(target, 'models', model.id), '--gpu', '-1']);
    await fs.mkdir(path.dirname(interpolationSidecarDir), { recursive: true });
    // Preserve the previous generated bundle if activation fails. Downloaded user models
    // live outside this build directory and are never touched by this helper.
    const backup = path.join(staging, 'previous-interpolation-sidecar');
    let previous = false;
    try { await fs.rename(interpolationSidecarDir, backup); previous = true; }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
    try { await fs.rename(target, interpolationSidecarDir); }
    catch (error) {
      if (previous) {
        try { await fs.rename(backup, interpolationSidecarDir); }
        catch {
          preserveStaging = true;
          throw new Error(`Interpolation activation failed; previous bundle retained in ${backup}`, { cause: error });
        }
      }
      throw error;
    }
    return interpolationSidecarDir;
  } finally { if (!preserveStaging) await fs.rm(staging, { recursive: true, force: true }); }
}
export async function ensureInterpolationBundle(options = {}) {
  await fs.mkdir(interpolationBundleRoot, { recursive: true });
  const lockPath = path.join(interpolationBundleRoot, 'build.lock');
  let lock;
  const deadline = Date.now() + 20 * 60_000;
  while (!lock) {
    try { lock = await fs.open(lockPath, 'wx'); }
    catch (error) {
      if (error.code !== 'EEXIST') throw error;
      // A live build owns shared CMake/cache/activation paths. Do not race it.
      if (Date.now() > deadline) throw new Error('Interpolation build lock timed out; inspect the generated build.lock and active build processes');
      await new Promise(resolve => setTimeout(resolve, 500));
    }
  }
  try {
    await lock.writeFile(JSON.stringify({ tool: 'vfl-interpolation-build-v1', pid: process.pid, platform: process.platform }) + '\n');
    return await buildInterpolationBundle(options);
  } finally {
    await lock.close();
    await fs.unlink(lockPath);
  }
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  if (args.some(a => a !== '--force')) { console.error('Usage: node scripts/build-interpolation-sidecar.mjs [--force]'); process.exitCode = 1; }
  else ensureInterpolationBundle({ force: args.includes('--force') }).then(dir => console.log(`Interpolation sidecar ready: ${dir}`)).catch(error => { console.error(error.message); process.exitCode = 1; });
}
