import fs from "node:fs/promises";
import path from "node:path";
import { spawn } from "node:child_process";

import {
  FFMPEG_BUNDLE,
  appRoot,
  repoRoot,
  ffmpegSourceArchiveNames,
  linuxBuildScriptsArchiveName,
  linuxBundleAssetUrl,
  linuxSourceArchiveName,
  linuxX264SourceArchiveName,
  windowsBuildScriptsArchiveName,
  windowsBuildScriptsUrl,
  windowsBundleAssetUrl,
  windowsSourceArchiveName,
  windowsSourceUrl,
  windowsX264SourceArchiveName,
  windowsX264SourceUrl,
} from "./ffmpegBundle.mjs";
import { getProjectVersion } from "./versioning.mjs";

const GENERATED_DOCS_DIR = path.resolve(repoRoot, "release", "generated-docs");
const PROJECT_URL = "https://github.com/Setmaster/Video_For_Lazies";
const SUBTITLE_FONT_LICENSE_PATH = path.resolve(appRoot, "src-tauri", "assets", "DEJAVU_FONT_LICENSE.txt");
const SUBTITLE_FONT_SOURCE_ARCHIVE_URL = "https://github.com/dejavu-fonts/dejavu-fonts/releases/download/version_2_37/dejavu-fonts-ttf-2.37.zip";
const SUBTITLE_FONT_SOURCE_ARCHIVE_SHA256 = "7576310b219e04159d35ff61dd4a4ec4cdba4f35c00e002a136f00e96a908b0a";
const SUBTITLE_FONT_SHA256 = "7da195a74c55bef988d0d48f9508bd5d849425c1770dba5d7bfc6ce9ed848954";
const SUBTITLE_FONT_LICENSE_SHA256 = "7a083b136e64d064794c3419751e5c7dd10d2f64c108fe5ba161eae5e5958a93";
const FULL_GIT_COMMIT_PATTERN = /^[0-9a-f]{40}$/i;

async function pathExists(targetPath) {
  try {
    await fs.access(targetPath);
    return true;
  } catch {
    return false;
  }
}

async function readJson(filePath) {
  return JSON.parse(await fs.readFile(filePath, "utf8"));
}

async function readJsonIfExists(filePath) {
  if (!(await pathExists(filePath))) {
    return null;
  }
  return await readJson(filePath);
}

async function capture(command, args, { cwd = repoRoot } = {}) {
  return await new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";

    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) {
        resolve(stdout.trim());
        return;
      }
      reject(new Error(`${command} ${args.join(" ")} failed with code ${code}\n${stderr}`));
    });
  });
}

async function captureOptional(command, args, options) {
  try {
    return await capture(command, args, options);
  } catch {
    return "";
  }
}

function markdownCell(value) {
  return String(value ?? "")
    .replaceAll("\r", " ")
    .replaceAll("\n", " ")
    .replaceAll("|", "\\|")
    .trim() || "Not declared";
}

function normalizeRepositoryUrl(repository) {
  const raw = typeof repository === "string" ? repository : repository?.url;
  if (!raw) {
    return "";
  }
  return raw
    .replace(/^git\+/, "")
    .replace(/^git:\/\//, "https://")
    .replace(/^ssh:\/\/git@github\.com\//, "https://github.com/")
    .replace(/^git@github\.com:/, "https://github.com/")
    .replace(/\.git$/, "");
}

function packageNameFromLockPath(lockPath) {
  const segments = lockPath.split("node_modules/");
  return segments[segments.length - 1]?.replace(/\/$/, "") ?? lockPath;
}

export async function collectNpmDependencies({ root = appRoot } = {}) {
  const lockPath = path.resolve(root, "package-lock.json");
  const lock = await readJson(lockPath);
  const packages = lock.packages ?? {};
  const dependencies = [];

  for (const [lockPathKey, lockPackage] of Object.entries(packages)) {
    if (!lockPathKey.startsWith("node_modules/")) {
      continue;
    }

    const packageJsonPath = path.resolve(root, lockPathKey, "package.json");
    const packageJson = await readJsonIfExists(packageJsonPath);
    const packageName = packageJson?.name ?? packageNameFromLockPath(lockPathKey);
    const packageVersion = packageJson?.version ?? lockPackage.version ?? "";
    const license = packageJson?.license ?? lockPackage.license ?? "Not declared";
    const repository = normalizeRepositoryUrl(packageJson?.repository) || normalizeRepositoryUrl(lockPackage.repository);
    const scope = lockPackage.dev ? "build/development" : "runtime";

    dependencies.push({
      name: packageName,
      version: packageVersion,
      license,
      repository,
      scope,
    });
  }

  return dependencies.sort((a, b) => `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`));
}

export async function collectCargoDependencies({ root = appRoot } = {}) {
  const cargoManifestPath = path.resolve(root, "src-tauri", "Cargo.toml");
  const metadataRaw = await capture("cargo", [
    "metadata",
    "--manifest-path",
    cargoManifestPath,
    "--format-version",
    "1",
    "--locked",
  ]);
  const metadata = JSON.parse(metadataRaw);

  return metadata.packages
    .map((pkg) => ({
      name: pkg.name,
      version: pkg.version,
      license: pkg.license ?? (pkg.license_file ? `See ${path.basename(pkg.license_file)}` : "Not declared"),
      repository: normalizeRepositoryUrl(pkg.repository) || pkg.homepage || "",
      source: pkg.source?.includes("crates.io") ? "crates.io" : "local project",
    }))
    .sort((a, b) => `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`));
}

function renderDependencyTable(dependencies, columns) {
  const header = `| ${columns.map((column) => column.label).join(" | ")} |`;
  const divider = `| ${columns.map(() => "---").join(" | ")} |`;
  const rows = dependencies.map((dependency) => (
    `| ${columns.map((column) => markdownCell(dependency[column.key])).join(" | ")} |`
  ));
  return [header, divider, ...rows].join("\n");
}

export function renderThirdPartyNotices({
  npmDependencies,
  cargoDependencies,
  version,
  commitSha,
  subtitleFontLicense = "",
} = {}) {
  const windowsBundle = FFMPEG_BUNDLE.windowsX64;
  const linuxBundle = FFMPEG_BUNDLE.linuxX64;
  return [
    "# Third-Party Notices",
    "",
    `Video For Lazies ${version} is licensed under GPL-3.0-or-later.`,
    commitSha ? `Generated for source commit \`${commitSha}\`.` : "Generated for the current source checkout.",
    "",
    "## FFmpeg Sidecars",
    "",
    "Windows x64 and Linux x64 portable builds bundle pinned GPL FFmpeg runtimes as `ffmpeg-sidecar/`.",
    "",
    "### Windows x64",
    "",
    `- Provider: ${windowsBundle.provider}`,
    `- Variant: \`${windowsBundle.variant}\``,
    `- BtbN release tag: \`${windowsBundle.releaseTag}\``,
    `- Binary archive: ${windowsBundleAssetUrl}`,
    `- Binary archive SHA256: \`${windowsBundle.assetSha256}\``,
    `- FFmpeg source archive: \`${windowsSourceArchiveName}\``,
    `- BtbN build recipe archive: \`${windowsBuildScriptsArchiveName}\``,
    `- x264 source archive: \`${windowsX264SourceArchiveName}\``,
    "",
    "### Linux x64",
    "",
    `- Provider: ${linuxBundle.provider}`,
    `- Variant: \`${linuxBundle.variant}\``,
    `- BtbN release tag: \`${linuxBundle.releaseTag}\``,
    `- Binary archive: ${linuxBundleAssetUrl}`,
    `- Binary archive SHA256: \`${linuxBundle.assetSha256}\``,
    `- FFmpeg source archive: \`${linuxSourceArchiveName}\``,
    `- BtbN build recipe archive: \`${linuxBuildScriptsArchiveName}\``,
    `- x264 source archive: \`${linuxX264SourceArchiveName}\``,
    "",
    "Each portable folder includes upstream FFmpeg license text, bundle notices, and the source/provenance archives under `ffmpeg-sidecar/source/`.",
    "",
    "## Native AI Frame Interpolation",
    "",
    "The interpolation-sidecar/ directory bundles the native RIFE worker and the default RIFE 4.25 model. The worker uses ncnn (BSD-3-Clause), glslang (see multi-party license) and a modified MIT RIFE Warp layer; the VFL wrapper is GPL-3.0-or-later.",
    "Pinned model lengths, SHA256 digests and immutable converted-source URLs are in interpolation-sidecar/model-catalog.json. Original Practical-RIFE linked trained models are MIT under the upstream declaration retained in interpolation-sidecar/licenses/model-license.txt. This build uses published converted bytes; it does not independently reproduce their conversion.",
    "See interpolation-sidecar/INTERPOLATION_NOTICES.txt, licenses/ and bundle-provenance.json for exact source/build/model identities and per-file checksums. Corresponding native sources and ncnn/glslang archives are included under interpolation-sidecar/source/.",
    "Optional GMFSS Fortuna (Union), GIMM-VFI-R-P and LDF-VFI use separately downloaded app-local Python/Torch runtimes and exact pinned upstream sources/weights. GIMM is noncommercial. Each download retains its upstream licenses, including runtime dependency terms. The bundled optional workers/manifests are included under interpolation-sidecar/optional/ and their corresponding sources under source/vfl-optional/; source/optional-interpolation-runtime.json records interpreter and wheel pins. No optional model weights or Torch runtime are included in this portable folder.",
    "GPU drivers remain an operating-system/vendor prerequisite. RIFE model downloads contain model data only; optional Torch backends also download pinned executable runtime and inference sources, verified against app-embedded manifests before use.",
    "",
    "## Embedded Subtitle Font",
    "",
    "External SRT burn-in uses DejaVu Sans 2.37, embedded in the application and staged privately for FFmpeg/libass so the fixed base face is available on every supported platform. Glyphs outside DejaVu Sans coverage may still require platform fallback.",
    "",
    `- Source archive: ${SUBTITLE_FONT_SOURCE_ARCHIVE_URL}`,
    `- Source archive SHA256: \`${SUBTITLE_FONT_SOURCE_ARCHIVE_SHA256}\``,
    `- Embedded \`DejaVuSans.ttf\` SHA256: \`${SUBTITLE_FONT_SHA256}\``,
    `- License file SHA256: \`${SUBTITLE_FONT_LICENSE_SHA256}\``,
    "- License: DejaVu Fonts license, reproduced below.",
    "",
    "```text",
    subtitleFontLicense.trim(),
    "```",
    "",
    "## npm Dependency Inventory",
    "",
    renderDependencyTable(npmDependencies, [
      { key: "name", label: "Package" },
      { key: "version", label: "Version" },
      { key: "license", label: "License" },
      { key: "scope", label: "Scope" },
      { key: "repository", label: "Repository" },
    ]),
    "",
    "## Cargo Dependency Inventory",
    "",
    renderDependencyTable(cargoDependencies, [
      { key: "name", label: "Crate" },
      { key: "version", label: "Version" },
      { key: "license", label: "License" },
      { key: "source", label: "Source" },
      { key: "repository", label: "Repository" },
    ]),
    "",
  ].join("\n");
}

export function renderSourceNotice({ version, commitSha, releaseTag, tagAtHead = false } = {}) {
  const tagLabel = releaseTag || `v${version}`;
  const tagStatus = tagAtHead ? "Release tag" : "Expected release tag";
  const commitLine = commitSha ? `- Source commit: \`${commitSha}\`` : "- Source commit: current checkout";
  const sourceArchiveLines = releaseTag
    ? [
        `- Source tar archive: ${PROJECT_URL}/archive/refs/tags/${tagLabel}.tar.gz`,
        `- Source zip archive: ${PROJECT_URL}/archive/refs/tags/${tagLabel}.zip`,
      ]
    : [
        `- Source tar archive after tagging: ${PROJECT_URL}/archive/refs/tags/${tagLabel}.tar.gz`,
        `- Source zip archive after tagging: ${PROJECT_URL}/archive/refs/tags/${tagLabel}.zip`,
      ];

  return [
    "# Source Availability",
    "",
    "Video For Lazies source code is distributed from the project repository.",
    "",
    `- Repository: ${PROJECT_URL}`,
    `- Version: \`${version}\``,
    `- ${tagStatus}: \`${tagLabel}\``,
    commitLine,
    ...sourceArchiveLines,
    "",
    "## FFmpeg Sidecar Source",
    "",
    "Windows and Linux portable builds include pinned GPL FFmpeg sidecars. The sidecar source and build-provenance archives are included in the portable folder under `ffmpeg-sidecar/source/`.",
    "The BtbN build recipe snapshot records the upstream dependency repositories and commits used by the selected GPL build recipe.",
    "",
    `- Windows FFmpeg binary archive: ${windowsBundleAssetUrl}`,
    `- Linux FFmpeg binary archive: ${linuxBundleAssetUrl}`,
    `- FFmpeg source archive: ${windowsSourceUrl}`,
    `- BtbN build recipe archive: ${windowsBuildScriptsUrl}`,
    `- x264 source archive: ${windowsX264SourceUrl}`,
    "",
    "## Native Interpolation Source",
    "",
    "The interpolation-sidecar/source/ directory includes the VFL native worker sources/build helpers and exact hash-pinned ncnn/glslang source archives. INTERPOLATION_NOTICES.txt, model-catalog.json, licenses/ and bundle-provenance.json record native code, dependency and model provenance. Published converted weights are verified by exact size/SHA256; independent checkpoint conversion is not claimed.",
    "Optional inference adapters and their immutable source/model manifests are retained in interpolation-sidecar/source/vfl-optional/. The adjacent optional-interpolation-runtime.json binds exact separately downloaded interpreter and wheel artifacts. Downloaded components retain their own license files, including GIMM's noncommercial terms and Windows app-local Microsoft redistributable terms; maintainers must comply with the applicable redistribution conditions.",
    "",
    "Expected FFmpeg files inside each portable payload:",
    "",
    ...ffmpegSourceArchiveNames.map((name) => `- \`ffmpeg-sidecar/source/${name}\``),
    "",
  ].join("\n");
}

function normalizeDeclaredSourceCommit(value, label) {
  const normalized = String(value ?? "").trim().toLowerCase();
  if (normalized === "") return "";
  if (!FULL_GIT_COMMIT_PATTERN.test(normalized)) {
    throw new Error(`${label} must be an exact 40-character Git commit SHA.`);
  }
  return normalized;
}

export function resolveDeclaredSourceCommit(env = process.env) {
  const explicitCommit = normalizeDeclaredSourceCommit(env.VFL_SOURCE_COMMIT, "VFL_SOURCE_COMMIT");
  const githubCommit = normalizeDeclaredSourceCommit(env.GITHUB_SHA, "GITHUB_SHA");
  if (explicitCommit && githubCommit && explicitCommit !== githubCommit) {
    throw new Error("VFL_SOURCE_COMMIT and GITHUB_SHA identify different source commits.");
  }
  return explicitCommit || githubCommit;
}

export async function getGitSourceContext({ version, env = process.env } = {}) {
  const normalizedVersion = version ?? await getProjectVersion();
  const expectedTag = `v${normalizedVersion}`;
  const declaredCommit = resolveDeclaredSourceCommit(env);
  const capturedCommitRaw = await captureOptional("git", ["rev-parse", "HEAD"]);
  const capturedCommit = normalizeDeclaredSourceCommit(capturedCommitRaw, "git rev-parse HEAD");
  if (declaredCommit && capturedCommit && declaredCommit !== capturedCommit) {
    throw new Error("Declared source commit does not match the current Git checkout.");
  }
  const commitSha = declaredCommit || capturedCommit;
  const tagsAtHead = (await captureOptional("git", ["tag", "--points-at", "HEAD", "--list", "v[0-9]*"]))
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  const releaseTag = tagsAtHead.includes(expectedTag) ? expectedTag : "";

  return {
    version: normalizedVersion,
    commitSha,
    releaseTag,
    tagAtHead: Boolean(releaseTag),
  };
}

export async function generatePortableDocs({ outputDir = GENERATED_DOCS_DIR } = {}) {
  const context = await getGitSourceContext();
  const npmDependencies = await collectNpmDependencies();
  const cargoDependencies = await collectCargoDependencies();
  const subtitleFontLicense = await fs.readFile(SUBTITLE_FONT_LICENSE_PATH, "utf8");
  const thirdPartyNoticesPath = path.resolve(outputDir, "THIRD_PARTY_NOTICES.md");
  const sourceNoticePath = path.resolve(outputDir, "SOURCE.md");

  await fs.mkdir(outputDir, { recursive: true });
  await fs.writeFile(
    thirdPartyNoticesPath,
    renderThirdPartyNotices({ ...context, npmDependencies, cargoDependencies, subtitleFontLicense }),
  );
  await fs.writeFile(sourceNoticePath, renderSourceNotice(context));

  return {
    thirdPartyNoticesPath,
    sourceNoticePath,
  };
}
