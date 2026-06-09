# Codex API Tools

Windows/macOS desktop toolbox for Codex provider repair, history synchronization, remote/plugin unlock, and OpenAI account quota checks.

## Install

Normal users do not need Node.js, npm, or a local Rust toolchain.

Download the latest assets from GitHub Releases:

- macOS desktop: install the `.dmg`.
- Windows desktop: run the `.exe` installer.

CLI install on macOS or Linux:

```bash
curl -fsSL https://github.com/simplaj/Codex-API-Tools/releases/latest/download/install-codex-tools.sh | bash
```

CLI install on Windows PowerShell:

```powershell
irm https://github.com/simplaj/Codex-API-Tools/releases/latest/download/install-codex-tools.ps1 | iex
```

Then log in once on each machine:

```bash
codex-tools cloud login --email user@example.com
```

Manual CLI downloads are still available if you need offline install or checksum verification. The raw binaries are the easiest manual path:

- macOS CLI: `codex-tools-macos-aarch64` or `codex-tools-macos-x64`
- Windows CLI: `codex-tools-windows-x64.exe`
- Linux CLI: `codex-tools-linux-x64`

Archive packages are also published:

- macOS CLI: `codex-tools-macos-aarch64.tar.gz` or `codex-tools-macos-x64.tar.gz`
- Windows CLI: `codex-tools-windows-x64.zip`
- Linux CLI: `codex-tools-linux-x64.tar.gz`

## Features

- Detects a Codex provider whose id or `name` is `OpenAI`; if none exists, uses the current root provider section.
- Renames that provider section to the new provider id/name, defaulting to `simplaj`.
- Syncs all historical Codex session metadata to the same new provider name with the built-in Rust/rusqlite engine, following the key behavior of `Dailin521/codex-provider-sync` without requiring `npx`: rollout `session_meta.payload.model_provider`, `state_5.sqlite.threads.model_provider`, user-event/cwd repair, and `.codex-global-state.json` workspace-root cache repair.
- Detects running Codex App / Codex CLI / app-server processes before writes, can request Codex App to quit, and blocks state writes until Codex is fully closed. If process detection itself fails, writes are blocked and the user is asked to quit Codex manually before retrying.
- Backs up and removes `~/.codex/auth.json` for the remote/plugin login flow.
- Writes `experimental_bearer_token = "sk-..."` into a selected `[model_providers.NAME]` section.
- Queries the current ChatGPT login quota through OpenAI's `wham/usage` endpoint using the local Codex ChatGPT login token, showing masked account info, plan type, usage windows, reset time, credits, and a clear fallback message when the endpoint is unavailable or the login is not ChatGPT-based.
- Can automatically comment or uncomment only the current provider's `base_url` and `experimental_bearer_token` lines, so users can switch between GPT subscription mode and relay mode without manually editing `config.toml`.
- Provides a headless `codex-tools` CLI for encrypted cloud session backup and restore. Production mode uses a Cloudflare Worker API in front of D1 and R2, so desktop/CLI clients never need R2 credentials.
- Uses the Simplaj logo in the app header and links to `https://sub2api.simplaj.top/` for stable relay and technical support.
- Backs up touched files under `~/.codex/backups_state/gpt-api-tools/<timestamp>`.

## User Flow

1. Open the app and refresh the current `~/.codex` state.
2. Click "try quit Codex", or fully quit Codex App, Codex CLI, and app-server manually.
3. After the app reports no Codex processes, tick "already closed Codex" in the app.
4. Run "rename and sync" to rename the OpenAI provider, or the current root provider if OpenAI is already gone, to `simplaj` or another custom provider name. The same action syncs every historical chat metadata record to that new provider name.
5. If the provider section is already named correctly, "sync only to new name" still syncs histories to the provider name typed in the app.
6. For remote/plugin unlock, keep Codex closed and run "backup and remove auth.json".
7. Start Codex App and sign in with the GPT account that should unlock remote control/plugin features. Use the same account as the phone if remote control is needed.
8. After GPT login finishes, fully quit Codex again.
9. Load the backed-up Simplaj API key or enter one manually, then write it as `experimental_bearer_token` under the target provider.
10. Restart Codex App so the new provider/auth combination is picked up.
11. In "OpenAI 额度", query the current ChatGPT account's Codex usage window. When quota has recovered, fully quit Codex, tick the closed confirmation, then click "切回 GPT 订阅"; the app backs up `config.toml` and comments the selected provider's `base_url` and `experimental_bearer_token` lines.
12. To switch back to the relay later, fully quit Codex, tick the same confirmation, then click "恢复中转"; the app backs up `config.toml` and uncomments those two lines.

Do not write `config.toml`, `state_5.sqlite`, rollout files, or `.codex-global-state.json` while Codex is running. Codex can keep SQLite locked or rewrite `config.toml` on exit, which may hide histories again or discard the injected token. The app enforces this in the backend for provider repair, metadata sync, `auth.json` removal, and token writing, and fails closed if it cannot confirm the Codex process state.

## Sync Semantics

The sync flow mirrors `codex-provider-sync sync`: it does not switch ChatGPT login state and does not rewrite message history. It only updates visibility metadata so histories belonging to older provider names become visible under the target provider name selected in the app:

- `~/.codex/sessions/**/rollout-*.jsonl`
- `~/.codex/archived_sessions/**/rollout-*.jsonl`
- `~/.codex/state_5.sqlite`
- `~/.codex/.codex-global-state.json`

If old rollouts contain `encrypted_content` from another provider or account, this can restore list visibility, but continuing or compacting those exact histories can still fail upstream because encrypted content is account/provider-bound.

## Cloud Session Backup

The CLI can upload and restore Codex rollout files through a Cloudflare Worker API. It compresses and encrypts every rollout locally before upload, stores encrypted blobs in R2, and stores searchable metadata in D1.

Production storage is split this way:

- Worker API handles user/device registration, authentication, upload, list, and download.
- D1 stores users, devices, latest session metadata, session versions, and audit events.
- R2 stores encrypted session blobs only.
- Clients store only the Worker URL, a device token, and the user's local sync passphrase.
- No `auth.json`, API keys, `config.toml`, or ChatGPT tokens are uploaded.
- The sync passphrase is required to decrypt restored sessions. If the passphrase is lost, uploaded blobs cannot be recovered.
- R2 credentials must stay on Cloudflare/server-side infrastructure. Do not ship R2 credentials inside desktop builds or share them with end users.

### User Setup

Run `cloud login` once on each machine:

```bash
codex-tools cloud login --email user@example.com
```

The CLI registers the device with the default invite code `sub2api.simplaj.top`, asks for a sync passphrase, and saves local cloud configuration under `~/.codex/codex-api-tools-cloud.json`. On macOS/Linux the file is written with `0600` permissions. Keep the same sync passphrase on the same user's machines; it is needed to decrypt restored sessions.

Commands:

```bash
# Verify the Worker API.
codex-tools cloud smoke

# Inspect local Codex sessions without uploading.
codex-tools sessions list --limit 20
codex-tools sessions list --json

# Upload one session or all local session versions.
codex-tools cloud push --session-id <session-id>
codex-tools cloud push --all --limit 20
codex-tools cloud push --all --force
codex-tools cloud push --all -n 8

# List latest cloud versions by session id.
codex-tools cloud list
codex-tools cloud list --json

# Restore one session to this machine's CODEX_HOME.
codex-tools cloud pull --session-id <session-id>
codex-tools cloud pull --session-id <session-id> -n 8
```

Check or remove the local login:

```bash
codex-tools cloud status
codex-tools cloud logout
```

Environment variables are optional overrides for CI or temporary automation:

- `CODEX_TOOLS_API_URL`
- `CODEX_TOOLS_DEVICE_TOKEN`
- `CODEX_TOOLS_SYNC_PASSPHRASE`
- `CODEX_TOOLS_DEVICE_NAME`
- `CODEX_TOOLS_INVITE_CODE`
- `CODEX_TOOLS_ADMIN_BOOTSTRAP_TOKEN`
- `CODEX_TOOLS_HTTP_RETRIES`
- `CODEX_TOOLS_HTTP_TIMEOUT_SECS`
- `CODEX_TOOLS_HTTP_CONNECT_TIMEOUT_SECS`
- `CODEX_TOOLS_CHUNK_SIZE_BYTES`
- `CODEX_TOOLS_CHUNK_UPLOAD_CONCURRENCY`
- `CODEX_TOOLS_CHUNK_DOWNLOAD_CONCURRENCY`
- `CODEX_TOOLS_SESSION_UPLOAD_CONCURRENCY`

`cloud register` is kept for debugging and admin automation, but normal users should use `cloud login`. A correct admin bootstrap token bypasses the invite code and registration rate limit for controlled automation.

`cloud push` is idempotent by default. Before uploading, the CLI checks the latest cloud version for the same session and skips it when the remote `raw_sha256` already matches the local file. The Worker also skips an already stored `session_id + raw_sha256` before reading the upload body, so a retry after a lost response will not re-upload the same blob. Use `--force` only when you intentionally want to bypass the duplicate preflight and rewrite an existing version.

Large encrypted rollouts are uploaded in chunks automatically. Files up to 50 MB encrypted size use a single request; larger encrypted payloads default to 16 MB chunks and are finalized by a chunk manifest in R2. `cloud push --all` processes 2 session groups in parallel by default, and each large session uploads up to 4 chunks in parallel. A session group is one `session_id`; if the same session has multiple local versions, those versions are still uploaded serially from old to new so the latest cloud version stays correct. Tune per run with `-n N`; on push, `-n` applies to both session upload groups and chunk uploads. Session upload concurrency is capped at 8, and chunk upload concurrency is capped at 16. Override `CODEX_TOOLS_CHUNK_SIZE_BYTES` only for debugging or constrained networks.

Chunked downloads are parallel too when the Worker supports the chunk manifest endpoint. Use `codex-tools cloud pull --session-id <id> -n N` to tune a restore; the default is 4 and the cap is 16. Advanced aliases `--threads`, `--session-concurrency`, `--chunk-concurrency`, and `--download-concurrency` remain available for debugging or scripts. The `CODEX_TOOLS_*_CONCURRENCY` environment variables are still available for CI or shell-wide defaults. If the Worker endpoint is not deployed yet, the CLI falls back to the older sequential blob download path.

If Codex is still writing an active rollout file, the same session id can produce a new `raw_sha256` on every run. That is treated as a new session version, not a duplicate. Close or pause Codex before a final backup if you want a stable snapshot.

If the CLI reports a TLS, connection, timeout, or body-stream failure but the Worker dashboard has no matching error, the request may have failed before it reached Cloudflare or before the response reached the client. Increase `CODEX_TOOLS_HTTP_RETRIES` when the local network is unstable. Lower `CODEX_TOOLS_HTTP_CONNECT_TIMEOUT_SECS` to fail and retry stuck connection attempts faster; increase `CODEX_TOOLS_HTTP_TIMEOUT_SECS` only for very large uploads.

After restoring sessions on a machine that uses a different provider name, run the provider metadata sync so the histories are visible under the current provider:

```bash
codex-tools sessions list
# Then use the desktop "sync only to new name" flow, or the existing native provider sync command in the app.
```

### Cloudflare Backend Setup

The production backend lives under `cloudflare/sync-api`.

1. Create the R2 bucket:

```bash
npx wrangler r2 bucket create codex-tools
```

2. Create the D1 database and copy the returned `database_id` into `cloudflare/sync-api/wrangler.jsonc`:

```bash
npx wrangler d1 create codex-tools-sync-prod
```

3. Apply the D1 schema:

```bash
cd cloudflare/sync-api
npx wrangler d1 migrations apply codex-tools-sync-prod --remote
```

4. The Worker already has a registration rate limit binding in `wrangler.jsonc`:

```jsonc
"ratelimits": [
  {
    "name": "REGISTER_RATE_LIMITER",
    "namespace_id": "5210608",
    "simple": {
      "limit": 10,
      "period": 60
    }
  }
]
```

5. The invite code defaults to `sub2api.simplaj.top` in `wrangler.jsonc`. Override it only if you need to rotate the public invite gate:

```jsonc
"vars": {
  "REGISTRATION_INVITE_CODE": "sub2api.simplaj.top"
}
```

6. Optional: set an admin bootstrap token for CI or support tooling. When the CLI sends this token, the Worker validates it and skips the invite/rate-limit checks:

```bash
ADMIN_BOOTSTRAP_TOKEN="$(openssl rand -hex 32)"
printf "%s" "$ADMIN_BOOTSTRAP_TOKEN" | npx wrangler secret put ADMIN_BOOTSTRAP_TOKEN
```

7. Deploy the Worker:

```bash
npx wrangler deploy
```

Current configured backend resources:

- Worker name: `codex-tools-sync-api`
- R2 bucket: `codex-tools`
- D1 database: `codex-tools-sync-prod`

## OpenAI Quota Check

The quota query itself is read-only. It reads `~/.codex/auth.json`, extracts only the ChatGPT account id, email/plan claims, and access token needed for the request, then calls:

```text
GET https://chatgpt.com/backend-api/wham/usage
```

The request matches Codex's own ChatGPT usage path by sending `Authorization: Bearer <access_token>` and `ChatGPT-Account-Id`. The UI masks account identifiers and never prints tokens. If `auth.json` is missing, uses API-key auth, has expired tokens, or the OpenAI endpoint rejects the request, the app reports the exact safe failure summary and keeps the switch controls visible.

The switch controls are write operations and are protected by the same Codex process guard as provider repair and token injection. They only comment/uncomment `base_url` and `experimental_bearer_token` inside the selected `[model_providers.NAME]` section; they do not change `requires_openai_auth` or other provider fields.

## Runtime Requirements

End users do not need Node.js, npm, or Rust. The release assets contain native desktop installers and standalone `codex-tools` CLI packages.

The upstream `codex-provider-sync` Node CLI requires Node.js 24+ because it uses the `node:sqlite` built-in module, but this app performs the sync inside the Tauri Rust backend with bundled SQLite through `rusqlite`.

## Development

Development still uses npm because the desktop UI is a Tauri + React app. This is only for contributors working from source.

```bash
npm install
npm run dev
```

On macOS external drives, AppleDouble `._*` sidecar files can break Tauri permission generation. The npm scripts run Tauri through `scripts/tauri-run.mjs`, which sets `CARGO_TARGET_DIR` to a system temp directory and `COPYFILE_DISABLE=1` to avoid that issue.

Build the headless CLI:

```bash
npm run cli:build
```

Run the source-tree CLI during development:

```bash
npm run cli -- cloud status
```

## Build Installers

```bash
npm run dist:mac
npm run dist:win
```

## Release Pipeline

GitHub Actions builds and publishes desktop releases with `.github/workflows/release.yml`.

- Pushing to `main` creates a release tag like `codex-api-tools-v__VERSION__-<run_number>`.
- Pushing a version tag such as `v0.1.1` publishes to that tag.
- The matrix builds macOS Apple Silicon, macOS Intel, and Windows x64 desktop installers.
- The same release publishes one-command CLI installers: `install-codex-tools.sh` and `install-codex-tools.ps1`.
- It also publishes raw standalone CLI binaries (`codex-tools-macos-aarch64`, `codex-tools-macos-x64`, `codex-tools-windows-x64.exe`, `codex-tools-linux-x64`) and manual archive packages, plus SHA-256 checksum files.
- macOS CI builds use ad-hoc signing. Windows artifacts are unsigned unless signing secrets are added later.

The packaged app is written with Tauri + React and is intended to run on both macOS and Windows.
