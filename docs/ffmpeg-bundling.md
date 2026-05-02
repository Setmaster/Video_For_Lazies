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
- `npm run release:portable` packages a versioned Windows zip such as `release/Video_For_Lazies-v0.1.0-win-x64.zip`, writes `release/SHA256SUMS.txt`, and verifies the extracted zip with startup plus packaged interaction/export smoke. That release smoke routes input through the shared drop logic, asserts crop-enabled playback before encode, verifies the output file from the extracted artifact, runs a second tight-target 1080p MP4 smoke, and fails if the shipped `ffmpeg-sidecar` is missing `libx264`. When `7z.exe` is available, it also emits a versioned `.7z` archive.

## Pinned Windows bundle

- Provider: BtbN FFmpeg Builds
- Variant: `win64-gpl-shared`
- BtbN release tag: `autobuild-2026-05-02-13-12`
- Binary archive:
  `https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-05-02-13-12/ffmpeg-n8.1-11-g75d37c499d-win64-gpl-shared-8.1.zip`
- Binary archive SHA256:
  `80a686ecdbb35a1d454ee25c0395dc728a5d280eda129dc7364b16a7474a92d1`
- FFmpeg version string from the bundle:
  `n8.1-11-g75d37c499d-20260502`

Note:

- The bundle sync uses a specific BtbN release tag and verifies the pinned SHA256 so upstream changes fail loudly instead of silently changing the shipped runtime.

## Corresponding source snapshot

- Upstream mirror:
  `https://github.com/FFmpeg/FFmpeg`
- Exact source archive:
  `https://codeload.github.com/FFmpeg/FFmpeg/tar.gz/75d37c499da2a9fd50e3ef5a69c7dd87cd96f62a`
- Source archive SHA256:
  `6eeab8eb0491f8722575d0c2ee07bebc2687bb4e77bbfb89f37eceac42f3ed99`

The staged Windows sidecar includes:

- `ffmpeg.exe`
- `ffprobe.exe`
- required shared FFmpeg DLLs from the pinned archive
- upstream `LICENSE.txt`
- `FFMPEG_BUNDLE_NOTICES.txt`
- `source/ffmpeg-75d37c499da2a9fd50e3ef5a69c7dd87cd96f62a.tar.gz`

## Codec behavior

- The pinned GPL Windows bundle ships `libx264`, so bundled MP4 export uses H.264 by default.
- If the active FFmpeg build does not expose `libx264` (for example via `VFL_FFMPEG_PATH`), MP4 export falls back to `mpeg4`.
- WebM and MP3 continue to prefer the bundled `libvpx-vp9`, `libopus`, and `libmp3lame` encoders when available.
