import path from "node:path";
import url from "node:url";
import { spawn } from "node:child_process";

import { getPortableOutputDir } from "./ffmpegBundle.mjs";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));
const __filename = url.fileURLToPath(import.meta.url);

async function runPortableSmoke({ portableDir = getPortableOutputDir() } = {}) {
  if (process.platform !== "win32") {
    console.log("Skipping portable startup smoke on non-Windows host.");
    return;
  }

  const scriptPath = path.resolve(__dirname, "windows-portable-smoke.ps1");

  await new Promise((resolve, reject) => {
    const child = spawn(
      "powershell.exe",
      [
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        scriptPath,
        "-PortableDir",
        portableDir,
      ],
      { stdio: "inherit" },
    );

    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(new Error(`Portable smoke exited with code ${code}`));
    });
  });
}

if (process.argv[1] && path.resolve(process.argv[1]) === __filename) {
  await runPortableSmoke();
}

export { runPortableSmoke };
