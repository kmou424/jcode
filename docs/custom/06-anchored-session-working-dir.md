# [06] Session project directory is anchored at creation and immutable

## Requirement

A session's `working_dir` is its project identity: memory buckets, goals,
AGENTS.md snapshots, env snapshots, swarm grouping, and project-local MCP
config all key off it. Before this patch, a remote client that attached
(`subscribe`/`resume`) from a different directory overwrote
`Session.working_dir` with its own cwd — silently re-bucketing the
session's project memory and pointing MCP discovery at the wrong tree.
The existing home-dir guard (issue #481) only rejected reports equal to
`$HOME`; any other divergent cwd still re-pinned the session.

Observed symptom: a session created in project A, resumed over SSH from a
client sitting in directory B, had its memory bucket moved to B — project
memories written under A became invisible.

An intermediate revision of this patch recorded divergent reports in a
`client_working_dir` field; the field had no consumer and the user asked
for strict semantics instead: the anchor is written once and nothing may
rewrite it, so divergent reports are dropped entirely rather than stored.

## Behavior

- `Session.working_dir` is the **anchored project directory**. It is
  bound once — at session creation (new/fork/split/crash-recovery/
  ambient/comm/import session constructors all pass the parent's or the
  requested dir) or, for sessions that somehow reach first contact
  unanchored (legacy persisted files), at the first subscribe/restore —
  and is **immutable afterwards by any mechanism**: subscribe reports,
  resume reports, MCP attach, `/clear`, nothing.
- `Session::anchor_working_dir_if_unset(&str)` is the single entry point
  for client-reported dirs:
  - empty/blank report → no-op;
  - session has no anchor → report becomes the anchor, returns true;
  - session already anchored → report dropped, returns false (a report
    equal to the anchor is just a no-op drop).
- Anchoring an unbound session refreshes the AGENTS.md snapshot, the
  initial session-context message, and logs an env snapshot
  (`Agent::anchor_working_dir_if_unset`, env label `working_dir_anchor`).
- A divergent report is logged at debug level
  ("keeps anchored working dir …; dropped divergent client cwd …").
- Subscribe path: `apply_or_defer_subscribe_working_dir` calls
  `apply_subscribe_working_dir` (immediate or deferred under the agent
  lock). `effective_subscribe_working_dir` resolves anchor-first.
- Project-local MCP resolution (`mcp_working_dir`) prefers the anchored
  dir over the subscribe report in both `handle_subscribe` and
  `handle_resume_session`; the report is only a fallback for unanchored
  sessions.
- `restore_session_with_working_dir` (remote resume) routes through
  `anchor_working_dir_if_unset` instead of overwriting. `/clear`
  constructs the replacement agent with the preserved anchor; no client
  dir is carried.
- `resolve_target_subscribe_working_dir` (server, reattachment path)
  resolves the session's **stored** dir and overwrites the client's
  reported `working_dir` with it — a reconnect launched from a different
  directory (e.g. an SSH bridge at login HOME) cannot even reach
  provisional init with the wrong dir. Only a session with no stored dir
  at all keeps the client report; an unknown session with no report is
  rejected.
- `Agent::set_working_dir` is now `#[cfg(test)]` — production has no
  post-construction rewrite path left.
- The client-side SSH placeholder session still clears its stub's
  `working_dir` in `tui_lifecycle.rs` so a stale local path is not
  mistaken for the remote project; that write only touches the
  client-local stub before first subscribe, never the server anchor.
- `ServerEvent::SessionId` carries the anchored dir on the wire (optional
  `working_dir` field, omitted when absent so older peers still parse).
  The server emits it at every bind point — subscribe/attach and
  `/clear` — so a remote client learns the real session anchor without
  reading a local session file.
- On a successful attach/resume the client records the anchor into the
  client-side stub `app.session.working_dir` (header, `/open` start,
  session picker). A **local** client additionally re-pins its process
  cwd to the anchor (`std::env::set_current_dir`) so relative-path tools
  and shells stay consistent; when an older server omits the wire field
  it falls back to `Session::load(&session_id).working_dir` from the
  local store. Over SSH (`is_ssh_remote()`) the anchor is a remote path —
  the stub write happens but the local process never chdirs.
- SSH subscribe reports the handshake-resolved remote workspace
  (`JCODE_SSH_WORKING_DIR`, exported by `src/cli/ssh.rs`) instead of the
  laptop cwd or a bare `None`: a fresh `--ssh` session anchors at the
  remote login HOME or the `/open` pick, while a `--resume` still lands
  on the session's stored dir because the server prefers it.

## Config surface

CLI flags only.

- `-C`/`--cwd` is kept: it is the client-process launch directory and a
  new session anchors to it. When resuming it is ignored — the process
  cwd switches to the resumed session's anchored directory instead.
- `--remote-working-dir` is **deprecated** (help text carries a
  `DEPRECATED:` prefix; `validate_remote_working_dir` prints a stderr
  warning on use). It still wins when passed — it overrides the remote
  launch dir for new sessions over `--socket`/`--ssh` — but nothing
  internal depends on it: the resume hint no longer emits it, `/open`'s
  spawned terminals carry the picked dir in `JCODE_SSH_OPEN_DIR` on the
  child environment instead, and subscribe reports the handshake dir via
  `JCODE_SSH_WORKING_DIR`. Do not remove the flag yet.
- `JCODE_SSH_OPEN_DIR` (env, input only): a remote directory request
  consumed by `src/cli/ssh.rs` — it seeds the workspace `--cwd` and the
  `open` picker's start. Set by `/open`-spawned terminals; not exported
  by the SSH entry path itself.

The removed `client_working_dir` serde key is ignored on deserialize,
so old session files/journals load cleanly.

## Affected files

- `crates/jcode-base/src/session.rs` — dropped `client_working_dir`,
  `anchor_working_dir_if_unset`, plumbing through `SessionStartupStub`,
  `RemoteStartupSessionSnapshot`, `session_from_*`, `apply_journal_meta`,
  `journal_meta`.
- `crates/jcode-base/src/session/journal.rs`,
  `crates/jcode-base/src/session/crash.rs` — dropped journal-meta field
  and crash-recovery copy.
- `crates/jcode-app-core/src/agent.rs` — `Agent::anchor_working_dir_if_unset`;
  deleted `client_working_dir()` getter and
  `set_working_dir_for_pending_context`.
- `crates/jcode-app-core/src/agent/provider.rs` — `set_working_dir`
  gated to `#[cfg(test)]`.
- `crates/jcode-app-core/src/agent/turn_execution.rs` — restore path and
  `/clear` no longer carry a client dir.
- `crates/jcode-app-core/src/ambient/runner.rs` — dropped redundant
  post-construction pin (child session already anchors at construction).
- `crates/jcode-app-core/src/server/client_session.rs` — subscribe
  rewrite (`apply_subscribe_working_dir`, anchor-first
  `effective_subscribe_working_dir`, deleted
  `subscribe_working_dir_replacement`), `/clear`, swarm `bound_dir`,
  `mcp_working_dir` sites; both `SessionId` emit sites send the
  resolved anchor as `working_dir`.
- `crates/jcode-app-core/src/server/client_lifecycle.rs` —
  `resolve_target_subscribe_working_dir` prefers the session's stored
  dir over the client report on reattachment.
- `crates/jcode-protocol/src/wire.rs` — `ServerEvent::SessionId`
  gains an optional `working_dir` field.
- `crates/jcode-tui/src/tui/app/remote/server_events.rs` — `SessionId`
  handler records the wire anchor into `app.session.working_dir`;
  local clients also re-pin the process cwd (local-store fallback when
  an older server omits the field), SSH clients never chdir.
- `crates/jcode-tui/src/tui/mod.rs` — `subscribe_metadata` SSH branch
  reports `JCODE_SSH_WORKING_DIR` (the handshake workspace).
- `src/cli/ssh.rs` — `run_unix` resolves the requested remote dir as
  flag → `JCODE_SSH_OPEN_DIR` → handshake default; the resume hint no
  longer emits `--remote-working-dir`.
- `src/cli/args.rs`, `src/cli/startup.rs` — `-C` help text,
  `--remote-working-dir` deprecation notice + stderr warning.
- Tests: `session_tests/cases.rs` (`anchor_working_dir_if_unset_*`),
  `client_session_tests.rs`, `agent_tests.rs`, `comm_session_tests.rs`,
  `desktop_selfdev.rs`.

## Upstream status

General fix, upstream candidate. The divergent-cwd re-bucketing bug is
not fork-specific — upstream's subscribe path still prefers the client
report (including for MCP resolution, which this patch also corrects to
anchor-first). If upstream lands an equivalent fix with its own field
naming, this patch collapses to the strict no-`client_working_dir`
semantics.

Note: while landing this, several `agent_tests` needed explicit titles
because upstream `783c979a0` ("avoid persisting untouched sessions") now
skips writing title-less sessions, breaking save→restore test fixtures.
The added titles are test scaffolding, not a behavioral fix.
