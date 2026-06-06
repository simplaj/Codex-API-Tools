# Codex API Tools

Windows/macOS desktop toolbox for Codex provider repair, history synchronization, and `experimental_bearer_token` wiring.

## Features

- Detects a Codex provider whose id or `name` is `OpenAI`.
- Renames that provider to a custom id/name, defaulting to `simplaj`.
- Syncs Codex history metadata with the built-in Rust/rusqlite engine, following the key behavior of `Dailin521/codex-provider-sync` without requiring `npx`.
- Backs up and removes `~/.codex/auth.json` for the remote/plugin login flow.
- Writes `experimental_bearer_token = "sk-..."` into a selected `[model_providers.NAME]` section.
- Backs up touched files under `~/.codex/backups_state/gpt-api-tools/<timestamp>`.

## User Flow

1. Open the app and refresh the current `~/.codex` state.
2. Run "one-click repair and sync" to rename the OpenAI provider to `simplaj` or another custom provider name.
3. Back up and remove `auth.json`, then restart Codex App and sign in with the GPT account that should unlock remote control/plugin features.
4. Load the backed-up Simplaj API key or enter one manually, then write it as `experimental_bearer_token` under the target provider.
5. Restart Codex App again so the new provider/auth combination is picked up.

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
- Pushing a version tag such as `v0.1.0` publishes to that tag.
- The matrix builds macOS Apple Silicon, macOS Intel, and Windows x64 installers.
- macOS CI builds use ad-hoc signing. Windows artifacts are unsigned unless signing secrets are added later.

The packaged app is written with Tauri + React and is intended to run on both macOS and Windows.
