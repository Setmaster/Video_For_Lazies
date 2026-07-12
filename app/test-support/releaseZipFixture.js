import fs from "node:fs/promises";
import path from "node:path";
import { spawn } from "node:child_process";

function powershellQuote(value) {
  return `'${String(value).replaceAll("'", "''")}'`;
}

async function runChecked(command, args, { cwd } = {}) {
  await new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      stdio: ["ignore", "ignore", "pipe"],
      windowsHide: true,
    });
    let stderr = "";
    let settled = false;
    const finish = (action) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      action();
    };
    const timeout = setTimeout(() => {
      child.kill();
      finish(() => reject(new Error(`${command} timed out while creating a release ZIP fixture.`)));
    }, 60_000);

    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });
    child.on("error", (error) => finish(() => reject(error)));
    child.on("close", (code, signal) => finish(() => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(new Error(
        `${command} failed to create a release ZIP fixture (code ${String(code)}, signal ${signal ?? "none"}): ${stderr}`,
      ));
    }));
  });
}

export async function createReleaseZipFixture(portableDir, zipPath) {
  await fs.mkdir(path.dirname(zipPath), { recursive: true });
  await fs.rm(zipPath, { force: true });
  if (process.platform === "win32") {
    await runChecked("powershell.exe", [
      "-NoProfile",
      "-Command",
      `$ErrorActionPreference='Stop'; Compress-Archive -LiteralPath ${powershellQuote(portableDir)} -DestinationPath ${powershellQuote(zipPath)} -Force`,
    ]);
    return;
  }
  if (process.platform === "linux") {
    await runChecked("zip", ["-r", "-q", zipPath, path.basename(portableDir)], {
      cwd: path.dirname(portableDir),
    });
    return;
  }
  throw new Error(`Release ZIP fixtures are unsupported on ${process.platform}.`);
}
