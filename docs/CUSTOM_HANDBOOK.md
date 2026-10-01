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
| `type` | string | `"openai-compatible"` | `"anthropic-compatible"` binds the profile to an isolated Anthropic Messages runtime ([27]) |
| `api` | string | unset | `"openai-responses"` selects the OpenAI Responses wire (`POST {base_url}/responses`); anything else keeps chat/completions |
| `auth` | string | `"api-key"` | anthropic-compatible only: `api-key` sends `x-api-key`, `bearer` sends `Authorization: Bearer`, `none` sends no credential ([27]) |
| `model_catalog` | bool | `false` | anthropic-compatible only: fetch `GET {base_url}/models` and merge remote ids into the profile's model list ([27]) |
| `models[].display_name` | string | unset | UI label for one model; replaces id prettification |
| `models[].reasoning` | bool | unset | Explicit `/effort` enable/disable for that model |
| `models[].reasoning_effort` | string | unset | Effort applied when this model becomes active (overrides `openai_reasoning_effort`) |
| `models[].context_window` | integer | unset | Absolute per-model window override, scoped to this profile, ahead of catalog/static defaults ([29]) |
| `models[].compaction_threshold_tokens` | integer | unset | Soft local trigger or native OpenAI auto `compact_threshold`; does not reduce the real safety window ([29]) |
| `models[].experimentals` | string[] | `[]` | Per-model experimental feature tags — see tag table below |

An `anthropic-compatible` profile never falls back to `api.anthropic.com`,
`ANTHROPIC_API_KEY`, `auth.json`, or Claude labels/errors — its secret
comes only from `api_key`/`api_key_env`/`api_key_cmd`, resolved once at
bind time ([27]).

`display_name` also reaches the model's system prompt via the
`provider/model(display_name)` identity line ([16]) and `${model}` in
git sign-off config ([15]).

For `[29]`, use `id`, not `name`, in each `[[providers.<name>.models]]`.
Claude can use `context_window = 1_000_000` and
`compaction_threshold_tokens = 272_000` to summarize near 272K while
retaining 1M-window hard protection. Local reactive triggering uses the larger
of estimated and observed tokens; proactive/semantic modes keep their own
prediction and guard rules. The existing `[compaction].max_context_tokens`
cap still applies.

On a named `api = "openai-responses"` profile, a GPT model carrying
`compaction_threshold_tokens` uses server-side compression when
`[provider].openai_native_compaction_mode = "auto"`, sending
`context_management[].compact_threshold` and replaying the encrypted
compaction item. Unconfigured GPT models, non-GPT models, and
chat/completions keep their existing behavior. Thresholds have a 1,000-token
minimum and cannot exceed the channel's context budget.
Changes/removals are read from the current config before requests, not a
stale override cache. Same-named models in different profiles stay isolated.
No new aliases or `/config` preview lines are added.

### `[features]` — feature flags ([06], [11])

| Key | Type | Default | Notes |
|---|---|---|---|
| `ssh_login_import_offer` | bool | `true` | `false` suppresses the SSH-attach login-import offer entirely (no remote status probe). `/login --import-local` still works |

### `[[providers.<name>.models]] experimentals` tags ([11])

Tags are strings on the model entry; unknown tags warn and are ignored.
Tool-gating tags swap the session tool map when the model is active.

| Tag | Effect |
|---|---|
| `tool_apply_patch` | Freeform `apply_patch` (Lark-grammar `custom` tool on the Responses wire; degrades to an `input`-string function tool elsewhere). On GPT model ids the tool's result text is codex-equivalent ([17]) |
| `tool_apply_patch_compat` | JSON `apply_patch` variant with a `patch_text` param — mutually exclusive with `tool_apply_patch` |
| `tool_ask_user_question` | `ask_user_question` blocking interactive questionnaire tool ([13]) |

Any `tool_apply_patch*` tag removes `edit` and `write` from the session
tool map while active (restored on model switch). Upstream merged
`multiedit` into `edit` and made `patch` a name alias of `apply_patch`,
so they no longer exist as separate registry entries. Setting both
apply_patch tags is a configuration error.

### `[display]` — TUI display ([09], [10])

| Key | Type | Default | Notes |
|---|---|---|---|
| `reasoning_display` | string | `"full"` | `"compact"` (aliases `claude`, `cc`) renders Claude-Code-style collapsed thinking rows; `full` = upstream rows. Runtime toggles (`/thinking-display`, `/reasoning`, `/thinking off|full|current`) re-render history live ([10]) |

### `[tools.git]` — structured commit tools ([15])

| Key | Type | Default | Notes |
|---|---|---|---|
| `signoff_name` | string | unset | Co-Authored-By name; `${model}` = active model display name, `${user}` = `git config user.name` |
| `signoff_email` | string | unset | Same placeholders; both fields required for a trailer |

An unresolvable `${model}`/`${user}` is a hard error — the commit is not
signed with a placeholder. No signoff configured → no trailer.

### `[agents]` — memory sidecar ([21])

| Key | Type | Default | Notes |
|---|---|---|---|
| `memory_model` | string | unset | Bare OpenAI/Claude model id uses the dedicated HTTP backend; `<providers.name>:<model>` binds that named provider profile on a provider fork (main session undisturbed). Unknown prefixes warn and fall back to auto-select. Env: `JCODE_MEMORY_MODEL` |
| `memory_reasoning_effort` | string | unset | Reasoning effort for the memory sidecar. OpenAI backend → request `reasoning.effort`; provider spec → `set_reasoning_effort` on the fork; dedicated Claude path (haiku) ignores it. Env: `JCODE_MEMORY_REASONING_EFFORT` |

### Secret values — `!{cmd}` substitution ([22])

Any secret-typed value may hold a whole-value `!{command}` that runs via
`sh -c` (10s timeout) and yields its trimmed stdout — e.g.
`api_key = "!{pass show keys/anthropic}"`. Covered surfaces:

- TOML secrets: `email_password`, `telegram_bot_token`,
  `discord_bot_token`, `jade_relay_token`/`_id`, `websearch.bing_api_key`,
  named-provider `api_key`, `memory.sync.access_key`/`secret_key` ([25]).
- Env-file `KEY=value` lines and process env vars read through the
  provider-env loaders — including `api_key_env`-indirect keys.
- MCP header values keep their existing inline `!{}` semantics ([08]).

The literal is never used as a credential: a failed/empty substitution
collapses to `None` ("not configured") with a stderr-bearing warning.
Results are memoized per command for the process lifetime;
`Config::save` clears the cache so saving re-reads secrets.

### `[memory.sync]` — S3-compatible memory sync ([25])

Syncs the global and per-project memory graphs across machines via an
S3-compatible bucket (Garage, MinIO, AWS, R2). Local files stay the source
of truth; a daemon-side engine pushes diffs within seconds and pulls
remote changes on the pull interval. Per-project stores are keyed by a
repo-stable id (`proj-<hash>` of the normalized git remote, falling back
to the legacy `path-<hash>`); `jcode memory project` prints it and
`jcode memory project set <id>` pins it via `.jcode/memory-project-id`.
`jcode memory sync` runs one full push+pull+reconcile cycle on demand.

| Key | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `false` | Master switch; daemon loop only runs when enabled AND backend is `s3` |
| `backend` | string | `"s3"` | Only `"s3"` exists today |
| `endpoint` | string | `""` | http(s) URL of the S3 endpoint — required |
| `region` | string | `"us-east-1"` | Garage often uses `garage`; empty also falls back to `us-east-1` |
| `bucket` | string | `""` | Required |
| `prefix` | string | `""` | Key prefix inside the bucket |
| `path_style` | bool | `true` | path-style (`/bucket/key`) vs virtual-host; Garage/MinIO need `true` |
| `access_key` | string | `""` | Supports `!{cmd}` secrets ([22]) |
| `secret_key` | string | `""` | Supports `!{cmd}` secrets ([22]) |
| `scopes` | string[] | `["global", "project"]` | Which memory scopes sync |
| `push_poll_secs` | u64 | `10` | Daemon tick: local dirty-check cadence (mtime stat, no network) |
| `pull_interval_secs` | u64 | `120` | How often pull LIST ops run per target |
| `reconcile_interval_secs` | u64 | `3600` | How often the `entries/` key-set reconcile + GC runs |
| `pull_on_start` | bool | `true` | Run one sync cycle when the daemon starts |
| `tombstone_retention_days` | u64 | `90` | Local tombstone retention; also the `ops/` journal GC horizon |

Conflict rule: LWW on `updated_at` with `client_id` tiebreak; deletes are
tombstones, never DELETEs. Traffic: idle <1 MB/day at defaults; active
traffic is proportional to real diffs (a per-target remote snapshot
skips unchanged bodies).

## Tools and behaviors

- `git_commit` / `git_checkpoint` ([15]): structured Conventional-Commits
  tool for agents. `git_commit` stages `paths`, refuses the default
  branch unless `allowDefaultBranch`, refuses foreign staged paths unless
  `includePreStaged`, and appends footers in separate blocks — issue
  references (`Closes #n`, `Refs: #n`) in one block, then the sign-off
  trailer in its own block.
- `ask_user_question` ([13], tag `tool_ask_user_question`): blocks the
  turn on a 1–4 question × 1–4 option questionnaire; answers return as
  the tool result. Options may carry `description`, `preview` (Markdown
  detail) and `recommended`; the client appends a free-text row; users
  may attach a per-question note (`n`).
- `apply_patch` variants ([12]): freeform and compat share one parse/exec
  engine; codex-style result text ([17]) applies only to the freeform
  variant under GPT-family model ids.
- `/open` directory browser ([20]): two-pane picker whose Enter launches
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
- `/open` over SSH ([20]) lists remote directories through the
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
  ([06]) — set it on the machine running the TUI.
- The remote daemon reads **remote** `~/.jcode/config.toml` for
  agent-side config (providers, experimentals, tool gates).

## MCP servers (`~/.jcode/mcp.json`) ([08])

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

- `[05]` a session's `working_dir` is anchored at creation: a client
  attaching from a different directory no longer re-pins the session's
  project memory, MCP discovery, or swarm grouping to the client's cwd.
- `[07]` MCP stdio pool: connects are cancel-safe and a dead handle
  triggers reconnect instead of permanently wedging the server.
- `[14]` inside a herdr pane, the client reports lifecycle state
  (idle/working/blocked) so the pane shows accurate status.
- `[23]` fork self-update + musl static assets. `jcode update` and
  `install.sh`/`install.ps1` point at `kmou424/jcode`; the updater picks
  the max strict-semver tag from `/releases`, and also updates when a
  same-tag release was repackaged on a different commit (embedded
  `GIT_HASH` vs `target_commitish`). Development builds compare the
  embedded hash against the release's `release-info.json` `commit`
  (the packaged commit), so both a newer version line and a repackaged
  same-tag release are detected; a diverged compiled commit is allowed
  only when it is reachable from a remote ref. Source-build (`main`) channel
  tracks `kmou424/v<PKG_VERSION>` (the version's patch branch).
  `scripts/install.sh` auto-selects the musl asset when
  `/etc/NIXOS` exists or the arch's ld-linux interp is absent. Local
  publish: `scripts/package_fork_release.sh v<tag>` (worktree build:
  host gnu + both musl arches, SHA256SUMS, tag ref moved + release
  recreated); `scripts/build_musl.sh <target>` runs a single musl
  build natively via musl-gcc when a musl toolchain is installed
  (`rust:alpine` docker fallback otherwise). Version is baked via
  `JCODE_BUILD_SEMVER` at compile time.
- `[24]` `selfdev build` auto-targets musl on NixOS (`/etc/NIXOS` →
  `<arch>-unknown-linux-musl`, `linux-compat-vendored-openssl`, musl CC
  detection incl. nixpkgs `<triple>-gcc`); the binary lands in
  `target/<triple>/selfdev/`. `JCODE_SELFDEV_TARGET` overrides (empty =
  host default). `flake.nix` devShell: `nix develop` gives rustc+cargo
  with musl std plus the musl cross gcc.
- `[04]` `scripts/gc_incremental.py` precise incremental-cache GC
  (run after builds; `--apply` deletes dead work products only).
- `[04]` benchmark stabilization (test-only).
- `[26]` `/model` picker hides channels with no usable credentials: a
  channel (`api_method`) with every route `available: false` is pruned,
  removing dead provider options and models reachable only through them.
  Placeholder rows, the current model, fully-dead catalogs, and
  synthesized names-only fallback catalogs are exempt.
