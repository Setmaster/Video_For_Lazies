import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import { downloadVerifiedFile } from "../scripts/sync-ffmpeg-sidecar.mjs";

function sha256(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

test("verified sidecar download falls back after a body/checksum failure under one deadline", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "vfl-sidecar-download-"));
  const destinationPath = path.join(root, "bundle.bin");
  const expected = Buffer.from("verified sidecar bytes");
  const partials = [];
  let clock = 1_000;
  const remaining = [];
  try {
    await downloadVerifiedFile({
      destinationPath,
      url: "https://example.invalid/bundle",
      expectedSha256: sha256(expected),
      timeoutMs: 1_000,
      now: () => clock,
      partialPathForAttempt: (_destination, index) => {
        const partial = path.join(root, `attempt-${index}.partial`);
        partials.push(partial);
        return partial;
      },
      attempts: [
        {
          label: "fetch",
          async run(_url, tempPath, remainingMs) {
            remaining.push(remainingMs);
            await fs.writeFile(tempPath, "truncated response");
            clock += 250;
          },
        },
        {
          label: "curl",
          async run(_url, tempPath, remainingMs) {
            remaining.push(remainingMs);
            await fs.writeFile(tempPath, expected);
          },
        },
      ],
    });

    assert.deepEqual(await fs.readFile(destinationPath), expected);
    assert.deepEqual(remaining, [1_000, 750]);
    for (const partial of partials) {
      await assert.rejects(fs.access(partial));
    }
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});

test("failed sidecar transports remove partials and do not publish unverified bytes", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "vfl-sidecar-download-"));
  const destinationPath = path.join(root, "bundle.bin");
  const partials = [];
  try {
    await assert.rejects(
      downloadVerifiedFile({
        destinationPath,
        url: "https://example.invalid/bundle",
        expectedSha256: sha256(Buffer.from("expected")),
        partialPathForAttempt: (_destination, index) => {
          const partial = path.join(root, `failed-${index}.partial`);
          partials.push(partial);
          return partial;
        },
        attempts: [
          {
            label: "fetch",
            async run(_url, tempPath) {
              await fs.writeFile(tempPath, "partial");
              throw new Error("body reset");
            },
          },
          {
            label: "curl",
            async run(_url, tempPath) {
              await fs.writeFile(tempPath, "wrong checksum");
            },
          },
        ],
      }),
      /fetch: body reset; curl: checksum mismatch/,
    );

    await assert.rejects(fs.access(destinationPath));
    for (const partial of partials) {
      await assert.rejects(fs.access(partial));
    }
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});

test("sidecar fallback cannot exceed the shared artifact deadline", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "vfl-sidecar-download-"));
  const destinationPath = path.join(root, "bundle.bin");
  let clock = 0;
  let fallbackCalls = 0;
  try {
    await assert.rejects(
      downloadVerifiedFile({
        destinationPath,
        url: "https://example.invalid/bundle",
        expectedSha256: sha256(Buffer.from("expected")),
        timeoutMs: 100,
        now: () => clock,
        attempts: [
          {
            label: "fetch",
            async run() {
              clock = 101;
              throw new Error("timed out");
            },
          },
          {
            label: "curl",
            async run() {
              fallbackCalls += 1;
            },
          },
        ],
      }),
      /shared deadline expired/,
    );
    assert.equal(fallbackCalls, 0);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});

test("a valid transport that returns after the shared deadline is never published", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "vfl-sidecar-download-"));
  const destinationPath = path.join(root, "bundle.bin");
  const partialPath = path.join(root, "late.partial");
  const expected = Buffer.from("valid but late");
  let clock = 0;
  let fallbackCalls = 0;
  try {
    await assert.rejects(
      downloadVerifiedFile({
        destinationPath,
        url: "https://example.invalid/bundle",
        expectedSha256: sha256(expected),
        timeoutMs: 100,
        now: () => clock,
        partialPathForAttempt: () => partialPath,
        attempts: [
          {
            label: "fetch",
            async run(_url, tempPath) {
              await fs.writeFile(tempPath, expected);
              clock = 101;
            },
          },
          {
            label: "curl",
            async run() {
              fallbackCalls += 1;
            },
          },
        ],
      }),
      /shared deadline expired after transport/,
    );
    assert.equal(fallbackCalls, 0);
    await assert.rejects(fs.access(destinationPath));
    await assert.rejects(fs.access(partialPath));
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});

test("verification cannot publish bytes after consuming the shared deadline", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "vfl-sidecar-download-"));
  const destinationPath = path.join(root, "bundle.bin");
  const partialPath = path.join(root, "verification-late.partial");
  const expected = Buffer.from("valid before slow verification");
  let nowCalls = 0;
  const clockSamples = [0, 0, 0, 101];
  try {
    await assert.rejects(
      downloadVerifiedFile({
        destinationPath,
        url: "https://example.invalid/bundle",
        expectedSha256: sha256(expected),
        timeoutMs: 100,
        now: () => clockSamples[Math.min(nowCalls++, clockSamples.length - 1)],
        partialPathForAttempt: () => partialPath,
        attempts: [{
          label: "fetch",
          async run(_url, tempPath) {
            await fs.writeFile(tempPath, expected);
          },
        }],
      }),
      /shared deadline expired during verification/,
    );
    await assert.rejects(fs.access(destinationPath));
    await assert.rejects(fs.access(partialPath));
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});
