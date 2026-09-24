//! `${VAR}` / `${VAR:-default}` / `!{cmd}` substitution shared by MCP server
//! config and secret-typed config values.
//!
//! Two consumers with deliberately different semantics:
//!
//! * [`expand_environment_string`] performs inline expansion inside a larger
//!   string. A failed or malformed `!{cmd}` leaves the literal in place, which
//!   is what MCP server configs want (the literal is harmless there).
//! * [`resolve_secret_value`] treats the *entire* value as a `!{cmd}`
//!   substitution and refuses to hand back the literal: a failed, empty, or
//!   malformed substitution returns `None` so an unresolved `!{` expression
//!   can never be sent as a credential.

use std::collections::{BTreeSet, HashMap};
use std::sync::{LazyLock, RwLock};

/// Process-wide memo for whole-value `!{cmd}` secret substitutions, keyed by
/// the inner command text. Both successes and failures are cached so a broken
/// keychain helper does not respawn on every credential lookup; entries are
/// dropped when [`clear_substitution_cache`] runs (config reload).
static SUBSTITUTION_CACHE: LazyLock<RwLock<HashMap<String, Option<String>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Drop every memoized `!{cmd}` result so the next secret lookup re-runs its
/// command. Called when the config cache is invalidated (e.g. `Config::save`).
pub fn clear_substitution_cache() {
    SUBSTITUTION_CACHE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

fn valid_environment_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('_' | 'A'..='Z' | 'a'..='z'))
        && chars.all(|ch| matches!(ch, '_' | 'A'..='Z' | 'a'..='z' | '0'..='9'))
}

/// Expand `${VAR}`, `${VAR:-default}`, and `!{cmd}` command substitution
/// (e.g. `!{pass show mcp/token}`, `!{op read op://vault/item/field}`) inside a
/// config string. Unsupported/malformed expressions and failed commands are
/// preserved verbatim; variables the lookup cannot resolve are collected in
/// `unresolved` and also preserved.
pub fn expand_environment_string<F>(
    value: &str,
    lookup: &F,
    unresolved: &mut BTreeSet<String>,
) -> String
where
    F: Fn(&str) -> Option<String>,
{
    let mut output = String::with_capacity(value.len());
    let mut remainder = value;

    loop {
        // Find the earliest `${` or `!{` marker.
        let (start, is_command) = match (remainder.find("${"), remainder.find("!{")) {
            (Some(env), Some(cmd)) => {
                if env < cmd {
                    (env, false)
                } else {
                    (cmd, true)
                }
            }
            (Some(env), None) => (env, false),
            (None, Some(cmd)) => (cmd, true),
            (None, None) => break,
        };
        output.push_str(&remainder[..start]);
        let expression_start = start + 2;
        let Some(relative_end) = remainder[expression_start..].find('}') else {
            output.push_str(&remainder[start..]);
            return output;
        };
        let end = expression_start + relative_end;
        let expression = &remainder[expression_start..end];
        let literal = &remainder[start..=end];

        if is_command {
            match run_substitution_command(expression) {
                Some(stdout) => output.push_str(&stdout),
                None => output.push_str(literal),
            }
        } else {
            let (variable, default) = match expression.split_once(":-") {
                Some((variable, default)) => (variable, Some(default)),
                None => (expression, None),
            };

            if !valid_environment_variable_name(variable) {
                output.push_str(literal);
            } else if let Some(expanded) = lookup(variable) {
                output.push_str(&expanded);
            } else if let Some(default) = default {
                output.push_str(default);
            } else {
                unresolved.insert(variable.to_string());
                output.push_str(literal);
            }
        }

        remainder = &remainder[end + 1..];
    }

    output.push_str(remainder);
    output
}

/// Resolve a secret-typed config value that may be a whole-value `!{cmd}`
/// substitution. Plain values pass through unchanged; a trimmed value of the
/// form `!{command}` runs the command once per process (memoized) and returns
/// its trimmed stdout.
///
/// Returns `None` — and never the literal — when the value starts with `!{`
/// but is malformed, the command fails or times out, or stdout is empty.
pub fn resolve_secret_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if !trimmed.starts_with("!{") {
        return Some(value.to_string());
    }
    let Some(command) = trimmed
        .strip_prefix("!{")
        .and_then(|rest| rest.strip_suffix('}'))
    else {
        jcode_logging::warn(&format!(
            "Secret value starts with '!{{' but is not a complete '!{{cmd}}' expression; refusing to use it as a credential"
        ));
        return None;
    };

    if let Some(cached) = SUBSTITUTION_CACHE
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(command)
    {
        return cached.clone();
    }

    let resolved = run_substitution_command(command).filter(|stdout| !stdout.is_empty());
    if resolved.is_none() {
        jcode_logging::warn(&format!(
            "Secret substitution '!{{{}}}' produced no usable value; treating the credential as unset",
            command
        ));
    }
    SUBSTITUTION_CACHE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(command.to_string(), resolved.clone());
    resolved
}

/// Run a `!{cmd}` config substitution via `sh -c`, returning trimmed stdout.
/// Returns `None` on spawn failure, non-zero exit, or timeout. The 10s
/// deadline keeps a blocking keychain/CLI helper from hanging config load.
/// Stderr is captured so failures surface a reason in the log instead of a
/// bare `None`.
fn run_substitution_command(command: &str) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    // Drain stdout and stderr on threads so a chatty command cannot fill a
    // pipe and deadlock the poll loop.
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stderr_text = stderr_reader
                    .join()
                    .map(|buf| String::from_utf8_lossy(&buf).trim_end().to_string())
                    .unwrap_or_default();
                if !status.success() {
                    jcode_logging::warn(&format!(
                        "'!{{{}…}}' exited with {}; stderr: {}",
                        command.chars().take(60).collect::<String>(),
                        status,
                        if stderr_text.is_empty() {
                            "<empty>"
                        } else {
                            &stderr_text
                        }
                    ));
                    return None;
                }
                let buf = stdout_reader.join().ok()?;
                return Some(String::from_utf8_lossy(&buf).trim_end().to_string());
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    jcode_logging::warn(&format!(
                        "'!{{{}…}}' timed out after 10s and was killed",
                        command.chars().take(60).collect::<String>()
                    ));
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_secret_value_passes_plain_values_through_unchanged() {
        assert_eq!(
            resolve_secret_value("sk-plain-key").as_deref(),
            Some("sk-plain-key")
        );
        // Leading/trailing bytes are preserved for non-`!{` values; callers
        // keep their own trim semantics.
        assert_eq!(
            resolve_secret_value("  padded  ").as_deref(),
            Some("  padded  ")
        );
    }

    #[test]
    fn resolve_secret_value_runs_whole_value_command() {
        clear_substitution_cache();
        assert_eq!(
            resolve_secret_value("!{printf %s hello-secret}").as_deref(),
            Some("hello-secret")
        );
        assert_eq!(
            resolve_secret_value("  !{printf %s trimmed}  ").as_deref(),
            Some("trimmed")
        );
    }

    #[test]
    fn resolve_secret_value_never_returns_literal() {
        clear_substitution_cache();
        assert_eq!(resolve_secret_value("!{exit 1}"), None);
        assert_eq!(resolve_secret_value("!{}"), None);
        // `!{` without a closing brace is a malformed expression, not a secret.
        assert_eq!(resolve_secret_value("!{unterminated"), None);
    }

    #[test]
    fn resolve_secret_value_memoizes_per_command() {
        clear_substitution_cache();
        // A side-effecting command proves the cache prevents a second spawn:
        // the counter file is written once, so both calls must observe "1".
        let counter = std::env::temp_dir().join(format!(
            "jcode-subst-memo-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let command = format!(
            "!{{n=$(cat {0} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {0}; printf %s $n}}",
            counter.display()
        );
        assert_eq!(resolve_secret_value(&command).as_deref(), Some("1"));
        assert_eq!(resolve_secret_value(&command).as_deref(), Some("1"));
        let _ = std::fs::remove_file(&counter);
        clear_substitution_cache();
    }

    #[test]
    fn expand_environment_string_still_expands_inline_commands() {
        let mut unresolved = BTreeSet::new();
        let out =
            expand_environment_string("Bearer !{printf %s mytoken}", &|_| None, &mut unresolved);
        assert_eq!(out, "Bearer mytoken");
        // Failed commands keep the literal, matching the historical MCP
        // contract for inline substitution.
        let out = expand_environment_string("!{exit 1}", &|_| None, &mut unresolved);
        assert_eq!(out, "!{exit 1}");
    }
}
