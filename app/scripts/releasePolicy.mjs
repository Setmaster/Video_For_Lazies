import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { normalizeVersionInput } from "./versioning.mjs";

export const DEFAULT_RELEASE_TARGETS = Object.freeze(["linux-x64", "windows-x64"]);

export function validateReleaseTargets(targets = DEFAULT_RELEASE_TARGETS) {
  if (!Array.isArray(targets) || targets.length === 0
    || new Set(targets).size !== targets.length
    || targets.some((target) => !DEFAULT_RELEASE_TARGETS.includes(target))) {
    throw new Error("Release targets must be a nonempty, unique list of supported updater targets.");
  }
  return [...targets];
}

export async function resolveReleasePolicy(version, { configPath = new URL("../release-policy.json", import.meta.url) } = {}) {
  const normalizedVersion = normalizeVersionInput(version);
  const config = JSON.parse(await fs.readFile(configPath, "utf8"));
  if (config.schemaVersion !== 1 || !config.releases || typeof config.releases !== "object"
    || Array.isArray(config.releases)) {
    throw new Error("Unsupported release policy configuration.");
  }
  const policy = Object.hasOwn(config.releases, normalizedVersion)
    ? config.releases[normalizedVersion]
    : { targets: DEFAULT_RELEASE_TARGETS, makeLatest: true };
  if (!policy || !Array.isArray(policy.targets) || typeof policy.makeLatest !== "boolean") {
    throw new Error(`Invalid release policy for ${normalizedVersion}.`);
  }
  const targets = validateReleaseTargets(policy.targets);
  if (targets.length !== DEFAULT_RELEASE_TARGETS.length && policy.makeLatest) {
    throw new Error("A platform-only release must not replace the shared latest updater channel.");
  }
  const matrix = { include: targets.map((target) => target === "windows-x64"
    ? { os: "windows-2022", target: "win-x64" }
    : { os: "ubuntu-24.04", target: "linux-x64" }) };
  return { version: normalizedVersion, targets, makeLatest: policy.makeLatest, matrix };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const policy = await resolveReleasePolicy(process.argv[2]);
    const output = `matrix=${JSON.stringify(policy.matrix)}\ntargets=${policy.targets.join(",")}\nmake_latest=${policy.makeLatest}\n`;
    if (process.env.GITHUB_OUTPUT) await fs.appendFile(process.env.GITHUB_OUTPUT, output);
    process.stdout.write(output);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
