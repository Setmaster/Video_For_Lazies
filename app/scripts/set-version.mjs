#!/usr/bin/env node
import { setProjectVersion } from "./versioning.mjs";

const version = process.argv.find((arg, index) => index >= 2 && !arg.startsWith("--"));

if (!version) {
  console.error("Usage: npm run version:set -- <version>");
  process.exitCode = 1;
} else {
  try {
    const { version: normalizedVersion } = await setProjectVersion(version);
    console.log(`Set project version to ${normalizedVersion}.`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
