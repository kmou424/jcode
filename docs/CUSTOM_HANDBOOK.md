# kmou424 Fork Handbook

This binary is the kmou424 fork of jcode. The fork extends upstream
behavior with a numbered patch series — `[NN]` tags below refer to those
patches (visible as `[NN]` prefixes in commit subjects). This handbook is
the complete agent-facing reference: every fork-added config key, tool
surface, and behavioral difference, so you can self-configure
`config.toml` and use the fork's features correctly. Nothing outside
this document is required.

Fork source: https://github.com/kmou424/jcode (upstream:
https://github.com/1jehuang/jcode). When behavior details are needed
beyond this handbook, read the fork's source — patch commits are tagged
`[NN]` and `docs/custom/NN-*.md` holds per-patch requirement docs.

## Config quick reference

All keys live in `~/.jcode/config.toml` (or `$JCODE_HOME/config.toml`).

### `[providers.<name>]` — named providers ([01])

| Key | Type | Default | Notes |
|---|---|---|---|
| `display_name` | string | unset | UI label for the provider; replaces the profile key everywhere |
| `api` | string | unset | `"openai-responses"` selects the OpenAI Responses wire (`POST {base_url}/responses`); anything else keeps chat/completions |
| `models[].display_name` | string | unset | UI label for one model; replaces id prettification |
| `models[].reasoning` | bool | unset | Explicit `/effort` enable/disable for that model |
| `models[].reasoning_effort` | string | unset | Effort applied when this model becomes active (overrides `openai_reasoning_effort`) |
| `models[].experimentals` | string[] | `[]` | Per-model experimental feature tags — see tag table below |

`display_name` also reaches the model's system prompt via the
`provider/model(display_name)` identity line ([17]) and `${model}` in
git sign-off config ([16]).

### `[features]` — feature flags ([07], [12])

| Key | Type | Default | Notes |
|---|---|---|---|
| `ssh_login_import_offer` | bool | `true` | `false` suppresses the SSH-attach login-import offer entirely (no remote status probe). `/login --import-local` still works |

### `[[providers.<name>.models]] experimentals` tags ([12])

Tags are strings on the model entry; unknown tags warn and are ignored.
Tool-gating tags swap the session tool map when the model is active.

| Tag | Effect |
|---|---|
| `tool_apply_patch` | Freeform `apply_patch` (Lark-grammar `custom` tool on the Responses wire; degrades to an `input`-string function tool elsewhere). On GPT model ids the tool's result text is codex-equivalent ([18]) |
| `tool_apply_patch_compat` | JSON `apply_patch` variant with a `patch_text` param — mutually exclusive with `tool_apply_patch` |
| `tool_ask_user_question` | `ask_user_question` blocking interactive questionnaire tool ([14]) |

Any `tool_apply_patch*` tag removes `edit`, `multiedit`, `write`, and
`patch` from the session tool map while active (restored on model
switch). Setting both apply_patch tags is a configuration error.

### `[display]` — TUI display ([10], [11])

| Key | Type | Default | Notes |
|---|---|---|---|
| `reasoning_display` | string | `"full"` | `"compact"` (aliases `claude`, `cc`) renders Claude-Code-style collapsed thinking rows; `full` = upstream rows. Runtime toggles (`/thinking-display`, `/reasoning`, `/thinking off|full|current`) re-render history live ([11]) |

### `[tools.git]` — structured commit tools ([16])

| Key | Type | Default | Notes |
|---|---|---|---|
| `signoff_name` | string | unset | Co-Authored-By name; `${model}` = active model display name, `${user}` = `git config user.name` |
| `signoff_email` | string | unset | Same placeholders; both fields required for a trailer |

An unresolvable `${model}`/`${user}` is a hard error — the commit is not
signed with a placeholder. No signoff configured → no trailer.

### `[agents]` — memory sidecar ([22])

| Key | Type | Default | Notes |
|---|---|---|---|
| `memory_model` | string | unset | Bare OpenAI/Claude model id uses the dedicated HTTP backend; `<providers.name>:<model>` binds that named provider profile on a provider fork (main session undisturbed). Unknown prefixes warn and fall back to auto-select. Env: `JCODE_MEMORY_MODEL` |
| `memory_reasoning_effort` | string | unset | Reasoning effort for the memory sidecar. OpenAI backend → request `reasoning.effort`; provider spec → `set_reasoning_effort` on the fork; dedicated Claude path (haiku) ignores it. Env: `JCODE_MEMORY_REASONING_EFFORT` |

## Tools and behaviors

- `git_commit` / `git_checkpoint` ([16]): structured Conventional-Commits
  tool for agents. `git_commit` stages `paths`, refuses the default
  branch unless `allowDefaultBranch`, refuses foreign staged paths unless
  `includePreStaged`, and appends footers in separate blocks — issue
  references (`Closes #n`, `Refs: #n`) in one block, then the sign-off
  trailer in its own block.
- `ask_user_question` ([14], tag `tool_ask_user_question`): blocks the
  turn on a 1–4 question × 1–4 option questionnaire; answers return as
  the tool result. Options may carry `description`, `preview` (Markdown
  detail) and `recommended`; the client appends a free-text row; users
  may attach a per-question note (`n`).
- `apply_patch` variants ([13]): freeform and compat share one parse/exec
  engine; codex-style result text ([18]) applies only to the freeform
  variant under GPT-family model ids.
- `/open` directory browser ([21]): two-pane picker whose Enter launches
  a **new** jcode rooted at the chosen directory in a separate terminal
  (the current session never moves); Left enters a dir, Right goes to the
  parent. `jcode open [path]` runs the same picker standalone — Enter
  continues a normal launch at the pick in-process. Arrow keys are
  deliberate: Left = enter, Right = parent.
- `jcode_docs` bundles this handbook — search it for fork config keys.

## SSH / remote attach

- Fish login shells on the remote are supported ([02]) — remote shell
  detection picks the right `PATH` syntax.
- `jcode remote ssh <host>` attach shows the session picker and a remote
  slash-command subset (e.g. `/fork`, `/clear`) routed to the remote
  daemon rather than acting on local state ([03]).
- Slash-command availability over SSH ([03]) is three-layered: the remote
  command table (wire-backed commands incl. `/fast` and `/fast default`,
  which writes the remote `config.toml`), a blocklist for laptop-owned or
  remote-credential commands (`/usage`, `/stats`, `/accounts`, …), and
  client-local handlers (`/plan`, `/poke`, `/hotkeys`, `/terminal-setup`,
  display prefs, debug tooling). Unknown slash input falls through to
  remote skills, then plain prompt text.
- **Remote state travels on the sideband** ([03]): `jcode server stdio` is
  a line router — daemon protocol passes through untouched while
  `{"__jcode_ssh_op":{...}}` lines execute client ops locally on the remote
  host (`list_sessions`, `session_preview`, `set_session_saved`,
  `get_todos`, `read_config`/`write_config`, `read_file`/`write_file`,
  `list_path`, `browse_dir`, `list_skills`). The remote binary advertises
  `sideband_ops` in its handshake (`JCODE_SSH_OPS=1` client-side); an old
  remote makes ops fail fast instead of hanging.
- **Remote config is authoritative** ([03]): the `read_config` op seeds
  the remote `config.toml` as a process-wide override after each attach —
  every `config()` read (providers, features, display toggles, agent model
  overrides) follows the remote machine, and `ssh_remote_active()` keeps
  the laptop's `config.toml` unread even before the seed lands.
  Writes are not rejected: `Config::save()` serializes and queues a
  `write_config` op request, so `Config::set_*` ("save as default",
  `/alignment`, `/agents` model overrides, …) and `/config init` mutate
  the remote `config.toml`; `/config edit` opens the remote file in a
  local temp buffer via `read_config`/`write_config`. `/swarm-prompt`
  reads and writes the remote `.jcode/swarm-prompt.md` /
  `~/.jcode/swarm-prompt.md` via `read_file`/`write_file` — there is no
  local file access for either.
- `/btw`, `/fork`, `/split` over SSH fork the session on the remote
  daemon and open a new local window attached to the forked session;
  a `/btw` prompt is staged via a local handoff file that the new window
  submits over the wire ([03]).
- `/open` over SSH ([21]) lists remote directories through the
  `browse_dir` sideband op and Enter spawns a new terminal running
  `jcode --ssh <host>` with `JCODE_SSH_OPEN_DIR=<dir>` on its
  environment; standalone `jcode --ssh <host> open` instead picks
  **before** the workspace bridge exists (a probe bridge answers browse
  ops) and continues in-process anchored at the pick. `--ssh` alone
  anchors at the remote login HOME; `--resume` restores the session's
  stored remote dir regardless of where the bridge launched
  (`--remote-working-dir` is deprecated — still honored when passed,
  but nothing internal uses it).
- The SSH login-import offer is gated by `features.ssh_login_import_offer`
  ([07]) — set it on the machine running the TUI.
- The remote daemon reads **remote** `~/.jcode/config.toml` for
  agent-side config (providers, experimentals, tool gates).

## MCP servers (`~/.jcode/mcp.json`) ([09])

`mcp.json` adds HTTP transports alongside stdio. `"servers"` and
`"mcpServers"` are both accepted.

| Field | Values | Notes |
|---|---|---|
| `type` | `stdio`, `http`/`streamable-http`/`remote`/`auto`, `sse` | `url` without `type`/`command` resolves to HTTP; `sse` is the deprecated 2024-11-05 HTTP+SSE protocol |
| `url` | endpoint | Required for HTTP transports |
| `headers` | map | Values support `${VAR}`, `${VAR:-default}`, `!{cmd}` expansion (e.g. `"Authorization": "Bearer !{pass show mcp/x}"`) |
| `request_timeout_ms` | int | Per-request timeout; `timeout_secs` wins when both set. snake_case only |
| `direct` | bool | `true` (default) injects tools as direct `mcp__*`; `false` leaves them discoverable via `mcp_search`, callable on demand |

Not supported: `lifecycle` (jcode connects eagerly), OAuth flows,
`toolResultRendering`, `directTools` (use `direct`).

## Other fork surfaces

- `[06]` a session's `working_dir` is anchored at creation: a client
  attaching from a different directory no longer re-pins the session's
  project memory, MCP discovery, or swarm grouping to the client's cwd.
- `[08]` MCP stdio pool: connects are cancel-safe and a dead handle
  triggers reconnect instead of permanently wedging the server.
- `[15]` inside a herdr pane, the client reports lifecycle state
  (idle/working/blocked) so the pane shows accurate status.
- `[05]` `scripts/gc_incremental.py` precise incremental-cache GC
  (run after builds; `--apply` deletes dead work products only).
- `[04]` benchmark stabilization (test-only).
