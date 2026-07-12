import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import url from "node:url";
import { spawn } from "node:child_process";

import { ffmpegSidecarResourceTarget, getPortableOutputDir } from "./ffmpegBundle.mjs";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));
const __filename = url.fileURLToPath(import.meta.url);
const WINDOWS_RETRYABLE_AUTOMATION_ERRORS = Object.freeze([
  "Smoke accessible activation did not open the save-recipe dialog.",
  "Portable export smoke could not foreground the app for real keyboard input.",
  "Portable export smoke could not restore foreground before real keyboard input.",
  "Portable export smoke could not establish stable WebView keyboard focus",
  "Portable export smoke lost stable WebView keyboard focus",
]);
const WINDOWS_SMOKE_MAX_ATTEMPTS = 3;
const WINDOWS_SMOKE_OUTPUT_TAIL_LIMIT = 64 * 1024;
const requiredSmokeStages = [
  "input-applied",
  "probe-ready",
  "workflow-recipe-ready",
  "workflow-recipe-saved",
  "workflow-queue-ready",
  "workflow-queue-complete",
  "workflow-ready",
  "preview-ready",
  "keyboard-trim-ready",
  "keyboard-trim-incremented",
  "keyboard-trim-complete",
  "keyboard-crop-ready",
  "keyboard-crop-complete",
  "keyboard-modal-ready",
  "keyboard-modal-open",
  "keyboard-complete",
  "accessibility-ready",
  "interaction-ready",
  "encoding",
  "success",
];
const supportedSmokeOutputFormats = new Set(["mp4", "webm", "mp3"]);
const codecFixtures = Object.freeze({
  "vp8-vorbis-webm": Object.freeze({
    extension: "webm",
    videoArgs: ["-c:v", "libvpx", "-deadline", "realtime", "-cpu-used", "8"],
    audioArgs: ["-c:a", "libvorbis", "-q:a", "4"],
  }),
  "vp9-opus-mkv": Object.freeze({
    extension: "mkv",
    videoArgs: ["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8"],
    audioArgs: ["-c:a", "libopus", "-b:a", "96k"],
  }),
  "h264-aac-mkv": Object.freeze({
    extension: "mkv",
    videoArgs: ["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"],
    audioArgs: ["-c:a", "aac", "-b:a", "128k"],
  }),
  "h264-opus-mkv": Object.freeze({
    extension: "mkv",
    videoArgs: ["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"],
    audioArgs: ["-c:a", "libopus", "-b:a", "96k"],
  }),
  "vp9-aac-mkv": Object.freeze({
    extension: "mkv",
    videoArgs: ["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8"],
    audioArgs: ["-c:a", "aac", "-b:a", "128k"],
  }),
  "h264-aac-mp4": Object.freeze({
    extension: "mp4",
    videoArgs: ["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"],
    audioArgs: ["-c:a", "aac", "-b:a", "128k"],
  }),
});

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

function appendOutputTail(current, chunk) {
  const combined = `${current}${String(chunk)}`;
  return combined.length > WINDOWS_SMOKE_OUTPUT_TAIL_LIMIT
    ? combined.slice(-WINDOWS_SMOKE_OUTPUT_TAIL_LIMIT)
    : combined;
}

function runWindowsPowerShellSmokeAttempt(args) {
  return new Promise((resolve, reject) => {
    const child = spawn("powershell.exe", args, { stdio: ["ignore", "pipe", "pipe"] });
    let outputTail = "";
    child.stdout.on("data", (chunk) => {
      process.stdout.write(chunk);
      outputTail = appendOutputTail(outputTail, chunk);
    });
    child.stderr.on("data", (chunk) => {
      process.stderr.write(chunk);
      outputTail = appendOutputTail(outputTail, chunk);
    });
    child.on("error", reject);
    child.on("close", (code, signal) => resolve({ code, signal, outputTail }));
  });
}

function resolveCodecFixture(name) {
  const fixture = codecFixtures[name];
  if (!fixture) {
    throw new Error(`Unsupported portable export smoke codec fixture: ${name}`);
  }
  return fixture;
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

async function probeMedia(ffprobePath, mediaPath) {
  return JSON.parse(await captureStdout(ffprobePath, [
    "-v",
    "quiet",
    "-print_format",
    "json",
    "-show_format",
    "-show_streams",
    mediaPath,
  ]));
}

async function streamPacketHashes(ffprobePath, mediaPath, selector) {
  const payload = JSON.parse(await captureStdout(ffprobePath, [
    "-v",
    "quiet",
    "-select_streams",
    selector,
    "-show_packets",
    "-show_entries",
    "packet=data_hash",
    "-show_data_hash",
    "sha256",
    "-print_format",
    "json",
    mediaPath,
  ]));
  const hashes = Array.isArray(payload?.packets)
    ? payload.packets.map((packet) => packet.data_hash).filter(Boolean)
    : [];
  if (hashes.length === 0) {
    throw new Error(`Portable export smoke found no ${selector} packet hashes in ${mediaPath}.`);
  }
  return hashes;
}

function selectedCodec(probe, type) {
  const stream = Array.isArray(probe?.streams)
    ? probe.streams.find((candidate) => candidate.codec_type === type)
    : null;
  return stream?.codec_name ?? null;
}

async function assertStreamAction({
  ffprobePath,
  inputPath,
  outputPath,
  selector,
  expectedAction,
}) {
  if (!expectedAction) return;
  if (expectedAction !== "copy" && expectedAction !== "encode") {
    throw new Error(`Unsupported expected stream action: ${expectedAction}`);
  }
  const [inputHashes, outputHashes] = await Promise.all([
    streamPacketHashes(ffprobePath, inputPath, selector),
    streamPacketHashes(ffprobePath, outputPath, selector),
  ]);
  const packetsMatch = JSON.stringify(inputHashes) === JSON.stringify(outputHashes);
  if (expectedAction === "copy" && !packetsMatch) {
    throw new Error(`Portable export smoke expected ${selector} packet copy, but packet hashes changed.`);
  }
  if (expectedAction === "encode" && packetsMatch) {
    throw new Error(`Portable export smoke expected ${selector} re-encode, but packet hashes were unchanged.`);
  }
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
  codecFixture = "vp8-vorbis-webm",
  outputFormat = "mp4",
  sizeLimitMb = 0,
  useFullDuration = false,
  expectedVideoCodec,
  expectedAudioCodec,
  expectedVideoAction,
  expectedAudioAction,
  resizeMode,
  resizeMaxEdgePx,
  resizeWidthPx,
  resizeHeightPx,
} = {}) {
  outputFormat = normalizeSmokeOutputFormat(outputFormat);
  const fixture = resolveCodecFixture(codecFixture);
  const { portableRoot, appPath, ffmpegPath, ffprobePath } = getLinuxPortablePaths(portableDir);
  for (const requiredPath of [appPath, ffmpegPath, ffprobePath]) {
    await assertRequiredFile(requiredPath);
  }

  const smokeRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-portable-export-smoke-"));
  const inputPath = path.resolve(smokeRoot, `smoke-input.${fixture.extension}`);
  const outputPath = path.resolve(smokeRoot, `smoke-output.${outputFormat}`);
  const statusPath = path.resolve(smokeRoot, "smoke-status.json");
  const xdgDataHome = path.resolve(smokeRoot, "xdg-data");
  const xdgConfigHome = path.resolve(smokeRoot, "xdg-config");
  const xdgCacheHome = path.resolve(smokeRoot, "xdg-cache");
  const stdoutPath = path.resolve(smokeRoot, "app.stdout.log");
  const stderrPath = path.resolve(smokeRoot, "app.stderr.log");
  let child = null;
  let stdoutFileHandle = null;
  let stderrFileHandle = null;
  let succeeded = false;

  try {
    await Promise.all([
      fs.mkdir(xdgDataHome, { recursive: true }),
      fs.mkdir(xdgConfigHome, { recursive: true }),
      fs.mkdir(xdgCacheHome, { recursive: true }),
    ]);
    stdoutFileHandle = await fs.open(stdoutPath, "w");
    stderrFileHandle = await fs.open(stderrPath, "w");

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
      ...fixture.videoArgs,
      "-b:v",
      `${inputVideoBitrateKbps}k`,
      ...fixture.audioArgs,
      inputPath,
    ]);
    const inputProbe = await probeMedia(ffprobePath, inputPath);

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
        ...(useFullDuration ? {} : { VFL_SMOKE_TRIM_END_S: String(trimEndSeconds) }),
        VFL_SMOKE_SKIP_PREVIEW_INTERACTIONS: "1",
        VFL_SMOKE_WORKFLOW_QUEUE: "1",
        XDG_DATA_HOME: xdgDataHome,
        XDG_CONFIG_HOME: xdgConfigHome,
        XDG_CACHE_HOME: xdgCacheHome,
        ...(resizeMode ? { VFL_SMOKE_RESIZE_MODE: String(resizeMode) } : {}),
        ...(resizeMaxEdgePx === undefined || resizeMaxEdgePx === null ? {} : { VFL_SMOKE_RESIZE_MAX_EDGE_PX: String(resizeMaxEdgePx) }),
        ...(resizeWidthPx === undefined || resizeWidthPx === null ? {} : { VFL_SMOKE_RESIZE_WIDTH_PX: String(resizeWidthPx) }),
        ...(resizeHeightPx === undefined || resizeHeightPx === null ? {} : { VFL_SMOKE_RESIZE_HEIGHT_PX: String(resizeHeightPx) }),
      },
      stdio: ["ignore", stdoutFileHandle.fd, stderrFileHandle.fd],
    });

    const status = await waitForLinuxSmokeSuccess(child, statusPath, timeoutSeconds);
    if (!(await exists(outputPath))) {
      throw new Error("Portable export smoke reported success but no output file was written.");
    }

    const stageHistory = Array.isArray(status.stageHistory) ? status.stageHistory : [];
    if (stageHistory.length === 0) {
      throw new Error("Portable export smoke reported success without any persisted stageHistory.");
    }

    const linuxRequiredSmokeStages = requiredSmokeStages.filter(
      (stage) => stage !== "preview-ready" && stage !== "accessibility-ready" && !stage.startsWith("keyboard-"),
    );
    const missingStages = linuxRequiredSmokeStages.filter((stage) => !stageHistory.includes(stage));
    if (missingStages.length > 0) {
      throw new Error(`Portable export smoke missed required app stages: ${missingStages.join(", ")}. Saw: ${stageHistory.join(" -> ")}`);
    }
    const observedRequiredStages = stageHistory.filter((stage) => linuxRequiredSmokeStages.includes(stage));
    if (JSON.stringify(observedRequiredStages) !== JSON.stringify(linuxRequiredSmokeStages)) {
      throw new Error(`Portable export smoke required app stages were out of order. Saw: ${stageHistory.join(" -> ")}`);
    }

    const outputStats = await fs.stat(outputPath);
    if (outputStats.size <= 0) {
      throw new Error("Portable export smoke wrote an empty output file.");
    }
    const inputMode = (await fs.stat(inputPath)).mode & 0o777;
    const outputMode = outputStats.mode & 0o777;
    if (outputMode !== inputMode) {
      throw new Error(`Portable export smoke output permissions mismatch. expected=${inputMode.toString(8)} actual=${outputMode.toString(8)}`);
    }

    const ffprobe = await probeMedia(ffprobePath, outputPath);
    const outputDurationS = Number.parseFloat(ffprobe?.format?.duration);
    if (!Number.isFinite(outputDurationS)) {
      throw new Error("Portable export smoke wrote an output file with an invalid duration.");
    }
    if (resizeMode === "custom") {
      const videoStream = Array.isArray(ffprobe?.streams) ? ffprobe.streams.find((stream) => stream.codec_type === "video") : null;
      const expectedWidth = Math.max(2, Math.floor(Number(resizeWidthPx) / 2) * 2);
      const expectedHeight = Math.max(2, Math.floor(Number(resizeHeightPx) / 2) * 2);
      if (!videoStream || videoStream.width !== expectedWidth || videoStream.height !== expectedHeight) {
        throw new Error(`Portable export smoke output dimensions mismatch. expected=${expectedWidth}x${expectedHeight} actual=${videoStream?.width ?? "none"}x${videoStream?.height ?? "none"}`);
      }
    }

    const actualVideoCodec = selectedCodec(ffprobe, "video");
    const actualAudioCodec = selectedCodec(ffprobe, "audio");
    if (expectedVideoCodec && actualVideoCodec !== expectedVideoCodec) {
      throw new Error(`Portable export smoke video codec mismatch. expected=${expectedVideoCodec} actual=${actualVideoCodec ?? "none"}`);
    }
    if (expectedAudioCodec && actualAudioCodec !== expectedAudioCodec) {
      throw new Error(`Portable export smoke audio codec mismatch. expected=${expectedAudioCodec} actual=${actualAudioCodec ?? "none"}`);
    }
    await assertStreamAction({
      ffprobePath,
      inputPath,
      outputPath,
      selector: "v:0",
      expectedAction: expectedVideoAction,
    });
    await assertStreamAction({
      ffprobePath,
      inputPath,
      outputPath,
      selector: "a:0",
      expectedAction: expectedAudioAction,
    });

    const { trimStartS, trimEndS, expectedDurationS } = parseStatusTrimMetrics(status);
    if (trimEndS <= trimStartS) {
      throw new Error(`Portable export smoke reported invalid trim metrics: start=${trimStartS} end=${trimEndS}`);
    }
    if (Math.abs(outputDurationS - expectedDurationS) > 0.18) {
      throw new Error(`Portable export smoke output duration mismatch. expected=${expectedDurationS.toFixed(3)}s actual=${outputDurationS.toFixed(3)}s`);
    }

    console.log(`Portable export smoke passed. fixture=${codecFixture} input=${selectedCodec(inputProbe, "video")}/${selectedCodec(inputProbe, "audio")} output=${actualVideoCodec}/${actualAudioCodec} output_path=${outputPath} size_bytes=${outputStats.size} duration_s=${outputDurationS.toFixed(3)} trim=${trimStartS.toFixed(3)}-${trimEndS.toFixed(3)} status=${statusPath} stages=${stageHistory.join(" -> ")}`);
    succeeded = true;
  } finally {
    try {
      if (child) {
        await terminateLinuxSmokeProcess(child);
      }
    } finally {
      await Promise.allSettled([
        stdoutFileHandle?.close(),
        stderrFileHandle?.close(),
      ]);
      if (succeeded) {
        await fs.rm(smokeRoot, { recursive: true, force: true });
      } else {
        console.error(`Portable export smoke evidence retained at ${smokeRoot}`);
      }
    }
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
  resizeMode,
  resizeMaxEdgePx,
  resizeWidthPx,
  resizeHeightPx,
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
      resizeMode,
      resizeMaxEdgePx,
      resizeWidthPx,
      resizeHeightPx,
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
    ["-ResizeMode", resizeMode],
    ["-ResizeMaxEdgePx", resizeMaxEdgePx],
    ["-ResizeWidthPx", resizeWidthPx],
    ["-ResizeHeightPx", resizeHeightPx],
  ];

  for (const [flag, value] of optionalArgs) {
    if (value === undefined || value === null) {
      continue;
    }
    args.push(flag, String(value));
  }

  for (let attempt = 1; attempt <= WINDOWS_SMOKE_MAX_ATTEMPTS; attempt += 1) {
    const result = await runWindowsPowerShellSmokeAttempt(args);
    if (result.code === 0) return;
    const retryableAutomationMiss = WINDOWS_RETRYABLE_AUTOMATION_ERRORS.some(
      (message) => result.outputTail.includes(message),
    );
    if (retryableAutomationMiss && attempt < WINDOWS_SMOKE_MAX_ATTEMPTS) {
      console.warn(
        `Portable export smoke missed native UI automation on attempt ${attempt}; retrying the complete smoke in a fresh process and WebView profile.`,
      );
      continue;
    }
    const exitDescription = result.code === null ? `signal ${result.signal ?? "unknown"}` : `code ${result.code}`;
    throw new Error(`Portable export smoke exited with ${exitDescription}`);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === __filename) {
  await runPortableExportSmoke();
}

export { buildLinuxLaunchCommand, runLinuxPortableExportSmoke, runPortableExportSmoke };
