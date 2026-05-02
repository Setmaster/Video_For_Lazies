#!/usr/bin/env node
import { assertSynchronizedVersion } from "./versioning.mjs";

const args = process.argv.slice(2);
const valueOnly = args.includes("--value");
const expectedVersion = args.find((arg) => !arg.startsWith("--")) ?? process.env.VFL_EXPECTED_VERSION;

try {
  const { version, versions } = await assertSynchronizedVersion(expectedVersion);
  if (valueOnly) {
    console.log(version);
  } else {
    console.log(`Version ${version} is synchronized across ${versions.length} metadata fields.`);
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
