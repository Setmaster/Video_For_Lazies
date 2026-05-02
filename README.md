<p align="center">
  <img src="docs/assets/readme-banner.png" alt="Video For Lazies interface banner">
</p>

# Video For Lazies

Video For Lazies is a small desktop app for the video jobs that should not require a full editor: trim the useful part, crop the frame, resize it, set a target size if needed, and export through FFmpeg.

It is built with Tauri, React, Rust, and FFmpeg. The goal is a practical local tool: drag in a file, choose the output shape, and get a predictable export without opening a heavyweight timeline.

## Highlights

- Trim clips with direct in/out handles, keyboard nudges, and a live preview.
- Crop by drawing over the preview, optionally with aspect lock or auto-detect crop.
- Resize by max edge, rotate, reverse, change playback speed, and adjust brightness, contrast, and saturation.
- Export to `mp4`, `webm`, or `mp3`.
- Target a file size in decimal MB, or disable size targeting for constant-quality export.
- Keep audio on/off, title metadata, recent export status, and output format in the app workflow.
- Auto-suggest safe output names in the same folder and avoid overwriting existing `-N` exports.
- Save a preview frame as PNG.

## Who It Is For

Use Video For Lazies when you need a focused export pass instead of a full editing session. It is meant for quick clips, smaller uploads, screen recordings, simple crops, speed changes, and "make this fit under a size limit" jobs.

It is not trying to replace a nonlinear editor. There are no multi-track timelines, effects stacks, or media bins.

## Status

The current app is the Tauri desktop version in [`app/`](app/). Windows portable builds can bundle a pinned FFmpeg sidecar, including `ffprobe`, so end users do not need to install FFmpeg separately for that build.

Source and release checks are documented in [`docs/release.md`](docs/release.md).

## Requirements

- Node.js
- Rust toolchain
- FFmpeg and FFprobe on `PATH` for development and non-Windows builds, unless you set:
  - `VFL_FFMPEG_PATH`
  - `VFL_FFPROBE_PATH`

Windows portable releases stage the pinned FFmpeg sidecar automatically.

## Run From Source

```bash
cd app
npm install
npm run tauri dev
```

## Build

```bash
cd app
npm run tauri build
```

## Windows Portable Build

```bash
cd app
npm run portable
```

This writes a portable folder at:

```text
release/Video_For_Lazies/
```

The folder contains the app executable, required project/legal files, and the bundled `ffmpeg-sidecar/`. On Windows, the portable command also runs a packaged-app startup smoke check.

To produce a verified release archive:

```bash
cd app
npm run release:portable
```

That command builds the portable folder, creates `release/Video_For_Lazies-win-x64.zip`, writes `release/SHA256SUMS.txt`, and verifies the extracted archive with packaged smoke checks.

If `7z.exe` is available on the Windows host, the release script also emits a `.7z` archive.

## Usage Notes

- Size targets use decimal MB: `1 MB = 1,000,000 bytes`.
- Set size limit to `0`, or leave it empty, to disable size targeting.
- Bundled Windows MP4 export uses H.264 when the staged FFmpeg sidecar exposes `libx264`.
- If the active FFmpeg build does not expose `libx264`, MP4 export falls back to `mpeg4`.

## Tests

```bash
cd app
npm test
npx tsc --noEmit
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
```

## Project Layout

```text
app/                  Tauri, React, and Rust app
app/src/              Frontend UI
app/src-tauri/        Rust backend and FFmpeg command layer
app/scripts/          Portable build, smoke, and FFmpeg sidecar scripts
docs/                 Release and FFmpeg bundling docs
docs/assets/          README banner and icon source/generated assets
```

The README banner is generated from [`docs/assets/readme-banner.html`](docs/assets/readme-banner.html).
The app icon is generated from [`docs/assets/app-icon.html`](docs/assets/app-icon.html).

## FFmpeg And Licensing

Video For Lazies is licensed under GPL-3.0-or-later. See [`LICENSE`](LICENSE).

Windows portable builds bundle a pinned GPL FFmpeg runtime as a sidecar. Runtime resolution order is:

1. `VFL_FFMPEG_PATH` / `VFL_FFPROBE_PATH`
2. bundled `ffmpeg-sidecar/` next to the app executable
3. plain `ffmpeg` / `ffprobe` on `PATH`

Bundle provenance, exact URLs, checksums, and corresponding source information are documented in [`docs/ffmpeg-bundling.md`](docs/ffmpeg-bundling.md). Third-party notices are in [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md), and source availability notes are in [`SOURCE.md`](SOURCE.md).

## Security

Please report security issues privately. See [`SECURITY.md`](SECURITY.md).
