# Source Availability

Video For Lazies source code is distributed from this repository.

Windows and Linux portable builds also include pinned GPL FFmpeg sidecars. The exact FFmpeg binary archives, checksums, upstream FFmpeg source archive, BtbN build recipe archive, x264 source archive, and source checksums are documented in [docs/ffmpeg-bundling.md](docs/ffmpeg-bundling.md). The portable folder includes a copy as `FFMPEG_BUNDLING.md`.

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

Native AI interpolation source, pinned dependency archives, complete notices and build/model provenance are included under `interpolation-sidecar/source/`, `interpolation-sidecar/licenses/` and `interpolation-sidecar/bundle-provenance.json`. The worker sources/build instructions are in [app/native/interpolation](app/native/interpolation/README.md); converted model origins and hashes are in [app/interpolation-models.json](app/interpolation-models.json). Model conversion is not independently reproduced by the build.
