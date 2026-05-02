# Release Checklist

This project is not publicized or released by the build scripts. Repository visibility and GitHub Releases are manual owner actions.

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

## Windows Portable Build

Run on Windows:

```bash
cd app
npm run release:portable
```

The script builds `release/Video_For_Lazies/`, creates archive/checksum files, and verifies the extracted portable folder.

Required portable payload:

- `Video_For_Lazies.exe`
- `README.md`
- `LICENSE.txt`
- `THIRD_PARTY_NOTICES.md`
- `SOURCE.md`
- `FFMPEG_BUNDLING.md`
- `ffmpeg-sidecar/` with FFmpeg binaries, FFmpeg license text, bundle notices, and source archive

## Unsigned Alpha Note

The first public Windows binary may be an unsigned portable alpha. Release notes should say that Windows may show SmartScreen or antivirus warnings because the executable is unsigned.
