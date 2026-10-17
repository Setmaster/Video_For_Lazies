# Third-Party Notices

Video For Lazies is licensed under GPL-3.0-or-later.

## FFmpeg Sidecars

Windows x64 and Linux x64 portable builds bundle pinned GPL FFmpeg runtimes as `ffmpeg-sidecar/`.

- Provider: BtbN FFmpeg Builds
- Windows variant: `win64-gpl-shared`
- Linux variant: `linux64-gpl-shared`
- FFmpeg bundle details, checksums, corresponding source archives, and build-provenance archives: [docs/ffmpeg-bundling.md](docs/ffmpeg-bundling.md)

The staged sidecar includes upstream FFmpeg license text, bundle notices, and source/provenance archives under `ffmpeg-sidecar/source/`.

## Native AI Frame Interpolation

Portable builds include a native RIFE worker and the default RIFE 4.25 model in `interpolation-sidecar/`. The VFL worker is GPL-3.0-or-later and uses ncnn (BSD-3-Clause), glslang (its multi-party license), and a modified RIFE Warp layer (MIT). Practical-RIFE explicitly applies MIT to its linked trained models. The exact converted model files, immutable origin, byte counts and SHA256 digests are pinned in [app/interpolation-models.json](app/interpolation-models.json).

The sidecar includes `INTERPOLATION_NOTICES.txt`, exact license texts, source archives/native build sources and `bundle-provenance.json`. See [native build documentation](app/native/interpolation/README.md). The build verifies published converted weights; it does not reproduce their conversion from the original checkpoints. GPU drivers remain an operating-system/vendor prerequisite.

## Embedded Subtitle Font

External SRT burn-in uses DejaVu Sans 2.37, embedded in the application and staged privately for FFmpeg/libass so the fixed base face is available on every supported platform. Glyphs outside DejaVu Sans coverage may still require platform fallback.

- Upstream: DejaVu Fonts
- Source archive: https://github.com/dejavu-fonts/dejavu-fonts/releases/download/version_2_37/dejavu-fonts-ttf-2.37.zip
- Source archive SHA256: `7576310b219e04159d35ff61dd4a4ec4cdba4f35c00e002a136f00e96a908b0a`
- Embedded `DejaVuSans.ttf` SHA256: `7da195a74c55bef988d0d48f9508bd5d849425c1770dba5d7bfc6ce9ed848954`
- License file SHA256: `7a083b136e64d064794c3419751e5c7dd10d2f64c108fe5ba161eae5e5958a93`
- License text: [app/src-tauri/assets/DEJAVU_FONT_LICENSE.txt](app/src-tauri/assets/DEJAVU_FONT_LICENSE.txt)

## JavaScript and Rust Dependencies

The application uses open source npm and Cargo packages listed in:

- [app/package-lock.json](app/package-lock.json)
- [app/src-tauri/Cargo.lock](app/src-tauri/Cargo.lock)

Portable builds generate an exact dependency notice inventory from those lockfiles and package metadata into `release/generated-docs/THIRD_PARTY_NOTICES.md`, then copy that generated file into the portable folder.
