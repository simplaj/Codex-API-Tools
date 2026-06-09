# Codex API Tools

Cross-platform `codex-tools` CLI for Codex provider repair, history sync, remote/plugin unlock, OpenAI quota checks, and encrypted cloud session backup.

## Install

End users do not need Node.js, npm, or Rust.

macOS or Linux:

```bash
curl -fsSL https://github.com/simplaj/Codex-API-Tools/releases/latest/download/install-codex-tools.sh | bash
```

Windows PowerShell:

```powershell
irm https://github.com/simplaj/Codex-API-Tools/releases/latest/download/install-codex-tools.ps1 | iex
```

Manual release assets:

- `codex-tools-cli-macos-aarch64`
- `codex-tools-cli-macos-x64`
- `codex-tools-cli-windows-x64.exe`
- `codex-tools-cli-linux-x64`
- `codex-tools-cli-checksums.txt`

## Local Codex Tools

Always close Codex before write operations. The CLI detects Codex App, Codex CLI, and app-server processes before touching `config.toml`, `state_5.sqlite`, rollout files, or `.codex-global-state.json`. If detection fails, close Codex manually and retry.

Check local state:

```bash
codex-tools status
codex-tools codex quit
```

Fix remote compression/provider visibility and sync all history metadata to the current provider:

```bash
codex-tools provider repair --name simplaj
codex-tools provider sync --provider simplaj
codex-tools provider status
```

Remote/plugin unlock flow:

```bash
# 1. Close Codex, then back up and remove auth.json.
codex-tools codex quit
codex-tools auth unlock

# 2. Start Codex, sign in with the ChatGPT account that should unlock remote/plugin features, then fully close Codex again.

# 3. Write the relay API key into the target provider.
codex-tools auth token --provider simplaj
```

Switch between GPT subscription mode and relay mode without manual TOML edits:

```bash
# Comment only base_url and experimental_bearer_token.
codex-tools relay gpt --provider simplaj

# Uncomment only base_url and experimental_bearer_token.
codex-tools relay restore --provider simplaj
```

Check the current Codex ChatGPT quota from local `auth.json`:

```bash
codex-tools quota
codex-tools quota --json
```

The quota command reads only the local ChatGPT account id, masked email/plan claims, and access token needed for `https://chatgpt.com/backend-api/wham/usage`. Tokens are not printed.

## Sync Semantics

Provider sync does not switch ChatGPT login state and does not rewrite message history content. It updates visibility metadata so histories belonging to older provider names become visible under the selected provider:

- `~/.codex/sessions/**/rollout-*.jsonl`
- `~/.codex/archived_sessions/**/rollout-*.jsonl`
- `~/.codex/state_5.sqlite`
- `~/.codex/.codex-global-state.json`

If old rollouts contain `encrypted_content` from another provider or account, list visibility can be repaired, but continuing or compacting those exact histories may still fail upstream because encrypted message content is account/provider-bound.

## Cloud Session Backup

Cloud sync uses one user-facing secret: the Sync key.

- The CLI compresses and encrypts rollouts locally with zstd + XChaCha20-Poly1305 before upload.
- The Sync key is saved locally in `~/.codex/codex-api-tools-cloud.json` for simple repeat usage.
- The Worker receives only a derived `syncKeyProof` during login and stores only a hash of that proof.
- D1 stores users, devices, session metadata, session versions, and audit events.
- R2 stores encrypted session blobs only.
- No `auth.json`, API keys, `config.toml`, ChatGPT tokens, or raw Sync keys are uploaded.

Login once on each machine with the same email and Sync key:

```bash
codex-tools cloud login --email user@example.com
```

Common commands:

```bash
codex-tools cloud smoke
codex-tools cloud status
codex-tools cloud logout

codex-tools sessions list --limit 20

codex-tools cloud push --all
codex-tools cloud push --all -n 8
codex-tools cloud push --session-id <session-id>

codex-tools cloud list
codex-tools cloud list --json

codex-tools cloud pull --all
codex-tools cloud pull --all -n 8
codex-tools cloud pull --session-id <session-id>
```

`cloud push` is idempotent by default. The CLI checks the latest cloud version for the same session and skips upload when the remote `raw_sha256` already matches. The Worker also skips an already stored `session_id + raw_sha256` before reading the upload body. Use `--force` only when you intentionally want to rewrite an existing version.

Large encrypted rollouts are uploaded in chunks automatically. Files up to 50 MB encrypted size use a single request; larger payloads default to 16 MB chunks and are finalized by a chunk manifest in R2. Tune transfer parallelism with `-n N`. On push, `-n` applies to session groups and chunk uploads. On pull, `-n` applies to chunk downloads.

If the CLI reports a TLS, connection, timeout, or body-stream failure but the Worker dashboard has no matching error, the request likely failed before it reached Cloudflare or before the response reached the client. Retry, or set `HTTP_PROXY` / `HTTPS_PROXY` if the local network needs a proxy.

After restoring sessions on a machine that uses a different provider name, run:

```bash
codex-tools provider sync --provider simplaj
```

## Cloudflare Backend

The production backend lives under `cloudflare/sync-api`.

Create resources:

```bash
npx wrangler r2 bucket create codex-tools
npx wrangler d1 create codex-tools-sync-prod
```

Copy the returned D1 `database_id` into `cloudflare/sync-api/wrangler.jsonc`, then apply migrations and deploy:

```bash
cd cloudflare/sync-api
npx wrangler d1 migrations apply codex-tools-sync-prod --remote
npx wrangler deploy
```

The Worker uses:

- D1 binding: `DB`
- R2 binding: `SESSION_BUCKET`
- Rate limit binding: `REGISTER_RATE_LIMITER`
- Invite code: `sub2api.simplaj.top`

Registration is protected by invite code + Cloudflare Rate Limiting + Sync-key proof. There is no direct R2 client mode and no operator bypass key path in production.

## Development

Build and run the CLI directly with Cargo:

```bash
cargo build --manifest-path src-tauri/Cargo.toml --bin codex-tools
cargo run --manifest-path src-tauri/Cargo.toml --bin codex-tools -- status
```

Release workflow:

- `.github/workflows/release.yml` publishes CLI-only releases.
- Main branch pushes create tags like `codex-api-tools-v0.1.1-<run_number>`.
- Version tags such as `v0.1.1` publish to that tag.
- Assets are raw standalone binaries plus install scripts and `codex-tools-cli-checksums.txt`.
