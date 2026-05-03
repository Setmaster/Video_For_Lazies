# Release Checklist

The repository is public, but release publication is still an owner-approved action. Build scripts create artifacts and GitHub release drafts; they do not decide when a stable release should be published.

`main` is the stable-release branch. Use `dev` for development, then merge to `main` only after the owner approves the final readiness report.

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

For stable tagged artifact builds, commit the version bump on `dev`, merge the approved result to `main`, then tag the `main` commit:

```bash
git tag v0.1.1
git push origin main v0.1.1
```

For draft prereleases or release-candidate checks before approval, run the manual workflow from the development branch and leave the release as a draft.

## Public Source Gate

- Confirm `npm audit --audit-level=moderate` is clean.
- Run `npm test`, `npx tsc --noEmit`, `npm run build`, `cargo fmt --manifest-path app/src-tauri/Cargo.toml --check`, `cargo test --manifest-path app/src-tauri/Cargo.toml`, `cargo clippy --manifest-path app/src-tauri/Cargo.toml --all-targets -- -D warnings`, and `cd app/src-tauri && cargo audit`.
- Confirm no tracked secrets, local usernames, private paths, or generated release artifacts are present.
- Confirm reachable commit author and committer emails are approved public identities, preferably GitHub noreply addresses.
- Confirm `LICENSE`, `SECURITY.md`, `THIRD_PARTY_NOTICES.md`, and `SOURCE.md` are present.
- Confirm GitHub private vulnerability reporting is enabled.
- Confirm `SECURITY.md` points reporters to GitHub private vulnerability reporting, not a public personal email.
- Scan committed history for local-machine identifiers:

```bash
git rev-list --all | xargs git grep -n -F "local-username" -- 2>/dev/null || true
git rev-list --all | xargs git grep -n -F "/home/local-username" -- 2>/dev/null || true
git rev-list --all | xargs git grep -n -F "C:/Users/local-username" -- 2>/dev/null || true
```

If a real local identifier appears in committed history, stop and get owner approval before any history rewrite or force-push.

## Portable Release Workflow

The `Portable Release` GitHub Actions workflow builds verified x64 artifacts for:

- `linux-x64`
- `win-x64`

It runs on manual dispatch with an explicit version, and on pushed `v*.*.*` tags. The requested version or tag must match the synchronized app metadata. The workflow builds both portable zips, creates release notes, stages a combined `SHA256SUMS.txt`, and creates a GitHub Release with the final assets. It defaults to a draft prerelease so the generated changelog and uploaded binaries can be reviewed before publishing.

Manual dispatch inputs:

- `version`: required SemVer value, for example `0.1.1`; must match app metadata.
- `release_notes`: optional curated changelog or summary. If blank, the workflow generates a summary from commits.
- `previous_tag`: optional changelog start tag. Use `none` for a first release.
- `draft`: defaults to `true`; keep this enabled until final review is complete.
- `prerelease`: defaults to `true`; set to `false` for an owner-approved stable release.

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
<Bundled FFmpeg sidecar notes>

## Verification
<workflow verification summary>
```

Recommended manual draft protocol:

1. Run `git fetch --tags origin`.
2. Find the previous release tag with `git tag --merged HEAD --sort=-version:refname --list 'v[0-9]*'`.
3. Review commits since that tag with `git log --oneline <previous-tag>..HEAD`, or use `git log --oneline HEAD` for the first release.
4. Write the curated `release_notes` input using the template above.
5. Trigger `Portable Release` with `draft=true`.
6. Review the draft release notes, Linux zip, Windows zip, and `SHA256SUMS.txt`.
7. Confirm the draft release is backed by the intended public `vX.Y.Z` tag.
8. Confirm generated `SOURCE.md` in each zip names the intended release tag and source commit.
9. Publish the draft only when the owner wants that release visible to the public.

Run locally on Linux or Windows:

```bash
cd app
npm run release:portable
```

The script builds `release/Video_For_Lazies/`, creates a versioned zip, writes `release/SHA256SUMS.txt`, extracts the zip, and verifies the payload.

Linux artifact example:

```text
release/Video_For_Lazies-vX.Y.Z-linux-x64.zip
```

Windows artifact example:

```text
release/Video_For_Lazies-vX.Y.Z-win-x64.zip
```

GitHub release assets are intentionally zip-only for now: the Linux zip, the Windows zip, and the combined checksum file.

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

Additional required sidecar payload on Windows and Linux:

- `ffmpeg-sidecar/` with FFmpeg binaries, FFmpeg license text, bundle notices, and source/provenance archives
- `ffmpeg-sidecar/source/ffmpeg-75d37c499da2a9fd50e3ef5a69c7dd87cd96f62a.tar.gz`
- `ffmpeg-sidecar/source/btbn-ffmpeg-builds-28ae7513e7b6477da5c9ba7edb07aa940d485fa2.tar.gz`
- `ffmpeg-sidecar/source/x264-0480cb05fa188d37ae87e8f4fd8f1aea3711f7ee.tar.gz`

Linux sidecar-specific payload also includes root `ffmpeg` and `ffprobe` wrapper scripts, `bin/ffmpeg`, `bin/ffprobe`, and the required shared libraries in `ffmpeg-sidecar/lib/`.

Linux release verification also launches the extracted packaged app in smoke mode, using Xvfb when no display is available, and requires the app to probe, encode, and write a readable trimmed MP4. The Windows smoke additionally verifies preview playback and trim/crop interactions.

## Unsigned Alpha Note

The first public Windows binary may be an unsigned portable alpha. Release notes should say that Windows may show SmartScreen or antivirus warnings because the executable is unsigned.
