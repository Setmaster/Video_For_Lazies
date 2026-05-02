# Video For Lazies (Tauri)

Rust + Tauri desktop app for exporting videos (optionally under a size limit), backed by `ffmpeg`.

## Requirements

- Windows x64 and Linux x64 release builds bundle `ffmpeg` and `ffprobe` automatically.
- Unsupported platforms and custom runtime builds can still use `ffmpeg` and `ffprobe` on `PATH`
  - Or set `VFL_FFMPEG_PATH` and `VFL_FFPROBE_PATH`
- Node.js and Rust toolchain

## Run (dev)

```bash
npm install
npm run tauri dev
```

## Build (bundle)

```bash
npm run tauri build
```

On Windows and Linux x64, the Tauri build runs `npm run prepare:ffmpeg-sidecar` first to stage the pinned GPL FFmpeg bundle and its license/source metadata.

## Portable Folder

```bash
npm run portable
```

Writes a portable folder at `release/Video_For_Lazies/`.
That folder contains the app executable and required legal/readme files.
On Windows and Linux it also contains the bundled `ffmpeg-sidecar/`.
On Windows, verification runs a startup smoke check against the packaged app window and fails the build if the portable release comes up on an obviously wrong surface.
On Linux, verification runs the bundled FFmpeg and FFprobe through an encode/probe smoke.
This path builds the portable release directly and does not require the WiX / NSIS installer steps.

## Portable Release Archive

```bash
npm run release:portable
```

Builds `release/Video_For_Lazies/`, packages a versioned x64 zip such as `release/Video_For_Lazies-v0.1.0-win-x64.zip` or `release/Video_For_Lazies-v0.1.0-linux-x64.zip`, writes `release/SHA256SUMS.txt`, and verifies the extracted zip.

On both platforms, verification checks that the shipped sidecar exposes `libx264` and can encode/probe a sample MP4. On Windows, verification also runs startup smoke, the packaged interaction/export smoke, and a second tight-target 1080p MP4 smoke.

The GitHub `Portable Release` workflow builds Linux and Windows x64 zips, creates release notes from the commit range since the previous release tag, and attaches the final zip assets plus a combined checksum file to a draft GitHub Release by default.

## Tests

```bash
npm test
cd src-tauri
cargo test
```

## Notes

- Size limit uses decimal MB (`1 MB = 1,000,000 bytes`).
- Set size limit to `0` (or leave it empty) to disable size targeting.
- The backend emits events:
  - `encode-progress`
  - `encode-finished`

## Features (current)

- Drag and drop a video anywhere onto the window to set it as input.
- Output path is auto-suggested in the same folder by incrementing a `-N` suffix, skips existing `-N` outputs to avoid overwrites, and changing the export format updates the output extension.
- Export format and recent export status are remembered between launches (input/output paths are not persisted).
- Quick size presets (8/10/25/50 MB), direct trim in/out handles under the preview, selected-boundary fine nudges plus compose shortcuts (`Space`, `Left` / `Right`, `Shift+Left` / `Shift+Right`, `[` and `]`), a `Current plan` summary card with plan status and last-export details, bottom-bar export/open actions, and a `Save frame (PNG)` button beside the live preview controls.
- Composing:
  - Trim
  - Crop (drag-select with optional aspect lock, plus auto-detect crop)
  - Reverse
  - Playback speed
  - Rotate
  - Color adjustments (brightness, contrast, saturation)
- General:
  - Output format (`mp4`, `webm`, `mp3`)
  - Size limit (MB, optional)
  - Max edge (resize)
  - Audio on/off
  - Title metadata

## FFmpeg runtime notes

- Runtime resolution order is env override -> bundled `ffmpeg-sidecar` -> `PATH`.
- The bundled Windows and Linux packages use pinned GPL shared builds and include `libx264`, so bundled MP4 export uses H.264 by default.
- If the active FFmpeg build does not expose `libx264`, MP4 export still falls back to `mpeg4`.
- Exact bundle/source provenance lives in [`../docs/ffmpeg-bundling.md`](../docs/ffmpeg-bundling.md).

## License

Video For Lazies is licensed under GPL-3.0-or-later. The Windows and Linux portable builds bundle pinned GPL FFmpeg sidecars; see [`../THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) and [`../docs/ffmpeg-bundling.md`](../docs/ffmpeg-bundling.md).
