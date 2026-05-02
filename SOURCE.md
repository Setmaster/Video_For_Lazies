# Source Availability

Video For Lazies source code is distributed from this repository.

Windows portable builds also include a pinned GPL FFmpeg sidecar. The exact FFmpeg binary archive, checksums, upstream FFmpeg source archive, BtbN build recipe archive, x264 source archive, and source checksums are documented in [docs/ffmpeg-bundling.md](docs/ffmpeg-bundling.md). The portable folder includes a copy as `FFMPEG_BUNDLING.md`.

The bundled FFmpeg source/provenance archives are staged inside the portable folder at:

```text
ffmpeg-sidecar/source/
```

Portable builds also generate an exact release-source note into `release/generated-docs/SOURCE.md`, then copy that generated file into the portable folder. That generated file includes the app version, source commit, release tag status, GitHub source archive URLs, and bundled FFmpeg source/provenance archive URLs.

The BtbN build recipe snapshot records the upstream dependency repositories and commits used by the selected GPL build recipe.

For reproducible release checks, use:

```bash
cd app
npm run release:portable
```
