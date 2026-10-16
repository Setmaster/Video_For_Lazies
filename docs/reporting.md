# Anonymous bug reporting

The `reporting/` service provides a public bug form without requiring a reporter's GitHub account. It creates issues only in `Setmaster/Video_For_Lazies`. The desktop app may open the form with `version` and `platform` query parameters. No media, local paths, filenames, logs, or diagnostics are collected automatically.

Reporters review their text and explicitly consent to public publication. Optional diagnostics are pasted manually and are **not automatically redacted**. Security reports belong in [private vulnerability reporting](https://github.com/Setmaster/Video_For_Lazies/security/advisories/new). The form supports Copy and Save before or after a submission error. Draft text is held in the current tab, not local storage; closing or reloading the page loses it unless copied or saved.

## Service layout

- One Cloudflare Worker serves the form and same-origin JSON endpoints. Assets use no third-party scripts, fonts, analytics, or uploads.
- A GitHub App installation token, restricted to this repository and `issues:write`, creates the issue. App keys, JWTs, and installation tokens stay server-side.
- D1 stores the random report reference, keyed report fingerprint, keyed IP value, timestamp, outcome, and verified repository issue URL. It never stores report text or raw IP addresses.
- A single SQL admission statement reserves a request and enforces at most 20 attempts per UTC day globally and 3 attempts per rolling hour per keyed IP. Failed and uncertain attempts consume quota. These limits can affect people sharing an IP.
- A stateless HMAC nonce, three-second minimum age, one-hour admission window, same-origin POST check, and honeypot provide basic friction. They do not exclude determined bots. Cloudflare platform limits remain a separate availability boundary.
- Metadata expires after seven days and is removed by a daily scheduled event, normally within eight days. Provider-managed backups, request processing, and logs follow Cloudflare's own retention policies. Issues remain public on GitHub. The Worker does not log request bodies, addresses, or credentials; Workers observability is disabled in the supplied configuration.

The nonce remains usable for receipt checks for seven days. Reserving before the GitHub write prevents blind duplicate retries. A successful response must contain an exact `https://github.com/Setmaster/Video_For_Lazies/issues/<number>` URL. Uncertain writes remain reserved and are never sent again automatically.

## Local development and checks

Use Node 22.13 or later, with the pinned project-local Wrangler:

```bash
cd reporting
npm ci
npm test
npm run check
npm run test:runtime
npm run build
```

`npm test` runs local SQLite admission tests, request validation, privacy boundaries, crypto, and stubbed GitHub responses. `test:runtime` uses a disposable home/config, real local workerd and D1, fake GitHub writes, and temporary fixture endpoints that are absent from the deploy artifact. It checks static assets, policy headers, replay behavior, concurrent quota admission, and metadata privacy. It does not access a real Cloudflare account or GitHub repository. `build` is a Wrangler deployment dry run and writes `reporting/dist/`.

For visual development:

```bash
npm run db:local
npm run dev
```

The form remains available with reporting disabled. `/api/session` and report submission fail closed until configured. Use the runtime fixture for simulated success; never point routine tests at live GitHub credentials. `node:sqlite` may print an experimental API warning on supported Node versions.

## Provisioning contract

Create a Cloudflare Worker and D1 database using the existing account and free services. No paid add-on is required by this implementation. Review the account's current [Workers limits](https://developers.cloudflare.com/workers/platform/limits/) and [D1 limits](https://developers.cloudflare.com/d1/platform/limits/) before enabling public traffic.

1. The production D1 database is `vfl-bug-reports`; its binding is recorded in `reporting/wrangler.jsonc`. For a separate deployment, create a new database and replace `database_id`; keep the binding named `DB`. Start with `REPORTING_ENABLED=false` until provisioning is complete.
2. Create a private GitHub App with repository **Issues: Read and write** and the required metadata access. Disable webhooks; this service does not use them. Install it only on `Setmaster/Video_For_Lazies`. Record its app ID and installation ID.
3. Generate an App private key. GitHub's downloaded RSA key must be converted to PKCS#8 for WebCrypto. In a private directory, use `openssl pkcs8 -topk8 -nocrypt -in downloaded-key.pem -out worker-key.pem`. The result starts with `BEGIN PRIVATE KEY`. Store the key in the approved secret manager and provision it through Wrangler's secret input; never commit it or paste it into an issue.
4. Set the four Worker secrets below. `REPORT_SECRET` should be a newly generated random secret of at least 32 characters. Keep the provisioned secret consistent across deploys so existing receipts remain valid.
5. Apply `npm exec wrangler -- d1 migrations apply DB --remote`. This creates the metadata schema; it does not migrate report bodies.
6. Attach the intended HTTPS custom domain through a Worker custom-domain route. Production uses `https://vfl-bugs.7dev.io/`. Keep `workers_dev` and `preview_urls` disabled to avoid additional public entrypoints. The desktop endpoint must match the chosen domain exactly. Cloudflare requires an account-level workers.dev subdomain before scheduled cleanup can be configured, even with this Worker's workers.dev route disabled.
7. Set `REPORTING_ENABLED` to `true` only after keys, database, route, and security checks are ready. Deploy the reviewed Worker and static assets together. Keep Workers observability disabled and do not enable request-body logging or analytics that collect form contents.
8. Check `/api/health`, then perform one clearly labeled synthetic public report, verify its GitHub URL and body, and retry the identical submission to verify that it produces no duplicate. Clean up the synthetic issue according to the maintainer's normal practice.

| Binding | Kind | Meaning |
| --- | --- | --- |
| `DB` | D1 database | Metadata and atomic admission |
| `ASSETS` | Static assets | `reporting/public/`, provided by Wrangler |
| `REPORTING_ENABLED` | Variable | Literal `true` enables submission; production is enabled, use `false` during fresh provisioning or an outage |
| `REPORT_SECRET` | Secret | HMAC nonce, IP-key, and fingerprint key; at least 32 characters |
| `GITHUB_APP_ID` | Secret | Numeric GitHub App ID |
| `GITHUB_INSTALLATION_ID` | Secret | Numeric installation ID for the repository |
| `GITHUB_PRIVATE_KEY` | Secret | PKCS#8 PEM App private key |

Provision each secret with `npm exec wrangler -- secret put NAME` from `reporting/`, using secure input. An ignored `.dev.vars` file may hold local development bindings, but use inert test credentials for local tests. Do not ship credentials to the desktop app, public JavaScript, Wrangler variables, or git.

`GET /api/health` exposes only `{ "ready": true|false }`. It reports configuration presence, not live GitHub authentication or database health. `GET /api/session` returns a signed nonce. `POST /api/reports` accepts the explicitly listed form fields, consent, honeypot, and nonce, with a 70 KB body cap and per-field limits.

The deploy chain is source plus pinned npm lockfile, Wrangler bundle plus static assets, then Cloudflare Worker with D1 and HTTPS custom domain. There is no separate application server or reverse proxy to provision. The initial schema is additive. No automatic deployment workflow is supplied.

## Failures and recovery

An explicit GitHub 4xx rejection or failure to acquire an installation token is recorded as `rejected`. The same nonce cannot retry the write; the form can request a fresh session while retaining the draft. Network failures, non-201 server responses, malformed success responses, and a failure to persist a completed GitHub write are conservative `pending` or `uncertain` outcomes. The form locks submitted fields, keeps Copy and Save available, and checks the original snapshot using its original nonce. Reloading and submitting again could duplicate an issue and should be avoided.

For an uncertain report, search the repository's issues for the exact `Report reference` UUID. Confirm both repository and issue content before setting that existing D1 row to `created` with its verified URL. Do not delete a pending row or resend its report as a recovery shortcut. If no matching issue can be confirmed, retain uncertainty and resolve with the reporter manually. Report bodies cannot be reconstructed from D1.

Rotate the App key by provisioning its replacement and testing authentication before revoking the old key. Rotating `REPORT_SECRET` invalidates existing sessions and receipt signatures; coordinate that change rather than using it as routine maintenance. Abuse controls may need adjustment after real traffic is observed.

To stop publication, set `REPORTING_ENABLED=false` and deploy that configuration. The form can still offer Copy and Save. To roll back code, redeploy the prior reviewed Worker and assets while retaining D1 metadata and secrets; deleting the database would remove duplicate protection. Revoke the GitHub App installation or key if credential compromise requires an immediate independent stop.

The implementation follows [D1's SQL binding and atomic statement semantics](https://developers.cloudflare.com/d1/worker-api/prepared-statements/), [GitHub App JWT authentication](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-json-web-token-jwt-for-a-github-app), [installation token scoping](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-an-installation-access-token-for-a-github-app), and [GitHub's create-issue API](https://docs.github.com/en/rest/issues/issues#create-an-issue).
