use base64::{engine::general_purpose::URL_SAFE, engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use filetime::{set_file_mtime, FileTime};
use reqwest::blocking::Client as BlockingClient;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use rusqlite::params_from_iter;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const APP_BACKUP_NAMESPACE: &str = "gpt-api-tools";
const DEFAULT_PROVIDER_NAME: &str = "simplaj";
const DB_FILE_BASENAME: &str = "state_5.sqlite";
const DEFAULT_CODEX_PROVIDER: &str = "openai";
const GLOBAL_STATE_FILE_BASENAME: &str = ".codex-global-state.json";
const GLOBAL_STATE_BACKUP_FILE_BASENAME: &str = ".codex-global-state.json.bak";
const SESSION_DIRS: &[&str] = &["sessions", "archived_sessions"];
const NATIVE_SYNC_ENGINE: &str = "native-rust-rusqlite";
const CHATGPT_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const RELAY_CONFIG_KEYS: &[&str] = &["base_url", "experimental_bearer_token"];

#[derive(Debug, Clone)]
struct ProviderSection {
    id: String,
    start: usize,
    end: usize,
    values: HashMap<String, String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderView {
    id: String,
    name: String,
    base_url: String,
    wire_api: String,
    requires_openai_auth: String,
    has_experimental_bearer_token: bool,
    experimental_bearer_token_masked: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigView {
    root_provider: String,
    providers: Vec<ProviderView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStatus {
    node_version: String,
    node_ok_for_provider_sync: bool,
    npx_version: String,
    sync_package: String,
    sync_engine: String,
    node_required_for_sync: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InspectState {
    codex_home: String,
    config_path: String,
    auth_path: String,
    config_exists: bool,
    auth_exists: bool,
    codex_running: bool,
    codex_processes: Vec<CodexProcess>,
    codex_process_detection_error: Option<String>,
    backup_root: String,
    backup_dirs: Vec<String>,
    provider_choices: Vec<String>,
    config: Option<ConfigView>,
    runtime: RuntimeStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ShellResult {
    ok: bool,
    code: Option<i32>,
    stdout: String,
    stderr: String,
    command: String,
    duration_ms: u128,
    node_requirement: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupFile {
    source: String,
    backup: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupInfo {
    backup_dir: String,
    files: Vec<BackupFile>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RepairResult {
    changed: bool,
    provider_id: String,
    backup_dir: Option<String>,
    message: String,
    sync: Option<ShellResult>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthRemoveResult {
    removed: bool,
    auth_path: String,
    backup_dir: Option<String>,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RelayConfigToggleResult {
    changed: bool,
    provider_id: String,
    commented: bool,
    changed_keys: Vec<String>,
    backup_dir: Option<String>,
    message: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CodexProcess {
    pid: u32,
    command: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QuitCodexResult {
    attempted: bool,
    commands: Vec<String>,
    still_running: bool,
    processes: Vec<CodexProcess>,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QuotaWindowView {
    label: String,
    used_percent: Option<f64>,
    remaining_percent: Option<f64>,
    window_minutes: Option<i64>,
    resets_at: Option<i64>,
    reset_after_seconds: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QuotaBucketView {
    limit_id: String,
    limit_name: Option<String>,
    allowed: Option<bool>,
    limit_reached: Option<bool>,
    primary: Option<QuotaWindowView>,
    secondary: Option<QuotaWindowView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QuotaCreditsView {
    has_credits: Option<bool>,
    unlimited: Option<bool>,
    balance: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpendControlView {
    limit: Option<String>,
    used: Option<String>,
    remaining: Option<String>,
    used_percent: Option<f64>,
    remaining_percent: Option<f64>,
    resets_at: Option<i64>,
    reset_after_seconds: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenAiQuotaResult {
    ok: bool,
    status: String,
    auth_mode: Option<String>,
    account_id_masked: Option<String>,
    email_masked: Option<String>,
    plan_type: Option<String>,
    last_refresh: Option<String>,
    endpoint: String,
    fetched_at_unix_ms: u128,
    buckets: Vec<QuotaBucketView>,
    credits: Option<QuotaCreditsView>,
    spend_control: Option<SpendControlView>,
    rate_limit_reached_type: Option<String>,
    message: String,
    recommendation: String,
}

#[derive(Deserialize)]
struct AuthDotJsonRaw {
    auth_mode: Option<String>,
    #[serde(default)]
    openai_api_key: Option<String>,
    tokens: Option<AuthTokensRaw>,
    last_refresh: Option<String>,
}

#[derive(Deserialize)]
struct AuthTokensRaw {
    id_token: String,
    access_token: String,
    #[serde(default)]
    account_id: Option<String>,
}

struct LocalChatGptAuth {
    auth_mode: Option<String>,
    account_id: String,
    email: Option<String>,
    plan_type: Option<String>,
    access_token: String,
    last_refresh: Option<String>,
    fedramp: bool,
}

#[derive(Deserialize)]
struct JwtClaims {
    #[serde(default)]
    email: Option<String>,
    #[serde(rename = "https://api.openai.com/profile", default)]
    profile: Option<ProfileClaims>,
    #[serde(rename = "https://api.openai.com/auth", default)]
    auth: Option<AuthClaims>,
}

#[derive(Deserialize)]
struct ProfileClaims {
    #[serde(default)]
    email: Option<String>,
}

#[derive(Deserialize)]
struct AuthClaims {
    #[serde(default)]
    chatgpt_plan_type: Option<String>,
    #[serde(default)]
    chatgpt_account_id: Option<String>,
    #[serde(default)]
    chatgpt_account_is_fedramp: bool,
}

struct FirstLineRecord {
    first_line: String,
    separator: String,
    offset: usize,
}

#[derive(Debug, Clone)]
struct RolloutChange {
    path: PathBuf,
    thread_id: Option<String>,
    cwd: Option<String>,
    has_user_event: bool,
    original_first_line: String,
    original_separator: String,
    original_offset: usize,
    original_size: u64,
    original_mtime: FileTime,
    updated_first_line: String,
}

#[derive(Debug, Clone)]
struct RolloutIndexRecord {
    path: PathBuf,
    thread_id: String,
    cwd: String,
    title: String,
    source: String,
    thread_source: String,
    cli_version: String,
    model: Option<String>,
    reasoning_effort: Option<String>,
    sandbox_policy: String,
    approval_mode: String,
    created_at: i64,
    updated_at: i64,
    has_user_event: bool,
    first_user_message: String,
    archived: bool,
}

#[derive(Default)]
struct RolloutScan {
    changes: Vec<RolloutChange>,
    index_records: Vec<RolloutIndexRecord>,
    provider_counts: HashMap<String, usize>,
    user_event_thread_ids: HashSet<String>,
    thread_cwd_by_id: HashMap<String, String>,
}

#[derive(Default)]
struct SqliteUpdateStats {
    thread_rows_inserted: usize,
    provider_rows_updated: usize,
    user_event_rows_updated: usize,
    cwd_rows_updated: usize,
    database_present: bool,
}

#[derive(Debug, Clone)]
struct ThreadCwdStat {
    cwd: String,
    normalized_cwd: String,
    count: usize,
    updated_at_ms: i64,
}

#[derive(Default)]
struct WorkspaceRootSyncStats {
    present: bool,
    updated: bool,
    updated_workspace_roots: usize,
    saved_workspace_root_count: usize,
}

#[derive(Default)]
struct RolloutContentDetails {
    has_user_event: bool,
    first_user_message: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    sandbox_policy: Option<String>,
    approval_mode: Option<String>,
}

const THREAD_INSERT_COLUMN_ORDER: &[&str] = &[
    "id",
    "rollout_path",
    "created_at",
    "updated_at",
    "source",
    "model_provider",
    "cwd",
    "title",
    "sandbox_policy",
    "approval_mode",
    "tokens_used",
    "has_user_event",
    "archived",
    "archived_at",
    "git_sha",
    "git_branch",
    "git_origin_url",
    "cli_version",
    "first_user_message",
    "agent_nickname",
    "agent_role",
    "memory_mode",
    "model",
    "reasoning_effort",
    "agent_path",
    "created_at_ms",
    "updated_at_ms",
    "thread_source",
    "preview",
];

fn command_error(error: impl ToString) -> String {
    error.to_string()
}

fn home_dir() -> Result<PathBuf, String> {
    if let Some(home) = env::var_os("HOME") {
        return Ok(PathBuf::from(home));
    }
    if let Some(profile) = env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(profile));
    }
    Err("无法定位用户 home 目录。".into())
}

fn codex_home() -> Result<PathBuf, String> {
    if let Ok(value) = env::var("CODEX_HOME") {
        if !value.trim().is_empty() {
            return Ok(PathBuf::from(value));
        }
    }
    Ok(home_dir()?.join(".codex"))
}

fn config_path() -> Result<PathBuf, String> {
    Ok(codex_home()?.join("config.toml"))
}

fn auth_path() -> Result<PathBuf, String> {
    Ok(codex_home()?.join("auth.json"))
}

fn backup_root() -> Result<PathBuf, String> {
    Ok(codex_home()?
        .join("backups_state")
        .join(APP_BACKUP_NAMESPACE))
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_millis(0))
        .as_millis()
}

fn timestamp_for_path() -> String {
    let millis = unix_millis();
    format!("backup-{millis}")
}

fn path_exists(path: &Path) -> bool {
    fs::metadata(path).is_ok()
}

fn mask_secret(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 14 {
        return format!("{}...", chars.iter().take(4).collect::<String>());
    }
    format!(
        "{}...{}",
        chars.iter().take(7).collect::<String>(),
        chars.iter().skip(chars.len() - 4).collect::<String>()
    )
}

fn strip_toml_quotes(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        return trimmed[1..trimmed.len() - 1].to_string();
    }
    trimmed.to_string()
}

fn escape_toml_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn split_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect()
}

fn sanitize_provider_id(value: Option<String>) -> Result<String, String> {
    let raw = value.unwrap_or_else(|| DEFAULT_PROVIDER_NAME.to_string());
    let mut output = String::new();
    let mut last_dash = false;

    for character in raw.trim().to_lowercase().chars() {
        let mapped = if character.is_ascii_alphanumeric() || character == '.' || character == '_' {
            last_dash = false;
            Some(character)
        } else if character.is_whitespace() || character == '-' {
            if last_dash {
                None
            } else {
                last_dash = true;
                Some('-')
            }
        } else if last_dash {
            None
        } else {
            last_dash = true;
            Some('-')
        };
        if let Some(mapped) = mapped {
            output.push(mapped);
        }
    }

    let normalized = output.trim_matches('-').to_string();
    if normalized.is_empty() {
        return Err("Provider 名称不能为空。".into());
    }
    if !normalized
        .chars()
        .next()
        .map(|c| c.is_ascii_alphanumeric())
        .unwrap_or(false)
    {
        return Err("Provider 名称需以字母或数字开头。".into());
    }
    Ok(normalized)
}

fn assignment_value(line: &str, key: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return None;
    }
    let (left, right) = trimmed.split_once('=')?;
    if left.trim() != key {
        return None;
    }
    let value = right.split('#').next().unwrap_or("").trim();
    Some(strip_toml_quotes(value))
}

fn parse_root_provider(config_text: &str) -> String {
    for line in split_lines(config_text) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') {
            break;
        }
        if let Some(value) = assignment_value(&line, "model_provider") {
            return value;
        }
    }
    "openai".into()
}

fn parse_section_id(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with("[model_providers.") || !trimmed.ends_with(']') {
        return None;
    }
    let inner = trimmed
        .trim_start_matches("[model_providers.")
        .trim_end_matches(']');
    Some(strip_toml_quotes(inner))
}

fn parse_provider_sections(config_text: &str) -> Vec<ProviderSection> {
    let lines = split_lines(config_text);
    let mut sections = Vec::<ProviderSection>::new();
    let mut current: Option<ProviderSection> = None;

    for (index, line) in lines.iter().enumerate() {
        if let Some(id) = parse_section_id(line) {
            if let Some(mut section) = current.take() {
                section.end = index;
                sections.push(section);
            }
            current = Some(ProviderSection {
                id,
                start: index,
                end: lines.len(),
                values: HashMap::new(),
            });
            continue;
        }

        if line.trim().starts_with('[') {
            if let Some(mut section) = current.take() {
                section.end = index;
                sections.push(section);
            }
            continue;
        }

        if let Some(section) = current.as_mut() {
            if let Some((left, _right)) = line.trim().split_once('=') {
                let key = left.trim();
                if !key.is_empty() && !key.starts_with('#') {
                    if let Some(value) = assignment_value(line, key) {
                        section.values.insert(key.to_string(), value);
                    }
                }
            }
        }
    }

    if let Some(section) = current {
        sections.push(section);
    }
    sections
}

fn parse_config(config_text: &str) -> ConfigView {
    let providers = parse_provider_sections(config_text)
        .into_iter()
        .map(|section| {
            let token = section
                .values
                .get("experimental_bearer_token")
                .cloned()
                .unwrap_or_default();
            ProviderView {
                id: section.id.clone(),
                name: section
                    .values
                    .get("name")
                    .cloned()
                    .unwrap_or_else(|| section.id.clone()),
                base_url: section.values.get("base_url").cloned().unwrap_or_default(),
                wire_api: section.values.get("wire_api").cloned().unwrap_or_default(),
                requires_openai_auth: section
                    .values
                    .get("requires_openai_auth")
                    .cloned()
                    .unwrap_or_default(),
                has_experimental_bearer_token: !token.is_empty(),
                experimental_bearer_token_masked: mask_secret(&token),
            }
        })
        .collect();

    ConfigView {
        root_provider: parse_root_provider(config_text),
        providers,
    }
}

fn set_root_provider(config_text: &str, provider_id: &str) -> String {
    let mut lines = split_lines(config_text);
    let mut insert_index = lines.len();

    for index in 0..lines.len() {
        let trimmed = lines[index].trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            insert_index = index + 1;
            continue;
        }
        if trimmed.starts_with('[') {
            insert_index = index;
            break;
        }
        if trimmed.starts_with("model_provider") && trimmed.contains('=') {
            lines[index] = format!("model_provider = \"{}\"", escape_toml_string(provider_id));
            return lines.join("\n");
        }
        insert_index = index + 1;
    }

    lines.insert(
        insert_index,
        format!("model_provider = \"{}\"", escape_toml_string(provider_id)),
    );
    lines.join("\n")
}

fn upsert_key_in_section(
    lines: &mut Vec<String>,
    section: &mut ProviderSection,
    key: &str,
    value: &str,
) {
    for index in (section.start + 1)..section.end {
        let trimmed = lines[index].trim();
        if trimmed.starts_with(key) && trimmed.contains('=') {
            lines[index] = format!("{key} = {value}");
            return;
        }
    }
    lines.insert(section.start + 1, format!("{key} = {value}"));
    section.end += 1;
}

fn relay_assignment_key(text: &str) -> Option<String> {
    let (left, _right) = text.trim_start().split_once('=')?;
    let key = left.trim();
    RELAY_CONFIG_KEYS.contains(&key).then(|| key.to_string())
}

fn active_relay_assignment_key(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return None;
    }
    relay_assignment_key(trimmed)
}

fn commented_relay_assignment_key(line: &str) -> Option<String> {
    let rest = line.trim_start();
    let after_hash = rest.strip_prefix('#')?.trim_start();
    relay_assignment_key(after_hash)
}

fn comment_toml_line(line: &str) -> String {
    let indent_len = line.len().saturating_sub(line.trim_start().len());
    let indent = &line[..indent_len];
    let content = line[indent_len..].trim_start();
    format!("{indent}# {content}")
}

fn uncomment_toml_line(line: &str) -> String {
    let indent_len = line.len().saturating_sub(line.trim_start().len());
    let indent = &line[..indent_len];
    let rest = &line[indent_len..];
    let after_hash = rest.strip_prefix('#').map(str::trim_start).unwrap_or(rest);
    format!("{indent}{after_hash}")
}

fn relay_config_lines_with_comment_state(
    config_text: &str,
    section: &ProviderSection,
    commented: bool,
) -> (String, Vec<String>) {
    let mut lines = split_lines(config_text);
    let mut changed_keys = Vec::new();

    for index in (section.start + 1)..section.end {
        if commented {
            if let Some(key) = active_relay_assignment_key(&lines[index]) {
                lines[index] = comment_toml_line(&lines[index]);
                changed_keys.push(key);
            }
        } else if let Some(key) = commented_relay_assignment_key(&lines[index]) {
            lines[index] = uncomment_toml_line(&lines[index]);
            changed_keys.push(key);
        }
    }

    changed_keys.sort();
    changed_keys.dedup();
    (lines.join("\n"), changed_keys)
}

fn create_backup(files: &[PathBuf], reason: &str) -> Result<BackupInfo, String> {
    let backup_dir = backup_root()?.join(timestamp_for_path());
    fs::create_dir_all(&backup_dir).map_err(command_error)?;
    let mut copied = Vec::new();

    for file_path in files {
        if !path_exists(file_path) {
            continue;
        }
        let file_name = file_path
            .file_name()
            .ok_or_else(|| format!("无效文件路径：{}", path_to_string(file_path)))?;
        let target_path = backup_dir.join(file_name);
        fs::copy(file_path, &target_path).map_err(command_error)?;
        copied.push(BackupFile {
            source: path_to_string(file_path),
            backup: path_to_string(&target_path),
        });
    }

    let manifest = serde_json::json!({
        "tool": "Codex API Tools",
        "reason": reason,
        "createdAtUnixMs": unix_millis().to_string(),
        "codexHome": path_to_string(&codex_home()?),
        "files": copied,
    });
    fs::write(
        backup_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).map_err(command_error)?,
    )
    .map_err(command_error)?;

    Ok(BackupInfo {
        backup_dir: path_to_string(&backup_dir),
        files: copied,
    })
}

fn find_openai_provider_section(sections: &[ProviderSection]) -> Option<ProviderSection> {
    sections
        .iter()
        .find(|section| {
            section.id.eq_ignore_ascii_case("openai")
                || section
                    .values
                    .get("name")
                    .map(|name| name.eq_ignore_ascii_case("openai"))
                    .unwrap_or(false)
        })
        .cloned()
}

fn find_provider_section_for_repair(
    sections: &[ProviderSection],
    current_provider: &str,
) -> Option<ProviderSection> {
    find_openai_provider_section(sections).or_else(|| {
        sections
            .iter()
            .find(|section| {
                section.id == current_provider
                    || section
                        .values
                        .get("name")
                        .map(|name| name == current_provider)
                        .unwrap_or(false)
            })
            .cloned()
    })
}

fn find_provider_section_by_id_or_name(
    sections: &[ProviderSection],
    provider_id: &str,
) -> Option<ProviderSection> {
    sections
        .iter()
        .find(|section| {
            section.id == provider_id
                || section
                    .values
                    .get("name")
                    .map(|name| name == provider_id)
                    .unwrap_or(false)
        })
        .cloned()
}

fn state_db_path(codex_home: &Path) -> PathBuf {
    codex_home.join(DB_FILE_BASENAME)
}

fn global_state_path(codex_home: &Path) -> PathBuf {
    codex_home.join(GLOBAL_STATE_FILE_BASENAME)
}

fn global_state_backup_path(codex_home: &Path) -> PathBuf {
    codex_home.join(GLOBAL_STATE_BACKUP_FILE_BASENAME)
}

fn sync_backup_file_paths(codex_home: &Path) -> Vec<PathBuf> {
    let mut paths = vec![codex_home.join("config.toml")];
    for suffix in ["", "-shm", "-wal"] {
        paths.push(codex_home.join(format!("{DB_FILE_BASENAME}{suffix}")));
    }
    paths.push(codex_home.join(GLOBAL_STATE_FILE_BASENAME));
    paths.push(codex_home.join(GLOBAL_STATE_BACKUP_FILE_BASENAME));
    paths
}

fn to_normal_workspace_path(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return value.to_string();
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("\\\\?\\unc\\") {
        return format!("\\\\{}", &trimmed[8..]).replace('/', "\\");
    }
    if trimmed.starts_with("\\\\?\\") {
        return trimmed[4..].replace('/', "\\");
    }
    value.to_string()
}

fn normalize_comparable_path(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    let mut normalized = if lower.starts_with("\\\\?\\unc\\") {
        format!("\\\\{}", &trimmed[8..])
    } else if trimmed.starts_with("\\\\?\\") {
        trimmed[4..].to_string()
    } else {
        trimmed.to_string()
    };
    normalized = normalized.replace('/', "\\");
    while normalized.ends_with('\\') {
        normalized.pop();
    }
    if normalized.len() == 2 && normalized.as_bytes().get(1) == Some(&b':') {
        normalized.push('\\');
    }
    if normalized.is_empty() {
        return None;
    }
    Some(normalized.to_ascii_lowercase())
}

fn to_path_array(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(ToString::to_string)
            .collect(),
        Some(Value::String(value)) if !value.trim().is_empty() => vec![value.clone()],
        _ => Vec::new(),
    }
}

fn dedupe_paths(paths: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for path in paths {
        let Some(comparable) = normalize_comparable_path(&path) else {
            continue;
        };
        if seen.insert(comparable) {
            output.push(path);
        }
    }
    output
}

fn count_array_changes(previous: &[String], next: &[String]) -> usize {
    let compared = previous.len().max(next.len());
    (0..compared)
        .filter(|index| previous.get(*index) != next.get(*index))
        .count()
}

fn strings_to_json_array(values: &[String]) -> Value {
    Value::Array(values.iter().cloned().map(Value::String).collect())
}

fn resolve_stored_path(value: &str, cwd_stats: &[ThreadCwdStat]) -> String {
    let Some(comparable) = normalize_comparable_path(value) else {
        return value.to_string();
    };
    let mut matches: Vec<&ThreadCwdStat> = cwd_stats
        .iter()
        .filter(|entry| entry.normalized_cwd == comparable)
        .collect();
    if matches.is_empty() {
        return to_normal_workspace_path(value);
    }
    matches.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms))
            .then_with(|| left.cwd.cmp(&right.cwd))
    });
    to_normal_workspace_path(&matches[0].cwd)
}

fn copy_resolved_object_keys(input: &Value, cwd_stats: &[ThreadCwdStat]) -> Value {
    let Value::Object(map) = input else {
        return input.clone();
    };
    let mut output = serde_json::Map::new();
    for (key, value) in map {
        let resolved = resolve_stored_path(key, cwd_stats);
        if !output.contains_key(&resolved) || resolved == *key {
            output.insert(resolved, value.clone());
        }
    }
    Value::Object(output)
}

fn read_first_line_record(path: &Path) -> Result<FirstLineRecord, String> {
    let bytes = fs::read(path).map_err(command_error)?;
    if let Some(newline_index) = bytes.iter().position(|byte| *byte == b'\n') {
        let crlf = newline_index > 0 && bytes[newline_index - 1] == b'\r';
        let line_end = if crlf {
            newline_index - 1
        } else {
            newline_index
        };
        return Ok(FirstLineRecord {
            first_line: String::from_utf8_lossy(&bytes[..line_end]).to_string(),
            separator: if crlf { "\r\n" } else { "\n" }.to_string(),
            offset: newline_index + 1,
        });
    }
    Ok(FirstLineRecord {
        first_line: String::from_utf8_lossy(&bytes).to_string(),
        separator: String::new(),
        offset: bytes.len(),
    })
}

fn parse_session_meta_record(first_line: &str) -> Option<Value> {
    let parsed = serde_json::from_str::<Value>(first_line).ok()?;
    let is_session_meta = parsed.get("type").and_then(Value::as_str) == Some("session_meta");
    let has_payload = parsed.get("payload").and_then(Value::as_object).is_some();
    if is_session_meta && has_payload {
        Some(parsed)
    } else {
        None
    }
}

fn record_has_user_event(record: &Value) -> bool {
    if record.get("type").and_then(Value::as_str) == Some("event_msg")
        && record
            .get("payload")
            .and_then(|payload| payload.get("type"))
            .and_then(Value::as_str)
            == Some("user_message")
    {
        return true;
    }

    for key in ["payload", "item", "msg"] {
        let Some(value) = record.get(key) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("message")
            && value.get("role").and_then(Value::as_str) == Some("user")
        {
            return true;
        }
    }

    false
}

fn extract_text_from_content(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(|item| {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        return Some(text.trim().to_string());
                    }
                    if let Some(text) = item.get("content").and_then(Value::as_str) {
                        return Some(text.trim().to_string());
                    }
                    item.as_str().map(|text| text.trim().to_string())
                })
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        Value::Object(map) => map
            .get("text")
            .or_else(|| map.get("content"))
            .and_then(extract_text_from_content),
        _ => None,
    }
}

fn looks_like_environment_context(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("<environment_context>")
        || trimmed.starts_with("# AGENTS.md instructions")
        || trimmed.starts_with("<permissions instructions>")
        || trimmed.starts_with("<collaboration_mode>")
}

fn user_message_text(record: &Value) -> Option<String> {
    if record.get("type").and_then(Value::as_str) == Some("event_msg") {
        let payload = record.get("payload")?;
        if payload.get("type").and_then(Value::as_str) == Some("user_message") {
            let text = payload
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(ToString::to_string);
            if let Some(text) = text {
                if !looks_like_environment_context(&text) {
                    return Some(text);
                }
            }
        }
    }

    for key in ["payload", "item", "msg"] {
        let Some(value) = record.get(key) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("message")
            || value.get("role").and_then(Value::as_str) != Some("user")
        {
            continue;
        }
        let Some(text) = value.get("content").and_then(extract_text_from_content) else {
            continue;
        };
        if !looks_like_environment_context(&text) {
            return Some(text);
        }
    }

    None
}

fn collect_rollout_content_details(path: &Path) -> Result<RolloutContentDetails, String> {
    let file = fs::File::open(path).map_err(command_error)?;
    let reader = BufReader::new(file);
    let mut details = RolloutContentDetails::default();
    let mut saw_turn_context = false;

    for line in reader.lines() {
        let line = line.map_err(command_error)?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if record_has_user_event(&record) {
            details.has_user_event = true;
        }
        if details.first_user_message.is_none() {
            details.first_user_message = user_message_text(&record);
        }
        if record.get("type").and_then(Value::as_str) == Some("turn_context") {
            saw_turn_context = true;
            if let Some(payload) = record.get("payload").and_then(Value::as_object) {
                if details.model.is_none() {
                    details.model = payload
                        .get("model")
                        .and_then(Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                        .map(ToString::to_string);
                }
                if details.reasoning_effort.is_none() {
                    details.reasoning_effort = payload
                        .get("effort")
                        .or_else(|| {
                            payload
                                .get("collaboration_mode")
                                .and_then(|mode| mode.get("settings"))
                                .and_then(|settings| settings.get("reasoning_effort"))
                        })
                        .and_then(Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                        .map(ToString::to_string);
                }
                if details.sandbox_policy.is_none() {
                    if let Some(sandbox_policy) = payload.get("sandbox_policy") {
                        details.sandbox_policy =
                            Some(serde_json::to_string(sandbox_policy).map_err(command_error)?);
                    }
                }
                if details.approval_mode.is_none() {
                    details.approval_mode = payload
                        .get("approval_policy")
                        .or_else(|| payload.get("approval_mode"))
                        .and_then(Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                        .map(ToString::to_string);
                }
            }
        }
        if details.has_user_event && details.first_user_message.is_some() && saw_turn_context {
            break;
        }
    }

    Ok(details)
}

fn session_id_unix_seconds(session_id: &str) -> Option<i64> {
    let hex = session_id
        .chars()
        .filter(|character| *character != '-')
        .take(12)
        .collect::<String>();
    if hex.len() != 12 {
        return None;
    }
    u64::from_str_radix(&hex, 16)
        .ok()
        .map(|millis| (millis / 1000) as i64)
}

fn system_time_unix_seconds(value: SystemTime) -> Option<i64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs() as i64)
}

fn file_modified_unix_seconds(path: &Path) -> Option<i64> {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(system_time_unix_seconds)
}

fn payload_string(payload: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn rollout_index_record(
    rollout_path: &Path,
    dir_name: &str,
    payload: &serde_json::Map<String, Value>,
    _current_provider: &str,
) -> Result<Option<RolloutIndexRecord>, String> {
    let Some(thread_id) = payload_string(payload, "id") else {
        return Ok(None);
    };
    let content = collect_rollout_content_details(rollout_path)?;
    let cwd = payload_string(payload, "cwd")
        .map(|value| to_normal_workspace_path(&value))
        .unwrap_or_default();
    let fallback_title = format!("Restored session {thread_id}");
    let first_user_message = content
        .first_user_message
        .clone()
        .or_else(|| payload_string(payload, "title"))
        .unwrap_or_else(|| fallback_title.clone());
    let title = payload_string(payload, "title").unwrap_or_else(|| first_user_message.clone());
    let created_at = session_id_unix_seconds(&thread_id)
        .or_else(|| file_modified_unix_seconds(rollout_path))
        .unwrap_or_else(|| (unix_millis() / 1000) as i64);
    let updated_at = file_modified_unix_seconds(rollout_path).unwrap_or(created_at);

    Ok(Some(RolloutIndexRecord {
        path: rollout_path.to_path_buf(),
        thread_id,
        cwd,
        title,
        source: payload_string(payload, "source").unwrap_or_else(|| "vscode".into()),
        thread_source: payload_string(payload, "thread_source").unwrap_or_else(|| "user".into()),
        cli_version: payload_string(payload, "cli_version").unwrap_or_default(),
        model: content.model,
        reasoning_effort: content.reasoning_effort,
        sandbox_policy: content
            .sandbox_policy
            .unwrap_or_else(|| json!({"type": "disabled"}).to_string()),
        approval_mode: content.approval_mode.unwrap_or_else(|| "never".into()),
        created_at,
        updated_at: updated_at.max(created_at),
        has_user_event: content.has_user_event,
        first_user_message,
        archived: dir_name == "archived_sessions",
    }))
}

fn list_rollout_files(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };

    for entry in entries {
        let entry = entry.map_err(command_error)?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(command_error)?;
        if file_type.is_dir() {
            list_rollout_files(&path, output)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if name.starts_with("rollout-") && name.ends_with(".jsonl") {
            output.push(path);
        }
    }

    Ok(())
}

fn collect_rollout_scan(
    codex_home: &Path,
    target_provider: Option<&str>,
) -> Result<RolloutScan, String> {
    let mut scan = RolloutScan::default();

    for dir_name in SESSION_DIRS {
        let root_dir = codex_home.join(dir_name);
        let mut rollout_files = Vec::new();
        list_rollout_files(&root_dir, &mut rollout_files)?;

        for rollout_path in rollout_files {
            let record = read_first_line_record(&rollout_path)?;
            let Some(mut parsed) = parse_session_meta_record(&record.first_line) else {
                continue;
            };

            let payload = parsed
                .get("payload")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    format!(
                        "无效 session_meta payload：{}",
                        path_to_string(&rollout_path)
                    )
                })?;
            let current_provider = payload
                .get("model_provider")
                .and_then(Value::as_str)
                .unwrap_or("(missing)")
                .to_string();
            let count_key = format!("{dir_name}/{current_provider}");
            *scan.provider_counts.entry(count_key).or_insert(0) += 1;

            let index_record = if target_provider.is_some() {
                rollout_index_record(&rollout_path, dir_name, payload, &current_provider)?
            } else {
                None
            };
            let thread_id = payload
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToString::to_string);
            let cwd = payload
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(to_normal_workspace_path);
            if let (Some(thread_id), Some(cwd)) = (thread_id.as_ref(), cwd.as_ref()) {
                scan.thread_cwd_by_id.insert(thread_id.clone(), cwd.clone());
            }

            let has_user_event = index_record
                .as_ref()
                .map(|record| record.has_user_event)
                .unwrap_or(false);
            if let Some(record) = index_record {
                if record.has_user_event {
                    scan.user_event_thread_ids.insert(record.thread_id.clone());
                }
                scan.index_records.push(record);
            }

            if let Some(target_provider) = target_provider {
                if current_provider != target_provider {
                    let metadata = fs::metadata(&rollout_path).map_err(command_error)?;
                    let original_mtime = FileTime::from_last_modification_time(&metadata);
                    let payload = parsed
                        .get_mut("payload")
                        .and_then(Value::as_object_mut)
                        .ok_or_else(|| {
                            format!(
                                "无效 session_meta payload：{}",
                                path_to_string(&rollout_path)
                            )
                        })?;
                    payload.insert(
                        "model_provider".to_string(),
                        Value::String(target_provider.to_string()),
                    );
                    scan.changes.push(RolloutChange {
                        path: rollout_path,
                        thread_id,
                        cwd,
                        has_user_event,
                        original_first_line: record.first_line,
                        original_separator: record.separator,
                        original_offset: record.offset,
                        original_size: metadata.len(),
                        original_mtime,
                        updated_first_line: serde_json::to_string(&parsed)
                            .map_err(command_error)?,
                    });
                }
            }
        }
    }

    Ok(scan)
}

fn apply_rollout_changes(changes: &[RolloutChange]) -> Result<(usize, Vec<String>), String> {
    let mut applied = 0usize;
    let mut skipped = Vec::new();

    for change in changes {
        let metadata = match fs::metadata(&change.path) {
            Ok(metadata) => metadata,
            Err(_) => {
                skipped.push(path_to_string(&change.path));
                continue;
            }
        };
        if metadata.len() != change.original_size {
            skipped.push(path_to_string(&change.path));
            continue;
        }

        let current = read_first_line_record(&change.path)?;
        if current.first_line != change.original_first_line
            || current.offset != change.original_offset
        {
            skipped.push(path_to_string(&change.path));
            continue;
        }

        let original_bytes = fs::read(&change.path).map_err(command_error)?;
        if change.original_offset > original_bytes.len() {
            skipped.push(path_to_string(&change.path));
            continue;
        }

        let mut next_bytes = Vec::new();
        next_bytes.extend_from_slice(change.updated_first_line.as_bytes());
        next_bytes.extend_from_slice(change.original_separator.as_bytes());
        next_bytes.extend_from_slice(&original_bytes[change.original_offset..]);

        let tmp_path = PathBuf::from(format!(
            "{}.gpt-api-tools-sync.{}.tmp",
            path_to_string(&change.path),
            unix_millis()
        ));
        fs::write(&tmp_path, next_bytes).map_err(command_error)?;
        fs::rename(&tmp_path, &change.path).map_err(|error| {
            let _ = fs::remove_file(&tmp_path);
            error.to_string()
        })?;
        let _ = set_file_mtime(&change.path, change.original_mtime);
        applied += 1;
    }

    skipped.sort();
    Ok((applied, skipped))
}

fn sqlite_error(action: &str, error: impl ToString) -> String {
    let message = error.to_string();
    let lower = message.to_lowercase();
    if lower.contains("database is locked") || lower.contains("busy") || lower.contains("locked") {
        return format!(
            "{action} 失败：state_5.sqlite 正在被 Codex 占用。请关闭 Codex App / Codex CLI 后重试。原始错误：{message}"
        );
    }
    if lower.contains("malformed") || lower.contains("not a database") || lower.contains("corrupt")
    {
        return format!(
            "{action} 失败：state_5.sqlite 损坏或不可读。请先备份/修复数据库。原始错误：{message}"
        );
    }
    format!("{action} 失败：{message}")
}

fn sqlite_columns(conn: &Connection, table: &str) -> Result<HashSet<String>, String> {
    let escaped_table = table.replace('"', "\"\"");
    let sql = format!("PRAGMA table_info(\"{escaped_table}\")");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|error| sqlite_error("读取 SQLite 表结构", error))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| sqlite_error("读取 SQLite 表结构", error))?;
    let mut columns = HashSet::new();
    for row in rows {
        columns.insert(row.map_err(|error| sqlite_error("读取 SQLite 表结构", error))?);
    }
    Ok(columns)
}

fn read_sqlite_provider_counts(
    codex_home: &Path,
) -> Result<Option<HashMap<String, usize>>, String> {
    let db_path = state_db_path(codex_home);
    if !path_exists(&db_path) {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| sqlite_error("读取 SQLite provider 分布", error))?;
    conn.busy_timeout(Duration::from_millis(5000))
        .map_err(|error| sqlite_error("读取 SQLite provider 分布", error))?;
    let columns = sqlite_columns(&conn, "threads")?;
    let provider_expr = if columns.contains("model_provider") {
        "CASE WHEN model_provider IS NULL OR model_provider = '' THEN '(missing)' ELSE model_provider END"
    } else {
        "'(missing)'"
    };
    let archived_expr = if columns.contains("archived") {
        "COALESCE(archived, 0)"
    } else {
        "0"
    };
    let sql = format!(
        "SELECT {provider_expr} AS provider, {archived_expr} AS archived, COUNT(*) AS count \
         FROM threads GROUP BY provider, archived ORDER BY archived, provider"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|error| sqlite_error("读取 SQLite provider 分布", error))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|error| sqlite_error("读取 SQLite provider 分布", error))?;

    let mut counts = HashMap::new();
    for row in rows {
        let (provider, archived, count) =
            row.map_err(|error| sqlite_error("读取 SQLite provider 分布", error))?;
        let scope = if archived == 0 {
            "sessions"
        } else {
            "archived_sessions"
        };
        counts.insert(format!("{scope}/{provider}"), count.max(0) as usize);
    }
    Ok(Some(counts))
}

fn build_time_expression(columns: &HashSet<String>) -> String {
    let mut expressions = Vec::new();
    if columns.contains("updated_at_ms") {
        expressions.push("updated_at_ms");
    }
    if columns.contains("updated_at") {
        expressions.push("updated_at * 1000");
    }
    if columns.contains("created_at_ms") {
        expressions.push("created_at_ms");
    }
    if columns.contains("created_at") {
        expressions.push("created_at * 1000");
    }
    expressions.push("0");
    format!("COALESCE({})", expressions.join(", "))
}

fn read_thread_cwd_stats(codex_home: &Path) -> Result<Vec<ThreadCwdStat>, String> {
    let db_path = state_db_path(codex_home);
    if !path_exists(&db_path) {
        return Ok(Vec::new());
    }

    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| sqlite_error("读取 SQLite cwd 统计", error))?;
    conn.busy_timeout(Duration::from_millis(5000))
        .map_err(|error| sqlite_error("读取 SQLite cwd 统计", error))?;
    let columns = sqlite_columns(&conn, "threads")?;
    if !columns.contains("cwd") {
        return Ok(Vec::new());
    }

    let updated_at_expr = if columns.contains("updated_at_ms") {
        if columns.contains("updated_at") {
            "COALESCE(MAX(updated_at_ms), MAX(updated_at) * 1000, 0)"
        } else {
            "COALESCE(MAX(updated_at_ms), 0)"
        }
    } else if columns.contains("updated_at") {
        "COALESCE(MAX(updated_at) * 1000, 0)"
    } else {
        "0"
    };
    let sql = format!(
        "SELECT cwd, COUNT(*) AS count, {updated_at_expr} AS updated_at_ms \
         FROM threads \
         WHERE cwd IS NOT NULL AND cwd <> '' \
         GROUP BY cwd \
         ORDER BY count DESC, updated_at_ms DESC, cwd"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|error| sqlite_error("读取 SQLite cwd 统计", error))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|error| sqlite_error("读取 SQLite cwd 统计", error))?;

    let mut stats = Vec::new();
    for row in rows {
        let (cwd, count, updated_at_ms) =
            row.map_err(|error| sqlite_error("读取 SQLite cwd 统计", error))?;
        let Some(normalized_cwd) = normalize_comparable_path(&cwd) else {
            continue;
        };
        stats.push(ThreadCwdStat {
            cwd,
            normalized_cwd,
            count: count.max(0) as usize,
            updated_at_ms,
        });
    }
    Ok(stats)
}

fn read_workspace_roots_from_global_state(state: &Value) -> Vec<String> {
    let saved_roots = to_path_array(state.get("electron-saved-workspace-roots"));
    let project_order = to_path_array(state.get("project-order"));
    let active_roots = to_path_array(state.get("active-workspace-roots"));
    let combined = if project_order.is_empty() {
        [saved_roots, active_roots].concat()
    } else {
        [project_order, saved_roots, active_roots].concat()
    };
    dedupe_paths(
        combined
            .into_iter()
            .map(|path| to_normal_workspace_path(&path))
            .collect(),
    )
}

fn read_project_thread_visibility(codex_home: &Path) -> Result<Vec<String>, String> {
    let file_path = global_state_path(codex_home);
    if !path_exists(&file_path) {
        return Ok(Vec::new());
    }
    let state_text = fs::read_to_string(&file_path).map_err(command_error)?;
    let state = serde_json::from_str::<Value>(&state_text).map_err(command_error)?;
    let roots = read_workspace_roots_from_global_state(&state);
    if roots.is_empty() {
        return Ok(Vec::new());
    }

    let db_path = state_db_path(codex_home);
    if !path_exists(&db_path) {
        return Ok(roots
            .into_iter()
            .map(|root| {
                format!(
                    "{}: interactive 0, first page 0/50, ranks (none), exact cwd 0/0, providers (none)",
                    to_normal_workspace_path(&root)
                )
            })
            .collect());
    }

    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| sqlite_error("读取项目可见性诊断", error))?;
    conn.busy_timeout(Duration::from_millis(5000))
        .map_err(|error| sqlite_error("读取项目可见性诊断", error))?;
    let columns = sqlite_columns(&conn, "threads")?;
    if !columns.contains("cwd") {
        return Ok(Vec::new());
    }

    let source_filter = if columns.contains("source") {
        "AND source IN ('cli', 'vscode')"
    } else {
        ""
    };
    let archived_filter = if columns.contains("archived") {
        "AND archived = 0"
    } else {
        ""
    };
    let first_user_filter = if columns.contains("first_user_message") {
        "AND first_user_message <> ''"
    } else {
        ""
    };
    let time_expr = build_time_expression(&columns);
    let provider_expr = if columns.contains("model_provider") {
        "model_provider"
    } else {
        "'' AS model_provider"
    };
    let sql = format!(
        "SELECT id, cwd, {provider_expr}, {time_expr} AS sort_ts \
         FROM threads \
         WHERE cwd IS NOT NULL AND cwd <> '' \
         {archived_filter} \
         {first_user_filter} \
         {source_filter} \
         ORDER BY sort_ts DESC, id DESC"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|error| sqlite_error("读取项目可见性诊断", error))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            ))
        })
        .map_err(|error| sqlite_error("读取项目可见性诊断", error))?;

    let mut ranked_rows = Vec::new();
    for (index, row) in rows.enumerate() {
        let (cwd, provider) = row.map_err(|error| sqlite_error("读取项目可见性诊断", error))?;
        ranked_rows.push((
            index + 1,
            cwd.clone(),
            to_normal_workspace_path(&cwd),
            normalize_comparable_path(&cwd),
            if provider.trim().is_empty() {
                "(missing)".to_string()
            } else {
                provider
            },
        ));
    }

    Ok(roots
        .into_iter()
        .map(|root| {
            let normalized_root = normalize_comparable_path(&root);
            let exact_root = to_normal_workspace_path(&root);
            let matching_rows: Vec<_> = ranked_rows
                .iter()
                .filter(|(_, _, _, normalized_cwd, _)| *normalized_cwd == normalized_root)
                .collect();
            let ranks: Vec<usize> = matching_rows.iter().map(|(rank, _, _, _, _)| *rank).collect();
            let rank_preview = if ranks.is_empty() {
                "(none)".to_string()
            } else {
                let preview = ranks
                    .iter()
                    .take(12)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                if ranks.len() > 12 {
                    format!("{preview} (+{} more)", ranks.len() - 12)
                } else {
                    preview
                }
            };
            let mut provider_counts = HashMap::new();
            let mut exact_matches = 0usize;
            let mut verbatim_cwd_rows = 0usize;
            for (_, cwd, desktop_cwd, _, provider) in &matching_rows {
                *provider_counts.entry((*provider).clone()).or_insert(0) += 1;
                if *cwd == exact_root {
                    exact_matches += 1;
                }
                if cwd.starts_with(r"\\?\") {
                    verbatim_cwd_rows += 1;
                }
                let _ = desktop_cwd;
            }
            let first_page = ranks.iter().filter(|rank| **rank <= 50).count();
            format!(
                "{exact_root}: interactive {}, first page {first_page}/50, ranks {rank_preview}, exact cwd {exact_matches}/{}, verbatim cwd {verbatim_cwd_rows}, providers {}",
                matching_rows.len(),
                matching_rows.len(),
                format_counts(&provider_counts)
            )
        })
        .collect())
}

fn sync_workspace_roots(
    codex_home: &Path,
    cwd_stats: &[ThreadCwdStat],
) -> Result<WorkspaceRootSyncStats, String> {
    let file_path = global_state_path(codex_home);
    if !path_exists(&file_path) {
        return Ok(WorkspaceRootSyncStats::default());
    }

    let original_text = fs::read_to_string(&file_path).map_err(command_error)?;
    let mut state = serde_json::from_str::<Value>(&original_text).map_err(command_error)?;
    let Some(map) = state.as_object_mut() else {
        return Err(format!(
            "{} 不是 JSON object，无法同步项目根路径缓存。",
            path_to_string(&file_path)
        ));
    };

    let existing_saved_roots = to_path_array(map.get("electron-saved-workspace-roots"));
    let existing_project_order = to_path_array(map.get("project-order"));
    let existing_active_roots = to_path_array(map.get("active-workspace-roots"));

    let next_saved_roots = dedupe_paths(
        (if existing_project_order.is_empty() {
            [existing_saved_roots.clone(), existing_active_roots.clone()].concat()
        } else {
            [
                existing_project_order.clone(),
                existing_saved_roots.clone(),
                existing_active_roots.clone(),
            ]
            .concat()
        })
        .into_iter()
        .map(|path| resolve_stored_path(&path, cwd_stats))
        .collect(),
    );
    let next_project_order = dedupe_paths(
        (if existing_project_order.is_empty() {
            next_saved_roots.clone()
        } else {
            [existing_project_order.clone(), existing_saved_roots.clone()].concat()
        })
        .into_iter()
        .map(|path| resolve_stored_path(&path, cwd_stats))
        .collect(),
    );
    let next_active_roots = dedupe_paths(
        existing_active_roots
            .iter()
            .map(|path| resolve_stored_path(path, cwd_stats))
            .collect(),
    );

    let original_active_value = map.get("active-workspace-roots").cloned();
    let next_active_value = if original_active_value
        .as_ref()
        .map(Value::is_array)
        .unwrap_or(false)
    {
        strings_to_json_array(&next_active_roots)
    } else {
        next_active_roots
            .first()
            .cloned()
            .map(Value::String)
            .unwrap_or_else(|| original_active_value.clone().unwrap_or(Value::Null))
    };
    let next_labels = map
        .get("electron-workspace-root-labels")
        .map(|value| copy_resolved_object_keys(value, cwd_stats));
    let next_open_targets = map.get("open-in-target-preferences").map(|value| {
        let Value::Object(original) = value else {
            return value.clone();
        };
        let mut next = original.clone();
        if let Some(per_path) = original.get("perPath") {
            next.insert(
                "perPath".to_string(),
                copy_resolved_object_keys(per_path, cwd_stats),
            );
        }
        Value::Object(next)
    });

    let saved_roots_changed = existing_saved_roots != next_saved_roots;
    let project_order_changed = existing_project_order != next_project_order;
    let active_roots_changed = original_active_value.as_ref() != Some(&next_active_value);
    let labels_changed = next_labels
        .as_ref()
        .map(|value| map.get("electron-workspace-root-labels") != Some(value))
        .unwrap_or(false);
    let open_targets_changed = next_open_targets
        .as_ref()
        .map(|value| map.get("open-in-target-preferences") != Some(value))
        .unwrap_or(false);
    let backup_missing = !path_exists(&global_state_backup_path(codex_home));

    map.insert(
        "electron-saved-workspace-roots".to_string(),
        strings_to_json_array(&next_saved_roots),
    );
    map.insert(
        "project-order".to_string(),
        strings_to_json_array(&next_project_order),
    );
    map.insert("active-workspace-roots".to_string(), next_active_value);
    if let Some(next_labels) = next_labels {
        map.insert("electron-workspace-root-labels".to_string(), next_labels);
    }
    if let Some(next_open_targets) = next_open_targets {
        map.insert("open-in-target-preferences".to_string(), next_open_targets);
    }

    let updated = saved_roots_changed
        || project_order_changed
        || active_roots_changed
        || labels_changed
        || open_targets_changed
        || backup_missing;
    if updated {
        let next_text = format!(
            "{}\n",
            serde_json::to_string_pretty(&state).map_err(command_error)?
        );
        fs::write(&file_path, &next_text).map_err(command_error)?;
        fs::write(global_state_backup_path(codex_home), next_text).map_err(command_error)?;
    }

    Ok(WorkspaceRootSyncStats {
        present: true,
        updated,
        updated_workspace_roots: count_array_changes(&existing_saved_roots, &next_saved_roots),
        saved_workspace_root_count: next_saved_roots.len(),
    })
}

fn assert_sqlite_writable(codex_home: &Path) -> Result<bool, String> {
    let db_path = state_db_path(codex_home);
    if !path_exists(&db_path) {
        return Ok(false);
    }
    let conn =
        Connection::open(&db_path).map_err(|error| sqlite_error("检查 SQLite 写入权限", error))?;
    conn.busy_timeout(Duration::from_millis(5000))
        .map_err(|error| sqlite_error("检查 SQLite 写入权限", error))?;
    conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
        .map_err(|error| sqlite_error("检查 SQLite 写入权限", error))?;
    Ok(true)
}

fn sqlite_quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn sqlite_thread_insert_value(
    record: &RolloutIndexRecord,
    column: &str,
    target_provider: &str,
) -> Option<SqlValue> {
    let value = match column {
        "id" => SqlValue::Text(record.thread_id.clone()),
        "rollout_path" => SqlValue::Text(path_to_string(&record.path)),
        "created_at" => SqlValue::Integer(record.created_at),
        "updated_at" => SqlValue::Integer(record.updated_at),
        "source" => SqlValue::Text(record.source.clone()),
        "model_provider" => SqlValue::Text(target_provider.to_string()),
        "cwd" => SqlValue::Text(record.cwd.clone()),
        "title" => SqlValue::Text(record.title.clone()),
        "sandbox_policy" => SqlValue::Text(record.sandbox_policy.clone()),
        "approval_mode" => SqlValue::Text(record.approval_mode.clone()),
        "tokens_used" => SqlValue::Integer(0),
        "has_user_event" => SqlValue::Integer(if record.has_user_event { 1 } else { 0 }),
        "archived" => SqlValue::Integer(if record.archived { 1 } else { 0 }),
        "archived_at" => SqlValue::Null,
        "git_sha" => SqlValue::Null,
        "git_branch" => SqlValue::Null,
        "git_origin_url" => SqlValue::Null,
        "cli_version" => SqlValue::Text(record.cli_version.clone()),
        "first_user_message" => SqlValue::Text(record.first_user_message.clone()),
        "agent_nickname" => SqlValue::Null,
        "agent_role" => SqlValue::Null,
        "memory_mode" => SqlValue::Text("enabled".into()),
        "model" => record
            .model
            .clone()
            .map(SqlValue::Text)
            .unwrap_or(SqlValue::Null),
        "reasoning_effort" => record
            .reasoning_effort
            .clone()
            .map(SqlValue::Text)
            .unwrap_or(SqlValue::Null),
        "agent_path" => SqlValue::Null,
        "created_at_ms" => SqlValue::Integer(record.created_at.saturating_mul(1000)),
        "updated_at_ms" => SqlValue::Integer(record.updated_at.saturating_mul(1000)),
        "thread_source" => SqlValue::Text(record.thread_source.clone()),
        "preview" => SqlValue::Text(record.first_user_message.clone()),
        _ => return None,
    };
    Some(value)
}

fn insert_missing_sqlite_thread_indexes(
    tx: &rusqlite::Transaction<'_>,
    columns: &HashSet<String>,
    scan: &RolloutScan,
    target_provider: &str,
) -> Result<usize, String> {
    if scan.index_records.is_empty() || !columns.contains("id") {
        return Ok(0);
    }

    let insert_columns = THREAD_INSERT_COLUMN_ORDER
        .iter()
        .copied()
        .filter(|column| columns.contains(*column))
        .collect::<Vec<_>>();
    if insert_columns.is_empty() {
        return Ok(0);
    }

    let escaped_columns = insert_columns
        .iter()
        .map(|column| sqlite_quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (1..=insert_columns.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_sql = format!("INSERT INTO threads ({escaped_columns}) VALUES ({placeholders})");

    let mut inserted = 0usize;
    let mut exists_stmt = tx
        .prepare("SELECT 1 FROM threads WHERE id = ?1 LIMIT 1")
        .map_err(|error| sqlite_error("检查 SQLite thread 索引", error))?;
    let mut insert_stmt = tx
        .prepare(&insert_sql)
        .map_err(|error| sqlite_error("插入 SQLite thread 索引", error))?;

    for record in &scan.index_records {
        if record.thread_id.trim().is_empty() {
            continue;
        }
        let exists = exists_stmt
            .exists(params![record.thread_id])
            .map_err(|error| sqlite_error("检查 SQLite thread 索引", error))?;
        if exists {
            continue;
        }

        let values = insert_columns
            .iter()
            .filter_map(|column| sqlite_thread_insert_value(record, column, target_provider))
            .collect::<Vec<_>>();
        if values.len() != insert_columns.len() {
            continue;
        }
        insert_stmt
            .execute(params_from_iter(values.iter()))
            .map_err(|error| sqlite_error("插入 SQLite thread 索引", error))?;
        inserted += 1;
    }

    Ok(inserted)
}

fn update_sqlite_provider(
    codex_home: &Path,
    target_provider: &str,
    scan: &RolloutScan,
) -> Result<SqliteUpdateStats, String> {
    let db_path = state_db_path(codex_home);
    if !path_exists(&db_path) {
        return Ok(SqliteUpdateStats::default());
    }

    let mut conn =
        Connection::open(&db_path).map_err(|error| sqlite_error("更新 SQLite provider", error))?;
    conn.busy_timeout(Duration::from_millis(5000))
        .map_err(|error| sqlite_error("更新 SQLite provider", error))?;
    let columns = sqlite_columns(&conn, "threads")?;
    let tx = conn
        .transaction()
        .map_err(|error| sqlite_error("更新 SQLite provider", error))?;
    let mut stats = SqliteUpdateStats {
        database_present: true,
        ..Default::default()
    };

    stats.thread_rows_inserted =
        insert_missing_sqlite_thread_indexes(&tx, &columns, scan, target_provider)?;

    if columns.contains("model_provider") {
        stats.provider_rows_updated = tx
            .execute(
                "UPDATE threads SET model_provider = ?1 WHERE COALESCE(model_provider, '') <> ?1",
                params![target_provider],
            )
            .map_err(|error| sqlite_error("更新 SQLite provider", error))?;
    }

    if columns.contains("has_user_event") && !scan.user_event_thread_ids.is_empty() {
        let mut stmt = tx
            .prepare(
                "UPDATE threads SET has_user_event = 1 \
                 WHERE id = ?1 AND COALESCE(has_user_event, 0) <> 1",
            )
            .map_err(|error| sqlite_error("更新 SQLite user-event 标记", error))?;
        for thread_id in &scan.user_event_thread_ids {
            stats.user_event_rows_updated += stmt
                .execute(params![thread_id])
                .map_err(|error| sqlite_error("更新 SQLite user-event 标记", error))?;
        }
    }

    if columns.contains("cwd") && !scan.thread_cwd_by_id.is_empty() {
        let mut stmt = tx
            .prepare("UPDATE threads SET cwd = ?1 WHERE id = ?2 AND COALESCE(cwd, '') <> ?1")
            .map_err(|error| sqlite_error("更新 SQLite cwd", error))?;
        for (thread_id, cwd) in &scan.thread_cwd_by_id {
            if thread_id.trim().is_empty() || cwd.trim().is_empty() {
                continue;
            }
            stats.cwd_rows_updated += stmt
                .execute(params![cwd, thread_id])
                .map_err(|error| sqlite_error("更新 SQLite cwd", error))?;
        }
    }

    tx.commit()
        .map_err(|error| sqlite_error("提交 SQLite 更新", error))?;
    Ok(stats)
}

fn format_counts(counts: &HashMap<String, usize>) -> String {
    if counts.is_empty() {
        return "(none)".into();
    }
    let mut entries: Vec<_> = counts.iter().collect();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    entries
        .into_iter()
        .map(|(key, count)| format!("{key}: {count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn create_sync_backup(scan: &RolloutScan, target_provider: &str) -> Result<BackupInfo, String> {
    let codex_home = codex_home()?;
    let backup = create_backup(&sync_backup_file_paths(&codex_home), "native-provider-sync")?;
    let backup_dir = PathBuf::from(&backup.backup_dir);
    let session_manifest = json!({
        "version": 1,
        "tool": "Codex API Tools",
        "engine": NATIVE_SYNC_ENGINE,
        "targetProvider": target_provider,
        "createdAtUnixMs": unix_millis().to_string(),
        "changedSessionFiles": scan.changes.len(),
        "files": scan.changes.iter().map(|change| {
            json!({
                "path": path_to_string(&change.path),
                "threadId": change.thread_id,
                "cwd": change.cwd,
                "hasUserEvent": change.has_user_event,
                "originalFirstLine": change.original_first_line,
                "originalSeparator": change.original_separator,
                "originalOffset": change.original_offset,
                "originalSize": change.original_size,
                "originalMtimeUnixSeconds": change.original_mtime.unix_seconds(),
                "originalMtimeNanoseconds": change.original_mtime.nanoseconds(),
            })
        }).collect::<Vec<_>>()
    });
    fs::write(
        backup_dir.join("session-meta-backup.json"),
        serde_json::to_string_pretty(&session_manifest).map_err(command_error)?,
    )
    .map_err(command_error)?;
    Ok(backup)
}

fn current_provider_from_config() -> String {
    config_path()
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|text| parse_root_provider(&text))
        .filter(|provider| !provider.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CODEX_PROVIDER.to_string())
}

fn resolve_sync_target(provider_id: Option<String>) -> String {
    provider_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(current_provider_from_config)
}

fn provider_choices(
    config_text: Option<&str>,
    rollout_counts: &HashMap<String, usize>,
    sqlite_counts: Option<&HashMap<String, usize>>,
) -> Vec<String> {
    let mut choices = HashSet::new();
    choices.insert(DEFAULT_CODEX_PROVIDER.to_string());

    if let Some(config_text) = config_text {
        choices.insert(parse_root_provider(config_text));
        for section in parse_provider_sections(config_text) {
            choices.insert(section.id);
        }
    }

    for key in rollout_counts.keys() {
        if let Some((_, provider)) = key.split_once('/') {
            if provider != "(missing)" {
                choices.insert(provider.to_string());
            }
        }
    }
    if let Some(sqlite_counts) = sqlite_counts {
        for key in sqlite_counts.keys() {
            if let Some((_, provider)) = key.split_once('/') {
                if provider != "(missing)" {
                    choices.insert(provider.to_string());
                }
            }
        }
    }

    let mut output: Vec<String> = choices
        .into_iter()
        .filter(|provider| !provider.trim().is_empty())
        .collect();
    output.sort();
    output
}

fn render_native_status() -> Result<String, String> {
    let codex_home = codex_home()?;
    let current_provider = current_provider_from_config();
    let configured_providers = config_path()
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|text| {
            parse_provider_sections(&text)
                .into_iter()
                .map(|section| section.id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let rollout_scan = collect_rollout_scan(&codex_home, None)?;
    let sqlite_counts = read_sqlite_provider_counts(&codex_home)?;
    let project_visibility = read_project_thread_visibility(&codex_home)?;

    let mut lines = vec![
        format!("Codex home: {}", path_to_string(&codex_home)),
        format!("Current provider: {current_provider}"),
        format!(
            "Configured providers: {}",
            if configured_providers.is_empty() {
                "(none)".into()
            } else {
                configured_providers.join(", ")
            }
        ),
        format!("Sync engine: {NATIVE_SYNC_ENGINE}"),
        String::new(),
        "Rollout files:".into(),
        format!("  {}", format_counts(&rollout_scan.provider_counts)),
        String::new(),
        "SQLite state:".into(),
    ];
    match sqlite_counts {
        Some(counts) => lines.push(format!("  {}", format_counts(&counts))),
        None => lines.push(format!("  {DB_FILE_BASENAME} not found")),
    }
    lines.push(format!(
        "Repair hints: user-event thread ids {}, cwd rows {}",
        rollout_scan.user_event_thread_ids.len(),
        rollout_scan.thread_cwd_by_id.len()
    ));
    if !project_visibility.is_empty() {
        lines.push(String::new());
        lines.push("Project visibility:".into());
        for project in project_visibility {
            lines.push(format!("  {project}"));
        }
    }

    Ok(lines.join("\n"))
}

fn run_native_sync(target_provider: &str, force_process_preflight: bool) -> Result<String, String> {
    if !force_process_preflight {
        ensure_codex_stopped_for_write("同步历史会话 metadata")?;
    }
    let codex_home = codex_home()?;
    let scan = collect_rollout_scan(&codex_home, Some(target_provider))?;
    let cwd_stats = read_thread_cwd_stats(&codex_home)?;
    let sqlite_present = assert_sqlite_writable(&codex_home)?;
    let backup = create_sync_backup(&scan, target_provider)?;
    let (applied_rollouts, skipped_rollouts) = apply_rollout_changes(&scan.changes)?;
    let sqlite_stats = update_sqlite_provider(&codex_home, target_provider, &scan)?;
    let workspace_stats = sync_workspace_roots(&codex_home, &cwd_stats)?;

    let mut lines = vec![
        format!("Target provider: {target_provider}"),
        format!("Backup: {}", backup.backup_dir),
        format!("Updated rollout files: {applied_rollouts}"),
        format!(
            "Updated SQLite rows: {}{}",
            sqlite_stats.thread_rows_inserted
                + sqlite_stats.provider_rows_updated
                + sqlite_stats.user_event_rows_updated
                + sqlite_stats.cwd_rows_updated,
            if sqlite_present && sqlite_stats.database_present {
                ""
            } else {
                " (state_5.sqlite not found)"
            }
        ),
        format!(
            "Updated workspace roots: {}{}",
            workspace_stats.updated_workspace_roots,
            if workspace_stats.present {
                format!(
                    " (saved roots {}, {})",
                    workspace_stats.saved_workspace_root_count,
                    if workspace_stats.updated {
                        "changed"
                    } else {
                        "unchanged"
                    }
                )
            } else {
                " (.codex-global-state.json not found)".to_string()
            }
        ),
    ];
    if force_process_preflight {
        lines.push(
            "Process preflight skipped by --force; SQLite write lock was still checked.".into(),
        );
    }
    if sqlite_stats.thread_rows_inserted > 0 {
        lines.push(format!(
            "Inserted SQLite thread indexes: {}",
            sqlite_stats.thread_rows_inserted
        ));
    }
    if sqlite_stats.user_event_rows_updated > 0 {
        lines.push(format!(
            "Updated SQLite user-event flags: {}",
            sqlite_stats.user_event_rows_updated
        ));
    }
    if sqlite_stats.cwd_rows_updated > 0 {
        lines.push(format!(
            "Updated SQLite cwd paths: {}",
            sqlite_stats.cwd_rows_updated
        ));
    }
    if !skipped_rollouts.is_empty() {
        lines.push(format!(
            "Skipped changed or locked rollout files: {}",
            skipped_rollouts.len()
        ));
        for path in skipped_rollouts.iter().take(5) {
            lines.push(format!("  {path}"));
        }
        if skipped_rollouts.len() > 5 {
            lines.push(format!("  (+{} more)", skipped_rollouts.len() - 5));
        }
    }
    lines.push(String::new());
    lines.push("Rollout files before sync:".into());
    lines.push(format!("  {}", format_counts(&scan.provider_counts)));
    Ok(lines.join("\n"))
}

fn run_native_switch(target_provider: &str) -> Result<String, String> {
    ensure_codex_stopped_for_write("切换 provider 并同步历史会话")?;
    let config_path = config_path()?;
    let config_text = fs::read_to_string(&config_path).map_err(command_error)?;
    let backup = create_backup(&[config_path.clone()], "switch-root-provider")?;
    fs::write(
        &config_path,
        set_root_provider(&config_text, target_provider),
    )
    .map_err(command_error)?;
    let sync_output = run_native_sync(target_provider, false)?;
    Ok(format!(
        "Switched root provider to {target_provider}.\nConfig backup: {}\n\n{sync_output}",
        backup.backup_dir
    ))
}

fn native_shell_result(
    started_at: u128,
    command_text: String,
    outcome: Result<String, String>,
) -> ShellResult {
    match outcome {
        Ok(stdout) => ShellResult {
            ok: true,
            code: Some(0),
            stdout,
            stderr: String::new(),
            command: command_text,
            duration_ms: unix_millis().saturating_sub(started_at),
            node_requirement: String::new(),
        },
        Err(stderr) => ShellResult {
            ok: false,
            code: Some(1),
            stdout: String::new(),
            stderr,
            command: command_text,
            duration_ms: unix_millis().saturating_sub(started_at),
            node_requirement: String::new(),
        },
    }
}

fn list_backup_dirs_newest_first() -> Result<Vec<PathBuf>, String> {
    let root = backup_root()?;
    if !path_exists(&root) {
        return Ok(Vec::new());
    }
    let mut dirs = Vec::new();
    for entry in fs::read_dir(root).map_err(command_error)? {
        let entry = entry.map_err(command_error)?;
        if entry.file_type().map_err(command_error)?.is_dir() {
            dirs.push(entry.path());
        }
    }
    dirs.sort_by(|left, right| right.cmp(left));
    Ok(dirs)
}

fn runtime_status() -> RuntimeStatus {
    RuntimeStatus {
        node_version: "not required".into(),
        node_ok_for_provider_sync: true,
        npx_version: "not required".into(),
        sync_package: NATIVE_SYNC_ENGINE.into(),
        sync_engine: NATIVE_SYNC_ENGINE.into(),
        node_required_for_sync: false,
    }
}

fn detect_codex_processes() -> Result<Vec<CodexProcess>, String> {
    let current_pid = process::id();
    let output = if cfg!(windows) {
        Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-CimInstance Win32_Process | ForEach-Object { \"$($_.ProcessId)`t$($_.CommandLine)\" }",
            ])
            .output()
    } else {
        Command::new("ps").args(["-axo", "pid=,args="]).output()
    };

    let output = output.map_err(|error| format!("无法执行 Codex 进程检测命令：{error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("退出码 {:?}", output.status.code())
        };
        return Err(format!("Codex 进程检测命令失败：{detail}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut processes = Vec::new();
    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let (pid, command) = if cfg!(windows) {
            let Some((pid_text, command_text)) = line.split_once('\t') else {
                continue;
            };
            let Ok(pid) = pid_text.trim().parse::<u32>() else {
                continue;
            };
            (pid, command_text.trim().to_string())
        } else {
            let Some((pid_text, command_text)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            let Ok(pid) = pid_text.trim().parse::<u32>() else {
                continue;
            };
            (pid, command_text.trim().to_string())
        };

        if pid == current_pid || !is_codex_process_command(&command) {
            continue;
        }
        processes.push(CodexProcess { pid, command });
    }
    processes.sort_by(|left, right| left.pid.cmp(&right.pid));
    Ok(processes)
}

fn is_codex_process_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    let executable = command_executable(command);
    let name = executable_file_name(executable);
    let name = name.trim_end_matches(".exe");
    if lower.is_empty()
        || lower.contains("codex api tools")
        || lower.contains("codex-api-tools")
        || lower.contains("gpt-api-tools")
        || lower.contains("codex-tools")
        || lower.contains("codex computer use.app")
        || lower.contains("skycomputeruseclient")
        || lower.contains("browser_crashpad_handler")
        || lower.contains("crashpad_handler")
        || name == "browser_crashpad_handler"
        || name == "crashpad_handler"
        || (lower.contains("/.vscode/extensions/openai.chatgpt-")
            && lower.contains("codex app-server"))
    {
        return false;
    }

    if lower.contains("/applications/codex.app/") || lower.contains("\\codex.app\\") {
        return true;
    }

    name == "codex"
}

fn command_executable(command: &str) -> &str {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return trimmed;
    }

    for quote in ['"', '\''] {
        if let Some(rest) = trimmed.strip_prefix(quote) {
            return rest.find(quote).map(|end| &rest[..end]).unwrap_or(rest);
        }
    }

    trimmed.split_whitespace().next().unwrap_or(trimmed)
}

fn executable_file_name(executable: &str) -> String {
    let trimmed = executable.trim().trim_matches('"').trim_matches('\'');
    trimmed
        .rsplit(|ch| ch == '/' || ch == '\\')
        .next()
        .unwrap_or(trimmed)
        .to_ascii_lowercase()
}

fn run_quit_codex_commands() -> Vec<String> {
    let mut commands = Vec::new();
    if cfg!(target_os = "macos") {
        let command_text = "osascript -e 'tell application \"Codex\" to quit'";
        commands.push(command_text.to_string());
        let _ = Command::new("osascript")
            .args(["-e", "tell application \"Codex\" to quit"])
            .status();
    } else if cfg!(windows) {
        for image in ["Codex.exe", "codex.exe"] {
            let command_text = format!("taskkill /IM {image}");
            commands.push(command_text);
            let _ = Command::new("taskkill").args(["/IM", image]).status();
        }
    } else {
        let command_text = "pkill -TERM -f 'codex app-server'";
        commands.push(command_text.to_string());
        let _ = Command::new("pkill")
            .args(["-TERM", "-f", "codex app-server"])
            .status();
    }
    commands
}

fn ensure_codex_stopped_for_write(action: &str) -> Result<(), String> {
    let processes = detect_codex_processes().map_err(|error| {
        format!("{action} 前无法确认 Codex 是否已完全退出。请手动关闭 Codex App、Codex CLI 和 app-server 后重试。检测错误：{error}")
    })?;
    if processes.is_empty() {
        return Ok(());
    }
    let preview = processes
        .iter()
        .take(5)
        .map(|process| format!("{} {}", process.pid, process.command))
        .collect::<Vec<_>>()
        .join("; ");
    Err(format!(
        "{action} 前必须完全退出 Codex App、Codex CLI 和 app-server。当前仍检测到 {} 个相关进程：{}",
        processes.len(),
        preview
    ))
}

fn mask_email(value: &str) -> String {
    let Some((local, domain)) = value.split_once('@') else {
        return mask_secret(value);
    };
    let mut chars = local.chars();
    let first = chars.next().unwrap_or('*');
    let visible_tail = local.chars().rev().next().filter(|_| local.len() > 3);
    match visible_tail {
        Some(tail) => format!("{first}***{tail}@{domain}"),
        None => format!("{first}***@{domain}"),
    }
}

fn decode_jwt_payload<T: for<'de> Deserialize<'de>>(jwt: &str) -> Result<T, String> {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() < 3 || parts[1].is_empty() {
        return Err("JWT 格式无效。".into());
    }
    let payload = URL_SAFE_NO_PAD
        .decode(parts[1])
        .or_else(|_| URL_SAFE.decode(parts[1]))
        .map_err(|error| format!("JWT payload base64 解码失败：{error}"))?;
    serde_json::from_slice::<T>(&payload)
        .map_err(|error| format!("JWT payload JSON 解析失败：{error}"))
}

fn load_local_chatgpt_auth_for_quota() -> Result<LocalChatGptAuth, String> {
    let auth_path = auth_path()?;
    if !path_exists(&auth_path) {
        return Err("auth.json 不存在。请先在 Codex 中登录 ChatGPT 账号。".into());
    }
    let raw = fs::read_to_string(&auth_path).map_err(command_error)?;
    let parsed: AuthDotJsonRaw = serde_json::from_str(&raw).map_err(command_error)?;
    if parsed.openai_api_key.is_some()
        || parsed
            .auth_mode
            .as_deref()
            .map(|mode| mode.eq_ignore_ascii_case("apikey") || mode.eq_ignore_ascii_case("api_key"))
            .unwrap_or(false)
    {
        return Err(
            "当前 auth.json 是 API Key 登录，不是 ChatGPT 登录，无法查询 GPT 订阅额度。".into(),
        );
    }
    let tokens = parsed.tokens.ok_or_else(|| {
        "auth.json 缺少 ChatGPT tokens，请在 Codex 中重新登录 ChatGPT。".to_string()
    })?;
    if tokens.access_token.trim().is_empty() {
        return Err("auth.json 缺少 access_token，请在 Codex 中重新登录 ChatGPT。".into());
    }
    let claims: JwtClaims = decode_jwt_payload(&tokens.id_token)?;
    let auth_claims = claims.auth;
    let account_id = tokens
        .account_id
        .or_else(|| {
            auth_claims
                .as_ref()
                .and_then(|auth| auth.chatgpt_account_id.clone())
        })
        .ok_or_else(|| {
            "auth.json 缺少 ChatGPT account_id，请在 Codex 中重新登录 ChatGPT。".to_string()
        })?;
    let email = claims
        .email
        .or_else(|| claims.profile.and_then(|profile| profile.email));
    let plan_type = auth_claims
        .as_ref()
        .and_then(|auth| auth.chatgpt_plan_type.clone());
    let fedramp = auth_claims
        .as_ref()
        .map(|auth| auth.chatgpt_account_is_fedramp)
        .unwrap_or(false);

    Ok(LocalChatGptAuth {
        auth_mode: parsed.auth_mode,
        account_id,
        email,
        plan_type,
        access_token: tokens.access_token,
        last_refresh: parsed.last_refresh,
        fedramp,
    })
}

fn number_as_f64(value: Option<&Value>) -> Option<f64> {
    value.and_then(|value| match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.parse::<f64>().ok(),
        _ => None,
    })
}

fn number_as_i64(value: Option<&Value>) -> Option<i64> {
    value.and_then(|value| match value {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.parse::<i64>().ok(),
        _ => None,
    })
}

fn text_value(value: Option<&Value>) -> Option<String> {
    value.and_then(|value| match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    })
}

fn parse_quota_window(value: Option<&Value>, label: &str) -> Option<QuotaWindowView> {
    let value = value?;
    if value.is_null() {
        return None;
    }
    let used_percent = number_as_f64(value.get("used_percent"));
    let remaining_percent = used_percent.map(|used| (100.0 - used).max(0.0));
    let limit_window_seconds = number_as_i64(value.get("limit_window_seconds"));
    Some(QuotaWindowView {
        label: label.into(),
        used_percent,
        remaining_percent,
        window_minutes: limit_window_seconds.map(|seconds| (seconds + 59) / 60),
        resets_at: number_as_i64(value.get("reset_at")),
        reset_after_seconds: number_as_i64(value.get("reset_after_seconds")),
    })
}

fn parse_quota_bucket(
    limit_id: String,
    limit_name: Option<String>,
    value: Option<&Value>,
) -> QuotaBucketView {
    let details = value.filter(|value| !value.is_null());
    QuotaBucketView {
        limit_id,
        limit_name,
        allowed: details
            .and_then(|value| value.get("allowed"))
            .and_then(Value::as_bool),
        limit_reached: details
            .and_then(|value| value.get("limit_reached"))
            .and_then(Value::as_bool),
        primary: parse_quota_window(
            details.and_then(|value| value.get("primary_window")),
            "primary",
        ),
        secondary: parse_quota_window(
            details.and_then(|value| value.get("secondary_window")),
            "secondary",
        ),
    }
}

fn parse_quota_payload(
    payload: &Value,
) -> (
    Vec<QuotaBucketView>,
    Option<QuotaCreditsView>,
    Option<SpendControlView>,
    Option<String>,
    Option<String>,
) {
    let plan_type = text_value(payload.get("plan_type"));
    let mut buckets = vec![parse_quota_bucket(
        "codex".into(),
        None,
        payload.get("rate_limit"),
    )];
    if let Some(additional) = payload
        .get("additional_rate_limits")
        .and_then(Value::as_array)
    {
        for item in additional {
            let limit_id = text_value(item.get("metered_feature"))
                .or_else(|| text_value(item.get("limit_name")))
                .unwrap_or_else(|| "additional".into());
            let limit_name = text_value(item.get("limit_name"));
            buckets.push(parse_quota_bucket(
                limit_id,
                limit_name,
                item.get("rate_limit"),
            ));
        }
    }

    let credits = payload
        .get("credits")
        .filter(|value| !value.is_null())
        .map(|value| QuotaCreditsView {
            has_credits: value.get("has_credits").and_then(Value::as_bool),
            unlimited: value.get("unlimited").and_then(Value::as_bool),
            balance: text_value(value.get("balance")),
        });
    let spend_control = payload
        .get("spend_control")
        .and_then(|value| value.get("individual_limit"))
        .filter(|value| !value.is_null())
        .map(|value| SpendControlView {
            limit: text_value(value.get("limit")),
            used: text_value(value.get("used")),
            remaining: text_value(value.get("remaining")),
            used_percent: number_as_f64(value.get("used_percent")),
            remaining_percent: number_as_f64(value.get("remaining_percent")),
            resets_at: number_as_i64(value.get("reset_at")),
            reset_after_seconds: number_as_i64(value.get("reset_after_seconds")),
        });
    let rate_limit_reached_type = payload
        .get("rate_limit_reached_type")
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        .map(str::to_string);
    (
        buckets,
        credits,
        spend_control,
        rate_limit_reached_type,
        plan_type,
    )
}

fn quota_recommendation() -> String {
    "额度恢复后，可以先完全退出 Codex，再执行 `codex-tools relay gpt` 自动注释 base_url 和 experimental_bearer_token，然后重启 Codex 使用当前 GPT 订阅；需要中转时执行 `codex-tools relay restore` 再重启 Codex。".into()
}

fn quota_result_unavailable(
    status: &str,
    message: String,
    auth: Option<LocalChatGptAuth>,
) -> OpenAiQuotaResult {
    OpenAiQuotaResult {
        ok: false,
        status: status.into(),
        auth_mode: auth.as_ref().and_then(|auth| auth.auth_mode.clone()),
        account_id_masked: auth.as_ref().map(|auth| mask_secret(&auth.account_id)),
        email_masked: auth
            .as_ref()
            .and_then(|auth| auth.email.as_ref().map(|email| mask_email(email))),
        plan_type: auth.as_ref().and_then(|auth| auth.plan_type.clone()),
        last_refresh: auth.and_then(|auth| auth.last_refresh),
        endpoint: CHATGPT_USAGE_URL.into(),
        fetched_at_unix_ms: unix_millis(),
        buckets: Vec::new(),
        credits: None,
        spend_control: None,
        rate_limit_reached_type: None,
        message,
        recommendation: quota_recommendation(),
    }
}

fn check_openai_quota() -> Result<OpenAiQuotaResult, String> {
    let auth = match load_local_chatgpt_auth_for_quota() {
        Ok(auth) => auth,
        Err(error) => return Ok(quota_result_unavailable("unavailable", error, None)),
    };
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("codex-cli"));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", auth.access_token))
            .map_err(|error| format!("access_token header 无效：{error}"))?,
    );
    headers.insert(
        HeaderName::from_static("chatgpt-account-id"),
        HeaderValue::from_str(&auth.account_id)
            .map_err(|error| format!("account_id header 无效：{error}"))?,
    );
    if auth.fedramp {
        headers.insert(
            HeaderName::from_static("x-openai-fedramp"),
            HeaderValue::from_static("true"),
        );
    }

    let client = BlockingClient::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(command_error)?;
    let response = match client.get(CHATGPT_USAGE_URL).headers(headers).send() {
        Ok(response) => response,
        Err(error) => {
            return Ok(quota_result_unavailable(
                "error",
                format!("无法连接 OpenAI ChatGPT usage 接口：{error}"),
                Some(auth),
            ))
        }
    };
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = response.text().unwrap_or_default();
    if !status.is_success() {
        let preview = body.chars().take(320).collect::<String>();
        return Ok(quota_result_unavailable(
            "error",
            format!(
                "OpenAI usage 接口返回 HTTP {status}，content-type={content_type}，响应摘要：{preview}"
            ),
            Some(auth),
        ));
    }
    let payload: Value = match serde_json::from_str(&body) {
        Ok(payload) => payload,
        Err(error) => {
            return Ok(quota_result_unavailable(
                "error",
                format!("OpenAI usage 响应不是可识别 JSON：{error}"),
                Some(auth),
            ))
        }
    };
    let (buckets, credits, spend_control, rate_limit_reached_type, plan_type_from_usage) =
        parse_quota_payload(&payload);
    let plan_type = plan_type_from_usage.or_else(|| auth.plan_type.clone());
    let message = if buckets
        .iter()
        .any(|bucket| bucket.limit_reached == Some(true))
    {
        "已读取当前 ChatGPT 账号额度，存在已触发的使用限制。".into()
    } else {
        "已读取当前 ChatGPT 账号额度。".into()
    };

    Ok(OpenAiQuotaResult {
        ok: true,
        status: "available".into(),
        auth_mode: auth.auth_mode,
        account_id_masked: Some(mask_secret(&auth.account_id)),
        email_masked: auth.email.as_ref().map(|email| mask_email(email)),
        plan_type,
        last_refresh: auth.last_refresh,
        endpoint: CHATGPT_USAGE_URL.into(),
        fetched_at_unix_ms: unix_millis(),
        buckets,
        credits,
        spend_control,
        rate_limit_reached_type,
        message,
        recommendation: quota_recommendation(),
    })
}

fn inspect() -> Result<InspectState, String> {
    let codex_home = codex_home()?;
    let config_path = config_path()?;
    let auth_path = auth_path()?;
    let config_text = fs::read_to_string(&config_path).ok();
    let rollout_scan = collect_rollout_scan(&codex_home, None).unwrap_or_default();
    let sqlite_counts = read_sqlite_provider_counts(&codex_home).ok().flatten();
    let provider_choices = provider_choices(
        config_text.as_deref(),
        &rollout_scan.provider_counts,
        sqlite_counts.as_ref(),
    );
    let backup_dirs = list_backup_dirs_newest_first()?
        .into_iter()
        .take(8)
        .map(|path| path_to_string(&path))
        .collect();
    let (codex_processes, codex_process_detection_error) = match detect_codex_processes() {
        Ok(processes) => (processes, None),
        Err(error) => (Vec::new(), Some(error)),
    };

    Ok(InspectState {
        codex_home: path_to_string(&codex_home),
        config_path: path_to_string(&config_path),
        auth_path: path_to_string(&auth_path),
        config_exists: config_text.is_some(),
        auth_exists: path_exists(&auth_path),
        codex_running: codex_process_detection_error.is_some() || !codex_processes.is_empty(),
        codex_processes,
        codex_process_detection_error,
        backup_root: path_to_string(&backup_root()?),
        backup_dirs,
        provider_choices,
        config: config_text.as_deref().map(parse_config),
        runtime: runtime_status(),
    })
}

fn run_provider_sync(
    command: Option<String>,
    provider_id: Option<String>,
    force_process_preflight: bool,
) -> Result<ShellResult, String> {
    let command_name = command.unwrap_or_else(|| "status".into());
    let started_at = unix_millis();
    let target_provider = resolve_sync_target(provider_id);
    let command_text = match command_name.as_str() {
        "status" => format!("{NATIVE_SYNC_ENGINE} status"),
        "sync" => format!(
            "{NATIVE_SYNC_ENGINE} sync --provider {target_provider}{}",
            if force_process_preflight {
                " --force"
            } else {
                ""
            }
        ),
        "switch" => format!("{NATIVE_SYNC_ENGINE} switch {target_provider}"),
        other => {
            return Ok(native_shell_result(
                started_at,
                format!("{NATIVE_SYNC_ENGINE} {other}"),
                Err(format!(
                    "不支持的同步命令：{other}。可用命令：status / sync / switch。"
                )),
            ))
        }
    };
    let outcome = match command_name.as_str() {
        "status" => render_native_status(),
        "sync" => run_native_sync(&target_provider, force_process_preflight),
        "switch" => run_native_switch(&target_provider),
        _ => unreachable!(),
    };
    Ok(native_shell_result(started_at, command_text, outcome))
}

fn repair_provider(
    custom_name: Option<String>,
    run_sync: Option<bool>,
) -> Result<RepairResult, String> {
    ensure_codex_stopped_for_write("重命名 provider 并同步历史会话")?;
    let provider_id = sanitize_provider_id(custom_name)?;
    let config_path = config_path()?;
    let config_text = fs::read_to_string(&config_path).map_err(command_error)?;
    let sections = parse_provider_sections(&config_text);
    let current_provider = parse_root_provider(&config_text);
    let mut target_section = find_provider_section_for_repair(&sections, &current_provider)
        .ok_or_else(|| {
            format!(
                "config.toml 里没有找到 OpenAI provider，也没有找到当前 provider {current_provider} 对应的 [model_providers.*] section。请先确认 config.toml。"
            )
        })?;
    let previous_provider_id = target_section.id.clone();
    let previous_provider_name = target_section
        .values
        .get("name")
        .cloned()
        .unwrap_or_else(|| previous_provider_id.clone());

    if sections
        .iter()
        .any(|section| section.id == provider_id && section.start != target_section.start)
    {
        return Err(format!(
            "config.toml 已存在 [model_providers.{provider_id}]，为避免覆盖没有自动合并。"
        ));
    }

    let backup = create_backup(&[config_path.clone()], "repair-provider-name")?;
    let mut lines = split_lines(&config_text);
    lines[target_section.start] = format!("[model_providers.{provider_id}]");
    upsert_key_in_section(
        &mut lines,
        &mut target_section,
        "name",
        &format!("\"{}\"", escape_toml_string(&provider_id)),
    );
    let with_provider_section = lines.join("\n");
    let next_text = set_root_provider(&with_provider_section, &provider_id);
    fs::write(&config_path, next_text).map_err(command_error)?;

    let sync = if run_sync.unwrap_or(true) {
        Some(run_provider_sync(
            Some("sync".into()),
            Some(provider_id.clone()),
            false,
        )?)
    } else {
        None
    };

    Ok(RepairResult {
        changed: true,
        provider_id: provider_id.clone(),
        backup_dir: Some(backup.backup_dir),
        message: format!(
            "已将 provider {previous_provider_id} / {previous_provider_name} 重命名为 {provider_id}，并将 root provider 指向 {provider_id}。"
        ),
        sync,
    })
}

fn backup_remove_auth() -> Result<AuthRemoveResult, String> {
    ensure_codex_stopped_for_write("备份并移除 auth.json")?;
    let auth_path = auth_path()?;
    if !path_exists(&auth_path) {
        return Ok(AuthRemoveResult {
            removed: false,
            auth_path: path_to_string(&auth_path),
            backup_dir: None,
            message: "auth.json 不存在，无需移除。".into(),
        });
    }
    let backup = create_backup(&[auth_path.clone()], "backup-and-remove-auth-json")?;
    fs::remove_file(&auth_path).map_err(command_error)?;
    Ok(AuthRemoveResult {
        removed: true,
        auth_path: path_to_string(&auth_path),
        backup_dir: Some(backup.backup_dir),
        message: "已备份并移除 auth.json。".into(),
    })
}

fn set_relay_lines_commented(
    provider_id: Option<String>,
    commented: bool,
) -> Result<RelayConfigToggleResult, String> {
    ensure_codex_stopped_for_write("切换中转配置注释状态")?;
    let config_path = config_path()?;
    let config_text = fs::read_to_string(&config_path).map_err(command_error)?;
    let target_provider = provider_id
        .unwrap_or_else(|| parse_root_provider(&config_text))
        .trim()
        .to_string();
    if target_provider.is_empty() {
        return Err("目标 Provider 不能为空。".into());
    }
    let sections = parse_provider_sections(&config_text);
    let section = find_provider_section_by_id_or_name(&sections, &target_provider)
        .ok_or_else(|| format!("config.toml 中没有找到 [model_providers.{target_provider}]。"))?;
    let (next_text, changed_keys) =
        relay_config_lines_with_comment_state(&config_text, &section, commented);
    if changed_keys.is_empty() {
        let action = if commented { "注释" } else { "恢复" };
        return Ok(RelayConfigToggleResult {
            changed: false,
            provider_id: target_provider,
            commented,
            changed_keys,
            backup_dir: None,
            message: format!("没有找到需要{action}的 base_url 或 experimental_bearer_token 行。"),
        });
    }

    let backup = create_backup(&[config_path.clone()], "toggle-relay-config-lines")?;
    fs::write(&config_path, next_text).map_err(command_error)?;
    let key_list = changed_keys.join("、");
    let message = if commented {
        format!(
            "已注释 [model_providers.{target_provider}] 中的 {key_list}；重启 Codex 后会优先使用当前 ChatGPT/GPT 订阅登录态。"
        )
    } else {
        format!(
            "已恢复 [model_providers.{target_provider}] 中的 {key_list}；重启 Codex 后会重新使用中转配置。"
        )
    };

    Ok(RelayConfigToggleResult {
        changed: true,
        provider_id: target_provider,
        commented,
        changed_keys,
        backup_dir: Some(backup.backup_dir),
        message,
    })
}

fn try_quit_codex() -> Result<QuitCodexResult, String> {
    let before = detect_codex_processes().map_err(|error| {
        format!("无法检测 Codex 是否正在运行。请手动完全关闭 Codex 后再重试。检测错误：{error}")
    })?;
    if before.is_empty() {
        return Ok(QuitCodexResult {
            attempted: false,
            commands: Vec::new(),
            still_running: false,
            processes: Vec::new(),
            message: "未检测到 Codex App / Codex CLI / app-server，可以写入配置和历史索引。".into(),
        });
    }

    let commands = run_quit_codex_commands();
    sleep(Duration::from_millis(1800));
    let processes = detect_codex_processes().map_err(|error| {
        format!("已尝试退出 Codex，但无法确认是否已完全退出。请手动完全关闭 Codex 后再写入。检测错误：{error}")
    })?;
    let still_running = !processes.is_empty();
    let message = if still_running {
        format!(
            "已尝试退出 Codex，但仍检测到 {} 个相关进程。请手动完全关闭 Codex App、Codex CLI 和 app-server 后再写入。",
            processes.len()
        )
    } else {
        "已请求 Codex 退出，当前未检测到相关进程。可以勾选确认后写入。".into()
    };

    Ok(QuitCodexResult {
        attempted: true,
        commands,
        still_running,
        processes,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_codex_home(name: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "codex-tools-{name}-{}-{}",
            process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_test_rollout(codex_home: &Path, provider: &str) -> PathBuf {
        let thread_id = "019cbf7c-1db4-7922-a155-d68fff7b82da";
        let rollout_path = codex_home
            .join("sessions")
            .join("2026")
            .join("03")
            .join("06")
            .join(format!("rollout-2026-03-06T03-31-48-{thread_id}.jsonl"));
        fs::create_dir_all(rollout_path.parent().unwrap()).unwrap();
        let records = vec![
            json!({
                "timestamp": "2026-03-06T03:31:48.000Z",
                "type": "session_meta",
                "payload": {
                    "id": thread_id,
                    "timestamp": "2026-03-06T03:31:48.000Z",
                    "cwd": "/Users/example/project",
                    "source": "vscode",
                    "cli_version": "0.137.0-alpha.4",
                    "model_provider": provider
                }
            }),
            json!({
                "timestamp": "2026-03-06T03:31:49.000Z",
                "type": "turn_context",
                "payload": {
                    "cwd": "/Users/example/project",
                    "approval_policy": "never",
                    "sandbox_policy": {"type": "disabled"},
                    "model": "gpt-5.3-codex",
                    "effort": "xhigh"
                }
            }),
            json!({
                "timestamp": "2026-03-06T03:31:50.000Z",
                "type": "event_msg",
                "payload": {
                    "type": "user_message",
                    "message": "请同步这个会话"
                }
            }),
        ];
        let text = records
            .into_iter()
            .map(|record| serde_json::to_string(&record).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(&rollout_path, text).unwrap();
        rollout_path
    }

    fn create_test_threads_db(codex_home: &Path) {
        let conn = Connection::open(state_db_path(codex_home)).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                rollout_path TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                source TEXT NOT NULL,
                model_provider TEXT NOT NULL,
                cwd TEXT NOT NULL,
                title TEXT NOT NULL,
                sandbox_policy TEXT NOT NULL,
                approval_mode TEXT NOT NULL,
                tokens_used INTEGER NOT NULL DEFAULT 0,
                has_user_event INTEGER NOT NULL DEFAULT 0,
                archived INTEGER NOT NULL DEFAULT 0,
                archived_at INTEGER,
                cli_version TEXT NOT NULL DEFAULT '',
                first_user_message TEXT NOT NULL DEFAULT '',
                memory_mode TEXT NOT NULL DEFAULT 'enabled',
                model TEXT,
                reasoning_effort TEXT,
                created_at_ms INTEGER,
                updated_at_ms INTEGER,
                thread_source TEXT,
                preview TEXT NOT NULL DEFAULT ''
            );
            "#,
        )
        .unwrap();
    }

    #[test]
    fn rollout_scan_collects_index_record_for_restored_session() {
        let codex_home = temp_codex_home("scan-index");
        let rollout_path = write_test_rollout(&codex_home, "openai");

        let scan = collect_rollout_scan(&codex_home, Some("simplaj")).unwrap();

        assert_eq!(scan.index_records.len(), 1);
        assert_eq!(scan.changes.len(), 1);
        assert_eq!(
            scan.provider_counts.get("sessions/openai").copied(),
            Some(1)
        );
        let record = &scan.index_records[0];
        assert_eq!(record.path, rollout_path);
        assert_eq!(record.thread_id, "019cbf7c-1db4-7922-a155-d68fff7b82da");
        assert_eq!(record.cwd, "/Users/example/project");
        assert_eq!(record.source, "vscode");
        assert_eq!(record.cli_version, "0.137.0-alpha.4");
        assert_eq!(record.first_user_message, "请同步这个会话");
        assert_eq!(record.title, "请同步这个会话");
        assert_eq!(record.model.as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(record.reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(record.approval_mode, "never");
        assert!(record.has_user_event);

        fs::remove_dir_all(codex_home).unwrap();
    }

    #[test]
    fn provider_sync_inserts_missing_sqlite_thread_index() {
        let codex_home = temp_codex_home("sqlite-index");
        write_test_rollout(&codex_home, "openai");
        create_test_threads_db(&codex_home);
        let scan = collect_rollout_scan(&codex_home, Some("simplaj")).unwrap();

        let stats = update_sqlite_provider(&codex_home, "simplaj", &scan).unwrap();

        assert!(stats.database_present);
        assert_eq!(stats.thread_rows_inserted, 1);
        let conn = Connection::open(state_db_path(&codex_home)).unwrap();
        let row = conn
            .query_row(
                "SELECT model_provider, source, cwd, first_user_message, preview, has_user_event, model, reasoning_effort \
                 FROM threads WHERE id = ?1",
                params!["019cbf7c-1db4-7922-a155-d68fff7b82da"],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0, "simplaj");
        assert_eq!(row.1, "vscode");
        assert_eq!(row.2, "/Users/example/project");
        assert_eq!(row.3, "请同步这个会话");
        assert_eq!(row.4, "请同步这个会话");
        assert_eq!(row.5, 1);
        assert_eq!(row.6.as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(row.7.as_deref(), Some("xhigh"));

        let second = update_sqlite_provider(&codex_home, "simplaj", &scan).unwrap();
        assert_eq!(second.thread_rows_inserted, 0);

        fs::remove_dir_all(codex_home).unwrap();
    }

    #[test]
    fn relay_comment_state_only_toggles_target_provider_relay_lines() {
        let config = r#"model_provider = "simplaj"

[model_providers.simplaj]
name = "simplaj"
base_url = "https://relay.example/v1"
wire_api = "responses"
experimental_bearer_token = "sk-relay"
requires_openai_auth = true

[model_providers.other]
name = "other"
base_url = "https://other.example/v1"
experimental_bearer_token = "sk-other"
"#;
        let sections = parse_provider_sections(config);
        let target = find_provider_section_by_id_or_name(&sections, "simplaj").unwrap();
        let (commented, changed_keys) =
            relay_config_lines_with_comment_state(config, &target, true);

        assert_eq!(
            changed_keys,
            vec![
                "base_url".to_string(),
                "experimental_bearer_token".to_string()
            ]
        );
        assert!(commented.contains("# base_url = \"https://relay.example/v1\""));
        assert!(commented.contains("# experimental_bearer_token = \"sk-relay\""));
        assert!(commented.contains("requires_openai_auth = true"));
        assert!(commented.contains("base_url = \"https://other.example/v1\""));
        assert!(commented.contains("experimental_bearer_token = \"sk-other\""));
    }

    #[test]
    fn relay_comment_state_can_uncomment_target_provider_relay_lines() {
        let config = r#"model_provider = "simplaj"

[model_providers.simplaj]
name = "simplaj"
# base_url = "https://relay.example/v1"
wire_api = "responses"
# experimental_bearer_token = "sk-relay"
requires_openai_auth = true
"#;
        let sections = parse_provider_sections(config);
        let target = find_provider_section_by_id_or_name(&sections, "simplaj").unwrap();
        let (uncommented, changed_keys) =
            relay_config_lines_with_comment_state(config, &target, false);

        assert_eq!(
            changed_keys,
            vec![
                "base_url".to_string(),
                "experimental_bearer_token".to_string()
            ]
        );
        assert!(uncommented.contains("base_url = \"https://relay.example/v1\""));
        assert!(uncommented.contains("experimental_bearer_token = \"sk-relay\""));
        assert!(!uncommented.contains("# base_url = \"https://relay.example/v1\""));
        assert!(!uncommented.contains("# experimental_bearer_token = \"sk-relay\""));
    }

    #[test]
    fn codex_process_detection_matches_real_codex_processes() {
        assert!(is_codex_process_command(
            "/Applications/Codex.app/Contents/MacOS/Codex"
        ));
        assert!(is_codex_process_command(
            "/Applications/Codex.app/Contents/Resources/codex app-server --analytics-default-enabled"
        ));
        assert!(is_codex_process_command(
            r#"C:\Users\user\AppData\Local\Programs\Codex\Codex.exe"#
        ));
        assert!(is_codex_process_command(
            r#""C:\Users\user\AppData\Local\Programs\Codex\codex.exe" app-server"#
        ));
    }

    #[test]
    fn codex_process_detection_ignores_this_tool_and_related_helpers() {
        assert!(!is_codex_process_command(
            "/Applications/Codex API Tools.app/Contents/MacOS/Codex API Tools"
        ));
        assert!(!is_codex_process_command(
            "/Users/user/.local/bin/codex-tools cloud push --all"
        ));
        assert!(!is_codex_process_command(
            r#"C:\Users\user\AppData\Local\CodexTools\bin\codex-tools.exe cloud status"#
        ));
        assert!(!is_codex_process_command(
            "/Users/user/.codex/computer-use/Codex Computer Use.app/Contents/MacOS/SkyComputerUseService"
        ));
        assert!(!is_codex_process_command(
            "/Applications/Codex.app/Contents/Frameworks/Codex Framework.framework/Versions/149.0.7827.54/Helpers/browser_crashpad_handler --monitor-self --database=/Users/user/Library/Application Support/com.openai.codex/web/Crashpad"
        ));
        assert!(!is_codex_process_command(
            "/Users/user/.vscode/extensions/openai.chatgpt-26.5602.71036-darwin-arm64/bin/macos-aarch64/codex app-server --analytics-default-enabled"
        ));
    }
}

pub fn inspect_text() -> Result<String, String> {
    let state = inspect()?;
    let mut lines = vec!["Codex 状态".to_string()];
    match state.codex_process_detection_error.as_deref() {
        Some(error) => {
            lines.push("  运行状态：无法确认".into());
            lines.push("  配置写入：不建议继续，请手动完全退出 Codex 后再操作".into());
            lines.push(format!("  检测错误：{error}"));
        }
        None if state.codex_running => {
            lines.push(format!(
                "  运行状态：运行中（检测到 {} 个相关进程）",
                state.codex_processes.len()
            ));
            lines.push(
                "  配置写入：暂不可写，先执行 `codex-tools codex quit` 或手动完全退出 Codex".into(),
            );
        }
        None => {
            lines.push("  运行状态：已退出".into());
            lines.push("  配置写入：可以安全执行 provider/auth/relay 修改".into());
        }
    }

    lines.push(String::new());
    lines.push("本机配置".into());
    lines.push(format!("  Codex 目录：{}", state.codex_home));
    lines.push(format!(
        "  config.toml：{}",
        if state.config_exists {
            state.config_path.as_str()
        } else {
            "未找到"
        }
    ));
    lines.push(format!(
        "  auth.json：{}",
        if state.auth_exists {
            "已登录/存在"
        } else {
            "不存在"
        }
    ));

    if let Some(config) = state.config.as_ref() {
        lines.push(String::new());
        lines.push("Provider".into());
        lines.push(format!("  当前 provider：{}", config.root_provider));
        if let Some(provider) = current_provider(config) {
            lines.push(format!("  显示名称：{}", provider.name));
            lines.push(format!(
                "  中转 URL：{}",
                configured_label(!provider.base_url.is_empty())
            ));
            lines.push(format!(
                "  中转 Key：{}",
                configured_label(provider.has_experimental_bearer_token)
            ));
            lines.push(format!(
                "  OpenAI 登录态：{}",
                match provider.requires_openai_auth.as_str() {
                    "true" => "需要",
                    "false" => "不需要",
                    _ => "未声明",
                }
            ));
        }
        if config.providers.len() > 1 {
            let names = config
                .providers
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("  可用 provider：{names}"));
        }
    }

    if !state.backup_dirs.is_empty() {
        lines.push(String::new());
        lines.push(format!("最近备份：{}", state.backup_dirs[0]));
        if state.backup_dirs.len() > 1 {
            lines.push(format!("  另外还有 {} 个备份", state.backup_dirs.len() - 1));
        }
    }
    Ok(lines.join("\n"))
}

pub fn provider_status_text() -> Result<String, String> {
    render_native_status()
}

pub fn provider_sync_text(
    provider_id: Option<String>,
    force_process_preflight: bool,
) -> Result<String, String> {
    let target = resolve_sync_target(provider_id);
    run_native_sync(&target, force_process_preflight)
}

pub fn provider_switch_text(provider_id: Option<String>) -> Result<String, String> {
    let target = resolve_sync_target(provider_id);
    run_native_switch(&target)
}

pub fn repair_provider_text(custom_name: Option<String>, run_sync: bool) -> Result<String, String> {
    let result = repair_provider(custom_name, Some(run_sync))?;
    let mut lines = vec![result.message];
    if let Some(backup) = result.backup_dir {
        lines.push(format!("Backup: {backup}"));
    }
    if let Some(sync) = result.sync {
        if sync.ok {
            lines.push(sync.stdout);
        } else {
            lines.push(format!("Sync failed: {}", sync.stderr));
        }
    }
    Ok(lines
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn backup_remove_auth_text() -> Result<String, String> {
    let result = backup_remove_auth()?;
    let mut lines = vec![result.message, format!("Auth path: {}", result.auth_path)];
    if let Some(backup) = result.backup_dir {
        lines.push(format!("Backup: {backup}"));
    }
    Ok(lines.join("\n"))
}

pub fn apply_experimental_token_text(
    provider_id: Option<String>,
    token: String,
) -> Result<String, String> {
    ensure_codex_stopped_for_write("写入 experimental_bearer_token")?;
    let config_path = config_path()?;
    let config_text = fs::read_to_string(&config_path).map_err(command_error)?;
    let target_provider =
        sanitize_provider_id(provider_id.or_else(|| Some(parse_root_provider(&config_text))))?;
    if !token.starts_with("sk-") {
        return Err("API Key 需以 sk- 开头。".into());
    }

    let mut sections = parse_provider_sections(&config_text);
    let section_index = sections
        .iter()
        .position(|section| section.id == target_provider)
        .ok_or_else(|| format!("config.toml 中没有找到 [model_providers.{target_provider}]。"))?;
    let mut section = sections.remove(section_index);
    let backup = create_backup(&[config_path.clone()], "apply-experimental-bearer-token")?;
    let mut lines = split_lines(&config_text);
    upsert_key_in_section(
        &mut lines,
        &mut section,
        "experimental_bearer_token",
        &format!("\"{}\"", escape_toml_string(&token)),
    );
    upsert_key_in_section(&mut lines, &mut section, "requires_openai_auth", "true");
    fs::write(&config_path, lines.join("\n")).map_err(command_error)?;
    Ok(format!(
        "Applied experimental_bearer_token for provider {target_provider}.\nToken: {}\nBackup: {}",
        mask_secret(&token),
        backup.backup_dir
    ))
}

pub fn relay_toggle_text(provider_id: Option<String>, commented: bool) -> Result<String, String> {
    let result = set_relay_lines_commented(provider_id, commented)?;
    let mut lines = vec![result.message];
    if let Some(backup) = result.backup_dir {
        lines.push(format!("Backup: {backup}"));
    }
    Ok(lines.join("\n"))
}

pub fn quit_codex_text() -> Result<String, String> {
    let result = try_quit_codex()?;
    let mut lines = vec![result.message];
    if !result.commands.is_empty() {
        lines.push(format!("Commands: {}", result.commands.join("; ")));
    }
    if !result.processes.is_empty() {
        lines.push(format!(
            "仍有 {} 个 Codex 相关进程，请手动完全退出后再继续。",
            result.processes.len()
        ));
    }
    Ok(lines.join("\n"))
}

pub fn quota_text(json_output: bool, raw_json_output: bool) -> Result<String, String> {
    let result = check_openai_quota()?;
    if raw_json_output {
        return serde_json::to_string_pretty(&result).map_err(command_error);
    }
    if json_output {
        return serde_json::to_string_pretty(&quota_readable_json(&result)).map_err(command_error);
    }
    let limit_reached = result
        .buckets
        .iter()
        .any(|bucket| bucket.limit_reached == Some(true));
    let mut lines = vec!["OpenAI 额度".to_string()];
    if let Some(email) = result.email_masked.as_deref() {
        lines.push(format!("  账号：{email}"));
    }
    if let Some(plan) = result.plan_type.as_deref() {
        lines.push(format!("  订阅：{plan}"));
    }
    if let Some(account) = result.account_id_masked.as_deref() {
        lines.push(format!("  Account：{account}"));
    }

    lines.push(format!(
        "  查询结果：{}",
        if result.ok {
            if limit_reached {
                "已触发额度限制"
            } else {
                "当前未显示额度限制"
            }
        } else {
            "无法查询"
        }
    ));
    if !result.ok {
        lines.push(format!("  原因：{}", result.message));
    } else if let Some(kind) = result.rate_limit_reached_type.as_deref() {
        lines.push(format!("  限制类型：{kind}"));
    }

    if !result.buckets.is_empty() {
        lines.push(String::new());
        lines.push("额度窗口".into());
        for bucket in &result.buckets {
            lines.push(format!(
                "  {}：{}",
                quota_bucket_label(bucket),
                quota_bucket_state(bucket)
            ));
            for window in [&bucket.primary, &bucket.secondary].into_iter().flatten() {
                lines.push(format!("    {}", quota_window_summary(window)));
            }
        }
    }

    if let Some(spend) = result.spend_control.as_ref() {
        lines.push(String::new());
        lines.push("消费限制".into());
        if let Some(remaining) = spend.remaining.as_deref() {
            lines.push(format!("  剩余：{remaining}"));
        }
        if let Some(used) = spend.used_percent {
            lines.push(format!("  已用：{used:.1}%"));
        }
        if let Some(reset) = reset_summary(spend.reset_after_seconds, spend.resets_at) {
            lines.push(format!("  恢复：{reset}"));
        }
    }

    lines.push(String::new());
    lines.push(result.recommendation);
    Ok(lines.join("\n"))
}

fn quota_readable_json(result: &OpenAiQuotaResult) -> Value {
    let limit_reached = result
        .buckets
        .iter()
        .any(|bucket| bucket.limit_reached == Some(true));
    let query_result = if result.ok {
        if limit_reached {
            "已触发额度限制"
        } else {
            "当前未显示额度限制"
        }
    } else {
        "无法查询"
    };

    json!({
        "title": "OpenAI 额度",
        "account": {
            "email": result.email_masked,
            "subscription": result.plan_type,
            "accountId": result.account_id_masked,
            "authMode": result.auth_mode,
            "lastRefresh": result.last_refresh,
        },
        "query": {
            "ok": result.ok,
            "status": result.status,
            "result": query_result,
            "message": result.message,
            "limitReachedType": result.rate_limit_reached_type,
        },
        "quotaWindows": result
            .buckets
            .iter()
            .map(quota_bucket_readable_json)
            .collect::<Vec<_>>(),
        "credits": result.credits.as_ref().map(quota_credits_readable_json),
        "spendControl": result.spend_control.as_ref().map(spend_control_readable_json),
        "recommendation": result.recommendation,
    })
}

fn quota_bucket_readable_json(bucket: &QuotaBucketView) -> Value {
    let windows = [&bucket.primary, &bucket.secondary]
        .into_iter()
        .flatten()
        .map(quota_window_readable_json)
        .collect::<Vec<_>>();
    json!({
        "name": quota_bucket_label(bucket),
        "id": bucket.limit_id,
        "state": quota_bucket_state(bucket),
        "allowed": bucket.allowed,
        "limitReached": bucket.limit_reached,
        "windows": windows,
    })
}

fn quota_window_readable_json(window: &QuotaWindowView) -> Value {
    let label = match window.label.as_str() {
        "primary" => "短周期",
        "secondary" => "长周期",
        other => other,
    };
    json!({
        "label": label,
        "summary": quota_window_summary(window),
        "usedPercent": window.used_percent,
        "remainingPercent": window.remaining_percent,
        "windowMinutes": window.window_minutes,
        "recovery": reset_summary(window.reset_after_seconds, window.resets_at),
    })
}

fn quota_credits_readable_json(credits: &QuotaCreditsView) -> Value {
    let summary = match (
        credits.unlimited,
        credits.has_credits,
        credits.balance.as_deref(),
    ) {
        (Some(true), _, _) => "无限额度".to_string(),
        (_, Some(true), Some(balance)) => format!("有可用余额：{balance}"),
        (_, Some(true), None) => "有可用余额".to_string(),
        (_, Some(false), _) => "没有可用余额".to_string(),
        (_, _, Some(balance)) => format!("余额：{balance}"),
        _ => "状态未知".to_string(),
    };
    json!({
        "summary": summary,
        "hasCredits": credits.has_credits,
        "unlimited": credits.unlimited,
        "balance": credits.balance,
    })
}

fn spend_control_readable_json(spend: &SpendControlView) -> Value {
    let used = spend
        .used_percent
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| "未知".into());
    let remaining = spend
        .remaining
        .as_deref()
        .map(str::to_string)
        .or_else(|| spend.remaining_percent.map(|value| format!("{value:.1}%")))
        .unwrap_or_else(|| "未知".into());
    json!({
        "summary": format!("已用 {used}，剩余 {remaining}"),
        "limit": spend.limit,
        "used": spend.used,
        "remaining": spend.remaining,
        "usedPercent": spend.used_percent,
        "remainingPercent": spend.remaining_percent,
        "recovery": reset_summary(spend.reset_after_seconds, spend.resets_at),
    })
}

fn current_provider(config: &ConfigView) -> Option<&ProviderView> {
    config.providers.iter().find(|provider| {
        provider.id == config.root_provider || provider.name == config.root_provider
    })
}

fn configured_label(configured: bool) -> &'static str {
    if configured {
        "已配置"
    } else {
        "未配置"
    }
}

fn quota_bucket_label(bucket: &QuotaBucketView) -> String {
    bucket
        .limit_name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&bucket.limit_id)
        .to_string()
}

fn quota_bucket_state(bucket: &QuotaBucketView) -> &'static str {
    match (bucket.allowed, bucket.limit_reached) {
        (_, Some(true)) => "已用尽",
        (Some(true), _) => "可用",
        (Some(false), _) => "暂不可用",
        _ => "状态未知",
    }
}

fn quota_window_summary(window: &QuotaWindowView) -> String {
    let label = match window.label.as_str() {
        "primary" => "短周期",
        "secondary" => "长周期",
        other => other,
    };
    let used = window
        .used_percent
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| "未知".into());
    let remaining = window
        .remaining_percent
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| "未知".into());
    let window_text = window
        .window_minutes
        .map(|minutes| format!("，窗口 {}", human_duration(minutes * 60)))
        .unwrap_or_default();
    let reset = reset_summary(window.reset_after_seconds, window.resets_at)
        .map(|value| format!("，恢复：{value}"))
        .unwrap_or_default();
    format!("{label}：已用 {used}，剩余 {remaining}{window_text}{reset}")
}

fn reset_summary(reset_after_seconds: Option<i64>, resets_at: Option<i64>) -> Option<String> {
    if let Some(seconds) = reset_after_seconds.filter(|value| *value >= 0) {
        return Some(format!("约 {}后", human_duration(seconds)));
    }
    let resets_at = resets_at?;
    let now_seconds = (unix_millis() / 1000) as i64;
    if resets_at > now_seconds {
        Some(format!("约 {}后", human_duration(resets_at - now_seconds)))
    } else {
        Some("随时可能恢复".into())
    }
}

fn human_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        return "不到 1 分钟".into();
    }
    let minutes = (seconds + 59) / 60;
    if minutes < 60 {
        return format!("{minutes} 分钟");
    }
    let hours = minutes / 60;
    let remaining_minutes = minutes % 60;
    if hours < 24 {
        if remaining_minutes == 0 {
            format!("{hours} 小时")
        } else {
            format!("{hours} 小时 {remaining_minutes} 分钟")
        }
    } else {
        let days = hours / 24;
        let remaining_hours = hours % 24;
        if remaining_hours == 0 {
            format!("{days} 天")
        } else {
            format!("{days} 天 {remaining_hours} 小时")
        }
    }
}
