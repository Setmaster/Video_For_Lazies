# Updater compatibility fixtures

`v1.9.1-apply-plan.template.json` pins the serialized `UpdateApplyPlan` shape
written by the public v1.9.1 updater. It is derived from:

- tag commit: `4962ff41765dfba0a3e4efb7973e2563265c5b74`
- `app/src-tauri/src/updater.rs` blob: `db94da8c4d28e05a9ab5df347ee65622af5b6275`
- tagged updater source SHA-256: `1ea32ac2d5cb4a6d73c6e3ba834e4d3dd49a8e327dc89689e0a23630230b9e8f`

Only paths, PID, update id, and version values are substituted by the runner.
The field names and order are asserted before every compatibility run.

Run the real current helper CLI seam from the repository root:

```sh
cargo build --manifest-path app/src-tauri/Cargo.toml --bin vfl-update-helper
node app/test-support/updater/run-v1.9.1-compat.mjs \
  --helper app/src-tauri/target/debug/vfl-update-helper
```

The runner is Linux-only because the crash assertions use `SIGKILL`. The first
command builds the current debug helper; the runner verifies the tagged provenance and exercises
normal migration plus apply and recovery hard-kill windows. The helper's test
trust and pause gates are compiled out of release builds and bind to the exact
payload-manifest digest and update id under test.
