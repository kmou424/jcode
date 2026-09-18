# [03] SSH remote parity via stdio bridge

## Requirement

Over SSH (`jcode --ssh`), the TUI client must behave as if it were running on
the remote host: the session picker, `/save`/`/unsave`, `/todos`, `/skills`,
`/config` (read/edit/init), `/swarm-prompt`, `@` path completion, the active
sessions manager and `/catchup` all operate on remote state.

The iron rule is strict: an SSH-mode client process performs **no functional
file access on the laptop**. Everything it needs that lives on disk must come
from the remote host. Exemptions, all non-functional state:

- auth/connection material (SSH keys, `known_hosts`, `ssh_remotes.json`, the
  adapter's private unix socket);
- scratch staging that never becomes state (`$EDITOR` temp files for
  `/config edit`/`/swarm-prompt`, clipboard-image staging);
- logs, and client-side UI state that has no remote meaning (catchup "seen"
  markers, in-memory prompt history — the persisted `prompt-history.jsonl`
  path is disabled under SSH, see `history_file_path`).

Session files, `config.toml`, skill registries, todo state, project files and
prompt sources are remote state and travel over the wire.

## Why a stdio bridge, not protocol extension

An earlier revision extended `jcode-protocol` (`Request::ListSessions`,
`ServerEvent::SessionList`, `History.remote_config`, …) plus daemon dispatch
in `jcode-app-core::server`. It worked, but every variant lived inside
upstream enums and server plumbing — the worst possible upstream merge
surface for a fork-local feature.

This patch instead makes `jcode server stdio` a **line-aware router**:

- daemon protocol traffic passes through to the daemon socket untouched;
- a private **sideband** carries client operations the bridge executes
  locally on the remote host, using the same jcode-base APIs a local client
  would use.

Upstream `jcode-protocol` and the daemon's request dispatch carry **none** of
this. The merge surface is: fork-private `ssh_ops.rs`, the framed router in
`ssh_transport.rs::bridge_stream`, a `sideband_ops` field on the (already
fork-owned) `NativeHandshake`, and TUI consumption points that are already
SSH-gated. `Request::Reload`, `continue_on_disconnect` and daemon-owned
sessions are upstream-native and pass through uninterpreted, so lifecycle
(including reload) is preserved for free.

## Wire format

One JSON line per message on the same stdio stream, distinguished by a
single envelope key:

```
client → bridge   {"__jcode_ssh_op":{"id":N,"op":"<name>",...params}}
bridge → client   {"__jcode_ssh_op":{"id":N,"result":{...}}}
                  {"__jcode_ssh_op":{"id":N,"error":"..."}}
```

`ssh_ops::OP_LINE_PREFIX` (`{"__jcode_ssh_op":`) is the cheap classifier on
both ends; anything else is daemon protocol. Daemon `ServerEvent`s are
`"type"`-tagged and can never collide with the envelope.

Ids come from the client's normal `next_request_id` counter — they share the
space with daemon request ids without ambiguity because op replies have no
`type` field and daemon replies never carry `result`/`error` envelopes.

## Bridge routing (`src/cli/ssh_transport.rs`)

`bridge_stream` after the handshake runs three pieces:

- **upload**: each stdin line is classified by `is_op_line`. Op lines decode
  and run `ssh_ops::execute` inside `spawn_blocking` (fs work must not stall
  the pass-through pump); other lines write to the daemon socket verbatim.
- **download**: daemon lines forward verbatim. Each line is also fed to
  `snoop_live_sessions`, which lifts `all_sessions` out of `History` events
  so `list_sessions` can mark live-attached rows.
- **single writer**: op replies go back over an unbounded channel that the
  download loop `select!`s on alongside daemon reads — stdout has exactly
  one writer, so reply lines can never interleave mid-frame.

Stdin EOF still half-closes the daemon socket and drains the last replies
(unchanged pipeline semantics).

Capability negotiation: the bridge's `NativeHandshake` gains
`sideband_ops: bool` (`#[serde(default)]` — absent means false). The local
adapter stores it from the first connection's handshake and `ssh run`
exports `JCODE_SSH_OPS=1`; `crate::tui::ssh_ops_supported()` gates every op
send, so attaching to an older remote binary degrades to a fast, explicit
failure instead of a hung request. The remote bridge itself always
advertises true once it carries this patch.

## Op table (`jcode-app-core::ssh_ops`)

| Op | Result | Backs |
|---|---|---|
| `list_sessions` | `Vec<SshSessionEntry>` | remote `/resume`, `/sessions`, `/active`, `/catchup` |
| `session_preview{session_id}` | `session_id + Vec<SshPreviewMessage>` | picker preview pane prefetch |
| `set_session_saved{session_id,saved,save_label}` | ack | `/save`, `/unsave` on remote rows |
| `get_todos{session_id}` | `todos + goals + plan` | `/todos` inline card |
| `read_config` | `path + content` (empty when absent) | config seed + `/config edit` |
| `write_config{content}` | ack (validates TOML first) | all remote config writes |
| `read_file{path}` | `path + content` (NotFound → empty) | `/swarm-prompt` probes |
| `write_file{path,content}` | ack (creates parents) | `/swarm-prompt` saves |
| `list_path{prefix}` | `Vec<String>` | `@` path completion (surface pending) |
| `list_skills` | `Vec<SshSkillInfo>` | `/skills` metadata rows |

Handlers run in the bridge process — i.e. on the remote host — against
`jcode-base` (`session` summaries + `render_messages`, `config`, `skill`,
task store). `~` and relative paths expand remote-side via
`resolve_remote_path` against the bridge's working dir (from the handshake).

`SshSessionEntry.live_attached` is filled from the snooped `all_sessions`;
`live_processing` is daemon-memory-only and unreachable from the bridge, so
picker busy badges degrade to attach-presence (accepted limitation).

## Client side (`jcode-tui`)

- `RemoteConnection::classify_protocol_line` intercepts `is_op_line` before
  `ServerEvent` parsing and surfaces `RemoteRead::OpReply(SshOpResponse)`.
  Malformed op lines consume the same stray-line budget as corrupt events.
- `RemoteConnection::{list_sessions, session_preview, set_session_saved,
  get_todos, list_skills, read_config, write_config, read_file, write_file,
  list_path}` keep their `Result<u64>` shape but serialize
  `SshOpRequest` envelopes via `send_sideband_op`, which fails fast when
  `ssh_ops_supported()` is false.
- `remote.rs::handle_sideband_reply` routes each `SshOpResult` to the same
  consumers the old daemon events fed (`apply_remote_session_list`,
  `apply_remote_session_preview`, `apply_remote_todos`,
  `remote_files::{handle_file_content, handle_path_candidates,
  handle_remote_file_error}`), and `ListSkills` rows land in
  `app.remote_skill_infos` (`Vec<SshSkillInfo>`).
- On every `History` in SSH mode the TUI sets `pending_remote_config_seed`
  and `pending_remote_skill_infos`; the remote poll issues `read_config` and
  `list_skills`. The `read_config` reply installs the process-wide
  `REMOTE_CONFIG_OVERRIDE` (`Config::from_str`, `Config::default()` when the
  remote file is absent), replacing the old `History.remote_config` seed.
- Slash-command routing for a connected session runs
  `remote::key_handling`'s remote command table first, then
  `commands_dispatch::handle_ssh_unsupported_command` (laptop-owned or
  remote-credential commands), then `dispatch_ssh_local_command`
  (client-local rendering/tooling), then remote skills, then plain prompt
  text. After auditing every registered command: `/fast` (including
  `/fast default`, which writes the remote `config.toml` via `write_config`
  and calls `remote.set_service_tier`), `/improve`, `/refactor`,
  `/workspace`, `/refresh-model-list`, `/context`, `/info`, and the debug
  tooling (`/screenshot`, `/record`, `/debug-visual`) work remotely;
  `/plan`, `/poke`, `/hotkeys`, and `/terminal-setup` run client-side
  (prompts go over the wire); `/usage`, `/model-status`, `/stats`, auth and
  subscription commands stay blocked because they read laptop files or
  remote credentials with no sideband op.

## Choke points (iron rule enforcement)

`jcode-base::config` is the only config choke point, so the gates live at
the file-touching entry points, keyed on `ssh_remote_active()`
(`JCODE_SSH_REMOTE` env) — they hold even in the gap between attach and the
`read_config` seed:

- `config()` → override when seeded, else a static `Config::default()` —
  never the local file.
- `Config::load_from_file_strict` → override clone / `None` (no local read).
- `Config::load_for_update` → override clone / `Self::default()`.
- `Config::save` / `create_default_config_file` → queue the serialized TOML
  via `queue_remote_config_write`; the remote poll drains it as
  `write_config` ops. Pre-seed saves do not install the override (the seed
  owns that slot).
- TUI skill snapshot: `App::new` uses an empty registry under SSH instead of
  `SkillRegistry::shared_snapshot()` (which scans `~/.jcode/skills`);
  `refresh_skills_snapshot` and `current_skills_snapshot` were already
  SSH-gated; `/skills` renders `remote_skill_infos`/`remote_skills`.
- Skill names reach `/` candidates from the `History.skills` seed; for
  sessions replayed from `send_history_from_persisted_session` (fresh or
  busy agents) the server now composes the same effective registry list
  (global + project overlay for the session working dir) that
  `available_skill_names` and the `list_skills` sideband op return —
  previously it sent an empty `skills` field and every skill stayed
  hidden from `/` until the next History.
  The `list_skills` sideband reply is the second name source: its arm
  unions `info.name` into `remote_skills`, and the `History.skills`
  assignment is guarded so an empty History list (every remote whose
  binary predates the persisted-session seed) cannot erase names the
  sideband already supplied. Without this a fresh connect to an older
  remote showed zero `/` skill candidates despite a full `list_skills`
  reply.
- Skill descriptions: `App::skill_description_for_display` resolves a
  name to the SSH `remote_skill_infos` metadata (or the live registry
  snapshot locally); `/` candidates and the `/help` overlay show it
  instead of the static "Activate skill". Candidates carry owned
  `String` descriptions since the text is now dynamic.
- Session picker / prompt history / paste path handling were already
  SSH-gated (`is_ssh_remote()` early returns).

## Reload

`Request::Reload` is upstream-native daemon protocol — it passes through
the bridge uninterpreted. The daemon's reload handoff and the client's
existing reconnect loop work unchanged: each reconnect spawns a fresh SSH
child → fresh bridge → fresh handshake (`sideband_ops` re-advertised) →
fresh `History` → re-seeded config/skills. The bridge itself is the same
`jcode server stdio` entry point, so an updated remote binary reloads it
through the ordinary daemon path.

## Limitations / degradations

- `live_processing` (busy badge) is unreachable from the bridge; the
  Active-sessions filter still works off `live_attached`.
- `list_path` is wired end to end but no composer surface consumes it yet.
- Old remote binaries lack `sideband_ops`: every op caller fails fast with
  "remote bridge does not support sideband client ops" instead of hanging.
- Op ids share `next_request_id` with daemon requests; `control_done_ids`
  only ever contains daemon-issued ids, so a `Done` can never collide with
  an outstanding op.

## Affected files

- `crates/jcode-app-core/src/ssh_ops.rs` — new, fork-private protocol +
  handlers.
- `src/cli/ssh_transport.rs` — `NativeHandshake.sideband_ops`, framed
  `bridge_stream` router, `line_feed` test helper.
- `src/cli/ssh.rs` — export `JCODE_SSH_OPS` from the adapter handshake.
- `crates/jcode-tui/src/tui/backend.rs` — `RemoteRead::OpReply`, op-line
  intercept, `send_sideband_op`, converted op methods.
- `crates/jcode-tui/src/tui/mod.rs` — `ssh_ops_supported()`.
- `crates/jcode-tui/src/tui/app/remote.rs` — OpReply dispatch arm +
  `handle_sideband_reply`, `pending_remote_skill_infos` /
  `pending_remote_config_seed` drains.
- `crates/jcode-tui/src/tui/app/remote/server_events.rs` — History arm now
  queues seeds instead of reading `remote_config`/`skill_infos`; removed
  dead event arms.
- `crates/jcode-tui/src/tui/app/remote/remote_files.rs` — seed routing in
  `handle_file_content`/`handle_remote_file_error`.
- `crates/jcode-tui/src/tui/app/inline_interactive.rs`,
  `session_picker/loading.rs`, `app.rs`, `tui_lifecycle.rs` — wire types
  swapped for `ssh_ops` types; empty skill registry under SSH.
- `crates/jcode-app-core/src/server/client_state.rs` —
  `send_history_from_persisted_session` seeds the effective skill list.
- `crates/jcode-tui/src/tui/app/state_ui_runtime.rs` —
  `skill_description_for_display`; `tui_state.rs`, `ui_input.rs`,
  `state_ui_input_helpers.rs`, `ui_overlays.rs`, `ui_tests/mod.rs` —
  `/` candidates, `/help` entries and the suggestion pipeline carry
  `String` descriptions.
- `crates/jcode-base/src/config.rs`, `config/config_file.rs`,
  `config/default_file.rs` — `REMOTE_CONFIG_OVERRIDE`,
  `PENDING_REMOTE_CONFIG_WRITES`, `ssh_remote_active()` choke gates.
- Reverted to upstream: `jcode-protocol`, `jcode-app-core::server` (all
  daemon dispatch), `protocol_tests`.

## Upstream status

Upstream-fix candidacy: none — this is fork-local transport design. The
`RemoteRead::OpReply` client pattern and `ssh_remote_active` config gating
are upstream-able in principle if upstream ever wants "client ops over a
remote attach", but the whole point of the patch is that nothing needs to
land upstream for the feature to work.
