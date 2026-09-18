# [21] `/open` directory browser

## What it does

`/open` opens a two-pane directory browser whose only action is "start a
new jcode rooted at the chosen directory". It exists in two forms:

- **CLI**: `jcode open [path]` runs the same picker standalone. Enter
  changes into the picked directory and continues a normal launch (as if
  the user had `cd`'d there first); Esc/q exits without starting a
  session.
- **In-TUI `/open`**: opens the picker as an overlay. Enter spawns a
  *new* jcode in a separate terminal window rooted at the picked dir —
  the current session is never moved. `/open <path>` opens the browser
  at a specific directory.

Left pane: directory listing, directories first then files, both
alphabetical. Right pane: path, dir/file counts, and a git summary
(branch, dirty count, ahead/behind vs upstream) for the listed
directory, plus a description of the highlighted entry.

## Key mapping

Per the spec, the arrows are mapped **literally as written**, which is
the reverse of the conventional mapping:

- **Left arrow** — enter the highlighted directory
- **Right arrow** — go to the parent directory
- **Backspace** — also goes to the parent
- **Up/Down or k/j** — move the highlight; PageUp/PageDown/Home/End too
- **Enter** — open jcode at the highlighted directory; when a file (or
  nothing) is highlighted, opens at the listed directory
- **Esc, q, Ctrl+C** — close

If the conventional mapping was intended instead, flip the `KeyCode::Left`/
`KeyCode::Right` arms in `DirBrowser::handle_overlay_key`
(`crates/jcode-tui/src/tui/dir_browser.rs`).

## Remote support

Over SSH the browser lists directories **on the remote host** through a
new sideband op so the picker works identically in both modes:

- New op `browse_dir` (`crates/jcode-app-core/src/ssh_ops.rs`):
  request `{id, op:"browse_dir", path}` → result `{path, entries:[{name,
  is_dir, is_symlink}], git:{is_repo, branch, dirty, ahead, behind}}`.
  `dirty` counts porcelain rows (staged + modified + untracked);
  `ahead`/`behind` come from `git rev-list --left-right --count
  HEAD...@{upstream}`. Path resolution reuses `resolve_remote_path`
  (`~`, relative-to-workspace). The client correlates replies by request
  id and drops stale replies so a slow navigation cannot clobber a newer
  listing.
- Gated on the bridge's `sideband_ops` handshake flag: on older remotes
  `/open` shows a "remote bridge too old" notice instead of opening.
- Remote Enter spawns a new local terminal running
  `jcode --ssh <host> [--ssh-binary ..] [--ssh-server-socket ..]` with
  `JCODE_SSH_OPEN_DIR=<remote path>` on the child's environment — the
  spawned client reads it in `ssh run` as the workspace `--cwd`, so the
  fresh session anchors on the remote host without touching the
  deprecated `--remote-working-dir` flag.
- When no terminal can be spawned (headless, unsupported terminal), the
  picker stays open and a message prints the equivalent manual command
  (`cd <dir> && jcode` locally, or the `jcode --ssh` line remotely).
- `jcode --ssh <host> open [path]` runs the picker against the remote
  host before the workspace bridge is established: a probe `jcode server
  stdio` bridge (`RemoteBrowser` in `src/cli/ssh_transport.rs`) answers
  `browse_dir` sideband ops while the blocking picker loop runs on a
  `spawn_blocking` thread, exchanging paths/listings over a channel pair.
  Browsing starts at the `open` arg, else `--remote-working-dir`, else
  `JCODE_SSH_OPEN_DIR`, else the remote login HOME; Enter continues
  **in-process** as the normal `--ssh` launch at the picked dir (no
  second window, unlike in-TUI `/open`), Esc exits without connecting
  the workspace.
  `--resume` combined with `open` is rejected; a remote bridge without
  sideband ops fails fast before the picker opens.

## Spawn mechanism

- Local in-TUI Enter → `spawn_dir_session_in_new_terminal`, which reuses
  the existing fresh-session terminal command (`--fresh-spawn`, plus
  `--socket` when `JCODE_SOCKET` is set) via
  `terminal_launch::spawn_command_in_new_terminal`, with the picked dir
  as the child terminal's cwd.
- Remote in-TUI Enter → same helper with `remote_dir` set, producing
  the `jcode --ssh …` argv plus `JCODE_SSH_OPEN_DIR` in `spawn_env`.
- CLI `jcode open` does **not** spawn a second window — the picker is
  the launcher; Enter transitions straight into a normal startup at the
  chosen dir.
- CLI `jcode --ssh <host> open` also stays in-process — after the pick,
  `args.command` is cleared and the normal `run_unix` path continues with
  `remote_working_dir` set to the chosen remote directory.

## Affected files

- `crates/jcode-app-core/src/ssh_ops.rs` — `SshOp::BrowseDir`,
  `SshOpResult::BrowseDir`, `SshDirEntry`, `SshDirGitSummary`,
  `op_browse_dir` + `remote_dir_git_summary`, serde/exec tests.
- `crates/jcode-tui/src/tui/dir_browser.rs` (new) — backend-agnostic
  overlay: `DirBrowser`, `DirEntry`, `DirListing`, `DirGitSummary`,
  `sort_entries`, `join_path`, `parent_path`, `load_local`,
  `local_dir_git_summary`, render, key handling, unit tests.
- `crates/jcode-tui/src/tui/app/dir_browser_cmds.rs` (new) — `/open`
  command handler, open/issue/poll/apply plumbing, Enter action.
- `crates/jcode-tui/src/tui/app.rs` — `dir_browser_overlay`,
  `pending_dir_browser_load`, `pending_remote_dir_browse`,
  `remote_dir_browse_inflight` fields; `dir_browser_cmds` module.
- `crates/jcode-tui/src/tui/app/{tui_lifecycle,tui_state}.rs` — field
  init + `TuiState::dir_browser_overlay`.
- `crates/jcode-tui/src/tui/{mod,ui}.rs` — `pub mod dir_browser`, trait
  method, render arm.
- `crates/jcode-tui/src/tui/app/input.rs` — modal key routing
  (`handle_modal_key`, test-only path).
- `crates/jcode-tui/src/tui/app/remote/key_handling.rs` —
  `dir_browser_overlay` arm in `handle_remote_key_internal`, the
  production key path (the TUI always runs remote mode; an overlay left
  out of this chain renders but receives no keys — the picker shipped
  with exactly that gap and arrows fell through to prompt history).
- `crates/jcode-tui/src/tui/app/{local,remote}.rs` — tick polling and
  the remote pending/inflight queue; `handle_sideband_reply` arm.
- `crates/jcode-tui/src/tui/app/commands_dispatch.rs` — `/open` removed
  from `ssh_unsupported_command` (it now has a remote implementation),
  wired into `dispatch_ssh_local_command` and the local chain.
- `crates/jcode-tui/src/tui/app/helpers.rs` — `ssh_open_dir_args`,
  `spawn_dir_session_in_new_terminal`.
- `crates/jcode-tui/src/tui/backend.rs` — `RemoteConnection::browse_dir`.
- `crates/jcode-tui/src/tui/app/{input_help,state_ui_input_helpers}.rs`
  — `/help open` text and slash-completion registration.
- `src/cli/ssh.rs` — `validate` accepts `Command::Open` (rejecting
  `open`+`--resume`), `pick_remote_dir` drives the probe-bridge picker.
- `src/cli/ssh_transport.rs` — `RemoteBrowser` probe connection +
  id-matched `browse_dir` round-trip with timeout and stray-line cap.
- `src/cli/tui_launch.rs` — `run_open_picker` refactored into
  `run_open_picker_loop(start, remote, loader)` shared by local and SSH
  pickers.
- `src/cli/args.rs` — `Command::Open { path }`.
- `src/cli/dispatch.rs` — `jcode open` arm: picker → chdir →
  `run_default_command`.
- `src/cli/tui_launch.rs` — `run_open_picker` standalone loop.
- `src/cli/proctitle.rs` — `jcode open` title.

## Config surface

None — no new config keys.

## Upstream status

Upstream-candidacy note: the picker UI, `browse_dir` op, and `jcode
open` CLI are all general-purpose features that could be PR'd upstream;
the SSH bridge already carries a versioned handshake so adding an op is
backward-compatible on both sides (old bridges fail fast via
`sideband_ops`). The unusual Left/Right arrow mapping is spec-driven and
worth revisiting before any upstream submission.
