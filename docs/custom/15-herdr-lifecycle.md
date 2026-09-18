# [15] Agent lifecycle reporting to herdr

## What it does

When jcode runs inside a herdr pane, every session-status transition is
reported to the multiplexer with
`herdr pane report-agent --source custom:jcode --agent jcode --state <state> <PANE_ID>`,
so herdr shows the agent's real state instead of guessing from terminal
output.

## Status mapping

| jcode status                       | herdr state |
|------------------------------------|-------------|
| session create/attach/resume       | idle        |
| turn start (processing/streaming)  | working     |
| turn end (any outcome)             | idle        |
| `SessionStatus::AwaitingUser` [14] | blocked     |
| answer received, turn resumes      | working     |
| orphaned answer on a dead turn     | idle        |
| session end/close                  | idle        |

herdr's `report-agent` enum is `idle|working|blocked|unknown` — there is no
"done"/"stopped" verb. A session blocked on `ask_user_question` is alive
and resumable, so it reports `blocked`, never a stopped-class state.

## Design

- `crates/jcode-app-core/src/herdr.rs`: `AgentState` + `resolve_target` +
  fire-and-forget `Command::spawn` (stdio null, spawn failure is
  log-only, never propagates).
- Env gate: `HERDR_ENV=1` AND `HERDR_PANE_ID`, with `HERDR_BIN_PATH`
  override for the binary. No config surface.
- The pane identity problem on a shared daemon: the pane vars belong to
  the *client*, not the server process. Clients already snapshot their
  `HERDR_*` env at connect (`terminal_launch::CLIENT_TERMINAL_ENV_VARS`)
  and request handlers run inside `hooks::with_client_terminal_env`; the
  new `hooks::client_terminal_env_value(key)` reads from that scope (an
  empty client snapshot is authoritative — it never falls back to the
  daemon's env), falling back to the process env only outside server
  request scope (in-process `jcode run`).
- Report sites: `agent.rs`/`agent/turn_execution.rs` (turn start/end),
  `server/ask_user_question.rs` + `tool/ask_user_question.rs`
  (blocked/working/orphaned-idle around the [14] flow).

## Exit release

`report-agent` registrations are sticky: nothing in the
`idle|working|blocked` enum removes the pane entry, so without an explicit
release herdr keeps listing a dead jcode forever. The removal verb is
`herdr pane release-agent --source custom:jcode --agent jcode <PANE_ID>`.

- `herdr::release_if_registered()` runs when the pane-hosting process
  leaves: the `cli::startup::run` dispatch return plus the direct
  `process::exit` sites that bypass it (`tui_launch.rs`, the unix
  `handle_termination_signal` in `terminal.rs`, and the debug-client
  orphan guard in `dispatch.rs`). This covers local, connect and SSH
  clients uniformly (the TUI process is always the one inside the pane)
  and `jcode run` the same way.
- A process-global `REGISTERED` flag (set by any successful report spawn)
  gates the release, so a transient `jcode status`/`jcode doctor` typed
  into a pane that hosts a *different* live jcode does not strip its agent.
- Session `close` still reports `idle`, not release: closing one session
  while the TUI stays alive leaves a live agent in the pane. Release means
  "the process that owned this pane's agent is gone".
- Uncatchable exits (SIGKILL, panic abort, power loss) cannot run release;
  the stale entry clears on pane close or on the next registration in that
  pane. Remote-side cleanup cannot help here either: on SSH the remote
  `herdr` spawn is a no-op, and daemon-side disconnect cleanup runs outside
  the client env scope so it resolves no pane at all.

## SSH sessions (`jcode --ssh`)

Server-side reporting cannot reach herdr over SSH: the server runs on the
remote host, where neither the `herdr` binary nor the local
`HERDR_PANE_ID` exists (the spawn is log-only, so the pane silently shows
nothing). The TUI process is the one inside the pane and owns the real
`HERDR_*` env, so `jcode-tui` re-derives the same lifecycle client-side:

- `tui/app/herdr_report.rs::observe` runs after every `handle_server_event`
  and maps client state onto the same table: open ask_user_question panel →
  `blocked` (with the same `awaiting answer` message), `is_processing` →
  `working`, otherwise `idle` (reusing `jcode_app_core::herdr` entry points,
  which resolve `HERDR_*` from the process env outside server request scope).
- Emission is edge-triggered through `App::herdr_reported`
  (last state + session), so interrupts, reload recovery, replay and turn
  adoption all report correctly without per-event wiring. A
  `remote_session_id` change re-publishes `report-agent-session` and
  re-asserts the state, matching the server-side attach report.
- The path is gated on `is_ssh_remote()` only: local `jcode` reports
  server-side already, and `jcode connect` forwards the client's `HERDR_*`
  snapshot to the daemon, so client-side reporting there would
  double-report.

## Affected files

- `crates/jcode-app-core/src/herdr.rs`, `tests/herdr.rs` (new)
- `crates/jcode-app-core/src/agent.rs`, `agent/turn_execution.rs`,
  `server/ask_user_question.rs`, `tool/ask_user_question.rs`, `lib.rs`
- `crates/jcode-base/src/hooks.rs` (`client_terminal_env_value`)
- `crates/jcode-tui/src/tui/app/herdr_report.rs` (new),
  `tui/app/remote/server_events.rs` (observe wrapper), `tui/app.rs` +
  `tui/app/tui_lifecycle.rs` (`herdr_reported` field),
  `tui/app/tests/herdr_ssh.rs`
- `src/cli/startup.rs`, `src/cli/tui_launch.rs`, `src/cli/terminal.rs`,
  `src/cli/dispatch.rs` (exit-path `release_if_registered` calls)

## Upstream status

Fork-local integration (depends on an external multiplexer); not an
upstream candidate unless herdr reporting becomes a plugin hook.
