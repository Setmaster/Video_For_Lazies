#!/usr/bin/env node
// Post-sign guard: verify the freshly signed update manifest against the
// public keys embedded in the shipped app. A rotated or mispasted signing
// secret would otherwise publish cleanly while every installed app silently
// rejects the manifest, killing the update channel with no pipeline signal.
import fs from "node:fs/promises";
import path from "node:path";
import crypto from "node:crypto";

import { repoRoot } from "./ffmpegBundle.mjs";
import {
  UPDATE_MANIFEST_FILE_NAME,
  UPDATE_MANIFEST_SIGNATURE_FILE_NAME,
} from "./updateManifests.mjs";

const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

export function decodeTauriSignerText(raw) {
  const trimmed = String(raw).trim();
  if (trimmed.startsWith("untrusted comment:")) return trimmed;
  return Buffer.from(trimmed, "base64").toString("utf8").trim();
}

function decodePayloadLine(text, expectedLength) {
  const lines = text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  const payloadLine = lines.find(
    (line) => !line.startsWith("untrusted comment:") && !line.startsWith("trusted comment:"),
  );
  if (!payloadLine) throw new Error("No payload line found in signer text.");
  const payload = Buffer.from(payloadLine, "base64");
  if (payload.length !== expectedLength) {
    throw new Error(`Unexpected signer payload length ${payload.length}, expected ${expectedLength}.`);
  }
  return payload;
}

export function parseMinisignPublicKey(text) {
  // 2-byte algorithm + 8-byte key id + 32-byte ed25519 public key.
  const payload = decodePayloadLine(text, 42);
  const algorithm = payload.subarray(0, 2).toString("latin1");
  if (algorithm !== "Ed") throw new Error(`Unsupported public key algorithm: ${algorithm}`);
  return { keyId: payload.subarray(2, 10), publicKey: payload.subarray(10, 42) };
}

export function parseMinisignSignature(text) {
  // 2-byte algorithm + 8-byte key id + 64-byte signature.
  const payload = decodePayloadLine(text, 74);
  const algorithm = payload.subarray(0, 2).toString("latin1");
  if (algorithm !== "ED" && algorithm !== "Ed") {
    throw new Error(`Unsupported signature algorithm: ${algorithm}`);
  }
  return { algorithm, keyId: payload.subarray(2, 10), signature: payload.subarray(10, 74) };
}

// Mirrors verify_manifest_signature in app/src-tauri/src/updater.rs (primary
// signature only; the trusted-comment global signature is not re-checked here
// because the failure mode this guards is key mismatch, not tampering).
export function verifyManifestSignature(manifestBytes, signatureText, publicKeyTexts) {
  const signature = parseMinisignSignature(decodeTauriSignerText(signatureText));
  for (const publicKeyText of publicKeyTexts) {
    let publicKey;
    try {
      publicKey = parseMinisignPublicKey(decodeTauriSignerText(publicKeyText));
    } catch {
      continue;
    }
    if (!publicKey.keyId.equals(signature.keyId)) continue;

    // Prehashed minisign ("ED", what tauri signer emits) signs the
    // BLAKE2b-512 digest of the content; legacy "Ed" signs the raw bytes.
    const message =
      signature.algorithm === "ED"
        ? crypto.createHash("blake2b512").update(manifestBytes).digest()
        : Buffer.from(manifestBytes);
    const keyObject = crypto.createPublicKey({
      key: Buffer.concat([ED25519_SPKI_PREFIX, publicKey.publicKey]),
      format: "der",
      type: "spki",
    });
    if (crypto.verify(null, message, keyObject, signature.signature)) {
      return true;
    }
  }
  return false;
}

export async function readEmbeddedPublicKeys() {
  const updaterSource = await fs.readFile(
    path.resolve(repoRoot, "app/src-tauri/src/updater.rs"),
    "utf8",
  );
  const block = updaterSource.match(/const UPDATE_PUBLIC_KEYS[^=]*=\s*&\[([\s\S]*?)\];/);
  if (!block) throw new Error("UPDATE_PUBLIC_KEYS not found in app/src-tauri/src/updater.rs");
  const keys = [...block[1].matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((m) => m[1]);
  if (!keys.length) throw new Error("UPDATE_PUBLIC_KEYS is empty in app/src-tauri/src/updater.rs");
  return keys;
}

function parseArgs(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith("--")) throw new Error(`Unexpected argument: ${arg}`);
    const key = arg.slice(2);
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) throw new Error(`Missing value for --${key}`);
    options[key] = value;
    index += 1;
  }
  return options;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const releaseAssetDir = path.resolve(repoRoot, options["release-assets"] ?? "release/release-assets");
  const manifestPath = path.resolve(releaseAssetDir, UPDATE_MANIFEST_FILE_NAME);
  const signaturePath = path.resolve(releaseAssetDir, UPDATE_MANIFEST_SIGNATURE_FILE_NAME);

  const manifestBytes = await fs.readFile(manifestPath);
  const signatureText = await fs.readFile(signaturePath, "utf8");
  const publicKeys = await readEmbeddedPublicKeys();

  if (!verifyManifestSignature(manifestBytes, signatureText, publicKeys)) {
    throw new Error(
      "Update manifest signature does not verify against the app's embedded public keys. The signing secret and the embedded UPDATE_PUBLIC_KEYS are out of sync; shipping this release would kill the update channel.",
    );
  }
  console.log(`Update manifest signature verified against embedded public keys: ${manifestPath}`);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  try {
    await main();
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
