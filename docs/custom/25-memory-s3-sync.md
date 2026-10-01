# [25] Memory S3 sync + repo-stable project identity

## What it does

Syncs jcode's memory stores (global + per-project graphs) across machines
through any S3-compatible bucket, and replaces the path-hash project memory
file names with a repo-stable id so the same checkout keeps its memories
when moved or shared.

Local files stay the source of truth. Every write path still hits the same
JSON files on disk; a background engine pushes diffs to the bucket and
pulls remote changes back in. Sync failures never block a memory
operation. They are logged and retried on the next tick.

## Project identity

`project_id::resolve_project_id(dir)` computes the store id in this order:

1. `~/.jcode/memory/project-map.json` override (`{ "<abs-path>": "<id>" }`).
2. `.jcode/memory-project-id` marker inside the repo (written by
   `jcode memory project set <id>`, shareable across checkouts of the same
   repo through the map).
3. `proj-<sha256[:16]>` of the normalized git remote URL. Normalization
   collapses `git@host:org/repo`, `ssh://git@host/org/repo`,
   `https://host/org/repo(.git)`, ports and creds to `host/org/repo`, so
   clones reached by different remotes still share one store.
4. `path-<sha256[:16]>` of the canonical directory, the legacy layout.
   `migrate_legacy_store` renames the old `<path-hash>.json` (checking raw
   and canonical dir hashes) to the new id the first time the directory
   resolves.

## Sync protocol (v1)

Layout inside `<prefix>` (default `jcode/memory`):

- `entries/<target>/<memory_id>.json` — newest body wins. Deletes write a
  `TombstoneBody` into the same key (never DELETE) so reconcile can still
  tell "resurrected" from "merely stale".
- `ops/<target>/<yyyymmddThhmmss.mmmZ>-<client>-<seq>.json` — append-only
  journal of `{op: upsert|delete, memory_id, updated_at, client_id}`.
  Pullers replay past their saved watermark; the lexicographic key order
  is the order they apply.
- `meta/clients/<id>.json`, `meta/projects/<id>.json` — last-seen and
  project metadata (display name, normalized remotes, known checkout
  paths) for a future picker.
- GC: `ops/` objects older than `tombstone_retention_days` (default 90) are deleted during reconcile;
  tombstones in local `sync-state` age out after
  `tombstone_retention_days` (default 90).

Conflict rule is LWW on `updated_at` with `client_id` as tiebreak;
strictly-newer local deletes beat equal-age remote upserts and lose to
strictly-newer remote upserts, so both directions converge.

The engine keeps a per-target snapshot (`remote-state/<target>.json` under
`~/.jcode/memory/`) of what it believes remote holds: pushes only upload
the serialized-diff vs. that snapshot, pulls only apply op journals after
the watermark, and the hourly reconcile lists `entries/` keys but GETs
only keys the snapshot does not know. Idle traffic stays at LIST-level.

## Driver

- Daemon (`jcode` server / `jcode run`): one loop at
  `push_poll_secs` cadence (default 10s, local mtime stat, zero network)
  calls `SyncEngine::tick`, which pushes targets whose `sync-state.json`
  dirty flag is set (`mark_dirty` is called by `save_global_graph` /
  `save_project_graph`), then pulls per `pull_interval_secs` (120s) and
  reconciles per `reconcile_interval_secs` (3600s).
  `pull_on_start = true` runs one cycle at daemon start.
- Targets: `global` plus every project id visible via local
  `projects/*.json`, the sync-state target set, and the remote
  `meta/projects/` catalog. The daemon syncs projects even with no
  session bound to that directory.
- `jcode memory sync` forces one full cycle for the manager bound to the
  current project (global + that project).
- `jcode memory project` prints the resolved id;
  `jcode memory project set <id>` pins it via the marker file.

## Config surface

```toml
[memory.sync]
enabled = true
backend = "s3"
endpoint = "https://s3.example.internal"
region = "garage"            # optional, default us-east-1
bucket = "jcode-memories"
prefix = ""                  # optional key prefix inside the bucket
path_style = true            # Garage/MinIO need virtual-host = false
access_key = "GK..."         # or "!{cmd ...}" / "!cat path" secrets
secret_key = "..."
scopes = ["global", "project"]
push_poll_secs = 10
pull_interval_secs = 120
reconcile_interval_secs = 3600
pull_on_start = true
tombstone_retention_days = 90
```

Secret values go through `jcode_provider_env::resolve_secret_value`
(patch [22]): literal, `!{cmd ...}` command substitution, or `!path`.

## Code surface

- `crates/jcode-config-types`: `MemoryConfig`, `MemorySyncConfig`.
- `crates/jcode-base/src/memory/project_id.rs`: id resolution, remote
  normalization, marker/map store, legacy migration, id cache.
- `crates/jcode-base/src/memory/s3.rs`: hand-rolled SigV4 client on
  reqwest (HEAD/GET/PUT/DELETE/ListObjectsV2, continuation-token
  pagination with start-after fallback); no new heavy deps.
- `crates/jcode-base/src/memory/sync.rs`: `ObjectStore` trait,
  `SyncEngine` (push/pull/reconcile/GC/tick), `SyncState`/`TargetState`,
  `RemoteState` snapshot, wire bodies, `run_sync_cycle`,
  `sync_enabled`.
- `crates/jcode-base/src/memory.rs`: `save_*_graph` calls
  `sync::mark_dirty` (skipped in test mode); `project_memory_path`
  resolves the stable id and migrates legacy files.
- `crates/jcode-app-core/src/server.rs`: daemon tick loop after the
  memory-agent init.
- `src/cli/{args,commands,dispatch}.rs`: `jcode memory project` and
  `jcode memory sync`; `memory stats` prints a Sync section.

## Upstream status

Upstream-candidate in principle (generic S3-compatible sync + a more
stable project naming), but the change is large and touches config,
memory layout, and the daemon. Upstream discussion required before any
PR. No known upstream conflicts.
