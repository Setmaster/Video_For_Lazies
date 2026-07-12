import path from "node:path";
import { spawn } from "node:child_process";

import {
  FFMPEG_CAPABILITY_CONTRACT_FILE_NAME,
  assertCapabilityContractCopy,
  verifyFfmpegCapabilityContract,
} from "./ffmpegCapabilities.mjs";
import { ffmpegSidecarResourceTarget } from "./ffmpegBundle.mjs";
import { validatePayloadManifest } from "./updateManifests.mjs";
import { getProjectVersion } from "./versioning.mjs";

function normalizeWindowsPathSearchText(value) {
  let normalized = String(value ?? "").toLowerCase();
  while (normalized.includes("\\\\")) normalized = normalized.replaceAll("\\\\", "\\");
  normalized = normalized.replaceAll("/", "\\");
  normalized = normalized.replaceAll("\\?\\unc\\", "\\");
  normalized = normalized.replaceAll("\\?\\", "");
  return normalized;
}

function windowsPathComponents(value) {
  const normalized = normalizeWindowsPathSearchText(value);
  return normalized
    .split("\\")
    .map((component) => component.trim())
    .filter((component) => component && !/^[a-z]:$/i.test(component));
}

function escapeRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function windowsShortAliasPattern(component) {
  const lastDot = component.lastIndexOf(".");
  const hasExtension = lastDot > 0 && lastDot < component.length - 1;
  const stem = hasExtension ? component.slice(0, lastDot) : component;
  const extension = hasExtension ? component.slice(lastDot + 1) : "";
  const shortStem = stem
    .replace(/[ .]/g, "")
    .replace(/["*/:<>?\\|+,;=\[\]]/g, "")
    .slice(0, 6);
  if (!shortStem) return null;
  const shortExtension = extension
    .replace(/[ .]/g, "")
    .replace(/["*/:<>?\\|+,;=\[\]]/g, "")
    .slice(0, 3);
  return `${escapeRegExp(shortStem)}~\\d+${shortExtension ? `\\.${escapeRegExp(shortExtension)}` : ""}`;
}

function windowsProtectedComponentPattern(component) {
  const exact = escapeRegExp(component);
  const alias = windowsShortAliasPattern(component);
  return alias ? `(?:${exact}|${alias})` : exact;
}

function containsProtectedWindowsPath(rawValue, pathValue) {
  const raw = normalizeWindowsPathSearchText(rawValue);
  const token = normalizeWindowsPathSearchText(pathValue);
  if (!token) return false;
  if (raw.includes(token)) return true;

  const components = windowsPathComponents(pathValue);
  if (components.length < 3) return false;
  for (let start = 0; start <= components.length - 3; start += 1) {
    const sequence = components.slice(start);
    const pattern = sequence.map(windowsProtectedComponentPattern).join("\\\\");
    if (new RegExp(`(?:^|\\\\)${pattern}(?=$|\\\\|[\\x00-\\x20<>:"|?*])`, "i").test(raw)) {
      return true;
    }
  }
  return false;
}

function assertTextExcludesPathValues(rawValue, pathValues, label, { platform = process.platform } = {}) {
  const raw = String(rawValue ?? "");
  for (const value of pathValues) {
    const token = String(value ?? "");
    if (!token) continue;
    if (platform === "win32") {
      if (containsProtectedWindowsPath(raw, token)) {
        throw new Error(`${label} exposed a protected filesystem path or basename.`);
      }
      continue;
    }
    const escaped = JSON.stringify(token).slice(1, -1);
    if (raw.includes(token) || (escaped && raw.includes(escaped))) {
      throw new Error(`${label} exposed a protected filesystem path or basename.`);
    }
  }
}

async function runBounded(command, args, {
  timeoutMs = 10_000,
  maxCaptureBytes = 512 * 1024,
} = {}) {
  return await new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    const stdout = [];
    const stderr = [];
    let stdoutBytes = 0;
    let stderrBytes = 0;
    let captureOverflow = false;
    const retain = (target, chunk, currentBytes) => {
      const bytes = Buffer.from(chunk);
      if (currentBytes + bytes.length > maxCaptureBytes) {
        captureOverflow = true;
        child.kill("SIGKILL");
        return currentBytes;
      }
      target.push(bytes);
      return currentBytes + bytes.length;
    };
    child.stdout.on("data", (chunk) => { stdoutBytes = retain(stdout, chunk, stdoutBytes); });
    child.stderr.on("data", (chunk) => { stderrBytes = retain(stderr, chunk, stderrBytes); });
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGKILL");
    }, timeoutMs);
    let settled = false;
    const finish = (action) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      action();
    };
    child.on("error", (error) => finish(() => reject(error)));
    child.on("close", (code, signal) => finish(() => {
      const stdoutBuffer = Buffer.concat(stdout);
      const stderrBuffer = Buffer.concat(stderr);
      if (code === 0 && !timedOut && !captureOverflow) {
        resolve({ stdout: stdoutBuffer, stderr: stderrBuffer });
        return;
      }
      const detail = stderrBuffer.toString("utf8").trim();
      const message = captureOverflow
        ? `${path.basename(command)} exceeded the ${maxCaptureBytes} byte capture limit.`
        : timedOut
          ? `${path.basename(command)} timed out after ${timeoutMs} ms.`
          : `${path.basename(command)} exited with code ${code} signal ${signal ?? "none"}${detail ? `\n${detail}` : ""}`;
      reject(new Error(message));
    }));
  });
}

function waitForChildExit(child, timeoutMs) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return Promise.resolve(true);
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      child.off("exit", onExit);
      resolve(false);
    }, timeoutMs);
    const onExit = () => {
      clearTimeout(timer);
      resolve(true);
    };
    child.once("exit", onExit);
  });
}

async function terminateSmokeChild(child, platform) {
  if (!child) return;
  const childIsRunning = () => child.exitCode === null && child.signalCode === null;
  if (platform === "win32" && child.pid) {
    let taskkillError = null;
    try {
      await runBounded("taskkill.exe", ["/PID", String(child.pid), "/T", "/F"]);
      if (!(await waitForChildExit(child, 5_000))) {
        throw new Error("Portable smoke taskkill completed but the packaged app process was not reaped.");
      }
      return;
    } catch (error) {
      taskkillError = error;
    }
    if (!childIsRunning()) return;
    try {
      child.kill("SIGKILL");
    } catch {
      // Preserve the taskkill failure below.
    }
    await waitForChildExit(child, 2_500);
    throw new Error(`Portable smoke could not terminate the Windows app process tree: ${taskkillError.message}`);
  }
  if (platform === "linux" && child.pid) {
    const groupExists = () => {
      try {
        process.kill(-child.pid, 0);
        return true;
      } catch (error) {
        return error?.code === "EPERM";
      }
    };
    const waitForGroupExit = async (timeoutMs) => {
      const deadline = Date.now() + timeoutMs;
      while (Date.now() < deadline) {
        if (!groupExists()) return true;
        await new Promise((resolve) => setTimeout(resolve, 100));
      }
      return !groupExists();
    };
    if (groupExists()) {
      try {
        process.kill(-child.pid, "SIGTERM");
      } catch {
        // The group may have exited between the existence check and signal.
      }
      if (await waitForGroupExit(2_500)) {
        if (!(await waitForChildExit(child, 2_500))) {
          throw new Error("Portable smoke Linux process group exited but its launcher child was not reaped.");
        }
        return;
      }
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch {
        // The group may have exited between the wait and signal.
      }
      if (!(await waitForGroupExit(2_500))) {
        throw new Error("Portable smoke Linux process group remained after SIGKILL.");
      }
      if (!(await waitForChildExit(child, 2_500))) {
        throw new Error("Portable smoke Linux launcher child remained after process-group SIGKILL.");
      }
      return;
    }
    if (!childIsRunning()) return;
  }
  const signal = (kind) => {
    try {
      child.kill(kind);
      return true;
    } catch {
      return false;
    }
  };
  signal("SIGTERM");
  if (await waitForChildExit(child, 2_500)) return;
  signal("SIGKILL");
  if (!(await waitForChildExit(child, 2_500))) {
    throw new Error("Portable smoke child remained after SIGKILL.");
  }
}

async function shutdownSmokeProcessAndLogs(child, platform, logHandles, { strict = true } = {}) {
  const failures = [];
  try {
    await terminateSmokeChild(child, platform);
  } catch (error) {
    failures.push(error);
  }
  const syncResults = await Promise.allSettled(logHandles.map((handle) => handle.sync()));
  failures.push(...syncResults.filter((result) => result.status === "rejected").map((result) => result.reason));
  const closeResults = await Promise.allSettled(logHandles.map((handle) => handle.close()));
  failures.push(...closeResults.filter((result) => result.status === "rejected").map((result) => result.reason));
  if (strict && failures.length > 0) throw failures[0];
}

async function verifyPortablePreflight(portableDir, {
  platform,
  ffmpegPath,
  appPath,
  ffprobePath,
} = {}) {
  const version = await getProjectVersion();
  const manifest = await validatePayloadManifest({ portableDir, version, platform });
  const capabilityPath = path.resolve(
    portableDir,
    ffmpegSidecarResourceTarget,
    FFMPEG_CAPABILITY_CONTRACT_FILE_NAME,
  );
  const contract = await assertCapabilityContractCopy(capabilityPath);
  await verifyFfmpegCapabilityContract({
    ffmpegPath,
    contract,
    label: `Portable ${platform} FFmpeg`,
  });
  const expectedPaths = [appPath, ffmpegPath, ffprobePath, capabilityPath]
    .map((absolutePath) => path.relative(portableDir, absolutePath).split(path.sep).join("/"));
  const manifestPaths = new Set(manifest.files.map((entry) => entry.path));
  const missing = expectedPaths.filter((relativePath) => !manifestPaths.has(relativePath));
  if (missing.length > 0) {
    throw new Error(`Portable payload manifest omitted runtime file(s): ${missing.join(", ")}`);
  }
  return Object.freeze({
    version,
    capabilitySchemaVersion: contract.schemaVersion,
    payloadFileCount: manifest.files.length,
  });
}

export {
  assertTextExcludesPathValues,
  shutdownSmokeProcessAndLogs,
  verifyPortablePreflight,
};
