import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import url from "node:url";
import { spawn } from "node:child_process";

import { ffmpegSidecarResourceTarget, getPortableOutputDir } from "./ffmpegBundle.mjs";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));
const __filename = url.fileURLToPath(import.meta.url);
const requiredSmokeStages = ["input-applied", "probe-ready", "preview-ready", "interaction-ready", "encoding", "success"];
const supportedSmokeOutputFormats = new Set(["mp4", "webm", "mp3"]);

function smokeNumber(value, fallback) {
  return value === undefined || value === null ? fallback : value;
}

function normalizeSmokeOutputFormat(value) {
  const outputFormat = String(value || "mp4").toLowerCase();
  if (!supportedSmokeOutputFormats.has(outputFormat)) {
    throw new Error(`Unsupported portable export smoke format: ${value}`);
  }
  return outputFormat;
}

function sleep(ms) {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

async function exists(filePath) {
  try {
    await fs.access(filePath);
    return true;
  } catch {
    return false;
  }
}

async function assertRequiredFile(filePath) {
  if (!(await exists(filePath))) {
    throw new Error(`Portable export smoke could not find required file: ${filePath}`);
  }
}

async function runChecked(command, args, options = {}) {
  await new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: options.cwd,
      env: options.env,
      stdio: options.stdio ?? "inherit",
    });

    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(new Error(`${command} exited with code ${code}`));
    });
  });
}

async function captureStdout(command, args) {
  return await new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";

    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) {
        resolve(stdout);
        return;
      }
      reject(new Error(`${command} exited with code ${code}\n${stderr}`));
    });
  });
}

async function readJsonFileOrNull(filePath) {
  try {
    return JSON.parse(await fs.readFile(filePath, "utf8"));
  } catch {
    return null;
  }
}

function getLinuxPortablePaths(portableDir) {
  const portableRoot = path.resolve(portableDir);
  return {
    portableRoot,
    appPath: path.resolve(portableRoot, "video_for_lazies"),
    ffmpegPath: path.resolve(portableRoot, ffmpegSidecarResourceTarget, "ffmpeg"),
    ffprobePath: path.resolve(portableRoot, ffmpegSidecarResourceTarget, "ffprobe"),
  };
}

function buildLinuxLaunchCommand(appPath, { env = process.env } = {}) {
  if (env.DISPLAY) {
    return { command: appPath, args: [] };
  }
  return { command: "xvfb-run", args: ["-a", appPath] };
}

async function waitForLinuxSmokeSuccess(child, statusPath, timeoutSeconds) {
  let childExit = null;
  child.on("exit", (code, signal) => {
    childExit = { code, signal };
  });

  const deadline = Date.now() + timeoutSeconds * 1000;
  let lastStatus = null;
  let lastStageName = null;

  while (Date.now() < deadline) {
    await sleep(500);
    const status = await readJsonFileOrNull(statusPath);
    if (status) {
      lastStatus = status;
      if (status.stage !== lastStageName) {
        console.log(`Portable export smoke stage: ${status.stage}`);
        lastStageName = status.stage;
      }
      if (status.stage === "success") {
        return status;
      }
      if (status.stage === "error") {
        throw new Error(`Portable export smoke failed inside the app: ${status.message ?? "Unknown smoke failure."}`);
      }
    }

    if (childExit && (!status || status.stage !== "success")) {
      throw new Error(`Portable export smoke app exited before reporting success (code=${childExit.code}, signal=${childExit.signal}).`);
    }
  }

  throw new Error(`Portable export smoke timed out waiting for success. Last stage: ${lastStatus?.stage ?? "none"}`);
}

async function waitForChildExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return true;
  }

  return await new Promise((resolve) => {
    const timeout = setTimeout(() => {
      child.off("exit", onExit);
      resolve(false);
    }, timeoutMs);
    const onExit = () => {
      clearTimeout(timeout);
      resolve(true);
    };
    child.once("exit", onExit);
  });
}

async function terminateLinuxSmokeProcess(child) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return;
  }

  const signalProcessGroup = (signal) => {
    if (!child.pid) return false;
    try {
      process.kill(-child.pid, signal);
      return true;
    } catch {
      return false;
    }
  };

  const signalChild = (signal) => {
    try {
      child.kill(signal);
      return true;
    } catch {
      return false;
    }
  };

  if (!signalProcessGroup("SIGTERM")) {
    signalChild("SIGTERM");
  }
  if (await waitForChildExit(child, 2500)) {
    return;
  }

  if (!signalProcessGroup("SIGKILL")) {
    signalChild("SIGKILL");
  }
  await waitForChildExit(child, 2500);
}

function parseStatusTrimMetrics(status) {
  if (
    Number.isFinite(status?.expectedDurationS) &&
    Number.isFinite(status?.trimStartS) &&
    Number.isFinite(status?.trimEndS)
  ) {
    return {
      expectedDurationS: status.expectedDurationS,
      trimStartS: status.trimStartS,
      trimEndS: status.trimEndS,
    };
  }

  const match = String(status?.message ?? "").match(/start at ([0-9]+(?:\.[0-9]+)?)s, end at ([0-9]+(?:\.[0-9]+)?)s/);
  if (!match) {
    throw new Error("Portable export smoke reported success without persisted trim metrics or a parsable interaction summary.");
  }

  const trimStartS = Number.parseFloat(match[1]);
  const trimEndS = Number.parseFloat(match[2]);
  return {
    trimStartS,
    trimEndS,
    expectedDurationS: trimEndS - trimStartS,
  };
}

async function runLinuxPortableExportSmoke({
  portableDir,
  timeoutSeconds = 60,
  trimEndSeconds = 1.25,
  inputWidth = 640,
  inputHeight = 360,
  inputRate = 30,
  inputVideoBitrateKbps = 900,
  outputFormat = "mp4",
  sizeLimitMb = 0,
} = {}) {
  outputFormat = normalizeSmokeOutputFormat(outputFormat);
  const { portableRoot, appPath, ffmpegPath, ffprobePath } = getLinuxPortablePaths(portableDir);
  for (const requiredPath of [appPath, ffmpegPath, ffprobePath]) {
    await assertRequiredFile(requiredPath);
  }

  const smokeRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-portable-export-smoke-"));
  const inputPath = path.resolve(smokeRoot, "smoke-input.webm");
  const outputPath = path.resolve(smokeRoot, `smoke-output.${outputFormat}`);
  const statusPath = path.resolve(smokeRoot, "smoke-status.json");
  let child = null;

  try {
    await runChecked(ffmpegPath, [
      "-y",
      "-hide_banner",
      "-loglevel",
      "error",
      "-f",
      "lavfi",
      "-i",
      `testsrc2=size=${inputWidth}x${inputHeight}:rate=${inputRate}`,
      "-f",
      "lavfi",
      "-i",
      "sine=frequency=880:sample_rate=48000",
      "-t",
      "2",
      "-c:v",
      "libvpx",
      "-deadline",
      "good",
      "-cpu-used",
      "8",
      "-b:v",
      `${inputVideoBitrateKbps}k`,
      "-c:a",
      "libvorbis",
      "-q:a",
      "4",
      inputPath,
    ]);

    const launch = buildLinuxLaunchCommand(appPath);
    child = spawn(launch.command, launch.args, {
      cwd: portableRoot,
      detached: true,
      env: {
        ...process.env,
        VFL_SMOKE_INPUT: inputPath,
        VFL_SMOKE_OUTPUT: outputPath,
        VFL_SMOKE_STATUS: statusPath,
        VFL_SMOKE_FORMAT: outputFormat,
        VFL_SMOKE_SIZE_LIMIT_MB: String(sizeLimitMb),
        VFL_SMOKE_TRIM_START_S: "0",
        VFL_SMOKE_TRIM_END_S: String(trimEndSeconds),
        VFL_SMOKE_SKIP_PREVIEW_INTERACTIONS: "1",
      },
      stdio: "ignore",
    });

    const status = await waitForLinuxSmokeSuccess(child, statusPath, timeoutSeconds);
    if (!(await exists(outputPath))) {
      throw new Error("Portable export smoke reported success but no output file was written.");
    }

    const stageHistory = Array.isArray(status.stageHistory) ? status.stageHistory : [];
    if (stageHistory.length === 0) {
      throw new Error("Portable export smoke reported success without any persisted stageHistory.");
    }

    const linuxRequiredSmokeStages = requiredSmokeStages.filter((stage) => stage !== "preview-ready");
    const missingStages = linuxRequiredSmokeStages.filter((stage) => !stageHistory.includes(stage));
    if (missingStages.length > 0) {
      throw new Error(`Portable export smoke missed required app stages: ${missingStages.join(", ")}. Saw: ${stageHistory.join(" -> ")}`);
    }

    const outputStats = await fs.stat(outputPath);
    if (outputStats.size <= 0) {
      throw new Error("Portable export smoke wrote an empty output file.");
    }

    const ffprobe = JSON.parse(await captureStdout(ffprobePath, [
      "-v",
      "quiet",
      "-print_format",
      "json",
      "-show_format",
      "-show_streams",
      outputPath,
    ]));
    const outputDurationS = Number.parseFloat(ffprobe?.format?.duration);
    if (!Number.isFinite(outputDurationS)) {
      throw new Error("Portable export smoke wrote an output file with an invalid duration.");
    }

    const { trimStartS, trimEndS, expectedDurationS } = parseStatusTrimMetrics(status);
    if (trimEndS <= trimStartS) {
      throw new Error(`Portable export smoke reported invalid trim metrics: start=${trimStartS} end=${trimEndS}`);
    }
    if (Math.abs(outputDurationS - expectedDurationS) > 0.18) {
      throw new Error(`Portable export smoke output duration mismatch. expected=${expectedDurationS.toFixed(3)}s actual=${outputDurationS.toFixed(3)}s`);
    }

    console.log(`Portable export smoke passed. output=${outputPath} size_bytes=${outputStats.size} duration_s=${outputDurationS.toFixed(3)} trim=${trimStartS.toFixed(3)}-${trimEndS.toFixed(3)} status=${statusPath} stages=${stageHistory.join(" -> ")}`);
  } finally {
    if (child) {
      await terminateLinuxSmokeProcess(child);
    }
    await fs.rm(smokeRoot, { recursive: true, force: true });
  }
}

async function runPortableExportSmoke({
  portableDir = getPortableOutputDir(),
  timeoutSeconds,
  trimEndSeconds,
  inputWidth,
  inputHeight,
  inputRate,
  inputVideoBitrateKbps,
  outputFormat,
  sizeLimitMb,
} = {}) {
  if (process.platform === "linux") {
    await runLinuxPortableExportSmoke({
      portableDir,
      timeoutSeconds: smokeNumber(timeoutSeconds, 60),
      trimEndSeconds: smokeNumber(trimEndSeconds, 1.25),
      inputWidth: smokeNumber(inputWidth, 640),
      inputHeight: smokeNumber(inputHeight, 360),
      inputRate: smokeNumber(inputRate, 30),
      inputVideoBitrateKbps: smokeNumber(inputVideoBitrateKbps, 900),
      outputFormat,
      sizeLimitMb: smokeNumber(sizeLimitMb, 0),
    });
    return;
  }

  if (process.platform !== "win32") {
    console.log("Skipping portable export smoke on unsupported non-Windows/non-Linux host.");
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
    ["-OutputFormat", outputFormat],
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

export { buildLinuxLaunchCommand, runLinuxPortableExportSmoke, runPortableExportSmoke };
