# Third-Party Notices

Video For Lazies is licensed under GPL-3.0-or-later.

## FFmpeg

Windows portable builds bundle a pinned GPL FFmpeg runtime as `ffmpeg-sidecar/`.

- Provider: BtbN FFmpeg Builds
- Variant: `win64-gpl-shared`
- FFmpeg bundle details, checksums, and corresponding source archive: [docs/ffmpeg-bundling.md](docs/ffmpeg-bundling.md)

The staged sidecar includes upstream FFmpeg license text, bundle notices, and the corresponding source archive under `ffmpeg-sidecar/source/`.

## JavaScript and Rust Dependencies

The application uses open source npm and Cargo packages listed in:

- [app/package-lock.json](app/package-lock.json)
- [app/src-tauri/Cargo.lock](app/src-tauri/Cargo.lock)

Review dependency lockfiles before release if licenses or package versions change.
