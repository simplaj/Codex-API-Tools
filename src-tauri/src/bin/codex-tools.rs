use base64::{engine::general_purpose::STANDARD, Engine};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand::{rngs::OsRng, RngCore};
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{self, BufRead, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[path = "../local_tools.rs"]
mod local_tools;

const SESSION_DIRS: &[&str] = &["sessions", "archived_sessions"];
const ENVELOPE_FORMAT: &str = "codex-tools-session-v1";
const USER_AGENT_VALUE: &str = "codex-tools-cloud-sync/0.1.1";
const DEFAULT_API_URL: &str = "https://codex-tools-sync-api.821099891.workers.dev";
const DEFAULT_INVITE_CODE: &str = "sub2api.simplaj.top";
const DEFAULT_HTTP_RETRIES: usize = 4;
const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 180;
const DEFAULT_HTTP_CONNECT_TIMEOUT_SECS: u64 = 20;
const MAX_SINGLE_UPLOAD_BYTES: usize = 50 * 1024 * 1024;
const DEFAULT_CHUNK_SIZE_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_CHUNK_UPLOAD_CONCURRENCY: usize = 4;
const MAX_CHUNK_UPLOAD_CONCURRENCY: usize = 16;
const DEFAULT_CHUNK_DOWNLOAD_CONCURRENCY: usize = 4;
const MAX_CHUNK_DOWNLOAD_CONCURRENCY: usize = 16;
const DEFAULT_SESSION_UPLOAD_CONCURRENCY: usize = 2;
const MAX_SESSION_UPLOAD_CONCURRENCY: usize = 8;
const MAX_DECOMPRESSED_SESSION_BYTES: usize = 512 * 1024 * 1024;
const CHUNK_MANIFEST_SUFFIX: &str = ".chunks.json";
const MAX_SESSION_LIST_PAGE_SIZE: usize = 500;

#[derive(Clone)]
struct ApiClient {
    api_url: String,
    device_token: Option<String>,
    sync_key: Option<String>,
    email: Option<String>,
    client: Client,
}

#[derive(Debug, Clone)]
struct RegistrationRequest {
    email: String,
    device_name: String,
    platform: String,
    invite_code: Option<String>,
    sync_key_proof: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalSessionMeta {
    session_id: String,
    relative_path: String,
    source_dir: String,
    title: Option<String>,
    cwd: Option<String>,
    provider_name: Option<String>,
    model: Option<String>,
    modified_at_ms: u128,
    size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionManifest {
    format: String,
    session_id: String,
    relative_path: String,
    source_dir: String,
    title: Option<String>,
    cwd: Option<String>,
    provider_name: Option<String>,
    model: Option<String>,
    raw_sha256: String,
    encrypted_sha256: String,
    encrypted_size: usize,
    blob_key: String,
    uploaded_at_ms: u128,
    device_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EncryptedEnvelopeHeader {
    format: String,
    cipher: String,
    compression: String,
    nonce: String,
    raw_sha256: String,
    created_at_ms: u128,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloudConfig {
    api_url: Option<String>,
    device_token: Option<String>,
    sync_key: Option<String>,
    email: Option<String>,
    user_id: Option<String>,
    device_id: Option<String>,
    device_name: Option<String>,
    saved_at_ms: Option<u128>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiStatusResponse {
    ok: bool,
    error: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiRegisterResponse {
    ok: bool,
    error: Option<String>,
    message: Option<String>,
    user_id: Option<String>,
    device_id: Option<String>,
    device_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiSessionsResponse {
    ok: bool,
    error: Option<String>,
    message: Option<String>,
    sessions: Option<Vec<SessionManifest>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiManifestResponse {
    ok: bool,
    error: Option<String>,
    message: Option<String>,
    manifest: Option<SessionManifest>,
    skipped: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChunkDescriptor {
    index: usize,
    size: usize,
    sha256: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChunkCompleteRequest {
    chunks: Vec<ChunkDescriptor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiChunkManifestResponse {
    ok: bool,
    error: Option<String>,
    message: Option<String>,
    chunks: Option<Vec<ChunkDescriptor>>,
}

#[derive(Debug, Clone, Copy)]
enum PushSessionOutcome {
    Uploaded,
    Skipped,
}

#[derive(Debug, Clone, Copy)]
struct UploadOptions {
    session_concurrency: usize,
    chunk_concurrency: usize,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("status") => run_local_status(),
        Some("provider") => run_provider(&args[1..]),
        Some("auth") => run_auth(&args[1..]),
        Some("relay") => run_relay(&args[1..]),
        Some("quota") => run_quota(&args[1..]),
        Some("codex") => run_codex(&args[1..]),
        Some("sessions") => run_sessions(&args[1..]),
        Some("cloud") => run_cloud(&args[1..]),
        Some("help") | Some("--help") | Some("-h") | None => {
            print_help();
            Ok(())
        }
        Some(other) => Err(format!("unknown command: {other}\n\n{}", help_text())),
    }
}

fn run_local_status() -> Result<(), String> {
    println!("{}", local_tools::inspect_text()?);
    Ok(())
}

fn run_provider(args: &[String]) -> Result<(), String> {
    let output = match args.first().map(String::as_str) {
        Some("status") | None => local_tools::provider_status_text(),
        Some("sync") => local_tools::provider_sync_text(option_value(args, "--provider")),
        Some("switch") => local_tools::provider_switch_text(
            option_value(args, "--provider").or_else(|| args.get(1).cloned()),
        ),
        Some("repair") => local_tools::repair_provider_text(
            option_value(args, "--name").or_else(|| option_value(args, "--provider")),
            !flag(args, "--no-sync"),
        ),
        Some(other) => Err(format!(
            "unknown provider command: {other}\n\n{}",
            help_text()
        )),
    }?;
    println!("{output}");
    Ok(())
}

fn run_auth(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("unlock") | Some("remove") | Some("reset") => {
            println!("{}", local_tools::backup_remove_auth_text()?);
            println!();
            println!("Next: restart Codex, sign in with the ChatGPT account that should unlock remote control/plugins, then fully quit Codex before writing the relay key.");
            println!("Then run: codex-tools auth token --provider <provider>");
            Ok(())
        }
        Some("token") | Some("apply-token") => {
            let provider = option_value(args, "--provider");
            let token = option_value(args, "--key")
                .or_else(|| optional_env("CODEX_TOOLS_RELAY_API_KEY"))
                .unwrap_or_else(prompt_api_key);
            println!(
                "{}",
                local_tools::apply_experimental_token_text(provider, token)?
            );
            println!("Restart Codex for the auth/provider change to take effect.");
            Ok(())
        }
        Some(other) => Err(format!("unknown auth command: {other}\n\n{}", help_text())),
        None => Err(format!("missing auth command\n\n{}", help_text())),
    }
}

fn run_relay(args: &[String]) -> Result<(), String> {
    let provider = option_value(args, "--provider");
    let output = match args.first().map(String::as_str) {
        Some("gpt") | Some("subscription") | Some("off") => {
            local_tools::relay_toggle_text(provider, true)
        }
        Some("relay") | Some("restore") | Some("on") => {
            local_tools::relay_toggle_text(provider, false)
        }
        Some(other) => Err(format!("unknown relay command: {other}\n\n{}", help_text())),
        None => Err(format!("missing relay command\n\n{}", help_text())),
    }?;
    println!("{output}");
    println!("Restart Codex for the config change to take effect.");
    Ok(())
}

fn run_quota(args: &[String]) -> Result<(), String> {
    println!(
        "{}",
        local_tools::quota_text(flag(args, "--json"), flag(args, "--raw-json"))?
    );
    Ok(())
}

fn run_codex(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("quit") => {
            println!("{}", local_tools::quit_codex_text()?);
            Ok(())
        }
        Some(other) => Err(format!("unknown codex command: {other}\n\n{}", help_text())),
        None => Err(format!("missing codex command\n\n{}", help_text())),
    }
}

fn run_sessions(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("list") => {
            let codex_home = option_path(args, "--codex-home")?.unwrap_or(codex_home()?);
            let json = flag(args, "--json");
            let limit = option_usize(args, "--limit")?.unwrap_or(50);
            let mut sessions = collect_local_session_metas(&codex_home)?;
            let total = sessions.len();
            sessions.truncate(limit);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&sessions).map_err(to_error)?
                );
                return Ok(());
            }
            println!("local sessions: {total}");
            for session in &sessions {
                println!(
                    "{}  {}  {}  {}",
                    session.session_id,
                    session.provider_name.as_deref().unwrap_or("-"),
                    session.model.as_deref().unwrap_or("-"),
                    session.relative_path
                );
            }
            if total > sessions.len() {
                println!("... {} more", total - sessions.len());
            }
            Ok(())
        }
        Some(other) => Err(format!("unknown sessions command: {other}")),
        None => Err("missing sessions command. Try: codex-tools sessions list".into()),
    }
}

fn run_cloud(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("login") => cloud_login(args),
        Some("logout") => cloud_logout(),
        Some("status") => cloud_status(),
        Some("smoke") => cloud_smoke(),
        Some("push") => cloud_push(args),
        Some("list") => cloud_list(args),
        Some("pull") => cloud_pull(args),
        Some(other) => Err(format!("unknown cloud command: {other}")),
        None => Err("missing cloud command. Try: codex-tools cloud smoke".into()),
    }
}

fn cloud_smoke() -> Result<(), String> {
    let api = ApiClient::from_config_or_env()?;
    let status = api.health()?;
    if !status.ok {
        return Err(api_error("health", status.error, status.message));
    }
    println!("API smoke test ok: {}", api.api_url);
    Ok(())
}

fn cloud_push(args: &[String]) -> Result<(), String> {
    let api = ApiClient::from_config_or_env()?;
    cloud_push_api(args, &api)
}

fn cloud_list(args: &[String]) -> Result<(), String> {
    let api = ApiClient::from_config_or_env()?;
    cloud_list_api(args, &api)
}

fn cloud_pull(args: &[String]) -> Result<(), String> {
    let api = ApiClient::from_config_or_env()?;
    cloud_pull_api(args, &api)
}

fn cloud_login(args: &[String]) -> Result<(), String> {
    let existing = load_cloud_config()?.unwrap_or_default();
    let api_url = option_value(args, "--api-url")
        .or_else(|| optional_env("CODEX_TOOLS_API_URL"))
        .or(existing.api_url.clone())
        .unwrap_or_else(|| DEFAULT_API_URL.into())
        .trim_end_matches('/')
        .to_string();
    let email = option_value(args, "--email")
        .or_else(|| prompt_required("Email: ").ok())
        .ok_or_else(|| "cloud login requires an email".to_string())?;
    let device_name = option_value(args, "--device").unwrap_or_else(default_device_name);
    let platform = option_value(args, "--platform").unwrap_or_else(default_platform);
    let sync_key = option_value(args, "--key")
        .or_else(|| optional_env("CODEX_TOOLS_SYNC_KEY"))
        .unwrap_or_else(|| prompt_sync_key());
    if sync_key.trim().is_empty() {
        return Err("cloud login requires a non-empty sync key".into());
    }

    let existing_device_token =
        optional_env("CODEX_TOOLS_DEVICE_TOKEN").or_else(|| existing.device_token.clone());
    let api = ApiClient::new(
        api_url.clone(),
        existing_device_token,
        Some(sync_key.clone()),
        Some(email.clone()),
    )?;
    let registration = registration_request(
        args,
        email.clone(),
        device_name.clone(),
        platform,
        Some(derive_sync_key_proof(&email, &sync_key)),
    );
    let result = api.register(&registration)?;
    if !result.ok {
        return Err(api_error("login", result.error, result.message));
    }
    let user_id = result
        .user_id
        .ok_or_else(|| "login response missing userId".to_string())?;
    let device_id = result
        .device_id
        .ok_or_else(|| "login response missing deviceId".to_string())?;
    let device_token = result
        .device_token
        .ok_or_else(|| "login response missing deviceToken".to_string())?;
    let config = CloudConfig {
        api_url: Some(api_url.clone()),
        device_token: Some(device_token),
        sync_key: Some(sync_key),
        email: Some(email),
        user_id: Some(user_id.clone()),
        device_id: Some(device_id.clone()),
        device_name: Some(device_name),
        saved_at_ms: Some(unix_millis()),
    };
    let path = save_cloud_config(&config)?;
    println!("cloud login ok");
    println!("user: {user_id}");
    println!("device: {device_id}");
    println!("api: {api_url}");
    println!("config: {}", path.display());
    Ok(())
}

fn cloud_logout() -> Result<(), String> {
    let path = cloud_config_path()?;
    match fs::remove_file(&path) {
        Ok(()) => println!("cloud logout ok: removed {}", path.display()),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            println!("cloud logout ok: no local config at {}", path.display())
        }
        Err(error) => return Err(error.to_string()),
    }
    Ok(())
}

fn cloud_status() -> Result<(), String> {
    let path = cloud_config_path()?;
    let config = load_cloud_config()?.unwrap_or_default();
    let api_url = optional_env("CODEX_TOOLS_API_URL")
        .or_else(|| config.api_url.clone())
        .unwrap_or_else(|| DEFAULT_API_URL.into());
    let device_token =
        optional_env("CODEX_TOOLS_DEVICE_TOKEN").or_else(|| config.device_token.clone());
    let sync_key = optional_env("CODEX_TOOLS_SYNC_KEY").or_else(|| config.sync_key.clone());
    println!("api: {api_url}");
    println!("config: {}", path.display());
    let email = optional_env("CODEX_TOOLS_EMAIL").or_else(|| config.email.clone());
    println!("email: {}", email.as_deref().unwrap_or("not saved"));
    println!(
        "device: {}",
        config.device_id.as_deref().unwrap_or("not saved")
    );
    println!(
        "device token: {}",
        device_token
            .as_deref()
            .map(mask_token)
            .unwrap_or_else(|| "missing".into())
    );
    println!(
        "sync key: {}",
        if sync_key.is_some() {
            "saved"
        } else {
            "missing"
        }
    );
    println!(
        "auth: {}",
        if device_token.is_some() && sync_key.is_some() && email.is_some() {
            "ready (device token + sync key proof)"
        } else {
            "incomplete; run `codex-tools cloud login` again"
        }
    );
    Ok(())
}

fn cloud_push_api(args: &[String], api: &ApiClient) -> Result<(), String> {
    let sync_key = api.required_sync_key()?;
    let codex_home = option_path(args, "--codex-home")?.unwrap_or(codex_home()?);
    let session_filter = option_value(args, "--session-id");
    let all = flag(args, "--all");
    let force = flag(args, "--force");
    let limit = option_usize(args, "--limit")?;
    let device_name = option_value(args, "--device").unwrap_or_else(default_device_name);
    let upload_options = UploadOptions {
        session_concurrency: concurrency_option(
            args,
            &["--session-concurrency", "--session-threads"],
            &["-n", "--n", "--threads"],
            "CODEX_TOOLS_SESSION_UPLOAD_CONCURRENCY",
            DEFAULT_SESSION_UPLOAD_CONCURRENCY,
            MAX_SESSION_UPLOAD_CONCURRENCY,
        )?,
        chunk_concurrency: concurrency_option(
            args,
            &["--chunk-concurrency", "--chunk-threads"],
            &["-n", "--n", "--threads"],
            "CODEX_TOOLS_CHUNK_UPLOAD_CONCURRENCY",
            DEFAULT_CHUNK_UPLOAD_CONCURRENCY,
            MAX_CHUNK_UPLOAD_CONCURRENCY,
        )?,
    };

    if session_filter.is_none() && !all {
        return Err("cloud push requires --all or --session-id <id>".into());
    }

    let mut sessions = collect_local_session_metas(&codex_home)?;
    if let Some(session_id) = session_filter {
        sessions.retain(|session| session.session_id == session_id);
    }
    if let Some(limit) = limit {
        sessions.truncate(limit);
    }
    if sessions.is_empty() {
        return Err("no local sessions matched the push filter".into());
    }

    let session_groups = group_local_sessions_by_id(sessions);
    let session_concurrency = upload_options.session_concurrency.min(session_groups.len());
    println!(
        "session upload concurrency: {} parallel session group(s)",
        session_concurrency
    );
    let mut uploaded = 0usize;
    let mut skipped = 0usize;
    let local_seen = Arc::new(Mutex::new(HashSet::new()));
    let mut session_iter = session_groups.into_iter();
    loop {
        let batch: Vec<_> = session_iter.by_ref().take(session_concurrency).collect();
        if batch.is_empty() {
            break;
        }
        let batch_results = thread::scope(|scope| {
            let mut handles = Vec::with_capacity(batch.len());
            for sessions in batch {
                let api = api.clone();
                let sync_key = sync_key.to_string();
                let codex_home = codex_home.clone();
                let device_name = device_name.clone();
                let local_seen = Arc::clone(&local_seen);
                let upload_options = upload_options;
                handles.push(scope.spawn(move || {
                    push_local_session_group(
                        api,
                        sync_key,
                        codex_home,
                        device_name,
                        sessions,
                        force,
                        local_seen,
                        upload_options,
                    )
                }));
            }
            let mut results = Vec::with_capacity(handles.len());
            for handle in handles {
                results.push(
                    handle
                        .join()
                        .unwrap_or_else(|_| Err("session upload worker panicked".into())),
                );
            }
            results
        });
        for result in batch_results {
            let (batch_uploaded, batch_skipped) = result?;
            uploaded += batch_uploaded;
            skipped += batch_skipped;
        }
    }

    println!("cloud push ok: uploaded {uploaded}, skipped {skipped}");
    Ok(())
}

fn group_local_sessions_by_id(sessions: Vec<LocalSessionMeta>) -> Vec<Vec<LocalSessionMeta>> {
    let mut groups: Vec<Vec<LocalSessionMeta>> = Vec::new();
    let mut group_indexes: HashMap<String, usize> = HashMap::new();
    for session in sessions {
        if let Some(index) = group_indexes.get(&session.session_id).copied() {
            groups[index].push(session);
            continue;
        }
        group_indexes.insert(session.session_id.clone(), groups.len());
        groups.push(vec![session]);
    }
    for group in &mut groups {
        group.sort_by(|left, right| {
            left.modified_at_ms
                .cmp(&right.modified_at_ms)
                .then_with(|| left.relative_path.cmp(&right.relative_path))
        });
    }
    groups
}

fn push_local_session_group(
    api: ApiClient,
    sync_key: String,
    codex_home: PathBuf,
    device_name: String,
    sessions: Vec<LocalSessionMeta>,
    force: bool,
    local_seen: Arc<Mutex<HashSet<String>>>,
    upload_options: UploadOptions,
) -> Result<(usize, usize), String> {
    let mut uploaded = 0usize;
    let mut skipped = 0usize;
    let mut last_uploaded_at_ms = 0u128;
    for session in sessions {
        let uploaded_at_ms = unix_millis().max(last_uploaded_at_ms.saturating_add(1));
        last_uploaded_at_ms = uploaded_at_ms;
        match push_local_session(
            api.clone(),
            sync_key.clone(),
            codex_home.clone(),
            device_name.clone(),
            session,
            force,
            Arc::clone(&local_seen),
            uploaded_at_ms,
            upload_options,
        )? {
            PushSessionOutcome::Uploaded => uploaded += 1,
            PushSessionOutcome::Skipped => skipped += 1,
        }
    }
    Ok((uploaded, skipped))
}

fn push_local_session(
    api: ApiClient,
    sync_key: String,
    codex_home: PathBuf,
    device_name: String,
    session: LocalSessionMeta,
    force: bool,
    local_seen: Arc<Mutex<HashSet<String>>>,
    uploaded_at_ms: u128,
    upload_options: UploadOptions,
) -> Result<PushSessionOutcome, String> {
    let raw_path = codex_home.join(&session.relative_path);
    println!(
        "checking {}  {} bytes  {}",
        session.session_id, session.size, session.relative_path
    );
    let raw = fs::read(&raw_path).map_err(to_error)?;
    let raw_sha256 = sha256_hex(&raw);

    let version_key = format!("{}\0{}", session.session_id, raw_sha256);
    {
        let mut seen = local_seen
            .lock()
            .map_err(|_| "local version lock poisoned".to_string())?;
        if !seen.insert(version_key) {
            println!(
                "skipped {}: duplicate local version {}",
                session.session_id, raw_sha256
            );
            return Ok(PushSessionOutcome::Skipped);
        }
    }

    if !force {
        match api.latest_version_optional(&session.session_id) {
            Ok(Some(remote)) if remote.raw_sha256 == raw_sha256 => {
                println!(
                    "skipped {}: already uploaded {}",
                    session.session_id, raw_sha256
                );
                return Ok(PushSessionOutcome::Skipped);
            }
            Ok(_) => {}
            Err(error) => {
                return Err(format!(
                    "cannot verify remote state for {} before upload: {}\nRe-run with --force to upload without the preflight duplicate check.",
                    session.session_id, error
                ));
            }
        }
    }

    let encrypted = encrypt_payload(&raw, &sync_key)?;
    let encrypted_sha256 = sha256_hex(&encrypted);
    println!(
        "uploading {}: raw {}, encrypted {} bytes",
        session.session_id,
        raw_sha256,
        encrypted.len()
    );
    let manifest = SessionManifest {
        format: ENVELOPE_FORMAT.into(),
        session_id: session.session_id.clone(),
        relative_path: session.relative_path.clone(),
        source_dir: session.source_dir.clone(),
        title: session.title.clone(),
        cwd: session.cwd.clone(),
        provider_name: session.provider_name.clone(),
        model: session.model.clone(),
        raw_sha256: raw_sha256.clone(),
        encrypted_sha256,
        encrypted_size: encrypted.len(),
        blob_key: String::new(),
        uploaded_at_ms,
        device_name,
    };
    let result = api.put_version(
        &manifest,
        encrypted,
        force,
        upload_options.chunk_concurrency,
    )?;
    if !result.ok {
        return Err(api_error("push", result.error, result.message));
    }
    if result.skipped.unwrap_or(false) {
        println!(
            "skipped {}: already existed on API {}",
            session.session_id, raw_sha256
        );
        return Ok(PushSessionOutcome::Skipped);
    }
    println!("uploaded {} -> API {}", session.session_id, raw_sha256);
    Ok(PushSessionOutcome::Uploaded)
}

fn cloud_list_api(args: &[String], api: &ApiClient) -> Result<(), String> {
    let json_output = flag(args, "--json");
    let limit = option_usize(args, "--limit")?.unwrap_or(100);
    let result = api.list_sessions(limit)?;
    if !result.ok {
        return Err(api_error("list", result.error, result.message));
    }
    let manifests = result.sessions.unwrap_or_default();
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&manifests).map_err(to_error)?
        );
        return Ok(());
    }
    let latest = latest_manifests(manifests);
    println!("cloud sessions: {} latest version(s)", latest.len());
    for manifest in latest {
        println!("{}", manifest.session_id);
        println!(
            "  provider/model: {}/{}",
            manifest.provider_name.as_deref().unwrap_or("-"),
            manifest.model.as_deref().unwrap_or("-")
        );
        if let Some(title) = manifest.title.as_deref() {
            println!("  title: {title}");
        }
        if let Some(cwd) = manifest.cwd.as_deref() {
            println!("  cwd: {cwd}");
        }
        println!("  path: {}", manifest.relative_path);
        println!(
            "  raw/encrypted: {} / {} bytes",
            short_hash(&manifest.raw_sha256),
            manifest.encrypted_size
        );
        println!("  uploaded_at_ms: {}", manifest.uploaded_at_ms);
        println!(
            "  pull: codex-tools cloud pull --session-id {}",
            manifest.session_id
        );
    }
    println!("Use `codex-tools cloud pull --all` to restore every listed latest session.");
    Ok(())
}

fn cloud_pull_api(args: &[String], api: &ApiClient) -> Result<(), String> {
    let sync_key = api.required_sync_key()?;
    let codex_home = option_path(args, "--codex-home")?.unwrap_or(codex_home()?);
    let session_id = option_value(args, "--session-id");
    let all = flag(args, "--all");
    if all == session_id.is_some() {
        return Err("cloud pull requires exactly one of --all or --session-id <id>".into());
    }
    let force = flag(args, "--force");
    let dry_run = flag(args, "--dry-run");
    let total_limit = option_usize(args, "--limit")?;
    let download_concurrency = concurrency_option(
        args,
        &["--download-concurrency", "--download-threads"],
        &["-n", "--n", "--threads"],
        "CODEX_TOOLS_CHUNK_DOWNLOAD_CONCURRENCY",
        DEFAULT_CHUNK_DOWNLOAD_CONCURRENCY,
        MAX_CHUNK_DOWNLOAD_CONCURRENCY,
    )?;

    let manifests = if all {
        latest_manifests(api.list_all_sessions(total_limit)?)
    } else {
        let session_id = session_id.expect("validated above");
        let latest = api.latest_version(&session_id)?;
        if !latest.ok {
            return Err(api_error("latest", latest.error, latest.message));
        }
        vec![latest
            .manifest
            .ok_or_else(|| format!("no cloud manifest found for session {session_id}"))?]
    };
    if manifests.is_empty() {
        println!("no cloud sessions matched");
        return Ok(());
    }

    let mut restored = 0usize;
    let mut skipped = 0usize;
    let mut planned = 0usize;
    for manifest in manifests {
        match restore_cloud_manifest(
            api,
            sync_key,
            &codex_home,
            &manifest,
            force,
            dry_run,
            download_concurrency,
        )? {
            RestoreOutcome::Restored => restored += 1,
            RestoreOutcome::Skipped => skipped += 1,
            RestoreOutcome::DryRun => planned += 1,
        }
    }
    if dry_run {
        println!("cloud pull dry run ok: planned {planned}, skipped {skipped}");
    } else {
        println!("cloud pull ok: restored {restored}, skipped {skipped}");
    }
    println!(
        "run provider metadata sync after restore if this machine uses a different provider name"
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreOutcome {
    Restored,
    Skipped,
    DryRun,
}

fn restore_cloud_manifest(
    api: &ApiClient,
    sync_key: &str,
    codex_home: &Path,
    manifest: &SessionManifest,
    force: bool,
    dry_run: bool,
    download_concurrency: usize,
) -> Result<RestoreOutcome, String> {
    let target_path = codex_home.join(&manifest.relative_path);
    if target_path.exists() && !force {
        let existing = fs::read(&target_path).map_err(to_error)?;
        if sha256_hex(&existing) == manifest.raw_sha256 {
            println!("already restored: {}", target_path.display());
            return Ok(RestoreOutcome::Skipped);
        }
        return Err(format!(
            "{} already exists with different content. Use --force to overwrite.",
            target_path.display()
        ));
    }

    let encrypted = api.get_version_encrypted(&manifest, download_concurrency)?;
    if sha256_hex(&encrypted) != manifest.encrypted_sha256 {
        return Err("downloaded encrypted blob hash does not match manifest".into());
    }
    let raw = decrypt_payload(&encrypted, sync_key)?;
    if sha256_hex(&raw) != manifest.raw_sha256 {
        return Err("decrypted rollout hash does not match manifest".into());
    }

    if dry_run {
        println!(
            "dry run: would restore {} bytes to {}",
            raw.len(),
            target_path.display()
        );
        return Ok(RestoreOutcome::DryRun);
    }
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent).map_err(to_error)?;
    }
    fs::write(&target_path, raw).map_err(to_error)?;
    println!(
        "restored {} -> {}",
        manifest.session_id,
        target_path.display()
    );
    println!(
        "verified local decrypt/decompress: raw {}",
        short_hash(&manifest.raw_sha256)
    );
    Ok(RestoreOutcome::Restored)
}

fn latest_manifests(manifests: Vec<SessionManifest>) -> Vec<SessionManifest> {
    let mut by_session: HashMap<String, SessionManifest> = HashMap::new();
    for manifest in manifests {
        let replace = by_session
            .get(&manifest.session_id)
            .map(|existing| manifest.uploaded_at_ms > existing.uploaded_at_ms)
            .unwrap_or(true);
        if replace {
            by_session.insert(manifest.session_id.clone(), manifest);
        }
    }
    let mut output: Vec<_> = by_session.into_values().collect();
    output.sort_by(|left, right| {
        right
            .uploaded_at_ms
            .cmp(&left.uploaded_at_ms)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    output
}

fn collect_local_session_metas(codex_home: &Path) -> Result<Vec<LocalSessionMeta>, String> {
    let mut paths = Vec::new();
    for dir_name in SESSION_DIRS {
        list_rollout_files(&codex_home.join(dir_name), &mut paths)?;
    }
    let mut sessions = Vec::new();
    for path in paths {
        if let Some(session) = read_local_session_meta(codex_home, &path)? {
            sessions.push(session);
        }
    }
    sessions.sort_by(|left, right| {
        right
            .modified_at_ms
            .cmp(&left.modified_at_ms)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    Ok(sessions)
}

fn read_local_session_meta(
    codex_home: &Path,
    path: &Path,
) -> Result<Option<LocalSessionMeta>, String> {
    let file = fs::File::open(path).map_err(to_error)?;
    let mut reader = io::BufReader::new(file);
    let mut line = String::new();
    if reader.read_line(&mut line).map_err(to_error)? == 0 {
        return Ok(None);
    }
    let parsed: Value = match serde_json::from_str(line.trim_end_matches(&['\r', '\n'][..])) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    if parsed.get("type").and_then(Value::as_str) != Some("session_meta") {
        return Ok(None);
    }
    let Some(payload) = parsed.get("payload").and_then(Value::as_object) else {
        return Ok(None);
    };
    let Some(session_id) = payload
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    let metadata = fs::metadata(path).map_err(to_error)?;
    let modified_at_ms = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis())
        .unwrap_or(0);
    let relative_path = relative_path(codex_home, path)?;
    let source_dir = relative_path
        .split('/')
        .next()
        .unwrap_or("sessions")
        .to_string();
    Ok(Some(LocalSessionMeta {
        session_id: session_id.to_string(),
        relative_path,
        source_dir,
        title: payload
            .get("title")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        cwd: payload
            .get("cwd")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        provider_name: payload
            .get("model_provider")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        model: payload
            .get("model")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        modified_at_ms,
        size: metadata.len(),
    }))
}

fn list_rollout_files(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(to_error)?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(to_error)?;
        if file_type.is_dir() {
            list_rollout_files(&path, output)?;
        } else if file_type.is_file() {
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                output.push(path);
            }
        }
    }
    Ok(())
}

fn encrypt_payload(raw: &[u8], sync_key: &str) -> Result<Vec<u8>, String> {
    let compressed = zstd::bulk::compress(raw, 3).map_err(to_error)?;
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&derive_key(sync_key)));
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce), compressed.as_ref())
        .map_err(|error| error.to_string())?;
    let header = EncryptedEnvelopeHeader {
        format: ENVELOPE_FORMAT.into(),
        cipher: "XChaCha20-Poly1305".into(),
        compression: "zstd".into(),
        nonce: STANDARD.encode(nonce),
        raw_sha256: sha256_hex(raw),
        created_at_ms: unix_millis(),
    };
    let mut output = serde_json::to_vec(&header).map_err(to_error)?;
    output.push(b'\n');
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

fn decrypt_payload(envelope: &[u8], sync_key: &str) -> Result<Vec<u8>, String> {
    let newline = envelope
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| "encrypted payload missing JSON header".to_string())?;
    let header: EncryptedEnvelopeHeader =
        serde_json::from_slice(&envelope[..newline]).map_err(to_error)?;
    if header.format != ENVELOPE_FORMAT {
        return Err(format!(
            "unsupported encrypted payload format: {}",
            header.format
        ));
    }
    let nonce = STANDARD.decode(header.nonce.as_bytes()).map_err(to_error)?;
    if nonce.len() != 24 {
        return Err("encrypted payload nonce has invalid length".into());
    }
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&derive_key(sync_key)));
    let compressed = cipher
        .decrypt(XNonce::from_slice(&nonce), &envelope[newline + 1..])
        .map_err(|_| {
            "decrypt failed: Sync Key does not match this cloud session. Run `codex-tools cloud logout`, then `codex-tools cloud login --email <email>` with the same Sync Key used when uploading."
                .to_string()
        })?;
    let raw =
        zstd::bulk::decompress(&compressed, MAX_DECOMPRESSED_SESSION_BYTES).map_err(to_error)?;
    if sha256_hex(&raw) != header.raw_sha256 {
        return Err("decrypted payload hash mismatch".into());
    }
    Ok(raw)
}

fn derive_key(sync_key: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"codex-tools-sync-key-v1\0");
    hasher.update(sync_key.as_bytes());
    hasher.finalize().into()
}

impl ApiClient {
    fn new(
        api_url: String,
        device_token: Option<String>,
        sync_key: Option<String>,
        email: Option<String>,
    ) -> Result<Self, String> {
        let timeout_secs = optional_env("CODEX_TOOLS_HTTP_TIMEOUT_SECS")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_HTTP_TIMEOUT_SECS);
        let connect_timeout_secs = optional_env("CODEX_TOOLS_HTTP_CONNECT_TIMEOUT_SECS")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_HTTP_CONNECT_TIMEOUT_SECS);
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .connect_timeout(Duration::from_secs(connect_timeout_secs))
            .build()
            .map_err(to_error)?;
        Ok(Self {
            api_url: api_url.trim_end_matches('/').to_string(),
            device_token,
            sync_key,
            email,
            client,
        })
    }

    fn from_config_or_env() -> Result<Self, String> {
        let config = load_cloud_config()?.unwrap_or_default();
        let api_url = optional_env("CODEX_TOOLS_API_URL")
            .or(config.api_url)
            .unwrap_or_else(|| DEFAULT_API_URL.into());
        let device_token = optional_env("CODEX_TOOLS_DEVICE_TOKEN").or(config.device_token);
        let sync_key = optional_env("CODEX_TOOLS_SYNC_KEY").or(config.sync_key);
        let email = optional_env("CODEX_TOOLS_EMAIL").or(config.email);
        Self::new(api_url, device_token, sync_key, email)
    }

    fn health(&self) -> Result<ApiStatusResponse, String> {
        let response = self.send_with_retries("health", || {
            self.client
                .get(self.url("/v1/health"))
                .header("user-agent", USER_AGENT_VALUE)
        })?;
        read_json_response(response, "health")
    }

    fn register(&self, registration: &RegistrationRequest) -> Result<ApiRegisterResponse, String> {
        let payload = json!({
            "email": registration.email.as_str(),
            "deviceName": registration.device_name.as_str(),
            "platform": registration.platform.as_str(),
            "inviteCode": registration.invite_code.as_deref(),
            "syncKeyProof": registration.sync_key_proof.as_deref()
        });
        let response = self.send_with_retries("register", || {
            let request = self
                .client
                .post(self.url("/v1/devices/register"))
                .header("user-agent", USER_AGENT_VALUE)
                .json(&payload);
            match self.device_token.as_deref() {
                Some(token) => request.bearer_auth(token),
                None => request,
            }
        })?;
        read_json_response(response, "register")
    }

    fn list_sessions(&self, limit: usize) -> Result<ApiSessionsResponse, String> {
        self.list_sessions_page(limit, 0)
    }

    fn list_all_sessions(
        &self,
        total_limit: Option<usize>,
    ) -> Result<Vec<SessionManifest>, String> {
        if total_limit == Some(0) {
            return Ok(Vec::new());
        }
        let mut sessions = Vec::new();
        let mut offset = 0usize;
        loop {
            let remaining = total_limit
                .map(|limit| limit.saturating_sub(sessions.len()))
                .unwrap_or(MAX_SESSION_LIST_PAGE_SIZE);
            if remaining == 0 {
                break;
            }
            let page_size = remaining.min(MAX_SESSION_LIST_PAGE_SIZE);
            let result = self.list_sessions_page(page_size, offset)?;
            if !result.ok {
                return Err(api_error("list", result.error, result.message));
            }
            let page = result.sessions.unwrap_or_default();
            let page_len = page.len();
            sessions.extend(page);
            if page_len < page_size {
                break;
            }
            offset += page_len;
        }
        Ok(sessions)
    }

    fn list_sessions_page(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<ApiSessionsResponse, String> {
        let (token, sync_key_proof) = self.required_auth_headers()?;
        let limit = limit.clamp(1, MAX_SESSION_LIST_PAGE_SIZE);
        let response = self.send_with_retries("list sessions", || {
            self.client
                .get(self.url(&format!("/v1/sessions?limit={limit}&offset={offset}")))
                .header("user-agent", USER_AGENT_VALUE)
                .header("x-codex-tools-sync-key-proof", sync_key_proof.as_str())
                .bearer_auth(token.as_str())
        })?;
        read_json_response(response, "list sessions")
    }

    fn put_version(
        &self,
        manifest: &SessionManifest,
        encrypted: Vec<u8>,
        force: bool,
        chunk_concurrency: usize,
    ) -> Result<ApiManifestResponse, String> {
        if encrypted.len() > MAX_SINGLE_UPLOAD_BYTES {
            return self.put_chunked_version(manifest, encrypted, force, chunk_concurrency);
        }
        let manifest_header = STANDARD.encode(serde_json::to_vec(manifest).map_err(to_error)?);
        let path = format!(
            "/v1/sessions/{}/versions/{}",
            percent_encode_path(&manifest.session_id),
            manifest.raw_sha256
        );
        let (token, sync_key_proof) = self.required_auth_headers()?;
        let response = self.send_with_retries("put version", || {
            self.client
                .put(self.url(&path))
                .header("user-agent", USER_AGENT_VALUE)
                .header("content-type", "application/octet-stream")
                .header("x-codex-tools-manifest", manifest_header.as_str())
                .header("x-codex-tools-sync-key-proof", sync_key_proof.as_str())
                .header("x-codex-tools-force", if force { "true" } else { "false" })
                .bearer_auth(token.as_str())
                .body(encrypted.clone())
        })?;
        read_json_response(response, "put version")
    }

    fn put_chunked_version(
        &self,
        manifest: &SessionManifest,
        encrypted: Vec<u8>,
        force: bool,
        chunk_concurrency: usize,
    ) -> Result<ApiManifestResponse, String> {
        let chunk_size = optional_env("CODEX_TOOLS_CHUNK_SIZE_BYTES")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0 && *value <= MAX_SINGLE_UPLOAD_BYTES)
            .unwrap_or(DEFAULT_CHUNK_SIZE_BYTES);
        let chunk_count = encrypted.len().div_ceil(chunk_size);
        let concurrency = chunk_concurrency
            .max(1)
            .min(MAX_CHUNK_UPLOAD_CONCURRENCY)
            .min(chunk_count);
        println!(
            "chunked upload {}: {} bytes in {} chunk(s) of up to {} bytes, {} parallel upload(s)",
            manifest.session_id,
            encrypted.len(),
            chunk_count,
            chunk_size,
            concurrency
        );
        let mut chunks = Vec::with_capacity(chunk_count);
        let mut chunk_iter = encrypted.chunks(chunk_size).enumerate();
        loop {
            let batch: Vec<_> = chunk_iter.by_ref().take(concurrency).collect();
            if batch.is_empty() {
                break;
            }
            let batch_results = thread::scope(|scope| {
                let mut handles = Vec::with_capacity(batch.len());
                for (index, chunk) in batch {
                    let client = self.clone();
                    let manifest = manifest.clone();
                    handles.push(scope.spawn(move || {
                        let sha256 = sha256_hex(chunk);
                        client.put_chunk(&manifest, index, chunk, &sha256)?;
                        Ok::<ChunkDescriptor, String>(ChunkDescriptor {
                            index,
                            size: chunk.len(),
                            sha256,
                        })
                    }));
                }
                let mut results = Vec::with_capacity(handles.len());
                for handle in handles {
                    results.push(
                        handle
                            .join()
                            .unwrap_or_else(|_| Err("chunk upload worker panicked".into())),
                    );
                }
                results
            });
            for result in batch_results {
                let descriptor = result?;
                println!(
                    "uploaded chunk {}/{} for {}",
                    descriptor.index + 1,
                    chunk_count,
                    manifest.session_id
                );
                chunks.push(descriptor);
            }
        }
        chunks.sort_by_key(|chunk| chunk.index);
        self.complete_chunked_version(manifest, chunks, force)
    }

    fn put_chunk(
        &self,
        manifest: &SessionManifest,
        index: usize,
        chunk: &[u8],
        sha256: &str,
    ) -> Result<ApiStatusResponse, String> {
        let manifest_header = STANDARD.encode(serde_json::to_vec(manifest).map_err(to_error)?);
        let path = format!(
            "/v1/sessions/{}/versions/{}/chunks/{}",
            percent_encode_path(&manifest.session_id),
            manifest.raw_sha256,
            index
        );
        let (token, sync_key_proof) = self.required_auth_headers()?;
        let body = chunk.to_vec();
        let response = self.send_with_retries("put chunk", || {
            self.client
                .put(self.url(&path))
                .header("user-agent", USER_AGENT_VALUE)
                .header("content-type", "application/octet-stream")
                .header("x-codex-tools-manifest", manifest_header.as_str())
                .header("x-codex-tools-sync-key-proof", sync_key_proof.as_str())
                .header("x-codex-tools-chunk-sha256", sha256)
                .header("x-codex-tools-chunk-size", chunk.len().to_string())
                .bearer_auth(token.as_str())
                .body(body.clone())
        })?;
        let result: ApiStatusResponse = read_json_response(response, "put chunk")?;
        if !result.ok {
            return Err(api_error("put chunk", result.error, result.message));
        }
        Ok(result)
    }

    fn complete_chunked_version(
        &self,
        manifest: &SessionManifest,
        chunks: Vec<ChunkDescriptor>,
        force: bool,
    ) -> Result<ApiManifestResponse, String> {
        let manifest_header = STANDARD.encode(serde_json::to_vec(manifest).map_err(to_error)?);
        let path = format!(
            "/v1/sessions/{}/versions/{}/chunks/complete",
            percent_encode_path(&manifest.session_id),
            manifest.raw_sha256
        );
        let (token, sync_key_proof) = self.required_auth_headers()?;
        let payload = ChunkCompleteRequest { chunks };
        let response = self.send_with_retries("complete chunked version", || {
            self.client
                .post(self.url(&path))
                .header("user-agent", USER_AGENT_VALUE)
                .header("x-codex-tools-manifest", manifest_header.as_str())
                .header("x-codex-tools-sync-key-proof", sync_key_proof.as_str())
                .header("x-codex-tools-force", if force { "true" } else { "false" })
                .bearer_auth(token.as_str())
                .json(&payload)
        })?;
        read_json_response(response, "complete chunked version")
    }

    fn latest_version(&self, session_id: &str) -> Result<ApiManifestResponse, String> {
        let response = self.latest_version_response(session_id)?;
        read_json_response(response, "latest version")
    }

    fn latest_version_optional(&self, session_id: &str) -> Result<Option<SessionManifest>, String> {
        let response = self.latest_version_response(session_id)?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        let result: ApiManifestResponse = read_json_response(response, "latest version")?;
        if !result.ok {
            return Err(api_error("latest", result.error, result.message));
        }
        Ok(result.manifest)
    }

    fn latest_version_response(&self, session_id: &str) -> Result<Response, String> {
        let path = format!(
            "/v1/sessions/{}/versions/latest",
            percent_encode_path(session_id)
        );
        let (token, sync_key_proof) = self.required_auth_headers()?;
        self.send_with_retries("latest version", || {
            self.client
                .get(self.url(&path))
                .header("user-agent", USER_AGENT_VALUE)
                .header("x-codex-tools-sync-key-proof", sync_key_proof.as_str())
                .bearer_auth(token.as_str())
        })
    }

    fn get_blob(&self, session_id: &str, raw_sha256: &str) -> Result<Vec<u8>, String> {
        let path = format!(
            "/v1/sessions/{}/versions/{}/blob",
            percent_encode_path(session_id),
            raw_sha256
        );
        let (token, sync_key_proof) = self.required_auth_headers()?;
        let response = self.send_with_retries("get blob", || {
            self.client
                .get(self.url(&path))
                .header("user-agent", USER_AGENT_VALUE)
                .header("x-codex-tools-sync-key-proof", sync_key_proof.as_str())
                .bearer_auth(token.as_str())
        })?;
        read_success_bytes(response, "get blob")
    }

    fn get_version_encrypted(
        &self,
        manifest: &SessionManifest,
        download_concurrency: usize,
    ) -> Result<Vec<u8>, String> {
        if manifest.blob_key.ends_with(CHUNK_MANIFEST_SUFFIX) {
            match self.get_chunk_manifest(&manifest.session_id, &manifest.raw_sha256) {
                Ok(chunks) => {
                    return self.get_chunked_blob(manifest, chunks, download_concurrency);
                }
                Err(error) => {
                    eprintln!(
                        "chunk manifest unavailable, falling back to sequential blob download: {error}"
                    );
                }
            }
        }
        self.get_blob(&manifest.session_id, &manifest.raw_sha256)
    }

    fn get_chunk_manifest(
        &self,
        session_id: &str,
        raw_sha256: &str,
    ) -> Result<Vec<ChunkDescriptor>, String> {
        let path = format!(
            "/v1/sessions/{}/versions/{}/chunks/manifest",
            percent_encode_path(session_id),
            raw_sha256
        );
        let (token, sync_key_proof) = self.required_auth_headers()?;
        let response = self.send_with_retries("get chunk manifest", || {
            self.client
                .get(self.url(&path))
                .header("user-agent", USER_AGENT_VALUE)
                .header("x-codex-tools-sync-key-proof", sync_key_proof.as_str())
                .bearer_auth(token.as_str())
        })?;
        let result: ApiChunkManifestResponse = read_json_response(response, "get chunk manifest")?;
        if !result.ok {
            return Err(api_error(
                "get chunk manifest",
                result.error,
                result.message,
            ));
        }
        let chunks = result
            .chunks
            .ok_or_else(|| "get chunk manifest failed: missing chunks".to_string())?;
        validate_chunk_descriptors(&chunks)?;
        Ok(chunks)
    }

    fn get_chunk(
        &self,
        session_id: &str,
        raw_sha256: &str,
        index: usize,
    ) -> Result<Vec<u8>, String> {
        let path = format!(
            "/v1/sessions/{}/versions/{}/chunks/{}",
            percent_encode_path(session_id),
            raw_sha256,
            index
        );
        let (token, sync_key_proof) = self.required_auth_headers()?;
        let response = self.send_with_retries("get chunk", || {
            self.client
                .get(self.url(&path))
                .header("user-agent", USER_AGENT_VALUE)
                .header("x-codex-tools-sync-key-proof", sync_key_proof.as_str())
                .bearer_auth(token.as_str())
        })?;
        read_success_bytes(response, "get chunk")
    }

    fn get_chunked_blob(
        &self,
        manifest: &SessionManifest,
        chunks: Vec<ChunkDescriptor>,
        download_concurrency: usize,
    ) -> Result<Vec<u8>, String> {
        let chunk_count = chunks.len();
        let expected_size: usize = chunks.iter().map(|chunk| chunk.size).sum();
        if expected_size != manifest.encrypted_size {
            return Err(format!(
                "chunk manifest encrypted size mismatch: expected {}, got {}",
                manifest.encrypted_size, expected_size
            ));
        }
        let concurrency = download_concurrency
            .max(1)
            .min(MAX_CHUNK_DOWNLOAD_CONCURRENCY)
            .min(chunk_count);
        println!(
            "chunked download {}: {} bytes in {} chunk(s), {} parallel download(s)",
            manifest.session_id, manifest.encrypted_size, chunk_count, concurrency
        );
        let mut results = Vec::with_capacity(chunk_count);
        let mut chunk_iter = chunks.into_iter();
        loop {
            let batch: Vec<_> = chunk_iter.by_ref().take(concurrency).collect();
            if batch.is_empty() {
                break;
            }
            let batch_results = thread::scope(|scope| {
                let mut handles = Vec::with_capacity(batch.len());
                for descriptor in batch {
                    let client = self.clone();
                    let session_id = manifest.session_id.clone();
                    let raw_sha256 = manifest.raw_sha256.clone();
                    handles.push(scope.spawn(move || {
                        let bytes = client.get_chunk(&session_id, &raw_sha256, descriptor.index)?;
                        if bytes.len() != descriptor.size {
                            return Err(format!(
                                "downloaded chunk {} size mismatch: expected {}, got {}",
                                descriptor.index,
                                descriptor.size,
                                bytes.len()
                            ));
                        }
                        let actual_sha256 = sha256_hex(&bytes);
                        if actual_sha256 != descriptor.sha256 {
                            return Err(format!(
                                "downloaded chunk {} hash mismatch",
                                descriptor.index
                            ));
                        }
                        Ok::<(usize, Vec<u8>), String>((descriptor.index, bytes))
                    }));
                }
                let mut scoped_results = Vec::with_capacity(handles.len());
                for handle in handles {
                    scoped_results.push(
                        handle
                            .join()
                            .unwrap_or_else(|_| Err("chunk download worker panicked".into())),
                    );
                }
                scoped_results
            });
            for result in batch_results {
                let (index, bytes) = result?;
                println!(
                    "downloaded chunk {}/{} for {}",
                    index + 1,
                    chunk_count,
                    manifest.session_id
                );
                results.push((index, bytes));
            }
        }
        results.sort_by_key(|(index, _)| *index);
        let mut encrypted = Vec::with_capacity(manifest.encrypted_size);
        for (expected_index, (index, bytes)) in results.into_iter().enumerate() {
            if index != expected_index {
                return Err(format!(
                    "chunk download result order mismatch at index {expected_index}"
                ));
            }
            encrypted.extend(bytes);
        }
        if encrypted.len() != manifest.encrypted_size {
            return Err(format!(
                "downloaded encrypted size mismatch: expected {}, got {}",
                manifest.encrypted_size,
                encrypted.len()
            ));
        }
        Ok(encrypted)
    }

    fn send_with_retries<F>(&self, action: &str, mut build: F) -> Result<Response, String>
    where
        F: FnMut() -> reqwest::blocking::RequestBuilder,
    {
        let retries = optional_env("CODEX_TOOLS_HTTP_RETRIES")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(DEFAULT_HTTP_RETRIES);
        let max_attempts = retries.saturating_add(1).max(1);
        let mut last_error = None;
        for attempt in 1..=max_attempts {
            match build().send() {
                Ok(response)
                    if should_retry_status(response.status().as_u16())
                        && attempt < max_attempts =>
                {
                    let status = response.status();
                    eprintln!("{action}: HTTP {status}, retrying ({attempt}/{max_attempts})...");
                    sleep_before_retry(attempt);
                    continue;
                }
                Ok(response) => return Ok(response),
                Err(error) if attempt < max_attempts && is_retryable_reqwest_error(&error) => {
                    let message = network_error_message(error);
                    eprintln!("{action}: {message}, retrying ({attempt}/{max_attempts})...");
                    last_error = Some(message);
                    sleep_before_retry(attempt);
                }
                Err(error) => {
                    return Err(network_error_message(error));
                }
            }
        }
        Err(last_error.unwrap_or_else(|| format!("{action} failed after retries")))
    }

    fn required_device_token(&self) -> Result<&str, String> {
        self.device_token
            .as_deref()
            .ok_or_else(|| "not logged in. Run: codex-tools cloud login".to_string())
    }

    fn required_sync_key(&self) -> Result<&str, String> {
        self.sync_key
            .as_deref()
            .ok_or_else(|| "missing sync key. Run: codex-tools cloud login".to_string())
    }

    fn required_email(&self) -> Result<&str, String> {
        self.email
            .as_deref()
            .ok_or_else(|| "missing cloud email. Run: codex-tools cloud login".to_string())
    }

    fn required_auth_headers(&self) -> Result<(String, String), String> {
        let token = self.required_device_token()?.to_string();
        let email = self.required_email()?;
        let sync_key = self.required_sync_key()?;
        Ok((token, derive_sync_key_proof(email, sync_key)))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.api_url, path)
    }
}

fn read_success_bytes(
    response: reqwest::blocking::Response,
    action: &str,
) -> Result<Vec<u8>, String> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(format!("{action} failed: HTTP {status}: {body}"));
    }
    response
        .bytes()
        .map(|bytes| bytes.to_vec())
        .map_err(to_error)
}

fn read_json_response<T>(response: Response, action: &str) -> Result<T, String>
where
    T: for<'de> Deserialize<'de>,
{
    let status = response.status();
    let body = response.text().map_err(to_error)?;
    if !status.is_success() {
        return Err(format!("{action} failed: HTTP {status}: {body}"));
    }
    serde_json::from_str(&body).map_err(|error| format!("{action} returned invalid JSON: {error}"))
}

fn should_retry_status(status: u16) -> bool {
    matches!(
        status,
        408 | 425 | 429 | 500 | 502 | 503 | 504 | 520 | 521 | 522 | 523 | 524 | 525 | 526
    )
}

fn is_retryable_reqwest_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_request() || error.is_body()
}

fn sleep_before_retry(attempt: usize) {
    let shift = attempt.saturating_sub(1).min(4) as u32;
    let millis = 500u64.saturating_mul(2u64.saturating_pow(shift));
    thread::sleep(Duration::from_millis(millis));
}

fn network_error_message(error: reqwest::Error) -> String {
    let kind = if error.is_timeout() {
        "network timeout"
    } else if error.is_connect() {
        "network connect/TLS failure"
    } else if error.is_body() {
        "network stream/body interrupted"
    } else if error.is_request() {
        "network request failure"
    } else {
        "network failure"
    };
    format!(
        "{kind}: {error}. This can happen before the request reaches the Worker, so the server may not show an error log. Retry, or set HTTP_PROXY/HTTPS_PROXY if your network needs a proxy."
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn short_hash(value: &str) -> String {
    value.chars().take(12).collect()
}

fn relative_path(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path.strip_prefix(root).map_err(to_error)?;
    Ok(relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/"))
}

fn codex_home() -> Result<PathBuf, String> {
    if let Ok(value) = env::var("CODEX_HOME") {
        if !value.trim().is_empty() {
            return Ok(PathBuf::from(value));
        }
    }
    Ok(home_dir()?.join(".codex"))
}

fn home_dir() -> Result<PathBuf, String> {
    if let Some(home) = env::var_os("HOME") {
        return Ok(PathBuf::from(home));
    }
    if let Some(profile) = env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(profile));
    }
    Err("cannot locate home directory".into())
}

fn optional_env(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn cloud_config_path() -> Result<PathBuf, String> {
    Ok(codex_home()?.join("codex-api-tools-cloud.json"))
}

fn load_cloud_config() -> Result<Option<CloudConfig>, String> {
    let path = cloud_config_path()?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("invalid cloud config {}: {error}", path.display()))
}

fn save_cloud_config(config: &CloudConfig) -> Result<PathBuf, String> {
    let path = cloud_config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(to_error)?;
    }
    let bytes = serde_json::to_vec_pretty(config).map_err(to_error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(to_error)?;
        file.write_all(&bytes).map_err(to_error)?;
        file.sync_all().map_err(to_error)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(to_error)?;
    }
    #[cfg(not(unix))]
    {
        fs::write(&path, bytes).map_err(to_error)?;
    }
    Ok(path)
}

fn prompt_required(prompt: &str) -> Result<String, String> {
    print!("{prompt}");
    io::stdout().flush().map_err(to_error)?;
    let mut value = String::new();
    io::stdin().read_line(&mut value).map_err(to_error)?;
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err("empty input".into());
    }
    Ok(value)
}

fn prompt_sync_key() -> String {
    let first = match rpassword::prompt_password("Sync key: ") {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => prompt_required("Sync key: ").unwrap_or_default(),
    };
    if first.is_empty() {
        return first;
    }
    let second = match rpassword::prompt_password("Confirm sync key: ") {
        Ok(value) => value.trim().to_string(),
        Err(_) => first.clone(),
    };
    if first != second {
        eprintln!("sync key confirmation mismatch");
        return String::new();
    }
    first
}

fn prompt_api_key() -> String {
    match rpassword::prompt_password("API key: ") {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => prompt_required("API key: ").unwrap_or_default(),
    }
}

fn derive_sync_key_proof(email: &str, sync_key: &str) -> String {
    let normalized_email = email.trim().to_lowercase();
    let material = format!("codex-tools-sync-login-proof-v1\0{normalized_email}\0{sync_key}");
    sha256_hex(material.as_bytes())
}

fn registration_request(
    args: &[String],
    email: String,
    device_name: String,
    platform: String,
    sync_key_proof: Option<String>,
) -> RegistrationRequest {
    RegistrationRequest {
        email,
        device_name,
        platform,
        invite_code: option_value(args, "--invite-code")
            .or_else(|| optional_env("CODEX_TOOLS_INVITE_CODE"))
            .or_else(|| Some(DEFAULT_INVITE_CODE.into())),
        sync_key_proof,
    }
}

fn mask_token(token: &str) -> String {
    if token.len() <= 12 {
        return "***".into();
    }
    format!("{}...{}", &token[..6], &token[token.len() - 4..])
}

fn option_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn option_path(args: &[String], name: &str) -> Result<Option<PathBuf>, String> {
    Ok(option_value(args, name).map(PathBuf::from))
}

fn option_usize(args: &[String], name: &str) -> Result<Option<usize>, String> {
    option_value(args, name)
        .map(|value| value.parse::<usize>().map_err(to_error))
        .transpose()
}

fn concurrency_option(
    args: &[String],
    names: &[&str],
    fallback_names: &[&str],
    env_key: &str,
    default_value: usize,
    max_value: usize,
) -> Result<usize, String> {
    let value = names
        .iter()
        .find_map(|name| option_value(args, name))
        .or_else(|| {
            fallback_names
                .iter()
                .find_map(|name| option_value(args, name))
        })
        .or_else(|| optional_env(env_key));
    let accepted_names = names
        .iter()
        .chain(fallback_names.iter())
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    let Some(value) = value else {
        return Ok(default_value);
    };
    let parsed = value.parse::<usize>().map_err(|_| {
        format!(
            "invalid concurrency value for {} / {}: {}",
            accepted_names, env_key, value
        )
    })?;
    if parsed == 0 {
        return Err(format!(
            "concurrency value for {} / {} must be greater than 0",
            accepted_names, env_key
        ));
    }
    Ok(parsed.min(max_value))
}

fn validate_chunk_descriptors(chunks: &[ChunkDescriptor]) -> Result<(), String> {
    if chunks.is_empty() {
        return Err("chunk manifest is empty".into());
    }
    for (expected_index, chunk) in chunks.iter().enumerate() {
        if chunk.index != expected_index {
            return Err(format!(
                "chunk manifest index mismatch: expected {}, got {}",
                expected_index, chunk.index
            ));
        }
        if chunk.size == 0 {
            return Err(format!("chunk manifest has empty chunk {}", chunk.index));
        }
        if chunk.sha256.len() != 64 || !chunk.sha256.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err(format!(
                "chunk manifest has invalid sha256 for chunk {}",
                chunk.index
            ));
        }
    }
    Ok(())
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

fn default_device_name() -> String {
    env::var("CODEX_TOOLS_DEVICE_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| env::var("HOSTNAME").ok())
        .or_else(|| env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "unknown-device".into())
}

fn default_platform() -> String {
    format!("{}-{}", env::consts::OS, env::consts::ARCH)
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_millis(0))
        .as_millis()
}

fn percent_encode_path(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

fn api_error(action: &str, error: Option<String>, message: Option<String>) -> String {
    match (error, message) {
        (Some(error), Some(message)) => format!("{action} failed: {error}: {message}"),
        (Some(error), None) => format!("{action} failed: {error}"),
        (None, Some(message)) => format!("{action} failed: {message}"),
        (None, None) => format!("{action} failed"),
    }
}

fn to_error(error: impl ToString) -> String {
    error.to_string()
}

fn print_help() {
    println!("{}", help_text());
}

fn help_text() -> String {
    r#"Codex API Tools CLI

Usage:
  codex-tools status
  codex-tools codex quit
  codex-tools provider status
  codex-tools provider repair [--name simplaj] [--no-sync]
  codex-tools provider sync [--provider NAME]
  codex-tools provider switch NAME
  codex-tools auth unlock
  codex-tools auth token [--provider NAME] [--key sk-...]
  codex-tools relay gpt [--provider NAME]
  codex-tools relay restore [--provider NAME]
  codex-tools quota [--json|--raw-json]
  codex-tools sessions list [--limit N] [--json] [--codex-home PATH]
  codex-tools cloud login [--email EMAIL] [--api-url URL] [--device NAME] [--key VALUE] [--invite-code CODE]
  codex-tools cloud status
  codex-tools cloud logout
  codex-tools cloud smoke
  codex-tools cloud push --all [--limit N] [--device NAME] [--codex-home PATH] [--force] [-n N]
  codex-tools cloud push --session-id ID [--device NAME] [--codex-home PATH] [--force] [-n N]
  codex-tools cloud list [--limit N] [--json]
  codex-tools cloud pull --all [--limit N] [--codex-home PATH] [--dry-run] [--force] [-n N]
  codex-tools cloud pull --session-id ID [--codex-home PATH] [--dry-run] [--force] [-n N]

Local config write commands require Codex to be fully closed. Run
`codex-tools codex quit` first, or close Codex manually if detection fails.

Run `codex-tools cloud login` once to save local cloud sync configuration.
Environment variables CODEX_TOOLS_API_URL, CODEX_TOOLS_EMAIL,
CODEX_TOOLS_DEVICE_TOKEN, and CODEX_TOOLS_SYNC_KEY are optional cloud-sync
overrides for automation.
CODEX_TOOLS_RELAY_API_KEY can provide the relay API key for `auth token`.
The sync key is the only user secret: the CLI uses it locally for zstd +
XChaCha20-Poly1305 encryption/decryption and sends only a derived login proof
to the Worker when registering a device.
Tune transfer parallelism with -n N. On push, -n applies to session upload
groups and chunk uploads. On pull, -n applies to chunk downloads.
Advanced aliases remain available: --threads, --session-concurrency,
--chunk-concurrency, and --download-concurrency.
Environment fallbacks: CODEX_TOOLS_SESSION_UPLOAD_CONCURRENCY,
CODEX_TOOLS_CHUNK_UPLOAD_CONCURRENCY, and CODEX_TOOLS_CHUNK_DOWNLOAD_CONCURRENCY.
New device registration sends invite code sub2api.simplaj.top by default.
Cloud push skips already uploaded versions by default; use --force to re-upload.
"#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encryption_round_trip_preserves_payload() {
        let payload = b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"abc\"}}\n";
        let encrypted = encrypt_payload(payload, "test-passphrase").unwrap();
        assert_ne!(encrypted, payload);
        let decrypted = decrypt_payload(&encrypted, "test-passphrase").unwrap();
        assert_eq!(decrypted, payload);
    }

    #[test]
    fn latest_manifest_keeps_newest_upload_per_session() {
        let older = SessionManifest {
            format: ENVELOPE_FORMAT.into(),
            session_id: "s1".into(),
            relative_path: "sessions/a/rollout-a.jsonl".into(),
            source_dir: "sessions".into(),
            title: None,
            cwd: None,
            provider_name: None,
            model: None,
            raw_sha256: "old".into(),
            encrypted_sha256: "old".into(),
            encrypted_size: 1,
            blob_key: "old".into(),
            uploaded_at_ms: 1,
            device_name: "dev-a".into(),
        };
        let mut newer = older.clone();
        newer.raw_sha256 = "new".into();
        newer.encrypted_sha256 = "new".into();
        newer.blob_key = "new".into();
        newer.uploaded_at_ms = 2;

        let latest = latest_manifests(vec![older, newer]);
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].raw_sha256, "new");
    }

    #[test]
    fn session_groups_keep_same_session_versions_old_to_new() {
        let newest_s1 = test_local_session("s1", "sessions/new.jsonl", 30);
        let s2 = test_local_session("s2", "sessions/other.jsonl", 20);
        let oldest_s1 = test_local_session("s1", "sessions/old.jsonl", 10);

        let groups = group_local_sessions_by_id(vec![newest_s1, s2, oldest_s1]);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 2);
        assert_eq!(groups[0][0].relative_path, "sessions/old.jsonl");
        assert_eq!(groups[0][1].relative_path, "sessions/new.jsonl");
        assert_eq!(groups[1].len(), 1);
        assert_eq!(groups[1][0].session_id, "s2");
    }

    #[test]
    fn n_option_sets_transfer_concurrency() {
        let args = vec!["-n".to_string(), "6".to_string()];

        let value =
            concurrency_option(&args, &["--specific"], &["-n"], "MISSING_ENV_KEY", 1, 8).unwrap();

        assert_eq!(value, 6);
    }

    #[test]
    fn specific_concurrency_option_overrides_n() {
        let args = vec![
            "-n".to_string(),
            "6".to_string(),
            "--specific".to_string(),
            "3".to_string(),
        ];

        let value =
            concurrency_option(&args, &["--specific"], &["-n"], "MISSING_ENV_KEY", 1, 8).unwrap();

        assert_eq!(value, 3);
    }

    fn test_local_session(
        session_id: &str,
        relative_path: &str,
        modified_at_ms: u128,
    ) -> LocalSessionMeta {
        LocalSessionMeta {
            session_id: session_id.into(),
            relative_path: relative_path.into(),
            source_dir: "sessions".into(),
            title: None,
            cwd: None,
            provider_name: None,
            model: None,
            modified_at_ms,
            size: 1,
        }
    }
}
