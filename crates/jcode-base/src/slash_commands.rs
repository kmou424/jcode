//! User-registered slash commands backed by external commands.
//!
//! `[slash_commands]` in config.toml maps a `/name` to an executable that
//! owns the command's runtime behavior. jcode resolves the slash, spawns the
//! command, and interprets its stdout as a JSON action describing what to do
//! (display text, activate a skill, etc). Plugins can provide slash surfaces
//! (mode toggles, status views, macro expansions) without any jcode source
//! changes.
//!
//! Schema description at startup: when a command is invoked as
//! `command --describe`, it must print a JSON `SlashCommandSpec` describing
//! its subcommands and (optionally) their argument candidates. jcode caches
//! the schema for the session so the completion UI is free of subprocess
//! spawn costs.
//!
//! Execution: `command <args...>` prints one JSON `SlashCommandAction` on
//! stdout. Non-JSON stdout is treated as `{"action":"display","text":…}` so
//! trivial scripts can skip the JSON wrapper.
//!
//! Completion: `command __completions <arg1> … <prefix>` returns
//! `{"candidates":[{"value":..,"description":..}, …]}`. This is used when a
//! `--describe` schema is not available (rare — `--describe` is preferred).

use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::hooks::HookEvent;

const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(2);
const EXEC_TIMEOUT: Duration = Duration::from_secs(5);
const COMPLETIONS_TIMEOUT: Duration = Duration::from_millis(500);
const STDOUT_LIMIT: usize = 64 * 1024;

/// One `register`-time subcommand spec returned by `--describe`.
#[derive(Debug, Clone, Deserialize)]
pub struct SlashSubcommandSpec {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Optional list of argument positions; each may carry a fixed `values`
    /// list for completion. Free-form args simply omit `values`.
    #[serde(default)]
    pub args: Vec<SlashArgSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SlashArgSpec {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub values: Vec<SlashCandidate>,
}

/// A completion candidate (value plus optional description shown alongside).
#[derive(Debug, Clone, Deserialize)]
pub struct SlashCandidate {
    pub value: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// The response to `--describe`. Either form is accepted:
///
/// ```text
/// {"subcommands": [{"name":"lite", "description":"…"}, …]}
/// ```
///
/// or a bare `{"description": "…"}` when the command takes no subcommands.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SlashCommandSpec {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub subcommands: Vec<SlashSubcommandSpec>,
}

/// Action returned by `command <args…>`. See module docs for the wire shape.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SlashCommandAction {
    /// Show `text` as a system message; nothing else happens.
    Display { text: String },
    /// Show `text` as an error message.
    Error { text: String },
    /// Activate `skill` (must be a loaded skill name) and pass `prompt` as
    /// the trailing user prompt if provided.
    ActivateSkill {
        skill: String,
        #[serde(default)]
        prompt: Option<String>,
    },
    /// Display `text` and then activate `skill` with an optional prompt.
    DisplayAndActivate {
        text: String,
        skill: String,
        #[serde(default)]
        prompt: Option<String>,
    },
    /// Do nothing — the command produced no visible output.
    Silent,
}

/// Response shape for `__completions`.
#[derive(Debug, Clone, Deserialize)]
struct CompletionsResponse {
    candidates: Vec<SlashCandidate>,
}

/// Cache for `--describe` results, keyed by command line. Populated lazily —
/// only queried when the user actually references the slash command — so
/// unused commands cost zero spawn.
static SPEC_CACHE: OnceLock<std::sync::Mutex<BTreeMap<String, Option<SlashCommandSpec>>>> =
    OnceLock::new();

fn spec_cache() -> &'static std::sync::Mutex<BTreeMap<String, Option<SlashCommandSpec>>> {
    SPEC_CACHE.get_or_init(|| std::sync::Mutex::new(BTreeMap::new()))
}

/// Look up the registered `/name` entry. Names in the config map are stored
/// without the leading slash; lookups accept either form.
pub fn lookup_command(name: &str) -> Option<crate::config::SlashCommandEntry> {
    let key = name.trim_start_matches('/');
    crate::config::config().slash_commands.get(key).cloned()
}

/// True when `/name` is backed by an external command (and not by a built-in
/// or a skill). Used by the dispatcher to decide who claims the slash.
pub fn is_external_command(name: &str) -> bool {
    lookup_command(name).is_some()
}

/// List every `[slash_commands]` entry — used by autocomplete to seed the
/// top-level command menu. Returns `(name, entry)` pairs sorted by name.
pub fn external_command_names() -> Vec<(String, crate::config::SlashCommandEntry)> {
    crate::config::config()
        .slash_commands
        .iter()
        .map(|(n, e)| (n.clone(), e.clone()))
        .collect()
}

/// Short help text for `/name` — the entry's `description` or `None`.
pub fn external_command_description(name: &str) -> Option<String> {
    lookup_command(name).and_then(|e| e.description)
}

/// Split `/name foo bar` into `(name, completed_args, prefix)` for completion.
/// The last whitespace-separated token is the in-progress prefix; earlier
/// tokens are completed args. Returns `None` when the input doesn't parse as
/// a slash command or has no name yet.
pub fn parse_input(input: &str) -> Option<(String, Vec<String>, String)> {
    let input = input.trim_start();
    if !input.starts_with('/') {
        return None;
    }
    let input = input.trim_start_matches('/');
    let mut parts = input.split_whitespace().map(str::to_owned);
    let name = parts.next()?;
    let args: Vec<String> = parts.collect();
    // If the input ends in whitespace, the user is starting a new arg —
    // the prefix is empty and every typed arg is complete. Otherwise the
    // last token is the in-progress prefix.
    let ends_with_space = input.chars().last().is_some_and(char::is_whitespace);
    let (completed, prefix) = if ends_with_space || args.is_empty() {
        (args, String::new())
    } else {
        let mut a = args;
        let p = a.pop().unwrap_or_default();
        (a, p)
    };
    Some((name, completed, prefix))
}

fn build_command(command_line: &str, event: &HookEvent) -> Option<Command> {
    let parts = crate::terminal_launch::parse_hook_command(command_line).ok()?;
    let (program, args) = parts.split_first()?;
    let mut cmd = Command::new(expand_home(program));
    // `Command::new().args()` bypasses the shell, so `~` / `$HOME` inside an
    // argument stay literal — expand them here the same way `expand_home`
    // does for the program name.
    let args: Vec<String> = args
        .iter()
        .map(|a| {
            expand_home(a)
                .to_str()
                .map(str::to_owned)
                .unwrap_or_else(|| a.clone())
        })
        .collect();
    cmd.args(args);
    if let Some(cwd) = event.cwd.as_deref().filter(|cwd| !cwd.is_empty())
        && std::path::Path::new(cwd).is_dir()
    {
        cmd.current_dir(cwd);
    }
    cmd.env("JCODE_HOOKS_DISABLED", "1");
    cmd.env("JCODE_HOOK_EVENT", event.event);
    if let Some(session_id) = &event.session_id {
        cmd.env("JCODE_HOOK_SESSION_ID", session_id);
        // Alias: tool-spawned commands read JCODE_SESSION_ID; hooks read
        // JCODE_HOOK_SESSION_ID. Slash commands are user-initiated, so we set
        // both — scripts can use either.
        cmd.env("JCODE_SESSION_ID", session_id);
    }
    if let Some(cwd) = &event.cwd {
        cmd.env("JCODE_HOOK_CWD", cwd);
    }
    for (key, value) in &event.fields {
        cmd.env(format!("JCODE_HOOK_{key}"), value);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    Some(cmd)
}

fn expand_home(program: &str) -> std::path::PathBuf {
    if let Some(rest) = program.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    std::path::PathBuf::from(program)
}

/// Run `cmd` to completion under `timeout`, capturing stdout. Returns
/// `None` on spawn error, timeout, or excessive output.
fn run_to_completion(mut cmd: Command, timeout: Duration) -> Option<String> {
    let start = Instant::now();
    let mut child = cmd.spawn().ok()?;
    let mut stdout_pipe = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        // Read up to STDOUT_LIMIT + a bit so we can detect overflow without
        // unbounded memory.
        let mut chunk = [0u8; 8192];
        loop {
            match stdout_pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.len() > STDOUT_LIMIT {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        buf
    });

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break None,
        }
    };

    let buf = reader.join().ok()?;
    let stdout = String::from_utf8_lossy(&buf).to_string();
    if buf.len() > STDOUT_LIMIT {
        crate::logging::warn("slash command stdout exceeded limit; truncated");
    }
    match status {
        Some(s) if s.success() => Some(stdout),
        _ => None,
    }
}

/// Fetch the `--describe` schema for a registered command, caching the result.
/// Returns `None` when the command doesn't implement `--describe`.
pub fn spec_for(name: &str, session_id: &str, cwd: Option<&str>) -> Option<SlashCommandSpec> {
    let entry = lookup_command(name)?;
    let cache_key = format!("{}|{}", name, entry.command);

    if let Ok(cache) = spec_cache().lock()
        && let Some(cached) = cache.get(&cache_key)
    {
        return cached.clone();
    }

    let event = HookEvent::new("slash_command_describe").session_id(session_id);
    let event = if let Some(cwd) = cwd {
        event.cwd(cwd)
    } else {
        event
    };
    let mut cmd = build_command(&entry.command, &event)?;
    cmd.arg("--describe");

    let spec = run_to_completion(cmd, DESCRIBE_TIMEOUT)
        .and_then(|out| serde_json::from_str::<SlashCommandSpec>(out.trim()).ok());

    if let Ok(mut cache) = spec_cache().lock() {
        cache.insert(cache_key, spec.clone());
    }
    spec
}

/// Run the external command bound to `/name` with the given args and return
/// the action to perform. Returns `None` when the command isn't registered,
/// fails to spawn, or times out — the caller should fall back to the next
/// dispatch layer in that case.
pub fn run_command(
    name: &str,
    args: &[String],
    session_id: &str,
    cwd: Option<&str>,
) -> Option<SlashCommandAction> {
    let entry = lookup_command(name)?;

    let mut event = HookEvent::new("slash_command").session_id(session_id);
    if let Some(cwd) = cwd {
        event = event.cwd(cwd);
    }
    event = event.field("COMMAND", name);
    event = event.field(
        "ARGV",
        serde_json::to_string(&args).unwrap_or_else(|_| "[]".to_string()),
    );

    let mut cmd = build_command(&entry.command, &event)?;
    cmd.args(args);

    let stdout = run_to_completion(cmd, EXEC_TIMEOUT)?;
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Some(SlashCommandAction::Silent);
    }

    // Try strict JSON first; fall back to "display the text as-is".
    if let Ok(action) = serde_json::from_str::<SlashCommandAction>(trimmed) {
        return Some(action);
    }
    Some(SlashCommandAction::Display {
        text: trimmed.to_string(),
    })
}

/// Fetch completion candidates for `/name <args…> <prefix>`. Preferred path is
/// the cached `--describe` schema; the `__completions` subprocess call is used
/// when no schema is available.
pub fn completions_for(
    name: &str,
    args: &[String],
    prefix: &str,
    session_id: &str,
    cwd: Option<&str>,
) -> Vec<SlashCandidate> {
    // Schema-driven completion is the preferred path — the command declares
    // its argument candidates once at startup, so Tab stays cheap.
    if let Some(spec) = spec_for(name, session_id, cwd) {
        return complete_from_spec(&spec, args, prefix);
    }

    // Fallback: live `__completions` subprocess for commands without
    // --describe. Still bounded by COMPLETIONS_TIMEOUT so a hung script
    // doesn't stall input handling.
    let Some(entry) = lookup_command(name) else {
        return Vec::new();
    };

    let mut event = HookEvent::new("slash_command_completions").session_id(session_id);
    if let Some(cwd) = cwd {
        event = event.cwd(cwd);
    }
    let mut cmd = match build_command(&entry.command, &event) {
        Some(cmd) => cmd,
        None => return Vec::new(),
    };
    cmd.arg("__completions");
    for arg in args {
        cmd.arg(arg);
    }
    cmd.arg(prefix);

    let Some(stdout) = run_to_completion(cmd, COMPLETIONS_TIMEOUT) else {
        return Vec::new();
    };
    let Ok(response) = serde_json::from_str::<CompletionsResponse>(stdout.trim()) else {
        return Vec::new();
    };
    response
        .candidates
        .into_iter()
        .filter(|c| c.value.starts_with(prefix))
        .collect()
}

fn complete_from_spec(
    spec: &SlashCommandSpec,
    args: &[String],
    prefix: &str,
) -> Vec<SlashCandidate> {
    // args is the list of completed tokens *before* the one being edited.
    // When empty we're completing the first token after the command name,
    // which is the subcommand position.
    if args.is_empty() {
        return spec
            .subcommands
            .iter()
            .filter(|sub| sub.name.starts_with(prefix))
            .map(|sub| SlashCandidate {
                value: sub.name.clone(),
                description: sub.description.clone(),
            })
            .collect();
    }

    // args[0] selects the subcommand; args[1..] are its positional args.
    let Some(sub) = spec
        .subcommands
        .iter()
        .find(|sub| sub.name == args[0].as_str())
    else {
        return Vec::new();
    };

    // args[1] is the first positional arg (index 0), args[2] is the second, etc.
    let arg_index = args.len().saturating_sub(1);
    let Some(arg_spec) = sub.args.get(arg_index) else {
        return Vec::new();
    };
    arg_spec
        .values
        .iter()
        .filter(|c| c.value.starts_with(prefix))
        .cloned()
        .collect()
}

/// Drop the `--describe` cache. Useful in tests; safe to call anytime.
#[cfg(test)]
pub fn reset_spec_cache() {
    if let Ok(mut cache) = spec_cache().lock() {
        cache.clear();
    }
}

/// Serde helper for tests — the Value form is what scripts print.
#[cfg(test)]
pub fn parse_action_for_test(value: serde_json::Value) -> Option<SlashCommandAction> {
    serde_json::from_value(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_input_splits_name_args_and_prefix() {
        assert_eq!(
            parse_input("/ponytail-ctl"),
            Some(("ponytail-ctl".into(), vec![], String::new()))
        );
        // No trailing space → last token is the in-flight prefix.
        assert_eq!(
            parse_input("/ponytail-ctl st"),
            Some(("ponytail-ctl".into(), vec![], "st".into(),))
        );
        // Trailing space → prefix is empty, args all committed.
        assert_eq!(
            parse_input("/ponytail-ctl default "),
            Some(("ponytail-ctl".into(), vec!["default".into()], String::new(),))
        );
        // Partial arg in second position.
        assert_eq!(
            parse_input("/ponytail-ctl default ul"),
            Some(("ponytail-ctl".into(), vec!["default".into()], "ul".into(),))
        );
        assert_eq!(parse_input("not-a-slash"), None);
        assert_eq!(parse_input("/"), None);
    }

    #[test]
    fn action_parses_all_variants() {
        let cases = [
            (r#"{"type":"silent"}"#, "Silent"),
            (r#"{"type":"display","text":"hello"}"#, "Display { text }"),
            (r#"{"type":"error","text":"oops"}"#, "Error { text }"),
            (
                r#"{"type":"activate_skill","skill":"ponytail"}"#,
                "ActivateSkill { .. }",
            ),
            (
                r#"{"type":"display_and_activate","text":"hi","skill":"ponytail"}"#,
                "DisplayAndActivate { .. }",
            ),
        ];
        for (json, want) in cases {
            let v: serde_json::Value = serde_json::from_str(json).unwrap();
            let action = parse_action_for_test(v).unwrap();
            let got = match action {
                SlashCommandAction::Silent => "Silent",
                SlashCommandAction::Display { .. } => "Display { text }",
                SlashCommandAction::Error { .. } => "Error { text }",
                SlashCommandAction::ActivateSkill { .. } => "ActivateSkill { .. }",
                SlashCommandAction::DisplayAndActivate { .. } => "DisplayAndActivate { .. }",
            };
            assert_eq!(got, want, "for {json}");
        }
    }

    #[test]
    fn complete_from_spec_walks_subcommands_then_args() {
        let spec = SlashCommandSpec {
            description: None,
            subcommands: vec![
                SlashSubcommandSpec {
                    name: "status".into(),
                    description: Some("show".into()),
                    args: vec![],
                },
                SlashSubcommandSpec {
                    name: "default".into(),
                    description: None,
                    args: vec![SlashArgSpec {
                        name: Some("level".into()),
                        values: vec![
                            SlashCandidate {
                                value: "lite".into(),
                                description: None,
                            },
                            SlashCandidate {
                                value: "full".into(),
                                description: None,
                            },
                            SlashCandidate {
                                value: "ultra".into(),
                                description: None,
                            },
                        ],
                    }],
                },
            ],
        };

        // First position → subcommands.
        let first: Vec<_> = complete_from_spec(&spec, &[], "")
            .into_iter()
            .map(|c| c.value)
            .collect();
        assert_eq!(first, vec!["status", "default"]);

        // Prefix filters subcommands.
        let filtered: Vec<_> = complete_from_spec(&spec, &[], "st")
            .into_iter()
            .map(|c| c.value)
            .collect();
        assert_eq!(filtered, vec!["status"]);

        // After `default`, complete its `level` arg.
        let levels: Vec<_> = complete_from_spec(&spec, &["default".into()], "")
            .into_iter()
            .map(|c| c.value)
            .collect();
        assert_eq!(levels, vec!["lite", "full", "ultra"]);
    }
}
