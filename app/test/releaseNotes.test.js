import test from "node:test";
import assert from "node:assert/strict";

import {
  categorizeCommitSubject,
  cleanCommitSubject,
  normalizeManualNotes,
  renderReleaseNotes,
} from "../scripts/generate-release-notes.mjs";

test("release notes categorize conventional commit subjects", () => {
  assert.equal(categorizeCommitSubject("feat: add trimming"), "Added");
  assert.equal(categorizeCommitSubject("feat(ui)!: redesign trim handles"), "Added");
  assert.equal(categorizeCommitSubject("fix(export): avoid overwrite"), "Fixed");
  assert.equal(categorizeCommitSubject("ci: build portable zip"), "Build and release");
  assert.equal(categorizeCommitSubject("docs: update release guide"), "Documentation");
  assert.equal(categorizeCommitSubject("test: cover release notes"), "Tests");
  assert.equal(categorizeCommitSubject("chore: refresh metadata"), "Maintenance");
  assert.equal(categorizeCommitSubject("Initial public release"), "Other changes");
});

test("release notes clean conventional prefixes without hiding the useful subject", () => {
  assert.equal(cleanCommitSubject("ci: add versioned portable artifacts"), "add versioned portable artifacts");
  assert.equal(cleanCommitSubject("feat(ui)!: redesign trim handles"), "redesign trim handles");
  assert.equal(cleanCommitSubject("Initial public release"), "Initial public release");
});

test("release notes renderer uses the defined template and curated notes", () => {
  const notes = renderReleaseNotes({
    version: "0.1.1",
    previousTag: "v0.1.0",
    manualNotes: "Curated release summary.",
    commits: [
      { sha: "abcdef123456", subject: "feat: add release flow" },
      { sha: "123456abcdef", subject: "fix: correct archive names" },
    ],
    targetLabels: ["linux-x64", "win-x64"],
  });

  assert.match(notes, /^# Video For Lazies v0\.1\.1/m);
  assert.match(notes, /Curated release summary\./);
  assert.match(notes, /### Added/);
  assert.match(notes, /add release flow \(abcdef1\)/);
  assert.match(notes, /### Fixed/);
  assert.match(notes, /Video_For_Lazies-v0\.1\.1-linux-x64\.zip/);
  assert.match(notes, /Windows portable zips are unsigned/);
  assert.match(notes, /Linux x64 portable releases require/);
  assert.match(notes, /runtime codec behavior depends on the local FFmpeg/);
});

test("manual release notes are trimmed before rendering", () => {
  assert.equal(normalizeManualNotes("\n  summary\n"), "summary");
});
