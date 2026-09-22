//! Private client-op protocol for the SSH stdio bridge.
//!
//! Daemon protocol traffic passes through `jcode server stdio` untouched;
//! client operations that need remote-local resources travel on a private
//! sideband in the same JSON-lines stream. A request is one line
//! `{"__jcode_ssh_op":{"id":N,"op":"<name>",...params}}`; the bridge answers
//! with `{"__jcode_ssh_op":{"id":N,"result":{...}}}` or
//! `{"__jcode_ssh_op":{"id":N,"error":"..."}}`.
//!
//! Ops are executed locally by the bridge process (running on the remote
//! host) against the same jcode-base APIs a local client would use — the
//! daemon never sees them, and upstream `jcode-protocol` carries none of
//! these types.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Envelope key marking a sideband line on the stdio stream.
pub const OP_KEY: &str = "__jcode_ssh_op";

/// Serialized envelope prefix — `serde_json` emits exactly this for the
/// single-key envelope, so a cheap prefix test classifies lines without
/// parsing the daemon protocol.
pub const OP_LINE_PREFIX: &str = "{\"__jcode_ssh_op\":";

/// Cheap line classifier for both bridge and client: is this line a
/// sideband op message? Anything else is daemon protocol and passes
/// through uninterpreted.
pub fn is_op_line(line: &[u8]) -> bool {
    line.trim_ascii_start()
        .starts_with(OP_LINE_PREFIX.as_bytes())
}

/// One session row returned by `list_sessions`. Mirrors the field set the
/// local session picker consumes (`SessionInfo`), minus lazy preview data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshSessionEntry {
    /// Full session id (file stem, e.g. `session_...`).
    pub id: String,
    /// Parent session for spawned/delegate sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Friendly short name, if the session has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    /// Display title (custom title, short name, or derived).
    pub title: String,
    /// Visible conversation message count (user + assistant).
    pub message_count: usize,
    pub user_message_count: usize,
    pub assistant_message_count: usize,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Timestamp of the last visible conversation message.
    pub last_message_time: chrono::DateTime<chrono::Utc>,
    /// Last time a client attached to this session, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_key: Option<String>,
    #[serde(default)]
    pub is_canary: bool,
    #[serde(default)]
    pub is_debug: bool,
    /// Whether the session was marked saved.
    #[serde(default)]
    pub saved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_label: Option<String>,
    #[serde(default)]
    pub status: jcode_session_types::SessionStatus,
    /// Rough token estimate for display (chars/4 of visible content).
    #[serde(default)]
    pub estimated_tokens: usize,
    /// First visible user prompt, truncated — used for search indexing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_user_prompt: Option<String>,
    /// A live agent is attached to this session (snooped from pass-through
    /// `History.all_sessions`; best-effort).
    #[serde(default)]
    pub live_attached: bool,
    /// A connected client reports this session is still processing.
    /// Daemon-memory-only — unreachable from the bridge; stays false.
    #[serde(default)]
    pub live_processing: bool,
}

/// One preview row returned by `session_preview` (role label + text +
/// tool names), mirroring the picker's `PreviewMessage`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshPreviewMessage {
    /// "user" or "assistant".
    pub role: String,
    /// Text content, truncated per message.
    pub content: String,
    /// Names of tools invoked in this message, if any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
    /// Wall-clock thinking time (seconds) on `role="reasoning"` rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
}

/// One skill descriptor returned by `list_skills` — the same fields a
/// local `/skills` row renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshSkillInfo {
    pub name: String,
    pub description: String,
    /// Daemon-side path of the skill's SKILL.md.
    pub path: String,
}

/// A client operation sent on the sideband. Internally tagged so the
/// request line reads `{"id":N,"op":"<name>",...params}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SshOp {
    /// Summary metadata for every session on the remote host.
    ListSessions,
    /// Bounded message preview for one session.
    SessionPreview { session_id: String },
    /// Toggle the saved/bookmarked flag on a remote session file.
    SetSessionSaved {
        session_id: String,
        saved: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        save_label: Option<String>,
    },
    /// Todo items/goals/plan stored for a session (disk-persisted).
    GetTodos { session_id: String },
    /// The remote `config.toml` text (empty when absent).
    ReadConfig,
    /// Replace the remote `config.toml` with validated TOML.
    WriteConfig { content: String },
    /// Remote file read; NotFound replies with empty content so existence
    /// probes are not errors.
    ReadFile { path: String },
    /// Remote file write that creates parent directories.
    WriteFile { path: String, content: String },
    /// Filename-prefix listing over a remote directory.
    ListPath { prefix: String },
    /// Full directory listing plus a compact git summary, powering the
    /// `/open` directory browser's two panes over the wire.
    BrowseDir { path: String },
    /// Effective skill list for the bridge's working directory (global
    /// registry + project overlay).
    ListSkills,
}

/// Request payload inside the sideband envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshOpRequest {
    pub id: u64,
    #[serde(flatten)]
    pub op: SshOp,
}

/// Typed op result — externally tagged by op name so empty collections
/// stay unambiguous.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshOpResult {
    ListSessions(Vec<SshSessionEntry>),
    SessionPreview {
        session_id: String,
        messages: Vec<SshPreviewMessage>,
    },
    SetSessionSaved,
    GetTodos {
        todos: Vec<jcode_task_types::TodoItem>,
        goals: Vec<jcode_task_types::TodoGoal>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan: Option<jcode_task_types::TodoPlan>,
    },
    ReadConfig {
        path: String,
        content: String,
    },
    WriteConfig,
    ReadFile {
        path: String,
        content: String,
    },
    WriteFile,
    ListPath(Vec<String>),
    BrowseDir {
        /// Absolute remote path that was listed.
        path: String,
        entries: Vec<SshDirEntry>,
        /// Git summary of `path`; None when it is not inside a work tree
        /// or git is unavailable.
        git: Option<SshDirGitSummary>,
    },
    ListSkills(Vec<SshSkillInfo>),
}

/// One directory entry returned by `browse_dir`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshDirEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
}

/// Compact git summary for a browsed directory. `dirty` counts tracked
/// modifications plus untracked files (mirroring the local picker's
/// dirty-file count); `ahead`/`behind` are vs. `@{upstream}` when set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshDirGitSummary {
    pub is_repo: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub dirty: usize,
    pub ahead: usize,
    pub behind: usize,
}

/// Response payload inside the sideband envelope. Flattened outcome:
/// `{"id":N,"result":{...}}` or `{"id":N,"error":"..."}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshOpResponse {
    pub id: u64,
    #[serde(flatten)]
    pub outcome: SshOpOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshOpOutcome {
    Result(SshOpResult),
    Error(String),
}

#[derive(Debug, Serialize, Deserialize)]
struct Envelope<T> {
    #[serde(rename = "__jcode_ssh_op")]
    payload: T,
}

/// Serialize a request into one sideband line (no trailing newline).
pub fn encode_request(request: &SshOpRequest) -> serde_json::Result<String> {
    serde_json::to_string(&Envelope { payload: request })
}

/// Serialize a response into one sideband line (no trailing newline).
pub fn encode_response(response: &SshOpResponse) -> serde_json::Result<String> {
    serde_json::to_string(&Envelope { payload: response })
}

/// Decode a sideband line into a request (bridge side).
pub fn decode_request(line: &str) -> serde_json::Result<SshOpRequest> {
    serde_json::from_str::<Envelope<SshOpRequest>>(line).map(|envelope| envelope.payload)
}

/// Decode a sideband line into a response (client side).
pub fn decode_response(line: &str) -> serde_json::Result<SshOpResponse> {
    serde_json::from_str::<Envelope<SshOpResponse>>(line).map(|envelope| envelope.payload)
}

/// Mutable context the bridge router carries per connection: the remote
/// working dir for relative-path ops plus live-session state snooped off
/// the pass-through stream.
#[derive(Debug, Default)]
pub struct SshOpContext {
    /// Remote working directory (the bridge process cwd at spawn).
    pub working_dir: String,
    /// Live session ids learned from `History.all_sessions` lines.
    pub live_sessions: HashSet<String>,
}

/// Snoop `all_sessions` out of a pass-through daemon event line. Cheap
/// substring gate first so ordinary events skip the JSON parse entirely.
pub fn snoop_live_sessions(line: &str, ctx: &mut SshOpContext) {
    if !line.contains("\"all_sessions\"") {
        return;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    let Some(ids) = value.get("all_sessions").and_then(|v| v.as_array()) else {
        return;
    };
    ctx.live_sessions = ids
        .iter()
        .filter_map(|id| id.as_str().map(str::to_string))
        .collect();
}

/// Execute one op against remote-local state. Blocking fs work — callers
/// run this inside `spawn_blocking`.
pub fn execute(request: SshOpRequest, ctx: &SshOpContext) -> SshOpResponse {
    let id = request.id;
    let outcome = match request.op {
        SshOp::ListSessions => op_list_sessions(ctx),
        SshOp::SessionPreview { session_id } => op_session_preview(&session_id),
        SshOp::SetSessionSaved {
            session_id,
            saved,
            save_label,
        } => op_set_session_saved(&session_id, saved, save_label),
        SshOp::GetTodos { session_id } => op_get_todos(&session_id),
        SshOp::ReadConfig => op_read_config(),
        SshOp::WriteConfig { content } => op_write_config(&content),
        SshOp::ReadFile { path } => op_read_file(&path, &ctx.working_dir),
        SshOp::WriteFile { path, content } => op_write_file(&path, &content, &ctx.working_dir),
        SshOp::ListPath { prefix } => op_list_path(&prefix, &ctx.working_dir),
        SshOp::BrowseDir { path } => op_browse_dir(&path, &ctx.working_dir),
        SshOp::ListSkills => op_list_skills(&ctx.working_dir),
    };
    SshOpResponse { id, outcome }
}

fn ok(result: SshOpResult) -> SshOpOutcome {
    SshOpOutcome::Result(result)
}

fn err(error: impl std::fmt::Display) -> SshOpOutcome {
    SshOpOutcome::Error(error.to_string())
}

// --- session ops -----------------------------------------------------

/// Preview rows per `session_preview` request. Mirrors the local picker's
/// `build_messages_preview` bound.
const REMOTE_PREVIEW_MESSAGES: usize = 20;
/// Cap for `first_user_prompt` so the reply stays compact.
const FIRST_PROMPT_MAX_CHARS: usize = 200;

fn op_list_sessions(ctx: &SshOpContext) -> SshOpOutcome {
    let live_processing = HashSet::new();
    ok(SshOpResult::ListSessions(collect_session_list_entries(
        &ctx.live_sessions,
        &live_processing,
    )))
}

fn collect_session_list_entries(
    live_attached: &HashSet<String>,
    live_processing: &HashSet<String>,
) -> Vec<SshSessionEntry> {
    let Ok(sessions_dir) = crate::storage::jcode_dir().map(|dir| dir.join("sessions")) else {
        return Vec::new();
    };
    let Ok(read_dir) = std::fs::read_dir(&sessions_dir) else {
        return Vec::new();
    };

    let mut entries: Vec<SshSessionEntry> = Vec::new();
    for dir_entry in read_dir.flatten() {
        let path = dir_entry.path();
        if !path.is_file() || !is_session_snapshot(&path) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // Imported external transcripts are surfaced by the picker's own
        // external-source scans; listing them here would double-report them.
        if stem.starts_with("imported_") {
            continue;
        }
        if let Some(entry) = session_list_entry(stem, live_attached, live_processing) {
            entries.push(entry);
        }
    }

    entries.sort_by(|a, b| b.last_message_time.cmp(&a.last_message_time));
    entries
}

fn is_session_snapshot(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("json")
        // `<id>.json.pre-wipe-*.bak` files end in `.bak`, not `.json`, but a
        // stem like `session_x.json` also parses here; keep it defensive.
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| !name.contains(".pre-wipe-"))
}

fn session_list_entry(
    session_id: &str,
    live_attached: &HashSet<String>,
    live_processing: &HashSet<String>,
) -> Option<SshSessionEntry> {
    use crate::util::truncate_str;
    use jcode_message_types::Role;

    let session = crate::session::Session::load(session_id).ok()?;
    let visible = session.visible_conversation_messages();
    if visible.is_empty() {
        return None;
    }

    let mut user_message_count = 0usize;
    let mut assistant_message_count = 0usize;
    let mut estimated_tokens = 0usize;
    let mut first_user_prompt: Option<String> = None;
    for message in &visible {
        match message.role {
            Role::User => {
                user_message_count += 1;
                if first_user_prompt.is_none() {
                    first_user_prompt = Some(
                        truncate_str(message.content_preview().trim(), FIRST_PROMPT_MAX_CHARS)
                            .to_string(),
                    );
                }
            }
            Role::Assistant => assistant_message_count += 1,
        }
        if let Some(usage) = &message.token_usage {
            estimated_tokens = estimated_tokens
                .saturating_add(usage.input_tokens as usize)
                .saturating_add(usage.output_tokens as usize);
        }
    }

    // Title precedence mirrors the local picker: custom rename, then the
    // todo-derived session title, then the generated title, then short name.
    let short_name = session
        .short_name
        .clone()
        .or_else(|| crate::id::extract_session_name(session_id).map(str::to_string))
        .unwrap_or_else(|| session_id.to_string());
    let title = session
        .custom_title
        .clone()
        .or_else(|| {
            crate::todo::load_session_title(session_id)
                .map(|title| truncate_str(title.trim(), 72).to_string())
        })
        .or_else(|| session.title.clone())
        .unwrap_or_else(|| short_name.clone());

    Some(SshSessionEntry {
        id: session_id.to_string(),
        parent_id: session.parent_id.clone(),
        short_name: Some(short_name),
        title,
        message_count: visible.len(),
        user_message_count,
        assistant_message_count,
        created_at: session.created_at,
        last_message_time: session.updated_at,
        last_active_at: session.last_active_at,
        working_dir: session.working_dir.clone(),
        model: session.model.clone(),
        provider_key: session.provider_key.clone(),
        is_canary: session.is_canary,
        is_debug: session.is_debug,
        saved: session.saved,
        save_label: session.save_label.clone(),
        status: session.status.clone(),
        estimated_tokens,
        first_user_prompt,
        live_attached: live_attached.contains(session_id),
        live_processing: live_processing.contains(session_id),
    })
}

fn op_session_preview(session_id: &str) -> SshOpOutcome {
    let messages = crate::session::Session::load(session_id)
        .map(|session| {
            crate::session::render_messages(&session)
                .into_iter()
                .rev()
                .take(REMOTE_PREVIEW_MESSAGES)
                .rev()
                .map(|message| SshPreviewMessage {
                    role: message.role,
                    content: message.content,
                    tool_calls: message.tool_calls,
                    timestamp: None,
                    duration_secs: message.duration_secs,
                })
                .collect()
        })
        .unwrap_or_default();
    ok(SshOpResult::SessionPreview {
        session_id: session_id.to_string(),
        messages,
    })
}

fn op_set_session_saved(session_id: &str, saved: bool, save_label: Option<String>) -> SshOpOutcome {
    let result = (|| -> anyhow::Result<()> {
        let mut session = crate::session::Session::load(session_id)?;
        if saved {
            session.mark_saved(save_label);
        } else {
            session.unmark_saved();
        }
        session.save()
    })();
    match result {
        Ok(()) => ok(SshOpResult::SetSessionSaved),
        Err(error) => err(format!("set_session_saved failed: {error}")),
    }
}

fn op_get_todos(session_id: &str) -> SshOpOutcome {
    ok(SshOpResult::GetTodos {
        todos: crate::todo::load_todos(session_id).unwrap_or_default(),
        goals: crate::todo::load_goals(session_id).unwrap_or_default(),
        plan: crate::todo::load_plan(session_id).ok(),
    })
}

// --- config ops ------------------------------------------------------

fn op_read_config() -> SshOpOutcome {
    let Some(path) = crate::config::Config::path() else {
        return err("No config path on the remote host");
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => ok(SshOpResult::ReadConfig {
            path: path.display().to_string(),
            content,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ok(SshOpResult::ReadConfig {
            path: path.display().to_string(),
            content: String::new(),
        }),
        Err(error) => err(format!("Failed to read config file: {error}")),
    }
}

fn op_write_config(content: &str) -> SshOpOutcome {
    let result = crate::config::Config::from_str(content)
        .map_err(|error| anyhow::anyhow!("Invalid config TOML: {error}"))
        .and_then(|config| config.save());
    match result {
        Ok(()) => ok(SshOpResult::WriteConfig),
        Err(error) => err(format!("Failed to write remote config: {error}")),
    }
}

// --- file ops --------------------------------------------------------

fn resolve_remote_path(path: &str, cwd: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return dirs::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(path));
    }
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        Path::new(cwd).join(candidate)
    }
}

fn op_read_file(path: &str, cwd: &str) -> SshOpOutcome {
    let resolved = resolve_remote_path(path, cwd);
    match std::fs::read_to_string(&resolved) {
        Ok(content) => ok(SshOpResult::ReadFile {
            path: resolved.display().to_string(),
            content,
        }),
        // A missing file is a normal probe result (e.g. `/swarm-prompt`
        // looking for the project-level override first); report it as empty
        // content rather than an error so callers do not need to
        // pattern-match error text.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ok(SshOpResult::ReadFile {
            path: resolved.display().to_string(),
            content: String::new(),
        }),
        Err(error) => err(format!("Failed to read {}: {error}", resolved.display())),
    }
}

fn op_write_file(path: &str, content: &str, cwd: &str) -> SshOpOutcome {
    let resolved = resolve_remote_path(path, cwd);
    let result = resolved
        .parent()
        .map(|parent| std::fs::create_dir_all(parent))
        .transpose()
        .and_then(|_| std::fs::write(&resolved, content));
    match result {
        Ok(()) => ok(SshOpResult::WriteFile),
        Err(error) => err(format!("Failed to write {}: {error}", resolved.display())),
    }
}

/// `list_path` completion: split the prefix at its last `/`, list that
/// directory, and return entries whose name starts with the trailing
/// fragment, formatted back in the prefix's own spelling (`~/`, relative,
/// absolute) with a trailing `/` on directories.
const LIST_PATH_MAX_CANDIDATES: usize = 300;

fn op_list_path(prefix: &str, cwd: &str) -> SshOpOutcome {
    let (dir_display, name_prefix) = match prefix.rfind('/') {
        Some(index) => (&prefix[..=index], &prefix[index + 1..]),
        None => ("", prefix),
    };
    // An empty dir display means the prefix had no slash: complete inside
    // the session working directory.
    let dir_to_scan = if dir_display.is_empty() {
        PathBuf::from(cwd)
    } else {
        resolve_remote_path(dir_display.trim_end_matches('/'), cwd)
    };
    match std::fs::read_dir(&dir_to_scan) {
        Ok(entries) => {
            let mut paths: Vec<String> = entries
                .filter_map(|entry| entry.ok())
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !name.starts_with(name_prefix) {
                        return None;
                    }
                    // Match local completion: hide dotfiles unless the user
                    // typed a leading dot.
                    if name.starts_with('.') && !name_prefix.starts_with('.') {
                        return None;
                    }
                    let mut candidate = format!("{dir_display}{name}");
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        candidate.push('/');
                    }
                    Some(candidate)
                })
                .collect();
            paths.sort();
            paths.truncate(LIST_PATH_MAX_CANDIDATES);
            ok(SshOpResult::ListPath(paths))
        }
        Err(error) => err(format!("Failed to list {}: {error}", dir_to_scan.display())),
    }
}

// --- directory browser op -------------------------------------------

/// `browse_dir` for the `/open` directory browser: one shot returns the
/// directory's full entry list (sorted, dotfiles included) plus a git
/// summary of that directory for the right pane.
fn op_browse_dir(path: &str, cwd: &str) -> SshOpOutcome {
    let resolved = resolve_remote_path(path, cwd);
    let entries = match std::fs::read_dir(&resolved) {
        Ok(read_dir) => {
            let mut entries: Vec<SshDirEntry> = read_dir
                .filter_map(|entry| entry.ok())
                .map(|entry| {
                    let file_type = entry.file_type().ok();
                    let is_symlink = file_type.is_some_and(|t| t.is_symlink());
                    let is_dir = if is_symlink {
                        entry.metadata().map(|m| m.is_dir()).unwrap_or(false)
                    } else {
                        file_type.is_some_and(|t| t.is_dir())
                    };
                    SshDirEntry {
                        name: entry.file_name().to_string_lossy().to_string(),
                        is_dir,
                        is_symlink,
                    }
                })
                .collect();
            entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
            entries
        }
        Err(error) => {
            return err(format!("Failed to list {}: {error}", resolved.display()));
        }
    };
    ok(SshOpResult::BrowseDir {
        git: remote_dir_git_summary(&resolved),
        path: resolved.display().to_string(),
        entries,
    })
}

/// Git summary for `dir`, run bridge-side. Returns None when the directory
/// is not inside a work tree or git is missing. `dirty` counts staged +
/// modified + untracked porcelain rows.
fn remote_dir_git_summary(dir: &Path) -> Option<SshDirGitSummary> {
    use std::process::Command;

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()
    };

    let in_repo = git(&["rev-parse", "--is-inside-work-tree"])
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !in_repo {
        return None;
    }

    let branch = git(&["branch", "--show-current"]).and_then(|o| {
        if !o.status.success() {
            return Some("HEAD".to_string());
        }
        let b = String::from_utf8_lossy(&o.stdout).trim().to_string();
        Some(if b.is_empty() { "HEAD".to_string() } else { b })
    });

    let dirty = git(&["status", "--porcelain"])
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|line| line.len() >= 3)
                .count()
        })
        .unwrap_or(0);

    let (ahead, behind) = git(&["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .filter(|o| o.status.success())
        .and_then(|o| {
            let text = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let mut parts = text.split('\t');
            Some((
                parts.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                parts.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            ))
        })
        .unwrap_or((0, 0));

    Some(SshDirGitSummary {
        is_repo: true,
        branch,
        dirty,
        ahead,
        behind,
    })
}

// --- skills op -------------------------------------------------------

fn op_list_skills(working_dir: &str) -> SshOpOutcome {
    let infos = crate::skill::SkillRegistry::shared_registry()
        .try_read()
        .map(|global| {
            let effective = crate::skill::SkillRegistry::effective_for_working_dir(
                &global,
                Some(Path::new(working_dir)),
            );
            effective
                .list()
                .iter()
                .map(|skill| SshSkillInfo {
                    name: skill.name.clone(),
                    description: skill.description.clone(),
                    path: skill.path.display().to_string(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ok(SshOpResult::ListSkills(infos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browse_dir_request_roundtrips() {
        let request = SshOpRequest {
            id: 7,
            op: SshOp::BrowseDir {
                path: "/srv/proj".to_string(),
            },
        };
        let line = encode_request(&request).unwrap();
        assert_eq!(
            line,
            r#"{"__jcode_ssh_op":{"id":7,"op":"browse_dir","path":"/srv/proj"}}"#
        );
        let decoded = decode_request(&line).unwrap();
        assert!(matches!(
            decoded.op,
            SshOp::BrowseDir { ref path } if path == "/srv/proj"
        ));
    }

    #[test]
    fn browse_dir_response_roundtrips() {
        let response = SshOpResponse {
            id: 7,
            outcome: SshOpOutcome::Result(SshOpResult::BrowseDir {
                path: "/srv/proj".to_string(),
                entries: vec![
                    SshDirEntry {
                        name: "src".to_string(),
                        is_dir: true,
                        is_symlink: false,
                    },
                    SshDirEntry {
                        name: "lib".to_string(),
                        is_dir: true,
                        is_symlink: true,
                    },
                ],
                git: Some(SshDirGitSummary {
                    is_repo: true,
                    branch: Some("main".to_string()),
                    dirty: 3,
                    ahead: 1,
                    behind: 2,
                }),
            }),
        };
        let line = encode_response(&response).unwrap();
        let decoded = decode_response(&line).unwrap();
        assert_eq!(decoded.id, 7);
        let SshOpOutcome::Result(SshOpResult::BrowseDir { path, entries, git }) = decoded.outcome
        else {
            panic!("expected browse_dir result");
        };
        assert_eq!(path, "/srv/proj");
        assert_eq!(entries.len(), 2);
        assert!(entries[1].is_symlink);
        let git = git.expect("git summary");
        assert_eq!(git.branch.as_deref(), Some("main"));
        assert_eq!((git.dirty, git.ahead, git.behind), (3, 1, 2));
    }

    #[test]
    fn browse_dir_lists_and_reports_git() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("zdir")).unwrap();
        std::fs::write(tmp.path().join("a.txt"), b"x").unwrap();
        let ctx = SshOpContext::default();
        let outcome = op_browse_dir(&tmp.path().display().to_string(), &ctx.working_dir);
        let SshOpOutcome::Result(SshOpResult::BrowseDir { path, entries, git }) = outcome else {
            panic!("expected browse_dir result");
        };
        assert_eq!(path, tmp.path().display().to_string());
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["zdir", "a.txt"]);
        assert!(git.is_none() || git.as_ref().is_some_and(|g| g.is_repo));
    }

    #[test]
    fn browse_dir_missing_dir_is_an_error() {
        let ctx = SshOpContext::default();
        let outcome = op_browse_dir("/definitely/not/here", &ctx.working_dir);
        assert!(matches!(outcome, SshOpOutcome::Error(_)));
    }
}
