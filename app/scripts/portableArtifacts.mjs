import fs from "node:fs/promises";
import path from "node:path";

export async function copyPortableArtifacts({
  builtBinaryPath,
  outPath,
  companionDirs = [],
  companionFiles = [],
  cleanupPaths = [],
} = {}) {
  const resolvedOutDir = path.dirname(outPath);
  for (const cleanupPath of cleanupPaths) {
    await fs.rm(cleanupPath, { recursive: true, force: true });
  }
  await fs.rm(resolvedOutDir, { recursive: true, force: true });
  await fs.mkdir(resolvedOutDir, { recursive: true });

  try {
    await fs.copyFile(builtBinaryPath, outPath);
  } catch (e) {
    console.error(`Failed to copy built binary.`);
    console.error(`Expected: ${builtBinaryPath}`);
    console.error(`Did you run: npm run tauri build`);
    throw e;
  }

  for (const { name, sourcePath, outputPath } of companionDirs) {
    try {
      await fs.rm(outputPath, { recursive: true, force: true });
      await fs.cp(sourcePath, outputPath, { recursive: true });
    } catch (e) {
      console.error(`Failed to copy portable companion directory: ${name}`);
      console.error(`Expected: ${sourcePath}`);
      throw e;
    }
  }

  for (const { name, sourcePath, outputPath } of companionFiles) {
    try {
      await fs.mkdir(path.dirname(outputPath), { recursive: true });
      await fs.copyFile(sourcePath, outputPath);
    } catch (e) {
      console.error(`Failed to copy portable companion file: ${name}`);
      console.error(`Expected: ${sourcePath}`);
      throw e;
    }
  }
}
