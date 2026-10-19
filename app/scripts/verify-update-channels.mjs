#!/usr/bin/env node
import path from "node:path";
import { fileURLToPath } from "node:url";
import { isDeepStrictEqual } from "node:util";

import { DEFAULT_RELEASE_TARGETS, validateReleaseTargets } from "./releasePolicy.mjs";
import { APP_ID, UPDATE_CHANNEL, UPDATE_MANIFEST_SCHEMA } from "./updateManifests.mjs";
import { readEmbeddedPublicKeys, verifyManifestSignature } from "./verify-update-manifest.mjs";

export const RELEASES_API_URL = "https://api.github.com/repos/Setmaster/Video_For_Lazies/releases";
export const RELEASES_BASE_URL = "https://github.com/Setmaster/Video_For_Lazies/releases";
export const DISCOVERY_LIMITS = Object.freeze({
  perPage: 100,
  maxPages: 10,
  pageBytes: 5 * 1024 * 1024,
  inventoryBytes: 20 * 1024 * 1024,
  manifestBytes: 5 * 1024 * 1024,
  signatureBytes: 1024 * 1024,
  assetsPerRelease: 256,
  timeoutMs: 45_000,
});
const MANIFEST_NAME = "vfl-update-manifest-v1.json";
const MAX_ARCHIVE_BYTES = 750 * 1024 * 1024;
const U64_MAX = 18_446_744_073_709_551_615n;

export class ChannelVerificationError extends Error {
  constructor(code, message, details = {}) {
    super(message);
    this.code = code;
    Object.assign(this, details);
  }
}

function fail(code, message, details) {
  throw new ChannelVerificationError(code, message, details);
}

export function stableReleaseVersion(tag) {
  if (typeof tag !== "string") return null;
  const match = /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.exec(tag);
  if (!match || match[0] !== tag || match.slice(1).some((part) => part.length > 20 || BigInt(part) > U64_MAX)) return null;
  return { version: tag.slice(1), parts: match.slice(1).map(BigInt) };
}

function expectedZipName(version, target) {
  return `Video_For_Lazies-v${version}-${target === "windows-x64" ? "win" : "linux"}-x64.zip`;
}

function canonicalAssetUrl(tag, name) {
  return `${RELEASES_BASE_URL}/download/${tag}/${name}`;
}

function versionCompare(left, right) {
  for (let index = 0; index < 3; index += 1) {
    if (left.parts[index] !== right.parts[index]) return left.parts[index] > right.parts[index] ? 1 : -1;
  }
  return 0;
}

function releaseIdentity(release) {
  return {
    id: release.id, tag_name: release.tag_name, draft: release.draft,
    prerelease: release.prerelease, published_at: release.published_at,
    assets: release.assets.map(({ id, name, state, size, browser_download_url }) => ({ id, name, state, size, browser_download_url })),
  };
}

function requireReleaseRecords(records) {
  for (const release of records) {
    if (!release || typeof release !== "object" || !Number.isSafeInteger(release.id) || release.id <= 0
      || typeof release.draft !== "boolean" || typeof release.prerelease !== "boolean"
      || typeof release.tag_name !== "string"
      || (release.published_at != null && typeof release.published_at !== "string")
      || !Array.isArray(release.assets) || release.assets.length > DISCOVERY_LIMITS.assetsPerRelease
      || release.assets.some((asset) => !asset || typeof asset.name !== "string"
        || typeof asset.state !== "string" || typeof asset.browser_download_url !== "string"
        || !Number.isSafeInteger(asset.id) || asset.id < 0 || !Number.isSafeInteger(asset.size) || asset.size < 0)) {
      fail("invalid-inventory", "Release inventory contains a malformed record.");
    }
  }
}

function selectedAsset(release, name, kind) {
  const matches = release.assets.filter((asset) => asset.name === name);
  const prefix = kind === "archive" ? "archive" : "selected-metadata";
  if (matches.length === 0) fail(`${prefix}-missing`, `Selected release is missing ${kind}: ${name}.`);
  if (matches.length !== 1) fail(`${prefix}-ambiguous`, `Selected release has ambiguous ${kind}: ${name}.`);
  const asset = matches[0];
  if (!Number.isSafeInteger(asset.id) || asset.id <= 0 || asset.state !== "uploaded"
    || !Number.isSafeInteger(asset.size) || asset.size <= 0
    || asset.browser_download_url !== canonicalAssetUrl(release.tag_name, name)) {
    fail(`${prefix}-invalid`, `Selected release has invalid ${kind}: ${name}.`);
  }
  return asset;
}

export function selectPlatformRelease(releases, target) {
  validateReleaseTargets([target]);
  if (!Array.isArray(releases)) fail("invalid-inventory", "Release inventory must be an array.");
  requireReleaseRecords(releases);
  const versions = new Map();
  const ids = new Map();
  const tags = new Set();
  for (const release of releases) {
    const repeated = ids.get(release.id);
    if (repeated) {
      if (isDeepStrictEqual(releaseIdentity(repeated), releaseIdentity(release))) continue;
      fail("conflicting-release", "Conflicting release records share one identity.");
    }
    if (tags.has(release.tag_name)) fail("conflicting-release", `Conflicting release records for ${release.tag_name}.`);
    ids.set(release.id, release);
    tags.add(release.tag_name);
    if (release.draft || release.prerelease || !release.published_at) continue;
    const version = stableReleaseVersion(release.tag_name);
    if (!version) continue;
    versions.set(release.tag_name, { ...version, release });
  }
  const candidates = [...versions.values()].filter(({ version, release }) => release.assets.some(
    (asset) => asset.name === expectedZipName(version, target),
  )).sort((left, right) => versionCompare(right, left));
  if (candidates.length === 0) fail("no-platform-release", `No stable update release exists for platform ${target}.`);
  const { version, release } = candidates[0];
  // Select by the platform ZIP first. Broken metadata on the selected release
  // must be reported, rather than silently choosing an older release.
  const candidate = {
    id: release.id,
    tag: release.tag_name,
    version,
    target,
    archive: selectedAsset(release, expectedZipName(version, target), "archive"),
    metadata: selectedAsset(release, MANIFEST_NAME, "manifest"),
    signature: selectedAsset(release, `${MANIFEST_NAME}.sig`, "signature"),
  };
  if (new Set([candidate.archive.id, candidate.metadata.id, candidate.signature.id]).size !== 3) {
    fail("selected-metadata-invalid", "Selected release asset IDs must be distinct.");
  }
  return candidate;
}

export function discoveryReadiness(releaseCount, maximum = 1000) {
  const warningAt = Math.floor(maximum * 0.8);
  return {
    ready: releaseCount < warningAt,
    releaseCount,
    maximum,
    warningAt,
    guidance: releaseCount >= warningAt
      ? "Migrate to a signed discovery index before installed clients reach the history cap. Keep all old releases; do not delete or truncate history."
      : "Release history has sufficient discovery headroom.",
  };
}

function ensureDeadline(context) {
  if (context.signal.aborted || Date.now() >= context.deadlineAt) {
    fail("deadline-exceeded", "Update discovery timed out. Try later or download a newer app manually.");
  }
}

async function boundedBytes(response, maximum) {
  if (Number(response.headers.get("content-length")) > maximum) {
    await response.body?.cancel();
    fail("metadata-too-large", "Update discovery metadata exceeds its byte limit.");
  }
  if (!response.body) return Buffer.alloc(0);
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > maximum) {
        await reader.cancel();
        fail("metadata-too-large", "Update discovery metadata exceeds its byte limit.");
      }
      chunks.push(Buffer.from(value));
    }
  } finally {
    reader.releaseLock();
  }
  return Buffer.concat(chunks, size);
}

function parseJson(bytes, label) {
  try {
    return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch {
    fail("invalid-json", `${label} is not valid UTF-8 JSON.`);
  }
}

async function requestBytes(context, url, maximum, inventory = false) {
  ensureDeadline(context);
  let response;
  try {
    response = await context.fetchImpl(url, {
      headers: inventory ? {
        "User-Agent": "Video For Lazies channel verifier",
        Accept: "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
      } : { "User-Agent": "Video For Lazies channel verifier" },
      redirect: inventory ? "manual" : "follow",
      signal: context.signal,
    });
  } catch {
    ensureDeadline(context);
    fail("network-error", "Could not fetch update discovery metadata. Try later or download the app manually.");
  }
  if (inventory && response.status >= 300 && response.status < 400) {
    await response.body?.cancel();
    fail("inventory-redirect", "Release inventory redirects are not allowed.");
  }
  if (!response.ok) {
    await response.body?.cancel();
    if (response.status === 403 || response.status === 429) {
      fail("rate-limited", "GitHub update discovery is temporarily unavailable. Respect the retry time or download the app manually.", {
        status: response.status,
        retryAfter: response.headers.get("retry-after"),
        rateLimitRemaining: response.headers.get("x-ratelimit-remaining"),
        rateLimitReset: response.headers.get("x-ratelimit-reset"),
      });
    }
    fail("http-error", `Update metadata request failed with HTTP ${response.status}.`, { status: response.status });
  }
  try {
    const bytes = await boundedBytes(response, maximum);
    ensureDeadline(context);
    return { bytes, headers: response.headers };
  } catch (error) {
    if (error instanceof ChannelVerificationError) throw error;
    ensureDeadline(context);
    fail("network-error", "Could not read update discovery metadata.");
  }
}

export function nextReleasePage(link, page, limits = DISCOVERY_LIMITS) {
  if (link === null) return false;
  const relations = new Set();
  let next = false;
  let last;
  for (const part of link.split(",")) {
    const fields = part.trim().split(";").map((field) => field.trim());
    const address = /^<([^>]+)>$/.exec(fields[0]);
    const relation = /^rel="([^"]+)"$/.exec(fields[1] ?? "");
    if (fields.length !== 2 || !address || !relation || relations.has(relation[1])) {
      fail("invalid-pagination", "Invalid or ambiguous release discovery pagination.");
    }
    relations.add(relation[1]);
    let linked;
    try { linked = new URL(address[1]).searchParams.get("page"); } catch { /* rejected below */ }
    if (!linked || !/^[1-9]\d*$/.test(linked) || Number(linked) > limits.maxPages) {
      fail("history-cap", "Release history exceeds this client's discovery cap. Download a newer app manually.");
    }
    const linkedPage = Number(linked);
    if (address[1] !== `${RELEASES_API_URL}?per_page=${limits.perPage}&page=${linkedPage}`) {
      fail("invalid-pagination", "Release discovery pagination left the expected repository.");
    }
    if (relation[1] === "next" && linkedPage === page + 1) next = true;
    else if (relation[1] === "prev" && page > 1 && linkedPage === page - 1) { /* valid previous page */ }
    else if (relation[1] === "first" && linkedPage === 1) { /* valid first page */ }
    else if (relation[1] === "last" && linkedPage >= page) last = linkedPage;
    else fail("invalid-pagination", "Inconsistent release discovery pagination.");
  }
  if (!next && last !== undefined && last !== page) fail("invalid-pagination", "Incomplete release discovery pagination.");
  return next;
}

async function inventoryWithContext(context) {
  const releases = [];
  let totalBytes = 0;
  let firstPage;
  for (let page = 1; page <= context.limits.maxPages; page += 1) {
    const url = `${RELEASES_API_URL}?per_page=${context.limits.perPage}&page=${page}`;
    const remaining = context.limits.inventoryBytes - totalBytes;
    if (remaining <= 0) fail("metadata-too-large", "Complete release inventory exceeds its byte limit.");
    const { bytes, headers } = await requestBytes(context, url, Math.min(context.limits.pageBytes, remaining), true);
    totalBytes += bytes.length;
    if (totalBytes > context.limits.inventoryBytes) fail("metadata-too-large", "Complete release inventory exceeds its byte limit.");
    const records = parseJson(bytes, "Release inventory");
    if (!Array.isArray(records) || records.length > context.limits.perPage) {
      fail("invalid-inventory", "Release inventory exceeds its page record limit or is not an array.");
    }
    requireReleaseRecords(records);
    if (page === 1) firstPage = records.map(releaseIdentity);
    releases.push(...records);
    if (releases.length >= context.limits.perPage * context.limits.maxPages) {
      fail("history-cap", "Release history reaches this client's discovery cap. Download a newer app manually; old releases must not be deleted or hidden.");
    }
    // Link is only a continuation hint. Never follow its URL or host.
    const hasNext = nextReleasePage(headers.get("link"), page, context.limits);
    if ((page > 1 || hasNext) && records.length === 0) fail("invalid-pagination", "Incomplete release discovery inventory.");
    if (!hasNext) {
      if (page > 1) {
        const remainingHead = context.limits.inventoryBytes - totalBytes;
        if (remainingHead <= 0) fail("metadata-too-large", "Complete release inventory exceeds its byte limit.");
        const head = await requestBytes(context, `${RELEASES_API_URL}?per_page=${context.limits.perPage}&page=1`, Math.min(context.limits.pageBytes, remainingHead), true);
        totalBytes += head.bytes.length;
        if (totalBytes > context.limits.inventoryBytes) fail("metadata-too-large", "Complete release inventory exceeds its byte limit.");
        const fresh = parseJson(head.bytes, "Release inventory head");
        if (!Array.isArray(fresh) || fresh.length > context.limits.perPage) fail("invalid-inventory", "Release inventory head is malformed.");
        requireReleaseRecords(fresh);
        if (!nextReleasePage(head.headers.get("link"), 1, context.limits) || !isDeepStrictEqual(firstPage, fresh.map(releaseIdentity))) {
          fail("inventory-changed", "Releases changed during discovery. Try later; no update was selected.");
        }
      }
      return { releases, pages: page, bytes: totalBytes, headRechecked: page > 1 };
    }
    if (page === context.limits.maxPages) {
      fail("history-cap", "Release history exceeds this client's discovery cap. Download a newer app manually; old releases must not be deleted or hidden.");
    }
  }
  fail("history-cap", "Release discovery could not enumerate the complete history.");
}

function repositoryUrl(raw, prefix) {
  try {
    const url = new URL(raw);
    return url.protocol === "https:" && url.hostname === "github.com" && url.pathname.startsWith(prefix);
  } catch { return false; }
}

function validateSignedCandidate(manifest, candidate) {
  const releaseUrl = `${RELEASES_BASE_URL}/tag/${candidate.tag}`;
  if (!manifest || manifest.schema !== UPDATE_MANIFEST_SCHEMA || manifest.appId !== APP_ID
    || manifest.channel !== UPDATE_CHANNEL || manifest.version !== candidate.version
    || manifest.releaseTag !== candidate.tag || manifest.releaseUrl !== releaseUrl
    || !repositoryUrl(manifest.notes?.url, "/Setmaster/Video_For_Lazies/releases/")
    || typeof manifest.notes?.title !== "string" || typeof manifest.notes?.summary !== "string"
    || typeof manifest.publishedAt !== "string" || !Number.isSafeInteger(manifest.minUpdaterProtocol)
    || manifest.minUpdaterProtocol < 0 || manifest.minUpdaterProtocol > 1) {
    fail("manifest-binding", "Selected signed manifest has an invalid release, channel or updater protocol binding.");
  }
  if (!manifest.artifacts || typeof manifest.artifacts !== "object" || Array.isArray(manifest.artifacts)) {
    fail("manifest-binding", "Signed manifest has no valid platform artifact map.");
  }
  for (const [target, artifact] of Object.entries(manifest.artifacts)) {
    if (!DEFAULT_RELEASE_TARGETS.includes(target) || !artifact || typeof artifact.fileName !== "string"
      || !artifact.fileName.endsWith(".zip") || /[/\\\0:]/.test(artifact.fileName)
      || !Number.isSafeInteger(artifact.sizeBytes) || artifact.sizeBytes <= 0 || artifact.sizeBytes > MAX_ARCHIVE_BYTES
      || artifact.rootDir !== "Video_For_Lazies" || !/^[a-f0-9]{64}$/.test(artifact.sha256 ?? "")
      || !repositoryUrl(artifact.url, "/Setmaster/Video_For_Lazies/releases/download/")
      || new URL(artifact.url).pathname.split("/").at(-1) !== artifact.fileName
      || artifact.payloadManifest?.path !== "Video_For_Lazies/VFL_PAYLOAD_MANIFEST.json"
      || !/^[a-f0-9]{64}$/.test(artifact.payloadManifest?.sha256 ?? "")) {
      fail("manifest-binding", "Signed manifest has an invalid platform artifact or payload binding.");
    }
  }
  const artifact = manifest.artifacts?.[candidate.target];
  if (!artifact || artifact.fileName !== candidate.archive.name || artifact.url !== candidate.archive.browser_download_url
    || artifact.sizeBytes !== candidate.archive.size || artifact.rootDir !== "Video_For_Lazies"
    || artifact.sizeBytes > MAX_ARCHIVE_BYTES
    || !/^[a-f0-9]{64}$/.test(artifact.sha256 ?? "")
    || artifact.payloadManifest?.path !== "Video_For_Lazies/VFL_PAYLOAD_MANIFEST.json"
    || !/^[a-f0-9]{64}$/.test(artifact.payloadManifest?.sha256 ?? "")) {
    fail("manifest-binding", "Selected signed manifest does not bind the expected platform archive and payload.");
  }
  return artifact;
}

export async function verifyUpdateChannels({
  targets = DEFAULT_RELEASE_TARGETS,
  inventoryOnly = false,
  fetchImpl = globalThis.fetch,
  publicKeys,
  limits = {},
} = {}) {
  const selectedTargets = validateReleaseTargets(targets);
  const resolvedLimits = { ...DISCOVERY_LIMITS, ...limits };
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), resolvedLimits.timeoutMs);
  const context = { fetchImpl, limits: resolvedLimits, signal: controller.signal, deadlineAt: Date.now() + resolvedLimits.timeoutMs };
  try {
    const inventory = await inventoryWithContext(context);
    const readiness = discoveryReadiness(inventory.releases.length, resolvedLimits.perPage * resolvedLimits.maxPages);
    const result = { ok: true, inventory: { pages: inventory.pages, bytes: inventory.bytes, headRechecked: inventory.headRechecked, ...readiness }, channels: {} };
    ensureDeadline(context);
    if (inventoryOnly) return result;
    const keys = publicKeys ?? await readEmbeddedPublicKeys();
    for (const target of selectedTargets) {
      const candidate = selectPlatformRelease(inventory.releases, target);
      const { bytes: manifestBytes } = await requestBytes(context, candidate.metadata.browser_download_url, resolvedLimits.manifestBytes);
      const { bytes: signatureBytes } = await requestBytes(context, candidate.signature.browser_download_url, resolvedLimits.signatureBytes);
      let signatureTrusted = false;
      try {
        signatureTrusted = verifyManifestSignature(manifestBytes, signatureBytes.toString("utf8"), keys);
      } catch {
        // Malformed signatures are untrusted too; never try an older release.
      }
      if (!signatureTrusted) {
        fail("untrusted-signature", `Selected update manifest signature is not trusted for ${target}.`);
      }
      const manifest = parseJson(manifestBytes, "Signed update manifest");
      const artifact = validateSignedCandidate(manifest, candidate);
      result.channels[target] = {
        version: candidate.version,
        releaseTag: candidate.tag,
        releaseId: candidate.id,
        releaseUrl: manifest.releaseUrl,
        archive: { name: artifact.fileName, url: artifact.url, sizeBytes: artifact.sizeBytes, sha256: artifact.sha256 },
        signatureVerified: true,
      };
    }
    ensureDeadline(context);
    return result;
  } finally {
    clearTimeout(timer);
  }
}

async function main() {
  const args = process.argv.slice(2);
  if (args.some((arg) => arg !== "--inventory-only")) throw new Error("Usage: node scripts/verify-update-channels.mjs [--inventory-only]");
  const result = await verifyUpdateChannels({ inventoryOnly: args.includes("--inventory-only") });
  console.log(JSON.stringify(result, null, 2));
  if (!result.inventory.ready) process.exitCode = 1;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    await main();
  } catch (error) {
    console.error(JSON.stringify({ ok: false, error: { code: error.code ?? "verification-failed", message: error.message, status: error.status, retryAfter: error.retryAfter, rateLimitRemaining: error.rateLimitRemaining, rateLimitReset: error.rateLimitReset } }, null, 2));
    process.exitCode = 1;
  }
}
