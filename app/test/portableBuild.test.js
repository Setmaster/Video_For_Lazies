import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import os from "node:os";

import { copyPortableArtifacts } from "../scripts/portableArtifacts.mjs";

test("copyPortableArtifacts copies the app binary, bundled sidecar, and legal files", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-portable-test-"));
  try {
    const builtBinaryPath = path.resolve(tempRoot, "build", "video_for_lazies.exe");
    const sidecarSource = path.resolve(tempRoot, "build", "ffmpeg-sidecar");
    const outPath = path.resolve(tempRoot, "release", "Video_For_Lazies", "Video_For_Lazies.exe");
    const sidecarOutput = path.resolve(tempRoot, "release", "Video_For_Lazies", "ffmpeg-sidecar");
    const readmeSource = path.resolve(tempRoot, "docs", "README.md");
    const readmeOutput = path.resolve(tempRoot, "release", "Video_For_Lazies", "README.md");
    const desktopSource = path.resolve(tempRoot, "docs", "Video_For_Lazies.desktop");
    const desktopOutput = path.resolve(tempRoot, "release", "Video_For_Lazies", "Video_For_Lazies.desktop");
    const legacyExe = path.resolve(tempRoot, "release", "Video_For_Lazies.exe");
    const legacySidecar = path.resolve(tempRoot, "release", "ffmpeg-sidecar");

    await fs.mkdir(path.dirname(builtBinaryPath), { recursive: true });
    await fs.mkdir(path.dirname(readmeSource), { recursive: true });
    await fs.mkdir(sidecarSource, { recursive: true });
    await fs.mkdir(legacySidecar, { recursive: true });
    await fs.writeFile(builtBinaryPath, "binary");
    await fs.writeFile(path.resolve(sidecarSource, "ffmpeg.exe"), "ffmpeg");
    await fs.writeFile(path.resolve(sidecarSource, "ffprobe.exe"), "ffprobe");
    await fs.writeFile(path.resolve(sidecarSource, "FFMPEG_BUNDLE_NOTICES.txt"), "notice");
    await fs.writeFile(readmeSource, "readme");
    await fs.writeFile(desktopSource, "desktop");
    await fs.writeFile(legacyExe, "stale-binary");
    await fs.writeFile(path.resolve(legacySidecar, "ffmpeg.exe"), "stale-ffmpeg");

    await copyPortableArtifacts({
      builtBinaryPath,
      outPath,
      companionDirs: [
        {
          name: "ffmpeg-sidecar",
          sourcePath: sidecarSource,
          outputPath: sidecarOutput,
        },
      ],
      companionFiles: [
        {
          name: "README.md",
          sourcePath: readmeSource,
          outputPath: readmeOutput,
        },
        {
          name: "Video_For_Lazies.desktop",
          sourcePath: desktopSource,
          outputPath: desktopOutput,
          mode: 0o755,
        },
      ],
      cleanupPaths: [legacyExe, legacySidecar],
    });

    assert.equal(await fs.readFile(outPath, "utf8"), "binary");
    assert.equal(await fs.readFile(path.resolve(sidecarOutput, "ffmpeg.exe"), "utf8"), "ffmpeg");
    assert.equal(await fs.readFile(path.resolve(sidecarOutput, "ffprobe.exe"), "utf8"), "ffprobe");
    assert.equal(await fs.readFile(readmeOutput, "utf8"), "readme");
    assert.equal(await fs.readFile(desktopOutput, "utf8"), "desktop");
    if (process.platform !== "win32") {
      assert.equal((await fs.stat(desktopOutput)).mode & 0o777, 0o755);
    }
    await assert.rejects(fs.access(legacyExe));
    await assert.rejects(fs.access(legacySidecar));
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});
