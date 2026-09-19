//! Remote file flows driven by `read_file`/`write_file`/`read_config`
//! sideband op replies.
//!
//! SSH mode is remote-primary: `/swarm-prompt`, `/config edit` and `@` path
//! completion operate on the daemon host's filesystem. Requests are queued on
//! the `App` and drained by `remote::handle_tick`; the replies land here as
//! `ReadFile`/`ReadConfig`/`ListPath` op results and are dispatched by
//! request id.

use super::super::commands::run_interactive_editor;
use super::super::{App, DisplayMessage, RemoteFileReadOp};

/// Temp file used for `/swarm-prompt` remote edits.
fn remote_edit_temp_path(suffix: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("jcode-remote-{}-{}", std::process::id(), suffix))
}

/// Open `content` in `$VISUAL`/`$EDITOR` via a local temp file, then queue a
/// `write_file` carrying the edited text back to `remote_path`.
fn edit_remote_file(
    app: &mut App,
    remote_path: &str,
    seed: &str,
    temp_suffix: &str,
    success_notice: &str,
) {
    let temp_path = remote_edit_temp_path(temp_suffix);
    if let Err(error) = std::fs::write(&temp_path, seed) {
        app.push_display_message(DisplayMessage::error(format!(
            "Failed to stage remote file for editing: {}",
            error
        )));
        return;
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "nano".to_string());
    let mut parts = editor.split_whitespace();
    let Some(bin) = parts.next() else {
        app.push_display_message(DisplayMessage::error(
            "$VISUAL/$EDITOR is empty; cannot open the remote file.".to_string(),
        ));
        return;
    };
    let extra: Vec<&str> = parts.collect();
    let mut command = std::process::Command::new(bin);
    command.args(&extra).arg(&temp_path);
    match run_interactive_editor(&mut command) {
        Ok(status) if status.success() => match std::fs::read_to_string(&temp_path) {
            Ok(edited) => {
                app.pending_remote_file_requests.push_back(
                    super::super::PendingRemoteFileRequest::Write {
                        path: remote_path.to_string(),
                        content: edited,
                    },
                );
                app.push_display_message(DisplayMessage::system(format!(
                    "Edited {remote_path} in {editor} — saving to the remote host.\n\n{success_notice}"
                )));
            }
            Err(error) => app.push_display_message(DisplayMessage::error(format!(
                "Failed to read back edited temp file: {}",
                error
            ))),
        },
        Ok(status) => app.push_display_message(DisplayMessage::error(format!(
            "Editor '{}' exited with status {} while editing {}",
            editor, status, remote_path
        ))),
        Err(error) => app.push_display_message(DisplayMessage::error(format!(
            "Failed to launch editor '{}' for {}: {}",
            editor, remote_path, error
        ))),
    }
    let _ = std::fs::remove_file(&temp_path);
}

/// `/config edit` continuation: the daemon's raw `config.toml` text arrived.
fn continue_remote_config_edit(app: &mut App, remote_path: String, content: String) {
    let seed = if content.is_empty() {
        crate::config::Config::default_config_file_contents()
    } else {
        content
    };
    let temp_path = remote_edit_temp_path("config.toml");
    if let Err(error) = std::fs::write(&temp_path, &seed) {
        app.push_display_message(DisplayMessage::error(format!(
            "Failed to stage remote config for editing: {}",
            error
        )));
        return;
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".to_string());
    let mut parts = editor.split_whitespace();
    let Some(bin) = parts.next() else {
        app.push_display_message(DisplayMessage::error(
            "$EDITOR is set to an empty value; cannot open config.".to_string(),
        ));
        return;
    };
    let extra: Vec<&str> = parts.collect();
    let mut command = std::process::Command::new(bin);
    command.args(&extra).arg(&temp_path);
    match run_interactive_editor(&mut command) {
        Ok(status) if status.success() => match std::fs::read_to_string(&temp_path) {
            Ok(edited) if edited != seed => {
                match crate::config::Config::from_str(&edited) {
                    Ok(parsed) => {
                        crate::config::queue_remote_config_write(edited);
                        crate::config::set_remote_config_override(Some(parsed));
                    }
                    Err(error) => {
                        app.push_display_message(DisplayMessage::error(format!(
                            "Edited config is not valid TOML — not sent to the remote: {}",
                            error
                        )));
                        let _ = std::fs::remove_file(&temp_path);
                        return;
                    }
                }
                app.push_display_message(DisplayMessage::system(format!(
                    "Edited remote config in {}:\n{}\n\n*Restart jcode on the remote host after editing for changes to take effect.*",
                    editor, remote_path
                )));
                app.set_status_notice("Edited remote config");
            }
            Ok(_) => app.push_display_message(DisplayMessage::system(
                "Remote config unchanged.".to_string(),
            )),
            Err(error) => app.push_display_message(DisplayMessage::error(format!(
                "Failed to read back edited config: {}",
                error
            ))),
        },
        Ok(status) => app.push_display_message(DisplayMessage::error(format!(
            "Editor '{}' exited with status {}",
            editor, status
        ))),
        Err(error) => app.push_display_message(DisplayMessage::error(format!(
            "Failed to launch editor '{}': {}",
            editor, error
        ))),
    }
    let _ = std::fs::remove_file(&temp_path);
}

/// `/swarm-prompt` continuation: decide which remote file to edit.
fn continue_swarm_prompt_edit(app: &mut App, op: RemoteFileReadOp, content: String) {
    match op {
        RemoteFileReadOp::SwarmPromptProject => {
            if content.trim().is_empty() {
                // No project-level override — fall through to the global file,
                // mirroring `ensure_swarm_prompt_edit_path`.
                app.pending_remote_file_requests.push_back(
                    super::super::PendingRemoteFileRequest::Read {
                        path: "~/.jcode/swarm-prompt.md".to_string(),
                        op: RemoteFileReadOp::SwarmPromptGlobal,
                    },
                );
            } else {
                edit_remote_file(
                    app,
                    ".jcode/swarm-prompt.md",
                    &content,
                    "swarm-prompt.md",
                    "New agents will use this prompt immediately. Existing agents retain their current prompt to preserve their context cache.",
                );
            }
        }
        RemoteFileReadOp::SwarmPromptGlobal => {
            let seed = if content.trim().is_empty() {
                let template = format!("{}\n", crate::prompt::DEFAULT_SWARM_PROMPT.trim());
                // Mirror the local flow: create the file before opening the
                // editor so it exists even if the user quits without saving.
                app.pending_remote_file_requests.push_back(
                    super::super::PendingRemoteFileRequest::Write {
                        path: "~/.jcode/swarm-prompt.md".to_string(),
                        content: template.clone(),
                    },
                );
                template
            } else {
                content
            };
            edit_remote_file(
                app,
                "~/.jcode/swarm-prompt.md",
                &seed,
                "swarm-prompt.md",
                "New agents will use this prompt immediately. Existing agents retain their current prompt to preserve their context cache.",
            );
        }
    }
}

/// Dispatch a `file_content` reply to the flow that requested it. Returns
/// true when the event was consumed.
pub(super) fn handle_file_content(app: &mut App, id: u64, path: String, content: String) -> bool {
    // Attach-time remote config seed: install the remote config as
    // the process-wide override so `config()` reads follow the remote
    // machine's settings. An absent/empty remote file seeds `Config::default`
    // so later reads never fall through to the laptop's config.
    if app.awaiting_remote_config_seed == Some(id) {
        app.awaiting_remote_config_seed = None;
        let parsed = if content.trim().is_empty() {
            crate::config::Config::default()
        } else {
            match crate::config::Config::from_str(&content) {
                Ok(config) => config,
                Err(error) => {
                    crate::logging::warn(&format!(
                        "remote config seed from {} is not valid TOML; using defaults: {}",
                        path, error
                    ));
                    crate::config::Config::default()
                }
            }
        };
        crate::config::set_remote_config_override(Some(parsed));
        return true;
    }
    if app.awaiting_remote_config_edit == Some(id) {
        app.awaiting_remote_config_edit = None;
        continue_remote_config_edit(app, path, content);
        return true;
    }
    if let Some(op) = app.remote_file_read_ops.remove(&id) {
        continue_swarm_prompt_edit(app, op, content);
        return true;
    }
    false
}

/// Dispatch a `path_candidates` reply to `@` completion.
pub(super) fn handle_path_candidates(app: &mut App, id: u64, paths: Vec<String>) -> bool {
    if app.remote_path_completion_id != Some(id) {
        return false;
    }
    app.remote_path_completion_id = None;
    // No `@` path-completion surface exists in the composer yet; the wire
    // request is ready for it. Stash nothing and just consume the reply.
    let _ = paths;
    true
}

/// An `Error` reply may belong to an in-flight remote file request — consume
/// it and surface the message instead of running the generic handler.
pub(super) fn handle_remote_file_error(app: &mut App, id: u64, message: &str) -> bool {
    if app.awaiting_remote_config_seed == Some(id) {
        app.awaiting_remote_config_seed = None;
        crate::logging::warn(&format!("remote config seed failed: {}", message));
        return true;
    }
    if app.awaiting_remote_config_edit == Some(id) {
        app.awaiting_remote_config_edit = None;
        app.push_display_message(DisplayMessage::error(message.to_string()));
        return true;
    }
    if app.remote_file_read_ops.remove(&id).is_some() {
        app.push_display_message(DisplayMessage::error(message.to_string()));
        return true;
    }
    if app.remote_path_completion_id == Some(id) {
        app.remote_path_completion_id = None;
        return true;
    }
    false
}
