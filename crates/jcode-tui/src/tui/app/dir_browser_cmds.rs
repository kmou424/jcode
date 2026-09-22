//! `/open` directory browser: slash-command entry point, key routing, and
//! the local/remote load plumbing behind the `DirBrowser` overlay.
//!
//! Enter opens a *new* jcode rooted at the chosen directory in a separate
//! terminal window — the current session is never moved. Over SSH the
//! browser lists remote directories via the `browse_dir` sideband op and
//! Enter spawns a new terminal running `jcode --ssh <host>` with
//! `JCODE_SSH_OPEN_DIR=<dir>` on its environment so the fresh session
//! anchors on the remote host's path through the workspace handshake.

use super::{App, PendingDirBrowserLoad};
use crate::tui::dir_browser::{DirBrowser, DirBrowserAction, DirListing};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers};
use jcode_tui_messages::DisplayMessage;
use std::cell::RefCell;

pub(super) fn handle_open_command(app: &mut App, trimmed: &str) -> bool {
    if trimmed != "/open" && !trimmed.starts_with("/open ") {
        return false;
    }
    let arg = trimmed.strip_prefix("/open").unwrap_or_default().trim();
    app.open_dir_browser(if arg.is_empty() {
        None
    } else {
        Some(arg.to_string())
    });
    true
}

impl App {
    /// Open the `/open` browser at `start` (or the session working dir).
    /// Local sessions list the laptop's filesystem on a blocking thread;
    /// SSH clients browse the remote host through the `browse_dir` sideband
    /// op, gated on the bridge's `sideband_ops` handshake flag.
    pub(super) fn open_dir_browser(&mut self, start: Option<String>) {
        let remote = crate::tui::is_ssh_remote();
        if remote && !crate::tui::ssh_ops_supported() {
            self.push_display_message(DisplayMessage::error(
                "The remote jcode bridge is too old for /open (missing sideband ops). \
                 Update jcode on the remote host."
                    .to_string(),
            ));
            return;
        }
        let start = start.unwrap_or_else(|| {
            self.session
                .working_dir
                .clone()
                .or_else(|| std::env::var("JCODE_SSH_WORKING_DIR").ok())
                .unwrap_or_else(|| ".".to_string())
        });
        self.dir_browser_overlay = Some(RefCell::new(DirBrowser::new(start.clone(), remote)));
        self.remote_dir_browse_inflight = None;
        self.issue_dir_browser_load(start);
        self.set_status_notice("Directory browser open");
    }

    /// Issue a listing for `path` through whichever backend is active:
    /// remote goes through the pending sideband queue (drained on the
    /// remote poll loop), local spawns a blocking read thread.
    fn issue_dir_browser_load(&mut self, path: String) {
        if crate::tui::is_ssh_remote() {
            self.pending_remote_dir_browse = Some(path);
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.pending_dir_browser_load = Some(PendingDirBrowserLoad { receiver: rx });
        let cwd = std::path::PathBuf::from(".");
        std::thread::spawn(move || {
            let _ = tx.send(crate::tui::dir_browser::load_local(&path, &cwd));
        });
    }

    /// Drain a completed local listing into the overlay.
    pub(super) fn poll_dir_browser_load(&mut self) -> bool {
        let recv_result = {
            let Some(pending) = self.pending_dir_browser_load.as_ref() else {
                return false;
            };
            pending.receiver.try_recv()
        };
        match recv_result {
            Ok(result) => {
                self.pending_dir_browser_load = None;
                if let Some(cell) = self.dir_browser_overlay.as_ref() {
                    cell.borrow_mut().apply_listing(result);
                    return true;
                }
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.pending_dir_browser_load = None;
                if let Some(cell) = self.dir_browser_overlay.as_ref() {
                    cell.borrow_mut()
                        .apply_listing(Err("Directory load stopped early.".to_string()));
                    return true;
                }
                false
            }
        }
    }

    /// Feed a `browse_dir` sideband reply into the overlay. Replies for a
    /// superseded request (the user already navigated again) are dropped so
    /// a slow listing cannot overwrite a newer one.
    pub(super) fn apply_remote_dir_browse(
        &mut self,
        id: u64,
        path: String,
        entries: Vec<jcode_app_core::ssh_ops::SshDirEntry>,
        git: Option<jcode_app_core::ssh_ops::SshDirGitSummary>,
    ) -> bool {
        if self.remote_dir_browse_inflight != Some(id) {
            // Stale (an older navigation's reply arriving late) or
            // unsolicited — do not clear the newer in-flight id.
            return false;
        }
        self.remote_dir_browse_inflight = None;
        let Some(cell) = self.dir_browser_overlay.as_ref() else {
            return false;
        };
        let listing = DirListing::from_ssh(path, entries, git);
        cell.borrow_mut().apply_listing(Ok(listing));
        true
    }

    /// Route a key press through the overlay and act on its result.
    pub(super) fn handle_dir_browser_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        let action = {
            let Some(cell) = self.dir_browser_overlay.as_ref() else {
                return Ok(());
            };
            cell.borrow_mut().handle_overlay_key(code, modifiers)
        };
        match action {
            DirBrowserAction::Continue => {}
            DirBrowserAction::Close => {
                self.dir_browser_overlay = None;
            }
            DirBrowserAction::Load(path) => self.issue_dir_browser_load(path),
            DirBrowserAction::OpenAt(path) => self.open_dir_in_new_terminal(&path),
        }
        Ok(())
    }

    /// Spawn a new jcode rooted at `path` in a separate terminal. For SSH
    /// clients `path` is a remote directory and the child attaches with
    /// `--ssh <host>` plus `JCODE_SSH_OPEN_DIR=<path>`; locally it is a plain
    /// fresh session with `dir` as the terminal cwd.
    fn open_dir_in_new_terminal(&mut self, path: &str) {
        let exe = super::helpers::launch_client_executable();
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
        let remote_dir = if crate::tui::is_ssh_remote() {
            Some(path)
        } else {
            None
        };
        match super::helpers::spawn_dir_session_in_new_terminal(
            &exe,
            std::path::Path::new(path),
            remote_dir,
            &cwd,
        ) {
            Ok(true) => {
                self.dir_browser_overlay = None;
                self.push_display_message(DisplayMessage::system(format!(
                    "Opened a new jcode at:\n\n  {path}"
                )));
                self.set_status_notice("Opened new jcode window");
            }
            Ok(false) => {
                // No terminal multiplexer/spawner available (headless,
                // SSH-in-tmux-less, unsupported terminal). Hand the user the
                // equivalent command instead of silently dropping the pick.
                let manual = if let Some(host) = crate::tui::ssh_remote_host() {
                    format!("env JCODE_SSH_OPEN_DIR='{path}' jcode --ssh {host}")
                } else {
                    format!("cd '{path}' && jcode")
                };
                self.push_display_message(DisplayMessage::system(format!(
                    "No terminal was opened automatically. Run this in a new terminal:\n\n  {manual}"
                )));
                self.set_status_notice("Open manually");
            }
            Err(error) => {
                self.push_display_message(DisplayMessage::error(format!(
                    "Failed to open a new jcode at {path}: {error}"
                )));
            }
        }
    }
}
