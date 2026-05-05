import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../..");

async function readRepoFile(relativePath) {
  return fs.readFile(path.resolve(repoRoot, relativePath), "utf8");
}

test("CI workflows use bounded reusable setup steps", async () => {
  const ci = await readRepoFile(".github/workflows/ci.yml");
  const release = await readRepoFile(".github/workflows/portable-artifacts.yml");
  const linuxSetupAction = await readRepoFile(".github/actions/setup-linux-tauri-deps/action.yml");
  const cargoAuditAction = await readRepoFile(".github/actions/install-cargo-audit/action.yml");

  assert.match(ci, /timeout-minutes:\s*45/);
  assert.match(release, /timeout-minutes:\s*75/);
  assert.match(release, /timeout-minutes:\s*45/);
  assert.match(ci, /uses:\s*\.\/\.github\/actions\/setup-linux-tauri-deps/);
  assert.match(release, /uses:\s*\.\/\.github\/actions\/setup-linux-tauri-deps/);
  assert.match(ci, /uses:\s*\.\/\.github\/actions\/install-cargo-audit/);
  assert.match(release, /uses:\s*\.\/\.github\/actions\/install-cargo-audit/);

  assert.match(linuxSetupAction, /timeout "\$APT_UPDATE_TIMEOUT_SECONDS" apt-get update/);
  assert.match(linuxSetupAction, /timeout "\$APT_INSTALL_TIMEOUT_SECONDS"[\s\S]*apt-get install -y --no-install-recommends/);
  assert.match(linuxSetupAction, /retry "\$APT_ATTEMPTS"/);
  assert.match(linuxSetupAction, /attempts:[\s\S]*default:\s*"1"/);
  assert.match(linuxSetupAction, /install-timeout-seconds:[\s\S]*default:\s*"1800"/);
  assert.match(cargoAuditAction, /actions\/cache@v4/);
  assert.match(cargoAuditAction, /cargo install cargo-audit --version "\$CARGO_AUDIT_VERSION" --locked/);
});

test("release docs make Portable Release the final blocking gate", async () => {
  const docs = await readRepoFile("docs/release.md");

  assert.match(docs, /## Release Gate Policy/);
  assert.match(docs, /The `Portable Release` workflow is the final release gate/);
  assert.match(docs, /`dev` CI already passed for the same commit hash/);
  assert.match(docs, /The merge to `main` was a fast-forward/);
  assert.match(docs, /stalled in runner setup or dependency installation/);
  assert.match(docs, /If `main` CI fails in repo code, tests, lint, version checks, or audits, stop/);
});
