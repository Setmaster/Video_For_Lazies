import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";

import {
  decodeTauriSignerText,
  parseMinisignPublicKey,
  parseMinisignSignature,
  readEmbeddedPublicKeys,
  verifyManifestSignature,
} from "../scripts/verify-update-manifest.mjs";

function rawEd25519PublicKey(publicKey) {
  const der = publicKey.export({ format: "der", type: "spki" });
  return der.subarray(der.length - 32);
}

function minisignPublicKeyText(publicKey, keyId) {
  const payload = Buffer.concat([Buffer.from("Ed", "latin1"), keyId, rawEd25519PublicKey(publicKey)]);
  return `untrusted comment: test public key\n${payload.toString("base64")}`;
}

function minisignSignatureText(privateKey, keyId, manifestBytes) {
  // Prehashed mode, matching what tauri signer produces.
  const digest = crypto.createHash("blake2b512").update(manifestBytes).digest();
  const signature = crypto.sign(null, digest, privateKey);
  const payload = Buffer.concat([Buffer.from("ED", "latin1"), keyId, signature]);
  const globalLine = Buffer.alloc(64).toString("base64");
  return `untrusted comment: test signature\n${payload.toString("base64")}\ntrusted comment: test\n${globalLine}`;
}

test("verifyManifestSignature accepts a matching prehashed minisign signature", () => {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
  const keyId = crypto.randomBytes(8);
  const manifest = Buffer.from(JSON.stringify({ schema: 1, version: "9.9.9" }));

  const publicKeyText = minisignPublicKeyText(publicKey, keyId);
  const signatureText = minisignSignatureText(privateKey, keyId, manifest);

  assert.equal(verifyManifestSignature(manifest, signatureText, [publicKeyText]), true);

  // base64-wrapped variants (how tauri artifacts ship them) decode the same.
  const wrappedKey = Buffer.from(publicKeyText, "utf8").toString("base64");
  const wrappedSignature = Buffer.from(signatureText, "utf8").toString("base64");
  assert.equal(verifyManifestSignature(manifest, wrappedSignature, [wrappedKey]), true);
});

test("verifyManifestSignature rejects wrong keys, key ids, and tampered manifests", () => {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
  const other = crypto.generateKeyPairSync("ed25519");
  const keyId = crypto.randomBytes(8);
  const manifest = Buffer.from(JSON.stringify({ schema: 1, version: "9.9.9" }));
  const signatureText = minisignSignatureText(privateKey, keyId, manifest);

  // Signed with a key the app does not embed.
  assert.equal(
    verifyManifestSignature(manifest, signatureText, [minisignPublicKeyText(other.publicKey, keyId)]),
    false,
  );
  // Right key material, wrong key id: skipped, not trusted.
  assert.equal(
    verifyManifestSignature(manifest, signatureText, [
      minisignPublicKeyText(publicKey, crypto.randomBytes(8)),
    ]),
    false,
  );
  // Tampered content.
  assert.equal(
    verifyManifestSignature(Buffer.from("tampered"), signatureText, [
      minisignPublicKeyText(publicKey, keyId),
    ]),
    false,
  );
});

test("embedded updater public keys parse as minisign ed25519 keys", async () => {
  const keys = await readEmbeddedPublicKeys();
  assert.ok(keys.length >= 1);

  for (const key of keys) {
    const parsed = parseMinisignPublicKey(decodeTauriSignerText(key));
    assert.equal(parsed.publicKey.length, 32);
    assert.equal(parsed.keyId.length, 8);
  }
});

test("parseMinisignSignature rejects malformed payloads", () => {
  assert.throws(() => parseMinisignSignature("untrusted comment: only a comment"));
  assert.throws(() => parseMinisignSignature(`untrusted comment: x\n${Buffer.alloc(10).toString("base64")}`));
});
