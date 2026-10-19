import fs from "node:fs/promises";
import path from "node:path";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const recordingSidecarDir = path.join(appRoot, "src-tauri", "target", "recording-sidecar");
export const recordingExecutableName = "vfl_recording_host.exe";
const notice = "Video For Lazies Windows recording helper\nGPL-3.0-or-later; see the application LICENSE.txt and SOURCE.md.\nCorresponding source: app/src-tauri/src/bin/vfl_recording_host.rs and app/src-tauri/src/recording/ in the application source snapshot.\nWindows Graphics Capture, Media Foundation and WASAPI are operating-system components, not redistributed DLLs. Recording availability depends on the installed Windows components. No recording runtime download is performed.\n";

// Cargo identifies the actual executable, including configured target directories
// and target triples. Never pick an older executable by guessing its location.
export async function prepareRecordingSidecar({ platform = process.platform, target = process.env.CARGO_BUILD_TARGET } = {}) {
  if (platform !== "win32") return null;
  if (process.platform !== "win32" || process.arch !== "x64") {
    throw new Error("Build the recording helper natively on Windows x64.");
  }
  if (target && target !== "x86_64-pc-windows-msvc") {
    throw new Error("The recording helper requires the Windows x64 MSVC target.");
  }
  // tauri-build copies resources even while compiling this helper. Populate the
  // resource directory first; a failed build never authorizes portable creation.
  await fs.mkdir(recordingSidecarDir, { recursive: true });
  await fs.writeFile(path.join(recordingSidecarDir, "RECORDING_NOTICES.txt"), notice);
  const args = ["build", "--locked", "--release", "--bin", "vfl_recording_host", "--message-format=json-render-diagnostics"];
  if (target) args.push("--target", target);
  let executable;
  await new Promise((resolve, reject) => {
    const child = spawn("cargo", args, {
      cwd: path.join(appRoot, "src-tauri"),
      stdio: ["ignore", "pipe", "inherit"],
      windowsHide: true,
    });
    const lines = createInterface({ input: child.stdout });
    lines.on("line", (line) => {
      let message;
      try { message = JSON.parse(line); } catch { console.log(line); return; }
      if (message.reason === "compiler-artifact" && message.target?.name === "vfl_recording_host" && message.executable) executable = message.executable;
      if (message.reason === "compiler-message" && message.message?.rendered) process.stderr.write(message.message.rendered);
    });
    child.on("error", reject);
    child.on("close", (code) => code === 0 ? resolve() : reject(new Error(`Recording helper cargo build failed (${code}).`)));
  });
  if (!executable || !(await fs.lstat(executable)).isFile()) {
    throw new Error("Cargo did not report a regular recording helper executable.");
  }
  const destination = path.join(recordingSidecarDir, recordingExecutableName);
  const temporary = `${destination}.${process.pid}.tmp`;
  try {
    await fs.copyFile(executable, temporary);
    await fs.rename(temporary, destination);
  } finally { await fs.rm(temporary, { force: true }); }
  return recordingSidecarDir;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  if (args.length && !(args.length === 2 && args[0] === "--target" && args[1])) {
    console.error("Usage: node scripts/build-recording-sidecar.mjs [--target x86_64-pc-windows-msvc]");
    process.exitCode = 1;
  } else {
    prepareRecordingSidecar({ target: args[1] || process.env.CARGO_BUILD_TARGET })
      .then((dir) => console.log(dir ? `Recording sidecar ready: ${dir}` : "Skipping Windows recording sidecar on non-Windows host."))
      .catch((error) => { console.error(error.message); process.exitCode = 1; });
  }
}
