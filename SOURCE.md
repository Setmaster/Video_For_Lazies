# Source Availability

Video For Lazies source code is distributed from this repository.

Windows portable builds also include a pinned GPL FFmpeg sidecar. The exact FFmpeg binary archive, checksums, upstream source archive, and source checksum are documented in [docs/ffmpeg-bundling.md](docs/ffmpeg-bundling.md). The portable folder includes a copy as `FFMPEG_BUNDLING.md`.

The bundled FFmpeg source archive is staged inside the portable folder at:

```text
ffmpeg-sidecar/source/
```

For reproducible release checks, use:

```bash
cd app
npm run release:portable
```
