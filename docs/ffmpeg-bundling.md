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
- BtbN release tag: `autobuild-2026-06-08-14-24`
- Binary archive:
  `https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-06-08-14-24/ffmpeg-n8.1.1-11-ge4c7fbf6c0-win64-gpl-shared-8.1.zip`
- Binary archive SHA256:
  `b11fdf2ed53372f334a7732883cdbe9814cd730450672d58fa56c47111526580`
- FFmpeg version string from the bundle:
  `n8.1.1-11-ge4c7fbf6c0-20260608`

## Pinned Linux bundle

- Provider: BtbN FFmpeg Builds
- Variant: `linux64-gpl-shared`
- BtbN release tag: `autobuild-2026-06-08-14-24`
- Binary archive:
  `https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-06-08-14-24/ffmpeg-n8.1.1-11-ge4c7fbf6c0-linux64-gpl-shared-8.1.tar.xz`
- Binary archive SHA256:
  `30b0ca389e7d0e34b4ca1acb5fe38d57fbc2542d690a8c5b06c72bdac0a234d1`
- FFmpeg version string from the bundle:
  `n8.1.1-11-ge4c7fbf6c0-20260608`

Note:

- The bundle sync uses a specific BtbN release tag and verifies the pinned SHA256 so upstream changes fail loudly instead of silently changing the shipped runtime.

## Corresponding source snapshot

### FFmpeg source

- Upstream mirror:
  `https://github.com/FFmpeg/FFmpeg`
- Exact source archive:
  `https://codeload.github.com/FFmpeg/FFmpeg/tar.gz/e4c7fbf6c0a922297d4df7288d7aea665af15e24`
- Source archive SHA256:
  `c4cb2356bef3c6d46a00098f42380a071475a44052933b499af6d7d4d7e6d520`

### BtbN build recipe snapshot

- Upstream repository:
  `https://github.com/BtbN/FFmpeg-Builds`
- Exact build recipe archive:
  `https://github.com/BtbN/FFmpeg-Builds/archive/97ef373e17b7c9de46e980de0a26698237f09a83.tar.gz`
- Build recipe archive SHA256:
  `0fc9bae62374dee03991cff5dacf192b3d9af4b36e16294bb70f7d66c7c22be8`
- Build recipe commit:
  `97ef373e17b7c9de46e980de0a26698237f09a83`

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
- `source/ffmpeg-e4c7fbf6c0a922297d4df7288d7aea665af15e24.tar.gz`
- `source/btbn-ffmpeg-builds-97ef373e17b7c9de46e980de0a26698237f09a83.tar.gz`
- `source/x264-0480cb05fa188d37ae87e8f4fd8f1aea3711f7ee.tar.gz`

The staged Linux sidecar includes:

- root `ffmpeg` and `ffprobe` wrapper scripts
- `bin/ffmpeg`
- `bin/ffprobe`
- required shared FFmpeg libraries under `lib/`
- upstream `LICENSE.txt`
- `FFMPEG_BUNDLE_NOTICES.txt`
- `source/ffmpeg-e4c7fbf6c0a922297d4df7288d7aea665af15e24.tar.gz`
- `source/btbn-ffmpeg-builds-97ef373e17b7c9de46e980de0a26698237f09a83.tar.gz`
- `source/x264-0480cb05fa188d37ae87e8f4fd8f1aea3711f7ee.tar.gz`

The Linux root wrapper scripts set `LD_LIBRARY_PATH` to the bundled `ffmpeg-sidecar/lib/` directory before executing `bin/ffmpeg` or `bin/ffprobe`. The app still launches `ffmpeg-sidecar/ffmpeg` and `ffmpeg-sidecar/ffprobe`, so the runtime lookup contract stays the same across Windows and Linux.

The project does not claim bit-for-bit reproducibility for the BtbN-hosted binary. The included source and build-provenance archives are intended to make the selected FFmpeg/x264 source basis explicit for public distribution review. The BtbN build recipe snapshot records the remaining upstream dependency repositories and commits used by the selected GPL build recipe.

## Codec behavior

- The pinned GPL Windows and Linux bundles ship `libx264`, so bundled MP4 export uses H.264 by default.
- If the active FFmpeg build does not expose `libx264` (for example via `VFL_FFMPEG_PATH`), MP4 export falls back to `mpeg4`.
- WebM and MP3 continue to prefer the bundled `libvpx-vp9`, `libopus`, and `libmp3lame` encoders when available.
