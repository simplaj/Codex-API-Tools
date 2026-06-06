#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use filetime::{set_file_mtime, FileTime};
use rusqlite::{params, Connection, OpenFlags};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{Manager, State};

const APP_BACKUP_NAMESPACE: &str = "gpt-api-tools";
const DEFAULT_PROVIDER_NAME: &str = "simplaj";
const DB_FILE_BASENAME: &str = "state_5.sqlite";
const DEFAULT_CODEX_PROVIDER: &str = "openai";
const GLOBAL_STATE_FILE_BASENAME: &str = ".codex-global-state.json";
const GLOBAL_STATE_BACKUP_FILE_BASENAME: &str = ".codex-global-state.json.bak";
const SESSION_DIRS: &[&str] = &["sessions", "archived_sessions"];
const NATIVE_SYNC_ENGINE: &str = "native-rust-rusqlite";

#[derive(Default)]
struct TokenCache(Mutex<HashMap<String, String>>);

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
struct TokenCandidate {
    id: String,
    backup_path: String,
    backup_dir: String,
    masked: String,
    length: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenCandidatesResult {
    candidates: Vec<TokenCandidate>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApplyTokenResult {
    applied: bool,
    provider_id: String,
    token_masked: String,
    backup_dir: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenPathResult {
    opened: bool,
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

#[derive(Default)]
struct RolloutScan {
    changes: Vec<RolloutChange>,
    provider_counts: HashMap<String, usize>,
    user_event_thread_ids: HashSet<String>,
    thread_cwd_by_id: HashMap<String, String>,
}

#[derive(Default)]
struct SqliteUpdateStats {
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

fn main() {
    tauri::Builder::default()
        .manage(TokenCache::default())
        .invoke_handler(tauri::generate_handler![
            inspect,
            repair_provider,
            run_provider_sync,
            backup_remove_auth,
            list_auth_token_candidates,
            apply_experimental_token,
            open_path
        ])
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title("Codex API Tools");
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run Codex API Tools");
}

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

fn to_desktop_workspace_path(value: &str) -> String {
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
        return to_desktop_workspace_path(value);
    }
    matches.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms))
            .then_with(|| left.cwd.cmp(&right.cwd))
    });
    to_desktop_workspace_path(&matches[0].cwd)
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

fn file_has_user_event(path: &Path) -> Result<bool, String> {
    let file = fs::File::open(path).map_err(command_error)?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line.map_err(command_error)?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<Value>(&line) {
            if record_has_user_event(&record) {
                return Ok(true);
            }
        }
    }
    Ok(false)
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

            let thread_id = payload
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToString::to_string);
            let cwd = payload
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(to_desktop_workspace_path);
            if let (Some(thread_id), Some(cwd)) = (thread_id.as_ref(), cwd.as_ref()) {
                scan.thread_cwd_by_id.insert(thread_id.clone(), cwd.clone());
            }

            let has_user_event = if thread_id.is_some() {
                file_has_user_event(&rollout_path)?
            } else {
                false
            };
            if has_user_event {
                if let Some(thread_id) = thread_id.as_ref() {
                    scan.user_event_thread_ids.insert(thread_id.clone());
                }
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
            .map(|path| to_desktop_workspace_path(&path))
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
                    to_desktop_workspace_path(&root)
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
            to_desktop_workspace_path(&cwd),
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
            let exact_root = to_desktop_workspace_path(&root);
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

fn run_native_sync(target_provider: &str) -> Result<String, String> {
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
            sqlite_stats.provider_rows_updated
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
    let config_path = config_path()?;
    let config_text = fs::read_to_string(&config_path).map_err(command_error)?;
    let backup = create_backup(&[config_path.clone()], "switch-root-provider")?;
    fs::write(
        &config_path,
        set_root_provider(&config_text, target_provider),
    )
    .map_err(command_error)?;
    let sync_output = run_native_sync(target_provider)?;
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

fn collect_strings_from_json(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::String(value) => output.push(value.clone()),
        Value::Array(items) => {
            for item in items {
                collect_strings_from_json(item, output);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                collect_strings_from_json(item, output);
            }
        }
        _ => {}
    }
}

fn extract_sk_tokens(text: &str) -> Vec<String> {
    let mut tokens = HashSet::new();
    for segment in text.split(|character: char| {
        !(character.is_ascii_alphanumeric() || character == '-' || character == '_')
    }) {
        if segment.starts_with("sk-") && segment.len() >= 15 {
            tokens.insert(segment.to_string());
        }
    }
    let mut output: Vec<String> = tokens.into_iter().collect();
    output.sort();
    output
}

#[tauri::command]
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

    Ok(InspectState {
        codex_home: path_to_string(&codex_home),
        config_path: path_to_string(&config_path),
        auth_path: path_to_string(&auth_path),
        config_exists: config_text.is_some(),
        auth_exists: path_exists(&auth_path),
        backup_root: path_to_string(&backup_root()?),
        backup_dirs,
        provider_choices,
        config: config_text.as_deref().map(parse_config),
        runtime: runtime_status(),
    })
}

#[tauri::command]
fn run_provider_sync(
    command: Option<String>,
    provider_id: Option<String>,
) -> Result<ShellResult, String> {
    let command_name = command.unwrap_or_else(|| "status".into());
    let started_at = unix_millis();
    let target_provider = resolve_sync_target(provider_id);
    let command_text = match command_name.as_str() {
        "status" => format!("{NATIVE_SYNC_ENGINE} status"),
        "sync" => format!("{NATIVE_SYNC_ENGINE} sync --provider {target_provider}"),
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
        "sync" => run_native_sync(&target_provider),
        "switch" => run_native_switch(&target_provider),
        _ => unreachable!(),
    };
    Ok(native_shell_result(started_at, command_text, outcome))
}

#[tauri::command]
fn repair_provider(
    custom_name: Option<String>,
    run_sync: Option<bool>,
) -> Result<RepairResult, String> {
    let provider_id = sanitize_provider_id(custom_name)?;
    let config_path = config_path()?;
    let config_text = fs::read_to_string(&config_path).map_err(command_error)?;
    let sections = parse_provider_sections(&config_text);
    let openai_section = find_openai_provider_section(&sections);

    if openai_section.is_none() {
        let sync = if run_sync.unwrap_or(true) {
            Some(run_provider_sync(
                Some("sync".into()),
                Some(provider_id.clone()),
            )?)
        } else {
            None
        };
        return Ok(RepairResult {
            changed: false,
            provider_id,
            backup_dir: None,
            message: "config.toml 里没有找到 name/id 为 OpenAI 的 provider。".into(),
            sync,
        });
    }

    let mut openai_section = openai_section.unwrap();
    if sections
        .iter()
        .any(|section| section.id == provider_id && section.start != openai_section.start)
    {
        return Err(format!(
            "config.toml 已存在 [model_providers.{provider_id}]，为避免覆盖没有自动合并。"
        ));
    }

    let backup = create_backup(&[config_path.clone()], "repair-openai-provider-name")?;
    let mut lines = split_lines(&config_text);
    lines[openai_section.start] = format!("[model_providers.{provider_id}]");
    upsert_key_in_section(
        &mut lines,
        &mut openai_section,
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
        )?)
    } else {
        None
    };

    Ok(RepairResult {
        changed: true,
        provider_id: provider_id.clone(),
        backup_dir: Some(backup.backup_dir),
        message: format!("已将 OpenAI provider 重命名为 {provider_id}。"),
        sync,
    })
}

#[tauri::command]
fn backup_remove_auth() -> Result<AuthRemoveResult, String> {
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

#[tauri::command]
fn list_auth_token_candidates(cache: State<TokenCache>) -> Result<TokenCandidatesResult, String> {
    let mut cached = cache.0.lock().map_err(|_| "token cache lock failed")?;
    cached.clear();
    let mut candidates = Vec::new();

    for dir in list_backup_dirs_newest_first()? {
        let auth_backup_path = dir.join("auth.json");
        if !path_exists(&auth_backup_path) {
            continue;
        }
        let raw = fs::read_to_string(&auth_backup_path).map_err(command_error)?;
        let mut strings = Vec::new();
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            collect_strings_from_json(&value, &mut strings);
        } else {
            strings.push(raw);
        }
        let mut matches = HashSet::new();
        for item in strings {
            for token in extract_sk_tokens(&item) {
                matches.insert(token);
            }
        }
        let mut tokens: Vec<String> = matches.into_iter().collect();
        tokens.sort();
        for token in tokens {
            let id = format!(
                "{}:{}",
                dir.file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "backup".into()),
                candidates.len()
            );
            cached.insert(id.clone(), token.clone());
            candidates.push(TokenCandidate {
                id,
                backup_path: path_to_string(&auth_backup_path),
                backup_dir: path_to_string(&dir),
                masked: mask_secret(&token),
                length: token.len(),
            });
        }
    }

    Ok(TokenCandidatesResult { candidates })
}

#[tauri::command]
fn apply_experimental_token(
    provider_id: Option<String>,
    token: Option<String>,
    candidate_id: Option<String>,
    cache: State<TokenCache>,
) -> Result<ApplyTokenResult, String> {
    let config_path = config_path()?;
    let config_text = fs::read_to_string(&config_path).map_err(command_error)?;
    let target_provider =
        sanitize_provider_id(provider_id.or_else(|| Some(parse_root_provider(&config_text))))?;
    let token_to_use = if let Some(candidate_id) = candidate_id {
        cache
            .0
            .lock()
            .map_err(|_| "token cache lock failed")?
            .get(&candidate_id)
            .cloned()
    } else {
        token
    };
    let token_to_use = token_to_use.ok_or_else(|| "没有可写入的 Simplaj API Key。".to_string())?;
    if !token_to_use.starts_with("sk-") {
        return Err("Simplaj API Key 需以 sk- 开头。".into());
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
        &format!("\"{}\"", escape_toml_string(&token_to_use)),
    );
    upsert_key_in_section(&mut lines, &mut section, "requires_openai_auth", "true");
    fs::write(&config_path, lines.join("\n")).map_err(command_error)?;

    Ok(ApplyTokenResult {
        applied: true,
        provider_id: target_provider,
        token_masked: mask_secret(&token_to_use),
        backup_dir: backup.backup_dir,
    })
}

#[tauri::command]
fn open_path(target_path: Option<String>) -> Result<OpenPathResult, String> {
    let Some(target_path) = target_path else {
        return Ok(OpenPathResult { opened: false });
    };
    let path = PathBuf::from(target_path);
    if !path_exists(&path) {
        return Ok(OpenPathResult { opened: false });
    }
    let status = if cfg!(target_os = "macos") {
        Command::new("open").arg(path).status()
    } else if cfg!(windows) {
        Command::new("explorer").arg(path).status()
    } else {
        Command::new("xdg-open").arg(path).status()
    }
    .map_err(command_error)?;
    Ok(OpenPathResult {
        opened: status.success(),
    })
}
