# Video For Lazies (Tauri)

Rust + Tauri desktop app for exporting videos (optionally under a size limit), backed by `ffmpeg`.

## Requirements

- Windows release builds bundle `ffmpeg` and `ffprobe` automatically.
- Dev and non-Windows environments can still use `ffmpeg` and `ffprobe` on `PATH`
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

On Windows, the Tauri build now runs `npm run prepare:ffmpeg-sidecar` first to stage the pinned GPL FFmpeg bundle and its license/source metadata.

## Portable Folder (Windows)

```bash
npm run portable
```

Writes a portable folder at `release/Video_For_Lazies/`.
That folder contains `Video_For_Lazies.exe`, required legal/readme files, and the bundled `ffmpeg-sidecar/`.
On Windows, this command also runs a startup smoke check against the packaged app window and fails the build if the portable release comes up on an obviously wrong surface.
This path builds the portable release directly and does not require the WiX / NSIS installer steps.

## Portable Release Archive (Windows)

```bash
npm run release:portable
```

Builds `release/Video_For_Lazies/`, packages `release/Video_For_Lazies-win-x64.zip`, writes `release/SHA256SUMS.txt`, and verifies the extracted zip with startup, the packaged interaction/export smoke, a second tight-target 1080p MP4 smoke, and an encoder check that the shipped sidecar exposes `libx264`.

If `7z.exe` is available on the Windows host, this command also writes `release/Video_For_Lazies-win-x64.7z`.

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

- Runtime resolution order is env override -> bundled Windows `ffmpeg-sidecar` -> `PATH`.
- The bundled Windows package uses a pinned GPL shared build and includes `libx264`, so bundled MP4 export uses H.264 by default.
- If the active FFmpeg build does not expose `libx264`, MP4 export still falls back to `mpeg4`.
- Exact bundle/source provenance lives in [`../docs/ffmpeg-bundling.md`](../docs/ffmpeg-bundling.md).

## License

Video For Lazies is licensed under GPL-3.0-or-later. The Windows portable build bundles a pinned GPL FFmpeg sidecar; see [`../THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) and [`../docs/ffmpeg-bundling.md`](../docs/ffmpeg-bundling.md).
