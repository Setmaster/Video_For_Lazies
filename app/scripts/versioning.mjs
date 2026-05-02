import fs from "node:fs/promises";
import path from "node:path";

import { appRoot } from "./ffmpegBundle.mjs";

const SEMVER_PATTERN =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/;

const packageJsonPath = path.resolve(appRoot, "package.json");
const packageLockPath = path.resolve(appRoot, "package-lock.json");
const tauriConfigPath = path.resolve(appRoot, "src-tauri", "tauri.conf.json");
const cargoTomlPath = path.resolve(appRoot, "src-tauri", "Cargo.toml");
const cargoLockPath = path.resolve(appRoot, "src-tauri", "Cargo.lock");

export function normalizeVersionInput(value) {
  const normalized = String(value ?? "").trim().replace(/^v/, "");
  if (!SEMVER_PATTERN.test(normalized)) {
    throw new Error(`Expected a SemVer version like 1.0.5 or v1.0.5, got: ${value}`);
  }
  return normalized;
}

function parseCargoPackageVersion(raw, fileLabel) {
  const match = raw.match(/^\[package\][\s\S]*?^\s*version\s*=\s*"([^"]+)"/m);
  if (!match) {
    throw new Error(`Could not find [package] version in ${fileLabel}.`);
  }
  return match[1];
}

function parseCargoLockPackageVersion(raw, packageName) {
  const blockPattern = new RegExp(
    `\\[\\[package\\]\\]\\nname = "${packageName}"\\nversion = "([^"]+)"`,
    "m",
  );
  const match = raw.match(blockPattern);
  if (!match) {
    throw new Error(`Could not find ${packageName} version in Cargo.lock.`);
  }
  return match[1];
}

function replaceCargoPackageVersion(raw, version, fileLabel) {
  const replaced = raw.replace(
    /^(\[package\][\s\S]*?^\s*version\s*=\s*")[^"]+(")/m,
    `$1${version}$2`,
  );
  if (replaced === raw) {
    throw new Error(`Could not update [package] version in ${fileLabel}.`);
  }
  return replaced;
}

function replaceCargoLockPackageVersion(raw, packageName, version) {
  const blockPattern = new RegExp(
    `(\\[\\[package\\]\\]\\nname = "${packageName}"\\nversion = ")[^"]+(")`,
    "m",
  );
  const replaced = raw.replace(blockPattern, `$1${version}$2`);
  if (replaced === raw) {
    throw new Error(`Could not update ${packageName} version in Cargo.lock.`);
  }
  return replaced;
}

async function readJson(filePath) {
  return JSON.parse(await fs.readFile(filePath, "utf8"));
}

async function writeJson(filePath, value) {
  await fs.writeFile(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

export async function readProjectVersions() {
  const packageJson = await readJson(packageJsonPath);
  const packageLock = await readJson(packageLockPath);
  const tauriConfig = await readJson(tauriConfigPath);
  const cargoToml = await fs.readFile(cargoTomlPath, "utf8");
  const cargoLock = await fs.readFile(cargoLockPath, "utf8");

  return [
    { label: "app/package.json", version: packageJson.version },
    { label: "app/package-lock.json", version: packageLock.version },
    { label: "app/package-lock.json packages[\"\"]", version: packageLock.packages?.[""]?.version },
    { label: "app/src-tauri/tauri.conf.json", version: tauriConfig.version },
    { label: "app/src-tauri/Cargo.toml", version: parseCargoPackageVersion(cargoToml, "Cargo.toml") },
    { label: "app/src-tauri/Cargo.lock", version: parseCargoLockPackageVersion(cargoLock, "video_for_lazies") },
  ];
}

export async function assertSynchronizedVersion(expectedVersionInput) {
  const expectedVersion =
    expectedVersionInput === undefined || expectedVersionInput === null || expectedVersionInput === ""
      ? null
      : normalizeVersionInput(expectedVersionInput);
  const versions = await readProjectVersions();
  const uniqueVersions = new Set();

  for (const entry of versions) {
    if (!entry.version || !SEMVER_PATTERN.test(entry.version)) {
      throw new Error(`${entry.label} has invalid SemVer version: ${entry.version}`);
    }
    uniqueVersions.add(entry.version);
  }

  if (uniqueVersions.size !== 1) {
    throw new Error(
      `Version fields are not synchronized:\n${versions
        .map((entry) => `- ${entry.label}: ${entry.version}`)
        .join("\n")}`,
    );
  }

  const [version] = uniqueVersions;
  if (expectedVersion && version !== expectedVersion) {
    throw new Error(`Requested version ${expectedVersion} does not match project version ${version}.`);
  }

  return { version, versions };
}

export async function getProjectVersion() {
  return (await assertSynchronizedVersion()).version;
}

export async function setProjectVersion(versionInput) {
  const version = normalizeVersionInput(versionInput);
  const packageJson = await readJson(packageJsonPath);
  const packageLock = await readJson(packageLockPath);
  const tauriConfig = await readJson(tauriConfigPath);
  const cargoToml = await fs.readFile(cargoTomlPath, "utf8");
  const cargoLock = await fs.readFile(cargoLockPath, "utf8");

  packageJson.version = version;
  packageLock.version = version;
  if (!packageLock.packages?.[""]) {
    throw new Error("Could not find root package entry in package-lock.json.");
  }
  packageLock.packages[""].version = version;
  tauriConfig.version = version;

  await writeJson(packageJsonPath, packageJson);
  await writeJson(packageLockPath, packageLock);
  await writeJson(tauriConfigPath, tauriConfig);
  await fs.writeFile(cargoTomlPath, replaceCargoPackageVersion(cargoToml, version, "Cargo.toml"));
  await fs.writeFile(cargoLockPath, replaceCargoLockPackageVersion(cargoLock, "video_for_lazies", version));

  return assertSynchronizedVersion(version);
}
