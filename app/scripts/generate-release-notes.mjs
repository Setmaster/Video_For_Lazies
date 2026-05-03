#!/usr/bin/env node
import fs from "node:fs/promises";
import path from "node:path";
import { spawn } from "node:child_process";

import { getPortableTargetLabel } from "./portableRelease.mjs";
import { normalizeVersionInput } from "./versioning.mjs";
import { repoRoot } from "./ffmpegBundle.mjs";

const CATEGORY_ORDER = [
  "Added",
  "Fixed",
  "Changed",
  "Build and release",
  "Documentation",
  "Tests",
  "Maintenance",
  "Other changes",
];

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

async function capture(command, args, { cwd = repoRoot } = {}) {
  return await new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";

    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) {
        resolve(stdout);
        return;
      }
      reject(new Error(`${command} ${args.join(" ")} failed with code ${code}\n${stderr}`));
    });
  });
}

function normalizeTag(version) {
  return `v${normalizeVersionInput(version)}`;
}

export function categorizeCommitSubject(subject) {
  const normalized = subject.trim();
  const lower = normalized.toLowerCase();

  if (/^(feat|feature)(\([^)]+\))?!?:/.test(lower)) {
    return "Added";
  }
  if (/^(fix|bugfix)(\([^)]+\))?!?:/.test(lower)) {
    return "Fixed";
  }
  if (/^(perf|refactor)(\([^)]+\))?!?:/.test(lower)) {
    return "Changed";
  }
  if (/^(ci|build|release)(\([^)]+\))?!?:/.test(lower)) {
    return "Build and release";
  }
  if (/^docs?(\([^)]+\))?!?:/.test(lower)) {
    return "Documentation";
  }
  if (/^tests?(\([^)]+\))?!?:/.test(lower)) {
    return "Tests";
  }
  if (/^(chore|style)(\([^)]+\))?!?:/.test(lower)) {
    return "Maintenance";
  }

  return "Other changes";
}

export function cleanCommitSubject(subject) {
  return subject.trim().replace(/^[a-z]+(?:\([^)]+\))?!?:\s*/i, "");
}

function shortSha(sha) {
  return sha.slice(0, 7);
}

export function normalizeManualNotes(notes) {
  return String(notes ?? "").trim();
}

function formatGeneratedSummary({ commits, previousTag, tag }) {
  const commitWord = commits.length === 1 ? "commit" : "commits";
  if (previousTag) {
    return `Release generated from ${commits.length} ${commitWord} in ${previousTag}..${tag}.`;
  }
  return `Initial release generated from ${commits.length} ${commitWord} for ${tag}.`;
}

function formatCommitLine(commit) {
  return `- ${cleanCommitSubject(commit.subject)} (${shortSha(commit.sha)})`;
}

export function renderReleaseNotes({
  version,
  previousTag,
  commits,
  manualNotes = "",
  targetLabels = ["linux-x64", "win-x64"],
} = {}) {
  const tag = normalizeTag(version);
  const grouped = new Map(CATEGORY_ORDER.map((category) => [category, []]));

  for (const commit of commits) {
    grouped.get(categorizeCommitSubject(commit.subject)).push(commit);
  }

  const lines = [
    `# Video For Lazies ${tag}`,
    "",
    "## Release Summary",
    "",
  ];

  const normalizedManualNotes = normalizeManualNotes(manualNotes);
  if (normalizedManualNotes) {
    lines.push(normalizedManualNotes, "");
  } else {
    lines.push(formatGeneratedSummary({ commits, previousTag, tag }), "");
  }

  lines.push("## Changes", "");
  for (const category of CATEGORY_ORDER) {
    const categoryCommits = grouped.get(category);
    if (!categoryCommits?.length) {
      continue;
    }
    lines.push(`### ${category}`, "");
    for (const commit of categoryCommits) {
      lines.push(formatCommitLine(commit));
    }
    lines.push("");
  }

  lines.push("## Artifacts", "");
  for (const targetLabel of targetLabels) {
    lines.push(`- \`Video_For_Lazies-v${version}-${targetLabel}.zip\``);
  }
  lines.push("- `SHA256SUMS.txt`");
  lines.push("- `vfl-update-manifest-v1.json`");
  lines.push("- `vfl-update-manifest-v1.json.sig`", "");

  lines.push("## Runtime Notes", "");
  lines.push("- Windows x64 and Linux x64 portable releases bundle pinned GPL FFmpeg sidecars and include FFmpeg source plus build-provenance archives in the portable payload.");
  lines.push("- Updater-capable releases verify a signed update manifest before downloading or applying future portable updates.");
  lines.push("- Windows portable zips are unsigned and may trigger SmartScreen or antivirus reputation warnings. Verify the SHA256 checksum before running.");
  lines.push("- `VFL_FFMPEG_PATH` and `VFL_FFPROBE_PATH` remain available for custom runtime overrides on both platforms.");
  lines.push("- Repository visibility is not changed by this release workflow.", "");

  lines.push("## Verification", "");
  lines.push("- GitHub Actions builds and verifies the portable zip artifacts before creating the release.");
  lines.push("- The release includes a combined SHA256 checksum file for uploaded portable archives.", "");

  return lines.join("\n");
}

function parseGitLog(raw) {
  return raw
    .split("\x1e")
    .map((entry) => entry.trim())
    .filter(Boolean)
    .map((entry) => {
      const [sha, subject] = entry.split("\x1f");
      return { sha, subject };
    });
}

async function listReleaseTags() {
  const raw = await capture("git", ["tag", "--merged", "HEAD", "--sort=-version:refname", "--list", "v[0-9]*"]);
  return raw
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
}

async function resolvePreviousTag({ currentTag, explicitPreviousTag }) {
  const normalizedExplicitPreviousTag = normalizeManualNotes(explicitPreviousTag);
  if (normalizedExplicitPreviousTag) {
    return normalizedExplicitPreviousTag === "none" ? null : normalizedExplicitPreviousTag;
  }

  const tags = await listReleaseTags();
  return tags.find((tag) => tag !== currentTag) ?? null;
}

async function readCommits(previousTag) {
  const range = previousTag ? `${previousTag}..HEAD` : "HEAD";
  return parseGitLog(await capture("git", ["log", "--reverse", "--format=%H%x1f%s%x1e", range]));
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const version = normalizeVersionInput(options.version ?? process.env.VFL_RELEASE_VERSION);
  const outputPath = path.resolve(repoRoot, options.output ?? "release/RELEASE_NOTES.md");
  const currentTag = normalizeTag(version);
  const previousTag = await resolvePreviousTag({
    currentTag,
    explicitPreviousTag: options["previous-tag"] ?? normalizeManualNotes(process.env.VFL_PREVIOUS_TAG),
  });
  const commits = await readCommits(previousTag);
  const targetLabels = [
    getPortableTargetLabel({ platform: "linux", arch: "x64" }),
    getPortableTargetLabel({ platform: "win32", arch: "x64" }),
  ];

  const notes = renderReleaseNotes({
    version,
    previousTag,
    commits,
    manualNotes: process.env.VFL_RELEASE_NOTES,
    targetLabels,
  });

  await fs.mkdir(path.dirname(outputPath), { recursive: true });
  await fs.writeFile(outputPath, notes);
  console.log(`Release notes written to ${outputPath}`);
  console.log(`Commit range: ${previousTag ? `${previousTag}..HEAD` : "HEAD"}`);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  try {
    await main();
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
