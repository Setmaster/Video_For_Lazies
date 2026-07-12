# Release Checklist

Release publication is an owner-approved action. Build scripts create artifacts and GitHub release drafts; they do not decide when a stable release should be published.

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
  - `app/src/App.tsx` (`APP_VERSION`)
  - `app/src-tauri/Cargo.toml`
  - `app/src-tauri/Cargo.lock`
  - `app/src-tauri/tauri.conf.json`

Useful commands:

```bash
cd app
npm run version:check
npm run version:set -- 1.2.3
npm run version:check -- 1.2.3
```

For stable tagged artifact builds, commit the version bump on `dev`, merge the approved result to `main`, then tag the `main` commit:

```bash
git tag v1.2.3
git push origin main v1.2.3
```

For release-candidate checks before approval, run the manual workflow from the development branch with `build_only=true`. This verifies and retains private artifacts without creating a GitHub Release. For an owner-approved draft release, explicitly use `build_only=false` and leave `draft=true` until review is complete.

Pushing a release tag intentionally creates a draft prerelease. Tag events do not have the manual `draft` or `prerelease` inputs, and the release step defaults missing values to `true` so assets can be reviewed before public promotion. After the tag workflow succeeds and the draft assets pass verification, explicitly publish the release as stable and latest.

## Release Gate Policy

The blocking gate for every stable release is:

1. Local checks pass on `dev`.
2. CI passes on `dev` for the exact commit that will be released.
3. The manual `Portable Release` workflow passes with `build_only=true` for that exact `dev` commit, including Windows and Linux portable artifacts.
4. `dev` fast-forwards into `main`, and CI passes on the same `main` commit unless the narrowly scoped patch-only setup-stall exception below applies.
5. The release tag workflow passes and creates the reviewable draft prerelease.
6. Draft assets are downloaded and verified against `SHA256SUMS.txt`, the update manifest hashes and signature, and any targeted runtime smoke needed for the change.
7. The verified draft is explicitly published as stable and latest, then the latest release endpoint and update channel are checked.

The `Portable Release` workflow is the final artifact gate because it rebuilds and verifies both Windows x64 and Linux x64 portable artifacts before publication. It does not replace an exact-SHA `main` CI result when that result is a hard gate.

Exact-SHA `main` CI is a hard gate for every `MAJOR` or `MINOR` release and for any release that changes update checks, signing, manifests, payload validation, staging, apply behavior, rollback, or recovery. A runner-setup or dependency-installation stall does not satisfy this gate and cannot use the setup-stall exception. Rerun `main` CI until the exact release SHA completes successfully.

Every `MAJOR` or `MINOR` release also requires curated release notes before publication. A tag-triggered workflow may create a draft with commit-generated categories, but the entire generated body must be replaced with an owner-reviewed release body before the draft becomes public. Do not merely prepend a curated summary while leaving misleading raw commit subjects below it.

The setup-stall exception is limited to an ordinary `PATCH` release that does not change the updater or its trust and recovery path. It may be used only when all of these are true:

- `dev` CI already passed for the same commit hash.
- The merge to `main` was a fast-forward.
- The `main` CI run is stalled in runner setup or dependency installation rather than failing a repo test.
- The `Portable Release` workflow passes for that same `main` commit.

If `main` CI fails in repo code, tests, lint, version checks, or audits, stop and fix it before release. For an eligible patch release only, rerun a setup-stalled job once; if it stalls again while every condition above remains true, record the exception in the private release log and continue to the same-SHA Portable Release gate. Do not describe the stalled run as passing.

For updater changes, FFmpeg bundle changes, release-script changes, or major/minor releases, add the relevant dogfood step against the verified draft artifacts before publication, then recheck the public endpoints after publication. For UI-only patch releases, one targeted real-runtime smoke is enough when the changed surface is not already covered by packaged release smoke.

## Public Source Gate

- Confirm `npm audit --audit-level=moderate` is clean.
- Run `npm test`, `npx tsc --noEmit`, `npm run build`, `cargo fmt --manifest-path app/src-tauri/Cargo.toml -- --check`, `cargo test --manifest-path app/src-tauri/Cargo.toml`, `cargo clippy --manifest-path app/src-tauri/Cargo.toml --all-targets --all-features -- -D warnings`, and `cd app/src-tauri && cargo audit`.
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

It runs on manual dispatch with an explicit version, and on pushed `v*.*.*` tags. The requested version or tag must match the synchronized app metadata. The workflow builds and verifies both portable zips on every run. Manual dispatch defaults to `build_only=true`, which uploads private artifacts with 30-day retention and skips the release job. Tag pushes, or an explicit manual `build_only=false`, continue through release notes, the combined `SHA256SUMS.txt`, updater-manifest signing, and GitHub Release creation. Tag events have no dispatch inputs, so the release script conservatively defaults them to `draft=true` and `prerelease=true`; stable publication is a separate, explicit step after draft verification.

Manual dispatch inputs:

- `version`: required SemVer value, for example `1.2.3`; must match app metadata.
- `build_only`: defaults to `true`; builds, smokes, and uploads private artifacts without creating a GitHub Release. Set it to `false` only for an intentional release draft or publication run.
- `release_notes`: optional workflow input for a curated changelog or summary. If blank, the workflow generates a draft summary from commits. Curated notes are mandatory before publishing a major or minor release.
- `previous_tag`: optional changelog start tag. Use `none` for a first release.
- `draft`: defaults to `true`; keep this enabled until final review is complete.
- `prerelease`: defaults to `true`; set to `false` for an owner-approved stable release.

Nonpublishing development-branch validation:

```bash
VERSION="$(cd app && node scripts/check-version.mjs --value)"
gh workflow run portable-artifacts.yml --ref dev \
  -f version="$VERSION" \
  -f build_only=true \
  -f draft=true \
  -f prerelease=true
```

This run is expected to finish green after both platform artifacts pass. The release job is skipped and no tag, draft, or public release is created.

Release notes follow this template:

```text
# Video For Lazies vX.Y.Z

## Release Summary
<curated notes, or generated summary>

## Changes
<categorized commits since previous release tag>

## Artifacts
<portable zip assets, checksum file, update manifest, and update manifest signature>

## Runtime Notes
<Bundled FFmpeg sidecar notes>

## Verification
<workflow verification summary>
```

Recommended stable tag protocol:

1. Require local checks, exact-SHA `dev` CI, and a `build_only=true` Windows/Linux workflow run to pass.
2. Fast-forward `dev` into `main`, push `main`, and require exact-SHA `main` CI to pass.
3. Create and push the lightweight `vX.Y.Z` tag at that commit.
4. Wait for the tag-triggered `Portable Release` workflow to finish green and create its draft prerelease.
5. Download and verify all five draft assets, both archive payloads, the signed update manifest, and generated release notes.
6. Replace the entire generated body with curated, owner-reviewed notes for every major or minor release, and whenever the generated summary does not adequately explain the change. Then publish only the verified draft:

```bash
gh release edit vX.Y.Z --draft=false --prerelease=false --latest
```

7. Confirm `main`, `dev`, and the tag resolve to the same commit, then verify the latest release endpoint and `releases/latest/download` update-manifest assets.

Alternative manual draft protocol for review without a pre-existing tag:

1. Run `git fetch --tags origin`.
2. Find the previous release tag with `git tag --merged HEAD --sort=-version:refname --list 'v[0-9]*'`.
3. Review commits since that tag with `git log --oneline <previous-tag>..HEAD`, or use `git log --oneline HEAD` for the first release.
4. Write the curated `release_notes` input using the template above.
5. Trigger `Portable Release` with `build_only=false` and `draft=true`.
6. Review the draft release notes, Linux zip, Windows zip, `SHA256SUMS.txt`, `vfl-update-manifest-v1.json`, and `vfl-update-manifest-v1.json.sig`.
7. Confirm the draft release is backed by the intended public `vX.Y.Z` tag.
8. Confirm generated `SOURCE.md` in each zip names the intended release tag and source commit.
9. Publish the draft only when the owner wants that release visible to the public.

For a stable release, prefer the tag-first protocol above. Publishing a manual draft whose tag does not yet exist can create that tag and start a redundant tag workflow after the release artifacts were already built.

Run locally on Linux or Windows:

```bash
cd app
npm run release:portable
```

The script builds `release/Video_For_Lazies/`, creates a versioned zip, writes `release/SHA256SUMS.txt`, writes a platform payload manifest sidecar, extracts the zip, and verifies the payload.

Linux artifact example:

```text
release/Video_For_Lazies-vX.Y.Z-linux-x64.zip
```

Windows artifact example:

```text
release/Video_For_Lazies-vX.Y.Z-win-x64.zip
```

GitHub release assets are the Linux zip, the Windows zip, the combined checksum file, the signed updater manifest, and the updater manifest signature. Payload manifest sidecars are intermediate workflow inputs and are intentionally not uploaded as separate release assets.

## Updater Signing

Portable update checks trust the signed `vfl-update-manifest-v1.json`, not the network transport alone. The manifest binds the release version and tag to each platform archive URL, SHA256, byte size, portable root, and embedded payload-manifest digest. The app verifies the detached manifest signature against its embedded updater public keys before showing an update prompt.

Release assembly requires one complete Linux/Windows artifact pair at one version. Each payload-manifest sidecar must be byte-for-byte identical to the copy embedded in its corresponding zip. Before signing, release staging validates zip integrity, canonical paths, the exact manifest-owned file set, every file hash and size, and Linux file modes. The workflow then verifies the new signature against the same public keys embedded in the shipped app.

After download, the app verifies the selected archive size and SHA256, validates the embedded payload manifest, and extracts only the signed file set into app-managed staging. Apply is single-owner and no-clobber: a late collision or unknown sibling is preserved rather than silently overwritten or deleted. A durable journal records the staged plan and verified backup digest before replacement. Startup finalizes a successful update; an interrupted or failed replacement uses the journal and verified backup for automatic recovery or rollback. If saved state cannot be trusted, cleanup stops and the staged files and backup remain available for explicit recovery.

The release workflow expects these GitHub Actions secrets:

- `VFL_UPDATE_SIGNING_PRIVATE_KEY`
- `VFL_UPDATE_SIGNING_PRIVATE_KEY_PASSWORD`

Do not commit the private key or password. Rotate the updater key with a dedicated compatibility plan, because already shipped apps can only trust public keys embedded in their binaries. Updater-manifest signing is separate from Windows Authenticode code signing; one does not substitute for the other.

## Workflow Linting

CI runs `actionlint` v1.7.12 against GitHub Actions workflows. Run the same check locally before committing workflow changes:

```bash
actionlint -color
```

Required portable payload on all platforms:

- app executable
- update helper executable
- `VFL_PAYLOAD_MANIFEST.json`
- `README.md`
- `LICENSE.txt`
- `THIRD_PARTY_NOTICES.md`
- `SOURCE.md`
- `FFMPEG_BUNDLING.md`

Additional required sidecar payload on Windows and Linux:

- `ffmpeg-sidecar/` with FFmpeg binaries, FFmpeg license text, bundle notices, `FFMPEG_CAPABILITIES.json`, and source/provenance archives
- the pinned FFmpeg, BtbN build-recipe, and x264 source archives under `ffmpeg-sidecar/source/` (exact names and hashes come from `app/scripts/ffmpegBundle.mjs`; the human-readable copy is `docs/ffmpeg-bundling.md`)

Linux sidecar-specific payload also includes root `ffmpeg` and `ffprobe` wrapper scripts, `bin/ffmpeg`, `bin/ffprobe`, and the required shared libraries in `ffmpeg-sidecar/lib/`.

Release verification extracts the exact zip that will be uploaded and validates its payload manifest, required legal/source files, bundled FFmpeg capability contract, and a real FFmpeg encode/probe. Packaged app smokes on both platforms cover media-depth safety, strict size fitting and subtitle burn-in, frame-accurate trim and no-clobber refusal, progress/finalization behavior, reset/cancel/drop interactions, and representative MP4/WebM export plans. Linux uses Xvfb when no display is available and also runs the codec-plan matrix. Windows additionally verifies the helper's embedded execution manifest and redirected self-test, the packaged startup surface, and native keyboard-driven preview, trim, crop, dialog, queue, and accessibility behavior.

The release workflow repeats the full test, lint, audit, build, archive, payload, and packaged-runtime chain independently on native Windows and Linux runners. Download the resulting private or draft artifacts and verify them again before publication; a green source build alone is not artifact proof.

## Windows Code-Signing Note

Current Windows portable executables are not Authenticode-signed. Windows may show SmartScreen or antivirus reputation warnings regardless of release maturity. Release notes must state this plainly and direct users to verify the matching entry in `SHA256SUMS.txt`. Do not call a stable release an alpha solely because its Windows executable is unsigned.
