# Codex API Tools

Windows/macOS desktop toolbox for Codex provider repair, history synchronization, remote/plugin unlock, and OpenAI account quota checks.

## Features

- Detects a Codex provider whose id or `name` is `OpenAI`; if none exists, uses the current root provider section.
- Renames that provider section to the new provider id/name, defaulting to `simplaj`.
- Syncs all historical Codex session metadata to the same new provider name with the built-in Rust/rusqlite engine, following the key behavior of `Dailin521/codex-provider-sync` without requiring `npx`: rollout `session_meta.payload.model_provider`, `state_5.sqlite.threads.model_provider`, user-event/cwd repair, and `.codex-global-state.json` workspace-root cache repair.
- Detects running Codex App / Codex CLI / app-server processes before writes, can request Codex App to quit, and blocks state writes until Codex is fully closed. If process detection itself fails, writes are blocked and the user is asked to quit Codex manually before retrying.
- Backs up and removes `~/.codex/auth.json` for the remote/plugin login flow.
- Writes `experimental_bearer_token = "sk-..."` into a selected `[model_providers.NAME]` section.
- Queries the current ChatGPT login quota through OpenAI's `wham/usage` endpoint using the local Codex ChatGPT login token, showing masked account info, plan type, usage windows, reset time, credits, and a clear fallback message when the endpoint is unavailable or the login is not ChatGPT-based.
- Can automatically comment or uncomment only the current provider's `base_url` and `experimental_bearer_token` lines, so users can switch between GPT subscription mode and relay mode without manually editing `config.toml`.
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

## OpenAI Quota Check

The quota query itself is read-only. It reads `~/.codex/auth.json`, extracts only the ChatGPT account id, email/plan claims, and access token needed for the request, then calls:

```text
GET https://chatgpt.com/backend-api/wham/usage
```

The request matches Codex's own ChatGPT usage path by sending `Authorization: Bearer <access_token>` and `ChatGPT-Account-Id`. The UI masks account identifiers and never prints tokens. If `auth.json` is missing, uses API-key auth, has expired tokens, or the OpenAI endpoint rejects the request, the app reports the exact safe failure summary and keeps the switch controls visible.

The switch controls are write operations and are protected by the same Codex process guard as provider repair and token injection. They only comment/uncomment `base_url` and `experimental_bearer_token` inside the selected `[model_providers.NAME]` section; they do not change `requires_openai_auth` or other provider fields.

## Runtime Requirements

End users do not need Node.js 24 for history sync. The upstream `codex-provider-sync` Node CLI requires Node.js 24+ because it uses the `node:sqlite` built-in module, but this app performs the sync inside the Tauri Rust backend with bundled SQLite through `rusqlite`.

## Development

```bash
npm install
npm run dev
```

On macOS external drives, AppleDouble `._*` sidecar files can break Tauri permission generation. The npm scripts run Tauri through `scripts/tauri-run.mjs`, which sets `CARGO_TARGET_DIR` to a system temp directory and `COPYFILE_DISABLE=1` to avoid that issue.

## Build Installers

```bash
npm run dist:mac
npm run dist:win
```

## Release Pipeline

GitHub Actions builds and publishes desktop releases with `.github/workflows/release.yml`.

- Pushing to `main` creates a release tag like `codex-api-tools-v__VERSION__-<run_number>`.
- Pushing a version tag such as `v0.1.1` publishes to that tag.
- The matrix builds macOS Apple Silicon, macOS Intel, and Windows x64 installers.
- macOS CI builds use ad-hoc signing. Windows artifacts are unsigned unless signing secrets are added later.

The packaged app is written with Tauri + React and is intended to run on both macOS and Windows.
