# FFmpeg Bundling

This app now bundles FFmpeg for Windows release builds so end users do not need a separate FFmpeg install.

## Runtime contract

- Resolution order:
  1. `VFL_FFMPEG_PATH` / `VFL_FFPROBE_PATH`
  2. bundled `ffmpeg-sidecar/` next to the app executable
  3. plain `ffmpeg` / `ffprobe` on `PATH`
- The Windows bundle is staged by `app/scripts/sync-ffmpeg-sidecar.mjs`.
- `npm run tauri build` and `npm run portable` trigger that staging automatically through Tauri's `beforeBuildCommand`.
- The portable release format is now a folder: `release/Video_For_Lazies/Video_For_Lazies.exe` plus `release/Video_For_Lazies/ffmpeg-sidecar/`.
- The portable build now also runs a Windows startup smoke (`app/scripts/windows-portable-smoke.ps1`) after packaging. It launches the portable exe, captures the app window, and fails the build if the rendered surface is overwhelmingly bright/white, which catches regressions like the accidental dev-surface/localhost build.
- `npm run release:portable` packages `release/Video_For_Lazies-win-x64.zip`, writes `release/SHA256SUMS.txt`, and verifies the extracted zip with startup plus packaged interaction/export smoke. That release smoke routes input through the shared drop logic, asserts crop-enabled playback before encode, verifies the output file from the extracted artifact, runs a second tight-target 1080p MP4 smoke, and fails if the shipped `ffmpeg-sidecar` is missing `libx264`. When `7z.exe` is available, it also emits `release/Video_For_Lazies-win-x64.7z`.

## Pinned Windows bundle

- Provider: BtbN FFmpeg Builds
- Variant: `win64-gpl-shared`
- Binary archive:
  `https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-n8.0-latest-win64-gpl-shared-8.0.zip`
- Binary archive SHA256:
  `68de96f55cc76d4ce656fee1889fab5b694edb42d23383d3d37f867d85727fc0`
- FFmpeg version string from the bundle:
  `n8.0.1-76-gfa4ee7ab3c-20260315`

Note:

- BtbN serves this archive through the repo's `releases/download/latest/...` route. The bundle sync intentionally verifies the pinned SHA256 so upstream refreshes fail loudly instead of silently changing the shipped runtime.

## Corresponding source snapshot

- Upstream mirror:
  `https://github.com/FFmpeg/FFmpeg`
- Exact source archive:
  `https://codeload.github.com/FFmpeg/FFmpeg/tar.gz/fa4ee7ab3c1734795149f6dbc3746e834e859e8c`
- Source archive SHA256:
  `b362a977a041c89494172007244e89b183b621a04af392cef90ae8a9609bdfac`

The staged Windows sidecar includes:

- `ffmpeg.exe`
- `ffprobe.exe`
- required shared FFmpeg DLLs from the pinned archive
- upstream `LICENSE.txt`
- `FFMPEG_BUNDLE_NOTICES.txt`
- `source/ffmpeg-fa4ee7ab3c1734795149f6dbc3746e834e859e8c.tar.gz`

## Codec behavior

- The pinned GPL Windows bundle ships `libx264`, so bundled MP4 export uses H.264 by default.
- If the active FFmpeg build does not expose `libx264` (for example via `VFL_FFMPEG_PATH`), MP4 export falls back to `mpeg4`.
- WebM and MP3 continue to prefer the bundled `libvpx-vp9`, `libopus`, and `libmp3lame` encoders when available.
