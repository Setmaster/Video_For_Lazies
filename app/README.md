# Video For Lazies (Tauri)

Rust and Tauri desktop app for focused local video exports through FFmpeg. The workbench accepts `mp4`, `mov`, `mkv`, `avi`, `webm`, and `m4v` input and exports `mp4`, `webm`, or audio-only `mp3`.

## Frame navigation

In Edit, enable **Frame step** beside **Drag snap (s)** to make Left/Right move one source picture, or ten with Shift. Focused trim handles use the same frame mode; Home/End retain their existing limits. Navigation pauses playback. The toggle starts off, and drag snapping remains a separate seconds setting.

Frame navigation reads actual source presentation timestamps, including variable-rate gaps. From a time between pictures, the displayed picture is the preceding source timestamp and stepping starts from that picture. Trim values retain full timestamp precision. The clip duration is an exclusive end boundary: a picture whose timestamp equals the duration is excluded, and Left from the End sentinel selects the final picture before it. Trim constraints can stop navigation before the requested number of pictures.

Frame navigation reads nearby timing only when stepping, in bounded windows with a small source-validated cache. If timing cannot be proved within the window or the preview cannot verify the requested picture, an explicit unavailable message appears. No seconds approximation is substituted. Particularly long GOPs, unsupported partial timing windows and timestamps too close for the preview can be unavailable. A small seek bias inside the known picture interval helps the preview display the selected picture; the trim boundary remains its exact source timestamp.

The counter beside the playback time shows the current **source frame**, numbered from 1. It follows presented video frames during playback and source frame selection while paused. It is independent of Smooth Motion output FPS. Variable-rate clips use their actual timestamps, not an average-FPS estimate. The duration remains an exclusive end boundary.

Frame numbering reads a complete source timing index in the background. While loading it shows **Frame …**; timing that cannot be verified shows **Frame unavailable** with Retry. Indexing is limited to 100,000 frames, 30 seconds of probing and 8 MiB of probe output. Large or unusually encoded clips may therefore have frame navigation available while the absolute frame counter is unavailable.

## Requirements

- Windows x64 and Linux x64 release builds stage pinned `ffmpeg` and `ffprobe` sidecars automatically.
- Unsupported platforms and custom runtime builds can use `ffmpeg` and `ffprobe` on `PATH`, or set `VFL_FFMPEG_PATH` and `VFL_FFPROBE_PATH`.
- Local development requires Node.js and a Rust toolchain.

## Run (development)

```bash
npm install
npm run tauri dev
```

## Build (Tauri)

```bash
npm run tauri build
```

On Windows and Linux x64, the Tauri build runs `npm run prepare:ffmpeg-sidecar` first. Staging verifies the pinned GPL FFmpeg archive and corresponding source/build-recipe archives before copying the runtime, capability contract, licenses, and source material.

Windows also runs `npm run prepare:recording-sidecar` to build the native recording helper using the installed Rust/MSVC toolchain. It is bundled under `recording-sidecar/`; users do not install a separate recording runtime. Linux skips this helper and exposes recording as unavailable.

## Portable Folder

```bash
npm run portable
```

This writes `release/Video_For_Lazies/` with:

- the app executable and `vfl-update-helper`
- the platform `ffmpeg-sidecar/` and canonical capability contract
- `recording-sidecar/` with the native capture helper and notices on Windows
- the DejaVu Sans subtitle font embedded in the app binary, with its license recorded in generated notices
- the payload manifest, release-specific `SOURCE.md` and `THIRD_PARTY_NOTICES.md`, and copies of `README.md`, `FFMPEG_BUNDLING.md`, and `LICENSE.txt`

The command builds directly with `tauri build --no-bundle`; it does not require WiX or NSIS. Windows also runs the packaged-window startup smoke and rejects a missing or obviously wrong app surface.

## Portable Release Archive

```bash
npm run release:portable
```

This builds the portable folder, packages `release/Video_For_Lazies-vX.Y.Z-win-x64.zip` or `release/Video_For_Lazies-vX.Y.Z-linux-x64.zip`, writes the matching payload-manifest sidecar and `release/SHA256SUMS.txt`, extracts the zip, and verifies the extracted artifact rather than trusting the build folder.

Both platforms verify:

- executable, legal/source, and exact payload-manifest ownership, hash, size, and mode contracts
- the canonical FFmpeg capability contract and a real bundled `libx264` encode/probe
- media-depth behavior for stream selection, HDR/high-depth policy, SAR/rotation geometry, and reverse/loop memory limits
- exact-byte targets, opt-in audio-preserving Strict Fit, and one bounded external UTF-8 SRT
- One frame-accurate trim workflow using decoded frame and sample boundaries
- recipe privacy, queue retry and snapshot restoration, multi-file routing, reset/dialog accessibility, phase progress, rotation/speed/frame-rate-cap truth, active cancellation, queued drops, and export-lifecycle behavior
- standard MP4, WebM, custom-size, and tight-target packaged exports

Linux additionally runs the full bundled codec-plan matrix. Windows additionally runs the native startup smoke and packaged keyboard/window interaction path.

The GitHub `Portable Release` workflow builds and verifies Linux and Windows x64 zips. Manual runs default to `build_only=true`, which uploads private GitHub Actions artifacts for 30 days and skips the release job. A deliberate `build_only=false` run continues through release notes (curated when supplied, otherwise generated from commits), combined checksums, signed updater-manifest creation, and a GitHub Release configured by the workflow inputs. Tag-triggered runs conservatively create a draft prerelease for asset review before explicit stable publication.

## Tests

```bash
npm test
npx tsc --noEmit
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features -- -D warnings
```

Useful targeted package checks:

```bash
npm run smoke:portable:export
npm run smoke:portable:codecs
npm run smoke:portable:g7
```

The targeted smoke commands expect an already assembled portable folder and are platform-gated. `npm run release:portable` is the complete archive-and-reextract gate.

## Implemented Product Contract

### Workbench and editing

- The preview, scrubber, trim sliders, and crop surface stay visible on the left. Source, recipes, export settings, crop, transform/color, advanced controls, current plan, and queue live in the collapsible right rail.
- Trim sliders expose labelled keyboard semantics and 0.1-second or 1-second nudges. Requested boundaries use decoded frames and samples; there is no alternate widened-boundary trim mode.
- Crop uses labelled source-pixel inputs plus pointer drawing. Quarter-turn rotation keeps preview geometry and crop coordinates aligned.
- Speed is applied before the optional frame-rate cap, and the current plan reports the post-speed rate and whether the cap applies.
- Transform's optional Target duration derives playback speed from the selected timeline and boomerang multiplier, within 0.05×–16×. Disabling it restores the manual speed. Anti-stutter removal resolves the retained timeline before choosing speed; requested audio uses the same timing. Results remain subject to frame rounding and the speed limits. Target duration is clip-scoped and travels with queued request snapshots, not recipes or same-settings batches. Exact samples use the full export's speed. They are unavailable for target duration with Anti-stutter removal or NUT input, because those timelines require further analysis.
- One UTF-8 SRT can be burned into MP4 or WebM using source timing, a fixed embedded DejaVu Sans style, and private temporary staging. It forces video re-encoding and rejects inline styling/position overrides.

### Planning and publication

- Probe selection is container and codec aware. Attached pictures are excluded from primary-video selection; compatible audio and video may be copied independently.
- Exact target classification uses backend-reported integer bytes. With Strict Fit off, the requested dimensions and audio are preserved, and a measured overshoot is retained as an openable target-miss artifact.
- Strict Fit is opt-in and globally bounded to four ordered plans. It stops at the first fit or retains the smallest miss. When audio is included, every plan retains it and the final applicable fallback uses 32 kbps audio.
- Every output uses a unique temporary path and exclusive no-clobber publication. Suggested paths and queue reservations also advance the `-N` suffix.
- Progress is attempt/job bound, monotonic, and phase aware (`copying`, `encoding`, `finalizing`). The UI does not show 100 percent before the matching terminal event.

### Media guardrails

- HDR10 and high-bit-depth SDR require an explicit standard-SDR conversion and supported MP4/H.264 capability. The result is 8-bit BT.709 SDR; HDR preservation is not supported.
- HLG, Dolby Vision, incomplete or contradictory HDR/high-depth metadata, unknown component depth, and arbitrary source rotations fail closed for video export. Audio-only MP3 may remain available.
- Non-square source pixels are normalized to square-pixel output while preserving display shape unless custom dimensions explicitly take authority.
- Reverse and loop estimate decoded video/audio memory, warn above 512 MiB, and block above 2 GiB.

### Queue, recipes, privacy, and recovery

- The queue owns immutable snapshots, one active FFmpeg job, exact item/run identity, stale-event rejection, a 100-item bound, and up to 10 retained outcomes per item. Retry, duplicate, apply snapshot, stop-after-current, and clear-finished paths preserve focus and allocate fresh outputs.
- Thumbnail replaces the actual first output frame with a chosen JPEG, PNG, WebP, BMP, TIFF or GIF image. Fit keeps the whole image with black bars; Fill crops the edges. Transparent areas appear black and animated images use their first frame. It adds no time and preserves the audio timeline. Websites may choose a different thumbnail. Images are limited to 32 MiB and 40 megapixels, validated again before export, and cleared for a new source or Reset. Queued snapshots retain the selection and reject an image that changed; recipes and preferences do not retain local image paths.
- A single supported drop while idle replaces the current source. Multi-file drops and drops during active work queue accepted files in order using a settings-only snapshot; unsupported, duplicate, and overflow counts stay visible.
- Device-local schema-v3 recipes store a short description alongside the recipe name and reusable format, size, resize, audio, Strict Fit, frame uniqueness, and encoder settings. They exclude every source/output/subtitle path and clip-scoped edit, title, metadata privacy, diagnostics, queue state, and job state.
- Metadata stripping is on by default. User-facing errors and retained smoke/diagnostic evidence redact process-unique and clip-private paths where those paths are not needed for the local workflow.
- Reset All uses a confirmation dialog and clears settings, trim, crop, color, and subtitle selection without clearing source, output, queue, or recipes. Cancel is a separate one-shot active-export action.
- Signed portable updates show bounded progress, verify the selected payload, journal replacement state, preserve a verified backup, and attempt rollback/startup recovery after interruption. A manual portable download remains the fallback when recovery cannot complete.

### Accessibility baseline

- Crop fields have explicit labels and source-pixel values; trim handles expose slider roles, names, values, and keyboard behavior.
- About, reset, recipe, and elevated-update dialogs contain focus, isolate the background, support appropriate Escape behavior, and restore focus to the trigger or safe queue control.
- Probe, subtitle, crop, queue, progress, update, and failure states use mounted status/alert semantics where relevant.

These are tested accessibility foundations, not a claim of WCAG conformance or complete coverage across screen readers, browsers, and operating systems.

## FFmpeg Runtime Notes

- Resolution order is environment override, bundled `ffmpeg-sidecar`, then `PATH`.
- The bundled Windows and Linux packages use pinned GPL shared builds and include `libx264`, `libvpx-vp9`, `libopus`, `libmp3lame`, libass subtitle support, and the filters required by the shipped capability contract.
- Bundled MP4 export uses H.264 by default. In Auto mode, a custom FFmpeg without `libx264` can fall back to `mpeg4` when that encoder is available; explicit H.264 and standard-SDR plans fail closed. Missing required filters or encoders block the affected feature.
- Exact bundle/source provenance lives in [`../docs/ffmpeg-bundling.md`](../docs/ffmpeg-bundling.md).

## License

Video For Lazies is licensed under GPL-3.0-or-later. Windows and Linux portable builds bundle pinned GPL FFmpeg sidecars; see [`../THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md), [`../SOURCE.md`](../SOURCE.md), and [`../docs/ffmpeg-bundling.md`](../docs/ffmpeg-bundling.md).
