#!/usr/bin/env node

import assert from "node:assert/strict";
import crypto from "node:crypto";
import { createReadStream } from "node:fs";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const TAG_COMMIT = "4962ff41765dfba0a3e4efb7973e2563265c5b74";
const UPDATER_BLOB = "db94da8c4d28e05a9ab5df347ee65622af5b6275";
const UPDATER_SHA256 = "1ea32ac2d5cb4a6d73c6e3ba834e4d3dd49a8e327dc89689e0a23630230b9e8f";
const PLAN_KEYS = [
  "schema",
  "updateId",
  "fromVersion",
  "toVersion",
  "target",
  "installDir",
  "stageDir",
  "backupDir",
  "parentPid",
  "executableName",
  "helperName",
  "relaunchViaShell",
];
const STATE_DIR = ".vfl-updates";
const PAYLOAD_MANIFEST = "VFL_PAYLOAD_MANIFEST.json";
const MAIN_NAME = "video_for_lazies";
const HELPER_NAME = "vfl-update-helper";
const FROM_VERSION = "1.9.1";
const TO_VERSION = "1.10.0";
const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "../../..");
const templatePath = path.join(here, "v1.9.1-apply-plan.template.json");
const activeHelperPids = new Set();

function parseArgs(argv) {
  const result = {};
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index];
    const value = argv[index + 1];
    if (!key?.startsWith("--") || value === undefined) throw new Error(`Invalid argument: ${key}`);
    result[key.slice(2)] = value;
  }
  return result;
}

function sha256(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

async function sha256File(filePath) {
  const hash = crypto.createHash("sha256");
  for await (const chunk of createReadStream(filePath)) hash.update(chunk);
  return hash.digest("hex");
}

function git(...args) {
  const result = spawnSync("git", args, { cwd: repoRoot, encoding: "utf8" });
  if (result.status !== 0) throw new Error(result.stderr || `git ${args.join(" ")} failed`);
  return result.stdout.trim();
}

async function verifyProvenance(template) {
  assert.equal(git("rev-parse", "v1.9.1^{commit}"), TAG_COMMIT);
  assert.equal(git("rev-parse", "v1.9.1:app/src-tauri/src/updater.rs"), UPDATER_BLOB);
  const taggedSource = spawnSync("git", ["show", "v1.9.1:app/src-tauri/src/updater.rs"], {
    cwd: repoRoot,
    encoding: null,
    maxBuffer: 8 * 1024 * 1024,
  });
  assert.equal(taggedSource.status, 0, taggedSource.stderr?.toString());
  assert.equal(sha256(taggedSource.stdout), UPDATER_SHA256);
  assert.deepEqual(Object.keys(template), PLAN_KEYS);
  assert.equal(template.fromVersion, FROM_VERSION);
  assert.equal(template.toVersion, TO_VERSION);
  assert.equal(template.relaunchViaShell, false);
  assert.ok(!Object.hasOwn(template, "expectedPayloadManifestSha256"));
}

async function writeFile(root, relativePath, bytes, mode) {
  const filePath = path.join(root, relativePath);
  await fs.mkdir(path.dirname(filePath), { recursive: true });
  await fs.writeFile(filePath, bytes, { mode });
  await fs.chmod(filePath, mode);
}

function appScript(label, requiresNewDependency) {
  return Buffer.from(`#!/bin/sh
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
${requiresNewDependency ? 'test -f "$root/new-runtime.dep" || exit 23' : ":"}
marker=${"${1:-${VFL_TEST_APP_LAUNCH_MARKER:-}}"}
if [ -n "$marker" ]; then printf ${label} > "$marker"; fi
exit 0
`);
}

async function buildPayload(root, version, helperPath, label) {
  const isNew = label === "new";
  const files = new Map([
    [MAIN_NAME, { bytes: appScript(label, isNew), mode: 0o755 }],
    ["README.md", { bytes: Buffer.from(label), mode: 0o444 }],
    ["LICENSE.txt", { bytes: Buffer.from(label), mode: 0o644 }],
    ["THIRD_PARTY_NOTICES.md", { bytes: Buffer.from(label), mode: 0o644 }],
    ["SOURCE.md", { bytes: Buffer.from(label), mode: 0o644 }],
    ["FFMPEG_BUNDLING.md", { bytes: Buffer.from(label), mode: 0o644 }],
    ["ffmpeg-sidecar/LICENSE.txt", { bytes: Buffer.from(label), mode: 0o644 }],
    ["ffmpeg-sidecar/FFMPEG_BUNDLE_NOTICES.txt", { bytes: Buffer.from(label), mode: 0o644 }],
    ["Video_For_Lazies.png", { bytes: Buffer.from(label), mode: 0o644 }],
    ["Video_For_Lazies.desktop", { bytes: Buffer.from(label), mode: 0o644 }],
  ]);
  if (isNew) files.set("new-runtime.dep", { bytes: Buffer.from("required"), mode: 0o644 });
  else files.set("old-only.dep", { bytes: Buffer.from("old"), mode: 0o644 });

  for (const [relativePath, entry] of files) {
    await writeFile(root, relativePath, entry.bytes, entry.mode);
  }
  const helperTarget = path.join(root, HELPER_NAME);
  if (isNew) {
    try {
      await fs.link(helperPath, helperTarget);
    } catch {
      await fs.copyFile(helperPath, helperTarget);
    }
    await fs.chmod(helperTarget, 0o755);
    const helperStat = await fs.stat(helperTarget);
    files.set(HELPER_NAME, {
      sha256: await sha256File(helperTarget),
      size: helperStat.size,
      mode: 0o755,
    });
  } else {
    const oldHelper = Buffer.from("#!/bin/sh\nexit 0\n");
    await writeFile(root, HELPER_NAME, oldHelper, 0o755);
    files.set(HELPER_NAME, { bytes: oldHelper, mode: 0o755 });
  }

  const manifestFiles = [...files.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([relativePath, entry]) => ({
      path: relativePath,
      sha256: entry.sha256 ?? sha256(entry.bytes),
      sizeBytes: entry.size ?? entry.bytes.length,
      mode: entry.mode,
      kind: "file",
    }));
  const manifest = {
    schema: "com.setmaster.video-for-lazies.payload-manifest.v1",
    appId: "com.setmaster.video-for-lazies",
    version,
    target: "linux-x64",
    rootDir: "Video_For_Lazies",
    files: manifestFiles,
  };
  const manifestBytes = Buffer.from(JSON.stringify(manifest));
  await writeFile(root, PAYLOAD_MANIFEST, manifestBytes, 0o644);
  return sha256(manifestBytes);
}

async function createInstall(root, helperPath) {
  const installDir = path.join(root, "Video_For_Lazies");
  await fs.mkdir(installDir, { recursive: true });
  await buildPayload(installDir, FROM_VERSION, helperPath, "old");
  await writeFile(installDir, "user-note.txt", Buffer.from("keep"), 0o644);
  return installDir;
}

async function createTransaction(root, installDir, helperPath, label) {
  const updateId = `1.10.0-${label}`;
  const stageDir = path.join(
    installDir,
    STATE_DIR,
    "staged",
    updateId,
    "extract",
    "Video_For_Lazies",
  );
  const backupDir = path.join(installDir, STATE_DIR, "backups", updateId);
  await fs.mkdir(stageDir, { recursive: true });
  await fs.mkdir(backupDir, { recursive: true });
  const payloadDigest = await buildPayload(stageDir, TO_VERSION, helperPath, "new");
  const planPath = path.join(installDir, STATE_DIR, "plans", `${updateId}.json`);
  const manifestPath = path.join(root, `${updateId}-verified-manifest.json`);
  const releaseUrl = `https://github.com/Setmaster/Video_For_Lazies/releases/tag/v${TO_VERSION}`;
  const updateManifest = {
    schema: "com.setmaster.video-for-lazies.update-manifest.v1",
    appId: "com.setmaster.video-for-lazies",
    channel: "stable",
    version: TO_VERSION,
    releaseTag: `v${TO_VERSION}`,
    releaseUrl,
    publishedAt: "2026-07-11T00:00:00Z",
    minUpdaterProtocol: 1,
    notes: { title: "Compatibility fixture", summary: "Compatibility fixture", url: releaseUrl },
    artifacts: {
      "linux-x64": {
        fileName: "Video_For_Lazies-v1.10.0-linux-x64.zip",
        url: "https://github.com/Setmaster/Video_For_Lazies/releases/download/v1.10.0/Video_For_Lazies-v1.10.0-linux-x64.zip",
        sha256: "a".repeat(64),
        sizeBytes: 1,
        rootDir: "Video_For_Lazies",
        payloadManifest: {
          path: "Video_For_Lazies/VFL_PAYLOAD_MANIFEST.json",
          sha256: payloadDigest,
        },
      },
    },
  };
  const manifestBytes = Buffer.from(JSON.stringify(updateManifest));
  await fs.writeFile(manifestPath, manifestBytes);
  return {
    updateId,
    installDir,
    stageDir,
    backupDir,
    planPath,
    payloadDigest,
    manifestPath,
    manifestDigest: sha256(manifestBytes),
  };
}

async function waitForFile(filePath, timeoutMs = 180_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      await fs.access(filePath);
      return;
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  throw new Error(`Timed out waiting for ${filePath}`);
}

async function waitForProcessExit(pid, timeoutMs = 180_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      process.kill(pid, 0);
    } catch (error) {
      if (error.code === "ESRCH") return;
      throw error;
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  throw new Error(`Process ${pid} did not exit`);
}

async function runApp(installDir, markerPath, expected) {
  await fs.rm(markerPath, { force: true });
  const result = spawnSync(path.join(installDir, MAIN_NAME), [markerPath], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(await fs.readFile(markerPath, "utf8"), expected);
}

async function launchFromStableWriter(template, tx, helperPath, root, options = {}) {
  const configPath = path.join(root, `${tx.updateId}-${crypto.randomUUID()}-writer.json`);
  const pidPath = `${configPath}.pid`;
  const outcomePath = `${configPath}.outcome`;
  const pauseMarker = `${configPath}.pause`;
  const launchMarker = `${configPath}.launch`;
  const logPath = `${configPath}.log`;
  const config = {
    template,
    tx,
    helperPath,
    pidPath,
    outcomePath,
    pauseMarker,
    launchMarker,
    logPath,
    pausePhase: options.pausePhase ?? "",
    pauseUpdateId: options.pauseBinding?.updateId ?? tx.updateId,
    pausePayloadDigest: options.pauseBinding?.payloadDigest ?? tx.payloadDigest,
  };
  await fs.writeFile(configPath, JSON.stringify(config));
  const writer = spawn(process.execPath, [fileURLToPath(import.meta.url), "--writer", configPath], {
    cwd: repoRoot,
    stdio: ["ignore", "inherit", "inherit"],
  });
  await waitForFile(pidPath);
  const helperPid = Number(await fs.readFile(pidPath, "utf8"));
  assert.ok(Number.isInteger(helperPid) && helperPid > 0);
  activeHelperPids.add(helperPid);
  const writerExit = await new Promise((resolve, reject) => {
    writer.once("error", reject);
    writer.once("exit", (code) => resolve(code));
  });
  assert.equal(writerExit, 0);
  return { helperPid, outcomePath, pauseMarker, launchMarker, logPath };
}

async function stableWriterMode(configPath) {
  const config = JSON.parse(await fs.readFile(configPath, "utf8"));
  const plan = structuredClone(config.template);
  plan.updateId = config.tx.updateId;
  plan.installDir = config.tx.installDir;
  plan.stageDir = config.tx.stageDir;
  plan.backupDir = config.tx.backupDir;
  plan.parentPid = process.pid;
  assert.deepEqual(Object.keys(plan), PLAN_KEYS);
  assert.ok(!Object.hasOwn(plan, "expectedPayloadManifestSha256"));
  await fs.mkdir(path.dirname(config.tx.planPath), { recursive: true });
  await fs.writeFile(config.tx.planPath, JSON.stringify(plan, null, 2));

  const logHandle = await fs.open(config.logPath, "w");
  const env = {
    ...process.env,
    VFL_UPDATER_TEST_MODE: "1",
    VFL_UPDATER_TEST_MANIFEST_PATH: config.tx.manifestPath,
    VFL_UPDATER_TEST_MANIFEST_SHA256: config.tx.manifestDigest,
    VFL_UPDATER_TEST_EXIT_MARKER: config.outcomePath,
    VFL_TEST_APP_LAUNCH_MARKER: config.launchMarker,
  };
  if (config.pausePhase) {
    env.VFL_UPDATER_TEST_PAUSE_PHASE = config.pausePhase;
    env.VFL_UPDATER_TEST_UPDATE_ID = config.pauseUpdateId;
    env.VFL_UPDATER_TEST_PAYLOAD_DIGEST = config.pausePayloadDigest;
    env.VFL_UPDATER_TEST_PAUSE_MARKER = config.pauseMarker;
  }
  const helper = spawn(config.helperPath, ["apply", "--plan", config.tx.planPath], {
    cwd: config.tx.installDir,
    env,
    detached: false,
    stdio: ["ignore", logHandle.fd, logHandle.fd],
  });
  await fs.writeFile(config.pidPath, String(helper.pid));
  helper.unref();
  await new Promise((resolve) => setTimeout(resolve, 350));
  await logHandle.close();
}

async function assertHelperSuccess(run) {
  await waitForFile(run.outcomePath);
  assert.equal(await fs.readFile(run.outcomePath, "utf8"), "0", await fs.readFile(run.logPath, "utf8"));
  await waitForProcessExit(run.helperPid);
  activeHelperPids.delete(run.helperPid);
}

async function killPausedHelper(run) {
  await waitForPause(run);
  process.kill(run.helperPid, "SIGKILL");
  await waitForProcessExit(run.helperPid);
  activeHelperPids.delete(run.helperPid);
  await assert.rejects(fs.access(run.outcomePath));
}

async function waitForPause(run, timeoutMs = 180_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      await fs.access(run.pauseMarker);
      return;
    } catch {}
    try {
      await fs.access(run.outcomePath);
      throw new Error(
        `Helper exited before pause (outcome ${await fs.readFile(run.outcomePath, "utf8")}): ${await fs.readFile(run.logPath, "utf8")}`,
      );
    } catch (error) {
      if (!error.message.startsWith("ENOENT")) throw error;
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  throw new Error(`Timed out waiting for helper pause ${run.pauseMarker}`);
}

async function readJournal(installDir) {
  return JSON.parse(await fs.readFile(path.join(installDir, STATE_DIR, "update-state.json"), "utf8"));
}

async function assertNoLocks(installDir) {
  for (const name of ["apply.lock", "staging.lock", "recovery.lock"]) {
    await assert.rejects(fs.access(path.join(installDir, STATE_DIR, name)));
  }
}

async function normalScenario(template, helperPath, root) {
  const scenarioRoot = path.join(root, "normal");
  const installDir = await createInstall(scenarioRoot, helperPath);
  const tx = await createTransaction(scenarioRoot, installDir, helperPath, "normal");
  const run = await launchFromStableWriter(template, tx, helperPath, scenarioRoot);
  await assertHelperSuccess(run);
  assert.equal((await readJournal(installDir)).phase, "awaitingStartup");
  await runApp(installDir, path.join(scenarioRoot, "normal-app"), "new");
  await assertNoLocks(installDir);
}

async function applyPrepublicationScenario(template, helperPath, root) {
  const scenarioRoot = path.join(root, "apply-prepublish");
  const installDir = await createInstall(scenarioRoot, helperPath);
  const oldMain = await fs.readFile(path.join(installDir, MAIN_NAME));
  const first = await createTransaction(scenarioRoot, installDir, helperPath, "apply-first");
  const interrupted = await launchFromStableWriter(template, first, helperPath, scenarioRoot, {
    pausePhase: "applyMain",
  });
  await waitForPause(interrupted);
  assert.deepEqual(await fs.readFile(path.join(installDir, MAIN_NAME)), oldMain);
  await runApp(installDir, path.join(scenarioRoot, "old-before-apply-kill"), "old");
  await killPausedHelper(interrupted);
  assert.equal((await readJournal(installDir)).phase, "replacing");
  assert.deepEqual(await fs.readFile(path.join(installDir, MAIN_NAME)), oldMain);
  await runApp(installDir, path.join(scenarioRoot, "old-after-apply-kill"), "old");

  const retry = await createTransaction(scenarioRoot, installDir, helperPath, "apply-retry");
  const recovery = await launchFromStableWriter(template, retry, helperPath, scenarioRoot);
  await assertHelperSuccess(recovery);
  assert.equal((await readJournal(installDir)).phase, "rolledBack");
  await waitForFile(recovery.launchMarker);
  assert.equal(await fs.readFile(recovery.launchMarker, "utf8"), "old");
  await assertNoLocks(installDir);

  const retryApply = await launchFromStableWriter(template, retry, helperPath, scenarioRoot);
  await assertHelperSuccess(retryApply);
  assert.equal((await readJournal(installDir)).phase, "awaitingStartup");
  await runApp(installDir, path.join(scenarioRoot, "new-after-retry"), "new");
  await assertNoLocks(installDir);
}

async function recoveryPrepublicationScenario(template, helperPath, root) {
  const scenarioRoot = path.join(root, "recovery-prepublish");
  const installDir = await createInstall(scenarioRoot, helperPath);
  const oldMain = await fs.readFile(path.join(installDir, MAIN_NAME));
  const first = await createTransaction(scenarioRoot, installDir, helperPath, "recovery-first");
  const afterMain = await launchFromStableWriter(template, first, helperPath, scenarioRoot, {
    pausePhase: "applyAfterMain",
  });
  await waitForPause(afterMain);
  const newMain = await fs.readFile(path.join(installDir, MAIN_NAME));
  assert.notDeepEqual(newMain, oldMain);
  await runApp(installDir, path.join(scenarioRoot, "new-before-apply-kill"), "new");
  await killPausedHelper(afterMain);
  assert.equal((await readJournal(installDir)).phase, "replacing");

  const recoveryAttempt = await createTransaction(scenarioRoot, installDir, helperPath, "recovery-attempt");
  const recoveryPaused = await launchFromStableWriter(
    template,
    recoveryAttempt,
    helperPath,
    scenarioRoot,
    { pausePhase: "recoveryMain", pauseBinding: first },
  );
  await waitForPause(recoveryPaused);
  assert.deepEqual(await fs.readFile(path.join(installDir, MAIN_NAME)), newMain);
  await runApp(installDir, path.join(scenarioRoot, "new-before-recovery-kill"), "new");
  await killPausedHelper(recoveryPaused);
  assert.equal((await readJournal(installDir)).phase, "rollingBack");
  assert.deepEqual(await fs.readFile(path.join(installDir, MAIN_NAME)), newMain);
  await runApp(installDir, path.join(scenarioRoot, "new-after-recovery-kill"), "new");

  const finalRetry = await createTransaction(scenarioRoot, installDir, helperPath, "recovery-final");
  const recovered = await launchFromStableWriter(template, finalRetry, helperPath, scenarioRoot);
  await assertHelperSuccess(recovered);
  assert.equal((await readJournal(installDir)).phase, "rolledBack");
  assert.deepEqual(await fs.readFile(path.join(installDir, MAIN_NAME)), oldMain);
  await waitForFile(recovered.launchMarker);
  assert.equal(await fs.readFile(recovered.launchMarker, "utf8"), "old");
  await assertNoLocks(installDir);

  const applied = await launchFromStableWriter(template, finalRetry, helperPath, scenarioRoot);
  await assertHelperSuccess(applied);
  assert.equal((await readJournal(installDir)).phase, "awaitingStartup");
  await runApp(installDir, path.join(scenarioRoot, "new-after-recovery-retry"), "new");
  await assertNoLocks(installDir);
}

async function main() {
  if (process.platform !== "linux") throw new Error("The updater SIGKILL compatibility runner is Linux-only.");
  const args = parseArgs(process.argv.slice(2));
  if (args.writer) return stableWriterMode(path.resolve(args.writer));
  if (!args.helper) throw new Error("Usage: run-v1.9.1-compat.mjs --helper <debug helper path>");
  const helperPath = path.resolve(repoRoot, args.helper);
  await fs.access(helperPath);
  const template = JSON.parse(await fs.readFile(templatePath, "utf8"));
  await verifyProvenance(template);
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "vfl-v191-compat-"));
  let succeeded = false;
  try {
    await normalScenario(template, helperPath, root);
    await applyPrepublicationScenario(template, helperPath, root);
    await recoveryPrepublicationScenario(template, helperPath, root);
    succeeded = true;
    console.log("v1.9.1 writer -> current helper compatibility: PASS");
  } finally {
    if (!succeeded) {
      for (const pid of activeHelperPids) {
        try {
          process.kill(pid, "SIGKILL");
          await waitForProcessExit(pid, 10_000);
        } catch (error) {
          if (error.code !== "ESRCH") console.error(`Failed to stop helper ${pid}: ${error.message}`);
        }
      }
      activeHelperPids.clear();
    }
    if (succeeded) {
      await fs.rm(root, { recursive: true, force: true });
    } else {
      console.error(`Updater compatibility failure artifacts retained at ${root}`);
    }
  }
}

main().catch((error) => {
  console.error(error.stack || error.message);
  process.exitCode = 1;
});
