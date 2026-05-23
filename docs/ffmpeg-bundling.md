# FFmpeg Bundling

This app bundles FFmpeg for Windows x64 and Linux x64 release builds so end users do not need a separate FFmpeg install on supported portable targets.

## Runtime contract

- Resolution order:
  1. `VFL_FFMPEG_PATH` / `VFL_FFPROBE_PATH`
  2. bundled `ffmpeg-sidecar/` next to the app executable
  3. plain `ffmpeg` / `ffprobe` on `PATH`
- Platform bundles are staged by `app/scripts/sync-ffmpeg-sidecar.mjs`.
- `npm run tauri build` and `npm run portable` trigger that staging automatically through Tauri's `beforeBuildCommand`.
- The portable release format is a folder: `release/Video_For_Lazies/` plus `release/Video_For_Lazies/ffmpeg-sidecar/`.
- The portable build also runs a Windows startup smoke (`app/scripts/windows-portable-smoke.ps1`) after packaging. It launches the portable exe, captures the app window, and fails the build if the rendered surface is overwhelmingly bright/white, which catches regressions like the accidental dev-surface/localhost build.
- `npm run release:portable` packages a versioned zip such as `release/Video_For_Lazies-vX.Y.Z-win-x64.zip` or `release/Video_For_Lazies-vX.Y.Z-linux-x64.zip`, writes `release/SHA256SUMS.txt`, writes a payload manifest sidecar for the updater workflow, and verifies the extracted zip. Both platforms verify the bundled FFmpeg sidecar exposes `libx264` and can encode/probe a sample MP4. Windows additionally runs startup plus packaged interaction/export smoke, including a second tight-target 1080p MP4 smoke.

## Pinned Windows bundle

- Provider: BtbN FFmpeg Builds
- Variant: `win64-gpl-shared`
- BtbN release tag: `autobuild-2026-05-22-15-11`
- Binary archive:
  `https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-05-22-15-11/ffmpeg-n8.1.1-8-gb21e00eda5-win64-gpl-shared-8.1.zip`
- Binary archive SHA256:
  `b691c0525c0fbe1bda64b3da470d0c2bdc4b485cfb72cae3d88c030f4116c9a0`
- FFmpeg version string from the bundle:
  `n8.1.1-8-gb21e00eda5-20260522`

## Pinned Linux bundle

- Provider: BtbN FFmpeg Builds
- Variant: `linux64-gpl-shared`
- BtbN release tag: `autobuild-2026-05-22-15-11`
- Binary archive:
  `https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-05-22-15-11/ffmpeg-n8.1.1-8-gb21e00eda5-linux64-gpl-shared-8.1.tar.xz`
- Binary archive SHA256:
  `3c75729401fbe1a1f7e1ee62e09ba7b895418489a2017e2ac6e9db7ccfc5f815`
- FFmpeg version string from the bundle:
  `n8.1.1-8-gb21e00eda5-20260522`

Note:

- The bundle sync uses a specific BtbN release tag and verifies the pinned SHA256 so upstream changes fail loudly instead of silently changing the shipped runtime.

## Corresponding source snapshot

### FFmpeg source

- Upstream mirror:
  `https://github.com/FFmpeg/FFmpeg`
- Exact source archive:
  `https://codeload.github.com/FFmpeg/FFmpeg/tar.gz/b21e00eda5b16c9d76ff97f029051d7be8f95d10`
- Source archive SHA256:
  `ffef396c7a19b8e99c58e762d315e0fdcb21bc69778bebd42e9b2a919486c8be`

### BtbN build recipe snapshot

- Upstream repository:
  `https://github.com/BtbN/FFmpeg-Builds`
- Exact build recipe archive:
  `https://github.com/BtbN/FFmpeg-Builds/archive/6e66d4d1e81f75b5f34dc2a369cc341e12edc531.tar.gz`
- Build recipe archive SHA256:
  `239a76c068e5a148d691a9b29b1b93498164a8df79578bf684a41e2eb28ac4cb`
- Build recipe commit:
  `6e66d4d1e81f75b5f34dc2a369cc341e12edc531`

### x264 source snapshot

- Upstream repository:
  `https://code.videolan.org/videolan/x264`
- Exact x264 source archive:
  `https://code.videolan.org/videolan/x264/-/archive/0480cb05fa188d37ae87e8f4fd8f1aea3711f7ee/x264-0480cb05fa188d37ae87e8f4fd8f1aea3711f7ee.tar.gz`
- x264 source archive SHA256:
  `d0967a1348c85dfde363bb52610403be898171493100561efa0dd05d5fd1ae50`
- x264 commit referenced by the pinned BtbN recipe:
  `0480cb05fa188d37ae87e8f4fd8f1aea3711f7ee`

The staged Windows sidecar includes:

- `ffmpeg.exe`
- `ffprobe.exe`
- required shared FFmpeg DLLs from the pinned archive
- upstream `LICENSE.txt`
- `FFMPEG_BUNDLE_NOTICES.txt`
- `source/ffmpeg-b21e00eda5b16c9d76ff97f029051d7be8f95d10.tar.gz`
- `source/btbn-ffmpeg-builds-6e66d4d1e81f75b5f34dc2a369cc341e12edc531.tar.gz`
- `source/x264-0480cb05fa188d37ae87e8f4fd8f1aea3711f7ee.tar.gz`

The staged Linux sidecar includes:

- root `ffmpeg` and `ffprobe` wrapper scripts
- `bin/ffmpeg`
- `bin/ffprobe`
- required shared FFmpeg libraries under `lib/`
- upstream `LICENSE.txt`
- `FFMPEG_BUNDLE_NOTICES.txt`
- `source/ffmpeg-b21e00eda5b16c9d76ff97f029051d7be8f95d10.tar.gz`
- `source/btbn-ffmpeg-builds-6e66d4d1e81f75b5f34dc2a369cc341e12edc531.tar.gz`
- `source/x264-0480cb05fa188d37ae87e8f4fd8f1aea3711f7ee.tar.gz`

The Linux root wrapper scripts set `LD_LIBRARY_PATH` to the bundled `ffmpeg-sidecar/lib/` directory before executing `bin/ffmpeg` or `bin/ffprobe`. The app still launches `ffmpeg-sidecar/ffmpeg` and `ffmpeg-sidecar/ffprobe`, so the runtime lookup contract stays the same across Windows and Linux.

The project does not claim bit-for-bit reproducibility for the BtbN-hosted binary. The included source and build-provenance archives are intended to make the selected FFmpeg/x264 source basis explicit for public distribution review. The BtbN build recipe snapshot records the remaining upstream dependency repositories and commits used by the selected GPL build recipe.

## Codec behavior

- The pinned GPL Windows and Linux bundles ship `libx264`, so bundled MP4 export uses H.264 by default.
- If the active FFmpeg build does not expose `libx264` (for example via `VFL_FFMPEG_PATH`), MP4 export falls back to `mpeg4`.
- WebM and MP3 continue to prefer the bundled `libvpx-vp9`, `libopus`, and `libmp3lame` encoders when available.
