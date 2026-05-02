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

## Portable Release Artifacts

The `Portable Artifacts` GitHub Actions workflow builds private x64 artifacts for:

- `linux-x64`
- `win-x64`

It runs on manual dispatch with an explicit version, and on pushed `v*.*.*` tags. The requested version or tag must match the synchronized app metadata. The workflow uploads private Actions artifacts only; it does not create a GitHub Release, publish binaries publicly, or change repository visibility.

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

If `7z.exe` is available on the Windows host, the script also creates a versioned `.7z` archive. The GitHub Actions workflow uploads the `.zip` artifact and checksum file.

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
