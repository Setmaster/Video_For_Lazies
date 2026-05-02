# Third-Party Notices

Video For Lazies is licensed under GPL-3.0-or-later.

## FFmpeg

Windows portable builds bundle a pinned GPL FFmpeg runtime as `ffmpeg-sidecar/`.

- Provider: BtbN FFmpeg Builds
- Variant: `win64-gpl-shared`
- FFmpeg bundle details, checksums, corresponding source archives, and build-provenance archives: [docs/ffmpeg-bundling.md](docs/ffmpeg-bundling.md)

The staged sidecar includes upstream FFmpeg license text, bundle notices, and source/provenance archives under `ffmpeg-sidecar/source/`.

## JavaScript and Rust Dependencies

The application uses open source npm and Cargo packages listed in:

- [app/package-lock.json](app/package-lock.json)
- [app/src-tauri/Cargo.lock](app/src-tauri/Cargo.lock)

Portable builds generate an exact dependency notice inventory from those lockfiles and package metadata into `release/generated-docs/THIRD_PARTY_NOTICES.md`, then copy that generated file into the portable folder.
