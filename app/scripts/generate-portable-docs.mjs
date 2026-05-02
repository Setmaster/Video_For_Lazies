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

export function renderThirdPartyNotices({ npmDependencies, cargoDependencies, version, commitSha } = {}) {
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
    "Expected files inside each portable payload:",
    "",
    ...ffmpegSourceArchiveNames.map((name) => `- \`ffmpeg-sidecar/source/${name}\``),
    "",
  ].join("\n");
}

export async function getGitSourceContext({ version } = {}) {
  const normalizedVersion = version ?? await getProjectVersion();
  const expectedTag = `v${normalizedVersion}`;
  const commitSha = await captureOptional("git", ["rev-parse", "HEAD"]);
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
  const thirdPartyNoticesPath = path.resolve(outputDir, "THIRD_PARTY_NOTICES.md");
  const sourceNoticePath = path.resolve(outputDir, "SOURCE.md");

  await fs.mkdir(outputDir, { recursive: true });
  await fs.writeFile(
    thirdPartyNoticesPath,
    renderThirdPartyNotices({ ...context, npmDependencies, cargoDependencies }),
  );
  await fs.writeFile(sourceNoticePath, renderSourceNotice(context));

  return {
    thirdPartyNoticesPath,
    sourceNoticePath,
  };
}
