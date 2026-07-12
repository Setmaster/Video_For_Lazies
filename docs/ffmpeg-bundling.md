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
- The portable folder includes the platform runtime, a byte-for-byte copy of the canonical `app/ffmpeg-capabilities.json` contract as `ffmpeg-sidecar/FFMPEG_CAPABILITIES.json`, the app and update helper, an owned-file payload manifest, and generated legal/source documents.
- `npm run portable` performs the platform build. On Windows, it also runs the baseline startup gate: `app/scripts/windows-portable-smoke.ps1` launches the packaged executable, captures the app window, and rejects a missing or overwhelmingly bright/white surface. This catches regressions such as an accidental development/localhost build.
- `npm run release:portable` packages a versioned zip such as `release/Video_For_Lazies-vX.Y.Z-win-x64.zip` or `release/Video_For_Lazies-vX.Y.Z-linux-x64.zip`, writes `release/SHA256SUMS.txt` and the payload-manifest sidecar, extracts the new zip, and verifies that extracted copy rather than trusting the build directory.

The extracted-archive verification on both supported platforms checks:

- exact payload ownership, hashes, sizes, and platform modes, including the app, update helper, legal/source files, sidecar runtime, and embedded payload manifest
- byte identity of the canonical capability contract, then live encoder/filter inspection against the extracted FFmpeg
- a real `libx264` encode followed by a duration probe through the extracted FFprobe
- packaged media-depth behavior for selected streams, attached pictures, HDR/high-depth policy, SAR/rotation geometry, and reverse/loop memory bounds
- exact-byte targets, opt-in Strict Fit, separate audio-removal permission, and one bounded external UTF-8 SRT with the fixed embedded subtitle font
- frame-accurate trim, existing-destination no-clobber refusal, and metadata/privacy checks
- recipe privacy, queue retry and snapshot restoration, multi-file routing, accessibility foundations, phase-aware progress, rotation/speed/frame-rate-cap behavior, Reset, Cancel, queued drops, and export-lifecycle behavior
- normal MP4 and WebM exports, custom-size output, and a tight-target 1080p case

Linux additionally runs the full bundled codec-plan matrix. Windows additionally runs the native packaged-window startup and keyboard interaction paths. These checks prove the selected repository contract and tested fixtures; they are not a general claim that every possible FFmpeg input or filter combination works.

## Pinned Windows bundle

- Provider: BtbN FFmpeg Builds
- Variant: `win64-gpl-shared`
- BtbN release tag: `autobuild-2026-06-30-13-34`
- Binary archive:
  `https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-06-30-13-34/ffmpeg-n8.1.2-21-gce3c09c101-win64-gpl-shared-8.1.zip`
- Binary archive SHA256:
  `ec51253085a831b517e68cb7a1e46d13fcc8324f5e61ac0b3fd73c56af41ca21`
- FFmpeg version string from the bundle:
  `n8.1.2-21-gce3c09c101-20260630`

## Pinned Linux bundle

- Provider: BtbN FFmpeg Builds
- Variant: `linux64-gpl-shared`
- BtbN release tag: `autobuild-2026-06-30-13-34`
- Binary archive:
  `https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-06-30-13-34/ffmpeg-n8.1.2-21-gce3c09c101-linux64-gpl-shared-8.1.tar.xz`
- Binary archive SHA256:
  `23f5d4c8e6fdc24fbbfcbbb8e83a727154f1ef70830b108ac7fd131856777405`
- FFmpeg version string from the bundle:
  `n8.1.2-21-gce3c09c101-20260630`

Note:

- The bundle sync uses a specific BtbN release tag and verifies the pinned SHA256 so upstream changes fail loudly instead of silently changing the shipped runtime.
- BtbN retains the last build of each month for two years but keeps only the last 14 ordinary daily builds. Release pins must use a month-end build covered by that retention policy. See the [upstream release retention policy](https://github.com/BtbN/FFmpeg-Builds/blob/master/README.md#release-retention-policy).

## Corresponding source snapshot

### FFmpeg source

- Upstream mirror:
  `https://github.com/FFmpeg/FFmpeg`
- Exact source archive:
  `https://codeload.github.com/FFmpeg/FFmpeg/tar.gz/ce3c09c101c83add623774d414a9f9498caf5c25`
- Source archive SHA256:
  `39bfd9846bea941da736683f79cdf7c87117c20efebc0734981d5e7033434dc5`

### BtbN build recipe snapshot

- Upstream repository:
  `https://github.com/BtbN/FFmpeg-Builds`
- Exact build recipe archive:
  `https://github.com/BtbN/FFmpeg-Builds/archive/7a83528ea3431e9eca982a712bc3a7cd0789d5d0.tar.gz`
- Build recipe archive SHA256:
  `0f0f15e02b4fd1b1bc37d2e3a6f57cd7a2078c31a51c8546110d3ccb40029d30`
- Build recipe commit:
  `7a83528ea3431e9eca982a712bc3a7cd0789d5d0`

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
- `source/ffmpeg-ce3c09c101c83add623774d414a9f9498caf5c25.tar.gz`
- `source/btbn-ffmpeg-builds-7a83528ea3431e9eca982a712bc3a7cd0789d5d0.tar.gz`
- `source/x264-0480cb05fa188d37ae87e8f4fd8f1aea3711f7ee.tar.gz`

The staged Linux sidecar includes:

- root `ffmpeg` and `ffprobe` wrapper scripts
- `bin/ffmpeg`
- `bin/ffprobe`
- required shared FFmpeg libraries under `lib/`
- upstream `LICENSE.txt`
- `FFMPEG_BUNDLE_NOTICES.txt`
- `source/ffmpeg-ce3c09c101c83add623774d414a9f9498caf5c25.tar.gz`
- `source/btbn-ffmpeg-builds-7a83528ea3431e9eca982a712bc3a7cd0789d5d0.tar.gz`
- `source/x264-0480cb05fa188d37ae87e8f4fd8f1aea3711f7ee.tar.gz`

The Linux root wrapper scripts set `LD_LIBRARY_PATH` to the bundled `ffmpeg-sidecar/lib/` directory before executing `bin/ffmpeg` or `bin/ffprobe`. The app still launches `ffmpeg-sidecar/ffmpeg` and `ffmpeg-sidecar/ffprobe`, so the runtime lookup contract stays the same across Windows and Linux.

The project does not claim bit-for-bit reproducibility for the BtbN-hosted binary. The included source and build-provenance archives are intended to make the selected FFmpeg/x264 source basis explicit for public distribution review. The BtbN build recipe snapshot records the remaining upstream dependency repositories and commits used by the selected GPL build recipe.

## Codec behavior

- The pinned GPL Windows and Linux bundles ship `libx264`, so bundled MP4 export uses H.264 by default.
- In Auto mode, an active FFmpeg build without `libx264` (for example via `VFL_FFMPEG_PATH`) can fall back to `mpeg4` when that encoder is available. An explicit H.264 request or standard-SDR conversion fails closed instead.
- WebM and MP3 continue to prefer the bundled `libvpx-vp9`, `libopus`, and `libmp3lame` encoders when available.
- The release-required capability contract also covers the encoders and filters used by crop, scale, color conversion, SAR normalization, reverse/loop, audio normalization, PNG frame export, and fixed-style SRT burn-in. A custom FFmpeg override can disable an affected feature when that capability is missing; the app checks the active runtime and fails closed instead of assuming parity with the bundle.
