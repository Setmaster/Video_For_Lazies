# Security Policy

## Supported Versions

The latest stable GitHub Release is the supported distribution. Reports against the current `main` branch are also accepted because it reflects shipped or release-ready source.

Older releases are not supported. Please confirm the issue still exists on the latest stable release when that is safe and practical. The `dev` branch is prerelease work: reports are welcome, but it is not a supported distribution.

## Reporting a Vulnerability

Please report security issues through GitHub private vulnerability reporting:

```text
https://github.com/Setmaster/Video_For_Lazies/security/advisories/new
```

Do not include secrets, private media, or sensitive personal files in a public issue.

Include the app version, operating system, affected workflow, expected result, and minimal reproduction steps. If a media sample is required, prefer a small synthetic file and attach it only to the private advisory. Redact local usernames, paths, filenames, update logs, and metadata that are not needed to reproduce the issue.

## Scope

Useful reports include:

- Output overwrite, path traversal, temporary-file, or unintended file-access behavior
- Signed updater, payload-manifest, rollback, recovery-journal, or update-helper trust failures
- Problems triggered by a crafted media or subtitle file in a supported user workflow
- FFmpeg sidecar packaging, checksum, capability-contract, license/source, or provenance failures
- Privacy failures that expose source paths, subtitle paths, metadata, or retained diagnostics beyond the local workflow
- Dependency vulnerabilities that are reachable in packaged Windows or Linux builds

Out of scope:

- Reports that require malware execution on the user's machine before the app starts
- Social engineering
- Windows SmartScreen or antivirus reputation warnings caused only by the current unsigned executable
- Availability or denial-of-service findings limited to development scripts, CI, or other tooling that is not shipped to users
- Findings that apply only to an unsupported old release and cannot be reproduced on the latest stable release
