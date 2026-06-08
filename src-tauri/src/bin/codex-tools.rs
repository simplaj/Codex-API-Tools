use base64::{engine::general_purpose::STANDARD, Engine};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand::{rngs::OsRng, RngCore};
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SESSION_DIRS: &[&str] = &["sessions", "archived_sessions"];
const ENVELOPE_FORMAT: &str = "codex-tools-session-v1";
const USER_AGENT_VALUE: &str = "codex-tools-cloud-sync/0.1.1";
const DEFAULT_API_URL: &str = "https://codex-tools-sync-api.821099891.workers.dev";
const DEFAULT_INVITE_CODE: &str = "sub2api.simplaj.top";

struct ApiClient {
    api_url: String,
    device_token: Option<String>,
    admin_bootstrap_token: Option<String>,
    sync_passphrase: Option<String>,
    client: Client,
}

#[derive(Debug, Clone)]
struct RegistrationRequest {
    email: String,
    device_name: String,
    platform: String,
    invite_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalSession {
    session_id: String,
    relative_path: String,
    source_dir: String,
    title: Option<String>,
    cwd: Option<String>,
    provider_name: Option<String>,
    model: Option<String>,
    modified_at_ms: u128,
    size: u64,
    raw_sha256: String,
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
    sync_passphrase: Option<String>,
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
        Some("sessions") => run_sessions(&args[1..]),
        Some("cloud") => run_cloud(&args[1..]),
        Some("help") | Some("--help") | Some("-h") | None => {
            print_help();
            Ok(())
        }
        Some(other) => Err(format!("unknown command: {other}\n\n{}", help_text())),
    }
}

fn run_sessions(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("list") => {
            let codex_home = option_path(args, "--codex-home")?.unwrap_or(codex_home()?);
            let json = flag(args, "--json");
            let limit = option_usize(args, "--limit")?.unwrap_or(50);
            let sessions = collect_local_sessions(&codex_home)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&sessions).map_err(to_error)?
                );
                return Ok(());
            }
            println!("local sessions: {}", sessions.len());
            for session in sessions.iter().take(limit) {
                println!(
                    "{}  {}  {}  {}",
                    session.session_id,
                    session.provider_name.as_deref().unwrap_or("-"),
                    session.model.as_deref().unwrap_or("-"),
                    session.relative_path
                );
            }
            if sessions.len() > limit {
                println!("... {} more", sessions.len() - limit);
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
        Some("register") => cloud_register(args),
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

fn cloud_register(args: &[String]) -> Result<(), String> {
    let api = ApiClient::from_config_or_env()?;
    let email = option_value(args, "--email")
        .or_else(|| env::var("CODEX_TOOLS_REGISTER_EMAIL").ok())
        .ok_or_else(|| "cloud register requires --email <email>".to_string())?;
    let device_name = option_value(args, "--device").unwrap_or_else(default_device_name);
    let platform = option_value(args, "--platform").unwrap_or_else(default_platform);
    let registration = registration_request(args, email, device_name, platform);
    let result = api.register(&registration)?;
    if !result.ok {
        return Err(api_error("register", result.error, result.message));
    }
    let user_id = result.user_id.unwrap_or_else(|| "-".into());
    let device_id = result.device_id.unwrap_or_else(|| "-".into());
    let token = result
        .device_token
        .ok_or_else(|| "register response missing deviceToken".to_string())?;
    println!("registered user: {user_id}");
    println!("registered device: {device_id}");
    println!("CODEX_TOOLS_DEVICE_TOKEN={token}");
    Ok(())
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
    let sync_passphrase = option_value(args, "--passphrase")
        .or_else(|| optional_env("CODEX_TOOLS_SYNC_PASSPHRASE"))
        .unwrap_or_else(|| prompt_sync_passphrase());
    if sync_passphrase.trim().is_empty() {
        return Err("cloud login requires a non-empty sync passphrase".into());
    }

    let api = ApiClient::new(
        api_url.clone(),
        None,
        optional_env("CODEX_TOOLS_ADMIN_BOOTSTRAP_TOKEN"),
        Some(sync_passphrase.clone()),
    )?;
    let registration = registration_request(args, email.clone(), device_name.clone(), platform);
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
        sync_passphrase: Some(sync_passphrase),
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
    let sync_passphrase =
        optional_env("CODEX_TOOLS_SYNC_PASSPHRASE").or_else(|| config.sync_passphrase.clone());
    println!("api: {api_url}");
    println!("config: {}", path.display());
    println!("email: {}", config.email.as_deref().unwrap_or("not saved"));
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
        "sync passphrase: {}",
        if sync_passphrase.is_some() {
            "saved"
        } else {
            "missing"
        }
    );
    Ok(())
}

fn cloud_push_api(args: &[String], api: &ApiClient) -> Result<(), String> {
    let passphrase = api.required_sync_passphrase()?;
    let codex_home = option_path(args, "--codex-home")?.unwrap_or(codex_home()?);
    let session_filter = option_value(args, "--session-id");
    let all = flag(args, "--all");
    let limit = option_usize(args, "--limit")?;
    let device_name = option_value(args, "--device").unwrap_or_else(default_device_name);

    if session_filter.is_none() && !all {
        return Err("cloud push requires --all or --session-id <id>".into());
    }

    let mut sessions = collect_local_sessions(&codex_home)?;
    if let Some(session_id) = session_filter {
        sessions.retain(|session| session.session_id == session_id);
    }
    if let Some(limit) = limit {
        sessions.truncate(limit);
    }
    if sessions.is_empty() {
        return Err("no local sessions matched the push filter".into());
    }

    let mut uploaded = 0usize;
    for session in sessions {
        let raw_path = codex_home.join(&session.relative_path);
        let raw = fs::read(&raw_path).map_err(to_error)?;
        let encrypted = encrypt_payload(&raw, &passphrase)?;
        let encrypted_sha256 = sha256_hex(&encrypted);
        let manifest = SessionManifest {
            format: ENVELOPE_FORMAT.into(),
            session_id: session.session_id.clone(),
            relative_path: session.relative_path.clone(),
            source_dir: session.source_dir.clone(),
            title: session.title.clone(),
            cwd: session.cwd.clone(),
            provider_name: session.provider_name.clone(),
            model: session.model.clone(),
            raw_sha256: session.raw_sha256.clone(),
            encrypted_sha256,
            encrypted_size: encrypted.len(),
            blob_key: String::new(),
            uploaded_at_ms: unix_millis(),
            device_name: device_name.clone(),
        };
        let result = api.put_version(&manifest, encrypted)?;
        if !result.ok {
            return Err(api_error("push", result.error, result.message));
        }
        uploaded += 1;
        println!("uploaded {} -> API", session.session_id);
    }

    println!("cloud push ok: uploaded {uploaded} session version(s)");
    Ok(())
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
    println!("cloud session versions: {}", manifests.len());
    for manifest in latest_manifests(manifests) {
        println!(
            "{}  {}  {}  {}",
            manifest.session_id,
            manifest.provider_name.as_deref().unwrap_or("-"),
            manifest.model.as_deref().unwrap_or("-"),
            manifest.relative_path
        );
    }
    Ok(())
}

fn cloud_pull_api(args: &[String], api: &ApiClient) -> Result<(), String> {
    let passphrase = api.required_sync_passphrase()?;
    let codex_home = option_path(args, "--codex-home")?.unwrap_or(codex_home()?);
    let session_id = option_value(args, "--session-id")
        .ok_or_else(|| "cloud pull requires --session-id <id>".to_string())?;
    let force = flag(args, "--force");
    let dry_run = flag(args, "--dry-run");

    let latest = api.latest_version(&session_id)?;
    if !latest.ok {
        return Err(api_error("latest", latest.error, latest.message));
    }
    let manifest = latest
        .manifest
        .ok_or_else(|| format!("no cloud manifest found for session {session_id}"))?;
    let encrypted = api.get_blob(&manifest.session_id, &manifest.raw_sha256)?;
    if sha256_hex(&encrypted) != manifest.encrypted_sha256 {
        return Err("downloaded encrypted blob hash does not match manifest".into());
    }
    let raw = decrypt_payload(&encrypted, &passphrase)?;
    if sha256_hex(&raw) != manifest.raw_sha256 {
        return Err("decrypted rollout hash does not match manifest".into());
    }

    let target_path = codex_home.join(&manifest.relative_path);
    if dry_run {
        println!(
            "dry run: would restore {} bytes to {}",
            raw.len(),
            target_path.display()
        );
        return Ok(());
    }
    if target_path.exists() && !force {
        let existing = fs::read(&target_path).map_err(to_error)?;
        if sha256_hex(&existing) == manifest.raw_sha256 {
            println!("already restored: {}", target_path.display());
            return Ok(());
        }
        return Err(format!(
            "{} already exists with different content. Use --force to overwrite.",
            target_path.display()
        ));
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
        "run provider metadata sync after restore if this machine uses a different provider name"
    );
    Ok(())
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

fn collect_local_sessions(codex_home: &Path) -> Result<Vec<LocalSession>, String> {
    let mut paths = Vec::new();
    for dir_name in SESSION_DIRS {
        list_rollout_files(&codex_home.join(dir_name), &mut paths)?;
    }
    let mut sessions = Vec::new();
    for path in paths {
        if let Some(session) = read_local_session(codex_home, &path)? {
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

fn read_local_session(codex_home: &Path, path: &Path) -> Result<Option<LocalSession>, String> {
    let bytes = fs::read(path).map_err(to_error)?;
    let first_line = first_line(&bytes);
    let parsed: Value = match serde_json::from_slice(first_line) {
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
    Ok(Some(LocalSession {
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
        raw_sha256: sha256_hex(&bytes),
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

fn encrypt_payload(raw: &[u8], passphrase: &str) -> Result<Vec<u8>, String> {
    let compressed = zstd::bulk::compress(raw, 3).map_err(to_error)?;
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&derive_key(passphrase)));
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

fn decrypt_payload(envelope: &[u8], passphrase: &str) -> Result<Vec<u8>, String> {
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
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&derive_key(passphrase)));
    let compressed = cipher
        .decrypt(XNonce::from_slice(&nonce), &envelope[newline + 1..])
        .map_err(|error| error.to_string())?;
    let raw = zstd::bulk::decompress(&compressed, 64 * 1024 * 1024).map_err(to_error)?;
    if sha256_hex(&raw) != header.raw_sha256 {
        return Err("decrypted payload hash mismatch".into());
    }
    Ok(raw)
}

fn derive_key(passphrase: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"codex-tools-sync-key-v1\0");
    hasher.update(passphrase.as_bytes());
    hasher.finalize().into()
}

impl ApiClient {
    fn new(
        api_url: String,
        device_token: Option<String>,
        admin_bootstrap_token: Option<String>,
        sync_passphrase: Option<String>,
    ) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(to_error)?;
        Ok(Self {
            api_url: api_url.trim_end_matches('/').to_string(),
            device_token,
            admin_bootstrap_token,
            sync_passphrase,
            client,
        })
    }

    fn from_config_or_env() -> Result<Self, String> {
        let config = load_cloud_config()?.unwrap_or_default();
        let api_url = optional_env("CODEX_TOOLS_API_URL")
            .or(config.api_url)
            .unwrap_or_else(|| DEFAULT_API_URL.into());
        let device_token = optional_env("CODEX_TOOLS_DEVICE_TOKEN").or(config.device_token);
        let admin_bootstrap_token = optional_env("CODEX_TOOLS_ADMIN_BOOTSTRAP_TOKEN");
        let sync_passphrase =
            optional_env("CODEX_TOOLS_SYNC_PASSPHRASE").or(config.sync_passphrase);
        Self::new(
            api_url,
            device_token,
            admin_bootstrap_token,
            sync_passphrase,
        )
    }

    fn health(&self) -> Result<ApiStatusResponse, String> {
        let response = self
            .client
            .get(self.url("/v1/health"))
            .header("user-agent", USER_AGENT_VALUE)
            .send()
            .map_err(to_error)?;
        read_json_response(response, "health")
    }

    fn register(&self, registration: &RegistrationRequest) -> Result<ApiRegisterResponse, String> {
        let mut request = self
            .client
            .post(self.url("/v1/devices/register"))
            .header("user-agent", USER_AGENT_VALUE)
            .json(&json!({
                "email": registration.email.as_str(),
                "deviceName": registration.device_name.as_str(),
                "platform": registration.platform.as_str(),
                "inviteCode": registration.invite_code.as_deref()
            }));
        if let Some(token) = self.admin_bootstrap_token.as_ref() {
            request = request.bearer_auth(token);
        }
        let response = request.send().map_err(to_error)?;
        read_json_response(response, "register")
    }

    fn list_sessions(&self, limit: usize) -> Result<ApiSessionsResponse, String> {
        let response = self
            .client
            .get(self.url(&format!("/v1/sessions?limit={limit}")))
            .header("user-agent", USER_AGENT_VALUE)
            .bearer_auth(self.required_device_token()?)
            .send()
            .map_err(to_error)?;
        read_json_response(response, "list sessions")
    }

    fn put_version(
        &self,
        manifest: &SessionManifest,
        encrypted: Vec<u8>,
    ) -> Result<ApiManifestResponse, String> {
        let manifest_header = STANDARD.encode(serde_json::to_vec(manifest).map_err(to_error)?);
        let path = format!(
            "/v1/sessions/{}/versions/{}",
            percent_encode_path(&manifest.session_id),
            manifest.raw_sha256
        );
        let response = self
            .client
            .put(self.url(&path))
            .header("user-agent", USER_AGENT_VALUE)
            .header("content-type", "application/octet-stream")
            .header("x-codex-tools-manifest", manifest_header)
            .bearer_auth(self.required_device_token()?)
            .body(encrypted)
            .send()
            .map_err(to_error)?;
        read_json_response(response, "put version")
    }

    fn latest_version(&self, session_id: &str) -> Result<ApiManifestResponse, String> {
        let path = format!(
            "/v1/sessions/{}/versions/latest",
            percent_encode_path(session_id)
        );
        let response = self
            .client
            .get(self.url(&path))
            .header("user-agent", USER_AGENT_VALUE)
            .bearer_auth(self.required_device_token()?)
            .send()
            .map_err(to_error)?;
        read_json_response(response, "latest version")
    }

    fn get_blob(&self, session_id: &str, raw_sha256: &str) -> Result<Vec<u8>, String> {
        let path = format!(
            "/v1/sessions/{}/versions/{}/blob",
            percent_encode_path(session_id),
            raw_sha256
        );
        let response = self
            .client
            .get(self.url(&path))
            .header("user-agent", USER_AGENT_VALUE)
            .bearer_auth(self.required_device_token()?)
            .send()
            .map_err(to_error)?;
        read_success_bytes(response, "get blob")
    }

    fn required_device_token(&self) -> Result<&str, String> {
        self.device_token
            .as_deref()
            .ok_or_else(|| "not logged in. Run: codex-tools cloud login".to_string())
    }

    fn required_sync_passphrase(&self) -> Result<&str, String> {
        self.sync_passphrase
            .as_deref()
            .ok_or_else(|| "missing sync passphrase. Run: codex-tools cloud login".to_string())
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

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn first_line(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(bytes.len());
    let end = if end > 0 && bytes[end - 1] == b'\r' {
        end - 1
    } else {
        end
    };
    &bytes[..end]
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

fn prompt_sync_passphrase() -> String {
    let first = match rpassword::prompt_password("Sync passphrase: ") {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => prompt_required("Sync passphrase: ").unwrap_or_default(),
    };
    if first.is_empty() {
        return first;
    }
    let second = match rpassword::prompt_password("Confirm sync passphrase: ") {
        Ok(value) => value.trim().to_string(),
        Err(_) => first.clone(),
    };
    if first != second {
        eprintln!("sync passphrase confirmation mismatch");
        return String::new();
    }
    first
}

fn registration_request(
    args: &[String],
    email: String,
    device_name: String,
    platform: String,
) -> RegistrationRequest {
    RegistrationRequest {
        email,
        device_name,
        platform,
        invite_code: option_value(args, "--invite-code")
            .or_else(|| optional_env("CODEX_TOOLS_INVITE_CODE"))
            .or_else(|| Some(DEFAULT_INVITE_CODE.into())),
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
  codex-tools sessions list [--limit N] [--json] [--codex-home PATH]
  codex-tools cloud login [--email EMAIL] [--api-url URL] [--device NAME] [--passphrase VALUE] [--invite-code CODE]
  codex-tools cloud status
  codex-tools cloud logout
  codex-tools cloud register --email EMAIL [--device NAME] [--platform NAME] [--invite-code CODE]
  codex-tools cloud smoke
  codex-tools cloud push --all [--limit N] [--device NAME] [--codex-home PATH]
  codex-tools cloud push --session-id ID [--device NAME] [--codex-home PATH]
  codex-tools cloud list [--json]
  codex-tools cloud pull --session-id ID [--codex-home PATH] [--dry-run] [--force]

Run `codex-tools cloud login` once to save local cloud configuration.
Environment variables CODEX_TOOLS_API_URL, CODEX_TOOLS_DEVICE_TOKEN, and
CODEX_TOOLS_SYNC_PASSPHRASE are optional overrides for automation.
New device registration sends invite code sub2api.simplaj.top by default.
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
}
