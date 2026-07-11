import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";
import { spawn } from "node:child_process";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

export const FFMPEG_CAPABILITY_CONTRACT_FILE_NAME = "FFMPEG_CAPABILITIES.json";
export const ffmpegCapabilityContractPath = path.resolve(__dirname, "..", "ffmpeg-capabilities.json");

const FEATURE_NAME_PATTERN = /^[a-z][A-Za-z0-9]*$/;
const CAPABILITY_NAME_PATTERN = /^[A-Za-z0-9_.-]+$/;
const TOP_LEVEL_KEYS = new Set(["schemaVersion", "features"]);
const FEATURE_KEYS = new Set(["releaseRequired", "encoders", "filters"]);

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function assertExactKeys(value, allowedKeys, label) {
  const unexpected = Object.keys(value).filter((key) => !allowedKeys.has(key));
  if (unexpected.length > 0) {
    throw new Error(`${label} contains unsupported field(s): ${unexpected.join(", ")}.`);
  }
}

function validateCapabilityNames(value, label) {
  if (!Array.isArray(value)) {
    throw new Error(`${label} must be an array.`);
  }

  const names = value.map((name, index) => {
    if (typeof name !== "string" || !CAPABILITY_NAME_PATTERN.test(name)) {
      throw new Error(`${label}[${index}] must be a non-empty FFmpeg capability name.`);
    }
    return name;
  });
  const duplicates = names.filter((name, index) => names.indexOf(name) !== index);
  if (duplicates.length > 0) {
    throw new Error(`${label} contains duplicate name(s): ${[...new Set(duplicates)].join(", ")}.`);
  }
  return names;
}

export function validateFfmpegCapabilityContract(value) {
  if (!isPlainObject(value)) {
    throw new Error("FFmpeg capability contract must be a JSON object.");
  }
  assertExactKeys(value, TOP_LEVEL_KEYS, "FFmpeg capability contract");
  if (value.schemaVersion !== 1) {
    throw new Error(`Unsupported FFmpeg capability contract schemaVersion: ${String(value.schemaVersion)}.`);
  }
  if (!isPlainObject(value.features) || Object.keys(value.features).length === 0) {
    throw new Error("FFmpeg capability contract features must be a non-empty object.");
  }

  const features = Object.create(null);
  for (const [featureName, feature] of Object.entries(value.features)) {
    if (!FEATURE_NAME_PATTERN.test(featureName)) {
      throw new Error(`Invalid FFmpeg capability feature name: ${featureName}.`);
    }
    if (!isPlainObject(feature)) {
      throw new Error(`FFmpeg capability feature ${featureName} must be an object.`);
    }
    assertExactKeys(feature, FEATURE_KEYS, `FFmpeg capability feature ${featureName}`);
    if (typeof feature.releaseRequired !== "boolean") {
      throw new Error(`FFmpeg capability feature ${featureName}.releaseRequired must be a boolean.`);
    }

    features[featureName] = Object.freeze({
      releaseRequired: feature.releaseRequired,
      encoders: Object.freeze(validateCapabilityNames(feature.encoders, `features.${featureName}.encoders`)),
      filters: Object.freeze(validateCapabilityNames(feature.filters, `features.${featureName}.filters`)),
    });
  }

  return Object.freeze({
    schemaVersion: 1,
    features: Object.freeze(features),
  });
}

export async function readFfmpegCapabilityContract(contractPath = ffmpegCapabilityContractPath) {
  let raw;
  try {
    raw = await fs.readFile(contractPath, "utf8");
  } catch (error) {
    throw new Error(`Failed to read FFmpeg capability contract ${contractPath}: ${error.message}`);
  }

  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw new Error(`FFmpeg capability contract ${contractPath} is invalid JSON: ${error.message}`);
  }
  return validateFfmpegCapabilityContract(parsed);
}

export function parseFfmpegVersion(stdout) {
  const firstLine = String(stdout).split(/\r?\n/, 1)[0]?.trim() ?? "";
  const match = firstLine.match(/^ffmpeg version\s+(\S+)/i);
  if (!match) {
    throw new Error("ffmpeg -version returned an unrecognized version line.");
  }
  return match[1];
}

export function parseFfmpegEncoderNames(stdout) {
  const names = new Set();
  for (const line of String(stdout).split(/\r?\n/)) {
    const parts = line.trim().split(/\s+/);
    if (parts.length < 2 || !/^[A-Za-z.]{6}$/.test(parts[0]) || parts[1] === "=") {
      continue;
    }
    names.add(parts[1]);
  }
  return names;
}

export function parseFfmpegFilterNames(stdout) {
  const names = new Set();
  for (const line of String(stdout).split(/\r?\n/)) {
    const parts = line.trim().split(/\s+/);
    if (parts.length < 2 || !/^[A-Za-z.]{2}$/.test(parts[0]) || parts[1] === "=") {
      continue;
    }
    names.add(parts[1]);
  }
  return names;
}

function captureStdout(command, args, { timeoutMs = 20_000 } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    let stdout = "";
    let stderr = "";
    let settled = false;
    const timeout = setTimeout(() => {
      if (settled) return;
      settled = true;
      child.kill();
      reject(new Error(`${path.basename(command)} ${args.join(" ")} timed out after ${timeoutMs} ms.`));
    }, timeoutMs);

    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });
    child.on("error", (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      reject(new Error(`Failed to start FFmpeg capability probe ${command}: ${error.message}`));
    });
    child.on("close", (code, signal) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      if (code === 0) {
        resolve(stdout);
        return;
      }
      const detail = stderr.trim() || `signal ${signal ?? "none"}`;
      reject(new Error(`${path.basename(command)} ${args.join(" ")} exited with code ${String(code)}: ${detail}`));
    });
  });
}

export async function probeFfmpegCapabilities(ffmpegPath, options = {}) {
  if (typeof ffmpegPath !== "string" || ffmpegPath.trim() === "") {
    throw new Error("An exact FFmpeg binary path is required for capability probing.");
  }

  const [versionOutput, encoderOutput, filterOutput] = await Promise.all([
    captureStdout(ffmpegPath, ["-version"], options),
    captureStdout(ffmpegPath, ["-hide_banner", "-loglevel", "error", "-encoders"], options),
    captureStdout(ffmpegPath, ["-hide_banner", "-filters"], options),
  ]);
  const encoders = parseFfmpegEncoderNames(encoderOutput);
  const filters = parseFfmpegFilterNames(filterOutput);
  if (encoders.size === 0 || filters.size === 0) {
    throw new Error("FFmpeg capability probe returned an empty encoder or filter inventory.");
  }

  return Object.freeze({
    ffmpegVersion: parseFfmpegVersion(versionOutput),
    encoders,
    filters,
  });
}

export function evaluateFfmpegCapabilityContract(contract, detected) {
  const validatedContract = validateFfmpegCapabilityContract(contract);
  if (!(detected?.encoders instanceof Set) || !(detected?.filters instanceof Set)) {
    throw new Error("Detected FFmpeg capabilities must provide encoder and filter sets.");
  }
  if (typeof detected.ffmpegVersion !== "string" || detected.ffmpegVersion.trim() === "") {
    throw new Error("Detected FFmpeg capabilities must provide a version string.");
  }

  const features = Object.create(null);
  for (const [featureName, requirement] of Object.entries(validatedContract.features)) {
    const missingEncoders = requirement.encoders.filter((name) => !detected.encoders.has(name));
    const missingFilters = requirement.filters.filter((name) => !detected.filters.has(name));
    features[featureName] = Object.freeze({
      releaseRequired: requirement.releaseRequired,
      available: missingEncoders.length === 0 && missingFilters.length === 0,
      missingEncoders: Object.freeze(missingEncoders),
      missingFilters: Object.freeze(missingFilters),
    });
  }

  return Object.freeze({
    schemaVersion: validatedContract.schemaVersion,
    ffmpegVersion: detected.ffmpegVersion,
    encoderCount: detected.encoders.size,
    filterCount: detected.filters.size,
    features: Object.freeze(features),
  });
}

export function assertReleaseRequiredFfmpegCapabilities(result, label = "FFmpeg") {
  const failures = Object.entries(result.features)
    .filter(([, feature]) => feature.releaseRequired && !feature.available)
    .map(([featureName, feature]) => {
      const parts = [];
      if (feature.missingEncoders.length > 0) parts.push(`encoders ${feature.missingEncoders.join(", ")}`);
      if (feature.missingFilters.length > 0) parts.push(`filters ${feature.missingFilters.join(", ")}`);
      return `${featureName}: ${parts.join("; ")}`;
    });
  if (failures.length > 0) {
    throw new Error(`${label} ${result.ffmpegVersion} does not satisfy release-required capabilities: ${failures.join(" | ")}.`);
  }
  return result;
}

export async function verifyFfmpegCapabilityContract({
  ffmpegPath,
  contract,
  contractPath = ffmpegCapabilityContractPath,
  label = "FFmpeg",
  timeoutMs,
} = {}) {
  const resolvedContract = contract ?? await readFfmpegCapabilityContract(contractPath);
  const detected = await probeFfmpegCapabilities(ffmpegPath, { timeoutMs });
  return assertReleaseRequiredFfmpegCapabilities(
    evaluateFfmpegCapabilityContract(resolvedContract, detected),
    label,
  );
}

export async function assertCapabilityContractCopy(
  copiedContractPath,
  sourceContractPath = ffmpegCapabilityContractPath,
) {
  const [source, copied] = await Promise.all([
    fs.readFile(sourceContractPath),
    fs.readFile(copiedContractPath),
  ]);
  if (!source.equals(copied)) {
    throw new Error(`Packaged FFmpeg capability contract is not a verbatim copy: ${copiedContractPath}`);
  }
  return readFfmpegCapabilityContract(copiedContractPath);
}

export function formatFfmpegCapabilitySummary(result) {
  const featureSummary = Object.entries(result.features)
    .map(([name, feature]) => `${name}=${feature.available ? "available" : "missing"}`)
    .join(", ");
  return `FFmpeg ${result.ffmpegVersion}; encoders=${result.encoderCount}; filters=${result.filterCount}; ${featureSummary}`;
}
