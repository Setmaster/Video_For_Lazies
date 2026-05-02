# Release Checklist

This project is not publicized or released by the build scripts. Repository visibility and GitHub Releases are manual owner actions.

## Versioning Protocol

Use SemVer for app versions: `MAJOR.MINOR.PATCH`, with optional prerelease suffixes such as `1.0.0-beta.1`.

- `PATCH`: bug fixes, UI polish, docs, packaging, or release-script fixes.
- `MINOR`: new user-facing features or meaningful workflow improvements.
- `MAJOR`: breaking behavior, major workflow changes, compatibility breaks, or a major product repositioning.
- Git tags use `vX.Y.Z`, for example `v1.0.5`.
- Keep these metadata fields synchronized before tagging or building release artifacts:
  - `app/package.json`
  - `app/package-lock.json`
  - `app/src-tauri/Cargo.toml`
  - `app/src-tauri/Cargo.lock`
  - `app/src-tauri/tauri.conf.json`

Useful commands:

```bash
cd app
npm run version:check
npm run version:set -- 0.1.1
npm run version:check -- 0.1.1
```

For tagged artifact builds, commit the version bump first, then tag the same commit:

```bash
git tag v0.1.1
git push origin main v0.1.1
```

## Source-Public Gate

- Confirm `npm audit --audit-level=moderate` is clean.
- Run `npm test`, `npx tsc --noEmit`, `npm run build`, and `cargo test --manifest-path app/src-tauri/Cargo.toml`.
- Confirm no tracked secrets, local usernames, private paths, or generated release artifacts are present.
- Confirm `LICENSE`, `SECURITY.md`, `THIRD_PARTY_NOTICES.md`, and `SOURCE.md` are present.
- Before changing repository visibility, scan committed history for local-machine identifiers:

```bash
git rev-list --all | xargs git grep -n -F "local-username" -- 2>/dev/null || true
git rev-list --all | xargs git grep -n -F "/home/local-username" -- 2>/dev/null || true
git rev-list --all | xargs git grep -n -F "C:/Users/local-username" -- 2>/dev/null || true
```

If a real local identifier appears in committed history, rewrite history from a clean worktree with `git filter-repo`, then force-push only after the owner explicitly approves the push.

## Portable Release Workflow

The `Portable Release` GitHub Actions workflow builds private x64 artifacts for:

- `linux-x64`
- `win-x64`

It runs on manual dispatch with an explicit version, and on pushed `v*.*.*` tags. The requested version or tag must match the synchronized app metadata. The workflow builds both portable zips, creates release notes, stages a combined `SHA256SUMS.txt`, and creates a GitHub Release with the final assets. It defaults to a draft prerelease so the generated changelog and uploaded binaries can be reviewed before publishing. The workflow does not change repository visibility.

Manual dispatch inputs:

- `version`: required SemVer value, for example `0.1.1`; must match app metadata.
- `release_notes`: optional curated changelog or summary. If blank, the workflow generates a summary from commits.
- `previous_tag`: optional changelog start tag. Use `none` for a first release.
- `draft`: defaults to `true`.
- `prerelease`: defaults to `true`.

Release notes follow this template:

```text
# Video For Lazies vX.Y.Z

## Release Summary
<curated notes, or generated summary>

## Changes
<categorized commits since previous release tag>

## Artifacts
<portable zip assets and checksum file>

## Runtime Notes
<Windows sidecar and Linux FFmpeg requirements>

## Verification
<workflow verification summary>
```

Recommended manual protocol for an agent-triggered release:

1. Run `git fetch --tags origin`.
2. Find the previous release tag with `git tag --merged HEAD --sort=-version:refname --list 'v[0-9]*'`.
3. Review commits since that tag with `git log --oneline <previous-tag>..HEAD`, or use `git log --oneline HEAD` for the first release.
4. Write the curated `release_notes` input using the template above.
5. Trigger `Portable Release` with `draft=true`.
6. Review the draft release notes, Linux zip, Windows zip, and `SHA256SUMS.txt`.
7. Publish the draft only when the owner wants that release visible to repo collaborators.

Run locally on Linux or Windows:

```bash
cd app
npm run release:portable
```

The script builds `release/Video_For_Lazies/`, creates a versioned zip, writes `release/SHA256SUMS.txt`, extracts the zip, and verifies the payload.

Linux artifact example:

```text
release/Video_For_Lazies-v0.1.0-linux-x64.zip
```

Windows artifact example:

```text
release/Video_For_Lazies-v0.1.0-win-x64.zip
```

If `7z.exe` is available on the Windows host, the local script also creates a versioned `.7z` archive. The GitHub Actions release workflow uploads the final `.zip` artifacts and combined checksum file.

## Workflow Linting

CI runs `actionlint` v1.7.12 against GitHub Actions workflows. Run the same check locally before committing workflow changes:

```bash
actionlint -color
```

Required portable payload on all platforms:

- app executable
- `README.md`
- `LICENSE.txt`
- `THIRD_PARTY_NOTICES.md`
- `SOURCE.md`
- `FFMPEG_BUNDLING.md`

Additional required Windows payload:

- `ffmpeg-sidecar/` with FFmpeg binaries, FFmpeg license text, bundle notices, and source archive

Linux artifacts currently do not bundle FFmpeg. They require `ffmpeg` and `ffprobe` on `PATH`, or `VFL_FFMPEG_PATH` and `VFL_FFPROBE_PATH` set at runtime.

## Unsigned Alpha Note

The first public Windows binary may be an unsigned portable alpha. Release notes should say that Windows may show SmartScreen or antivirus warnings because the executable is unsigned.
