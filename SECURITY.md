# Security Policy

## Supported Versions

Security reports are accepted for the current `main` branch and the latest published release, once public releases exist.

## Reporting a Vulnerability

Please report security issues through GitHub private vulnerability reporting:

```text
https://github.com/Setmaster/Video_For_Lazies/security/advisories/new
```

Do not include secrets, private media, or sensitive personal files in a public issue.

## Scope

Useful reports include:

- Unsafe file access or output overwrite behavior
- Problems caused by untrusted video files
- FFmpeg sidecar packaging or provenance concerns
- Dependency vulnerabilities that affect packaged builds

Out of scope:

- Reports that require malware execution on the user's machine before the app starts
- Social engineering
- Denial of service against development-only tooling that is not shipped to users
