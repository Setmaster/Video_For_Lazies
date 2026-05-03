#!/usr/bin/env node
import fs from "node:fs/promises";
import path from "node:path";
import { spawn } from "node:child_process";

import { repoRoot } from "./ffmpegBundle.mjs";
import {
  UPDATE_MANIFEST_FILE_NAME,
  UPDATE_MANIFEST_SIGNATURE_FILE_NAME,
} from "./updateManifests.mjs";

function parseArgs(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith("--")) {
      throw new Error(`Unexpected argument: ${arg}`);
    }
    const key = arg.slice(2);
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) {
      throw new Error(`Missing value for --${key}`);
    }
    options[key] = value;
    index += 1;
  }
  return options;
}

async function runChecked(command, args, options = {}) {
  await new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: options.cwd,
      env: options.env,
      stdio: "inherit",
      shell: process.platform === "win32",
    });

    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(new Error(`${command} exited with code ${code}`));
    });
  });
}

export async function signUpdateManifest({ releaseAssetDir } = {}) {
  const resolvedReleaseAssetDir = path.resolve(repoRoot, releaseAssetDir ?? "release/release-assets");
  const manifestPath = path.resolve(resolvedReleaseAssetDir, UPDATE_MANIFEST_FILE_NAME);
  const signaturePath = path.resolve(resolvedReleaseAssetDir, UPDATE_MANIFEST_SIGNATURE_FILE_NAME);

  await fs.access(manifestPath);
  await fs.rm(signaturePath, { force: true });

  if (!process.env.TAURI_SIGNING_PRIVATE_KEY && !process.env.TAURI_SIGNING_PRIVATE_KEY_PATH) {
    throw new Error("TAURI_SIGNING_PRIVATE_KEY or TAURI_SIGNING_PRIVATE_KEY_PATH is required to sign the update manifest.");
  }

  await runChecked("npm", ["run", "tauri", "signer", "sign", "--", manifestPath], {
    cwd: path.resolve(repoRoot, "app"),
    env: process.env,
  });
  await fs.access(signaturePath);
  return signaturePath;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const signaturePath = await signUpdateManifest({
    releaseAssetDir: options["release-assets"],
  });
  console.log(`Update manifest signature written to: ${signaturePath}`);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  try {
    await main();
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
