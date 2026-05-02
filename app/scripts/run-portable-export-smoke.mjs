import path from "node:path";
import url from "node:url";
import { spawn } from "node:child_process";

import { getPortableOutputDir } from "./ffmpegBundle.mjs";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));
const __filename = url.fileURLToPath(import.meta.url);

async function runPortableExportSmoke({
  portableDir = getPortableOutputDir(),
  timeoutSeconds,
  trimEndSeconds,
  inputWidth,
  inputHeight,
  inputRate,
  inputVideoBitrateKbps,
  sizeLimitMb,
} = {}) {
  if (process.platform !== "win32") {
    console.log("Skipping portable export smoke on non-Windows host.");
    return;
  }

  const scriptPath = path.resolve(__dirname, "windows-portable-export-smoke.ps1");
  const args = [
    "-NoProfile",
    "-ExecutionPolicy",
    "Bypass",
    "-File",
    scriptPath,
    "-PortableDir",
    portableDir,
  ];

  const optionalArgs = [
    ["-TimeoutSeconds", timeoutSeconds],
    ["-TrimEndSeconds", trimEndSeconds],
    ["-InputWidth", inputWidth],
    ["-InputHeight", inputHeight],
    ["-InputRate", inputRate],
    ["-InputVideoBitrateKbps", inputVideoBitrateKbps],
    ["-SizeLimitMb", sizeLimitMb],
  ];

  for (const [flag, value] of optionalArgs) {
    if (value === undefined || value === null) {
      continue;
    }
    args.push(flag, String(value));
  }

  await new Promise((resolve, reject) => {
    const child = spawn("powershell.exe", args, { stdio: "inherit" });

    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(new Error(`Portable export smoke exited with code ${code}`));
    });
  });
}

if (process.argv[1] && path.resolve(process.argv[1]) === __filename) {
  await runPortableExportSmoke();
}

export { runPortableExportSmoke };
