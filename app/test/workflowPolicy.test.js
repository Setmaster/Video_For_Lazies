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
  assert.match(
    release,
    /build_only:[\s\S]*description:[^\n]*private artifacts[^\n]*[\s\S]*required:\s*true[\s\S]*default:\s*true[\s\S]*type:\s*boolean/,
  );
  assert.match(
    release,
    /release:[\s\S]*if:\s*\$\{\{\s*github\.event_name != 'workflow_dispatch' \|\| !inputs\.build_only\s*\}\}/,
  );
  assert.match(ci, /uses:\s*\.\/\.github\/actions\/setup-linux-tauri-deps/);
  assert.match(release, /uses:\s*\.\/\.github\/actions\/setup-linux-tauri-deps/);
  assert.match(ci, /uses:\s*\.\/\.github\/actions\/install-cargo-audit/);
  assert.match(release, /uses:\s*\.\/\.github\/actions\/install-cargo-audit/);

  assert.match(linuxSetupAction, /timeout "\$APT_UPDATE_TIMEOUT_SECONDS" apt-get update/);
  assert.match(linuxSetupAction, /timeout "\$APT_INSTALL_TIMEOUT_SECONDS"[\s\S]*apt-get install -y --no-install-recommends/);
  assert.match(linuxSetupAction, /retry "\$APT_ATTEMPTS"/);
  assert.match(linuxSetupAction, /attempts:[\s\S]*default:\s*"1"/);
  assert.match(linuxSetupAction, /install-timeout-seconds:[\s\S]*default:\s*"1800"/);
  assert.match(cargoAuditAction, /actions\/cache@v5/);
  assert.match(cargoAuditAction, /cargo install cargo-audit --version "\$CARGO_AUDIT_VERSION" --locked/);
});

test("release docs require exact-SHA CI and portable artifacts by release class", async () => {
  const [docs, appReadme] = await Promise.all([
    readRepoFile("docs/release.md"),
    readRepoFile("app/README.md"),
  ]);

  assert.match(docs, /## Release Gate Policy/);
  assert.match(docs, /The `Portable Release` workflow is the final artifact gate/);
  assert.match(docs, /Exact-SHA `main` CI is a hard gate for every `MAJOR` or `MINOR` release/);
  assert.match(docs, /changes update checks, signing, manifests, payload validation, staging, apply behavior, rollback, or recovery/);
  assert.match(docs, /runner-setup or dependency-installation stall does not satisfy this gate and cannot use the setup-stall exception/);
  assert.match(docs, /Every `MAJOR` or `MINOR` release also requires curated release notes before publication/);
  assert.match(docs, /entire generated body must be replaced/);
  assert.match(docs, /setup-stall exception is limited to an ordinary `PATCH` release that does not change the updater/);
  assert.match(docs, /`dev` CI already passed for the same commit hash/);
  assert.match(docs, /The merge to `main` was a fast-forward/);
  assert.match(docs, /stalled in runner setup or dependency installation/);
  assert.match(docs, /If `main` CI fails in repo code, tests, lint, version checks, or audits, stop/);
  assert.match(docs, /Do not describe the stalled run as passing/);
  assert.match(docs, /`build_only=true`/);
  assert.match(docs, /`build_only=false`/);
  assert.match(docs, /Tag events have no dispatch inputs/);
  assert.match(docs, /defaults them to `draft=true` and `prerelease=true`/);
  assert.match(docs, /gh release edit vX\.Y\.Z --draft=false --prerelease=false --latest/);
  assert.match(appReadme, /Manual runs default to `build_only=true`/);
  assert.match(appReadme, /private GitHub Actions artifacts for 30 days/);
  assert.match(appReadme, /(?:intentional|deliberate) `build_only=false` run/);
  assert.match(appReadme, /Tag-triggered runs (?:intentionally|conservatively) create a draft prerelease/);
});

test("release docs describe updater trust, recovery, and current package proof", async () => {
  const docs = await readRepoFile("docs/release.md");

  assert.match(docs, /manifest binds the release version and tag to each platform archive URL, SHA256, byte size/);
  assert.match(docs, /payload-manifest sidecar must be byte-for-byte identical to the copy embedded/);
  assert.match(docs, /Apply is single-owner and no-clobber/);
  assert.match(docs, /durable journal records the staged plan and verified backup digest/);
  assert.match(docs, /automatic recovery or rollback/);
  assert.match(docs, /extracts the exact zip that will be uploaded and validates its payload manifest/);
  assert.match(docs, /Packaged app smokes on both platforms cover media-depth safety/);
  assert.match(docs, /## Windows Code-Signing Note/);
  assert.match(docs, /not Authenticode-signed/);
  assert.doesNotMatch(docs, /## Unsigned Alpha Note/);
});
