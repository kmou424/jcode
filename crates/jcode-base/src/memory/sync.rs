//! S3-compatible replication for memory graphs.
//!
//! Local JSON files stay the primary store; the bucket is a shared replication
//! medium. Layout under the configured prefix:
//!
//! ```text
//! entries/<target>/<memory_id>.json   current state (or tombstone body)
//! ops/<target>/<ts>-<client>-<seq>.json   immutable op records
//! meta/projects/<project_id>.json     project metadata for the future picker
//! meta/clients/<client_id>.json       last-seen markers
//! ```
//!
//! `<target>` is `global` or a project id (`proj-…`, `path-…`, or a custom id).
//!
//! Push is diff-driven: saves only set `dirty` in `sync-state.json`; the pusher
//! diffs the live graph against `remote-state/<target>.json` (the last-synced
//! snapshot) and PUTs changed entries plus an op record per change. Pull walks
//! the ops journal since a watermark; a periodic `entries/` key-set reconcile
//! covers ops that were GC'd before a stale client pulled.
//!
//! Merge rule: whole-entry last-writer-wins on `updated_at`, tie-broken by
//! client id. `entries/` keys are never DELETE'd — a delete overwrites the
//! object with a tombstone body so reconcile still sees it.

use crate::config::MemorySyncConfig;
use crate::memory_graph::MemoryGraph;
use crate::memory_types::MemoryEntry;
use crate::storage;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use super::s3::S3Client;
use super::{MemoryManager, project_id};

// ---------------------------------------------------------------------------
// Object store abstraction (S3Client in prod, in-memory map in tests)
// ---------------------------------------------------------------------------

pub trait ObjectStore: Send + Sync {
    fn head_bucket(&self) -> Result<()>;
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    fn put(&self, key: &str, body: &[u8]) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
    /// Lexicographically ordered keys under `prefix`, strictly after `start_after`.
    fn list(&self, prefix: &str, start_after: Option<&str>) -> Result<Vec<String>>;
}

impl ObjectStore for S3Client {
    fn head_bucket(&self) -> Result<()> {
        S3Client::head_bucket(self)
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.get_object(key)
    }
    fn put(&self, key: &str, body: &[u8]) -> Result<()> {
        self.put_object(key, body)
    }
    fn delete(&self, key: &str) -> Result<()> {
        self.delete_object(key)
    }
    fn list(&self, prefix: &str, start_after: Option<&str>) -> Result<Vec<String>> {
        self.list_keys(prefix, start_after)
    }
}

// ---------------------------------------------------------------------------
// Local state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncState {
    #[serde(default)]
    pub targets: BTreeMap<String, TargetState>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TargetState {
    /// Local writes happened since last successful push.
    #[serde(default)]
    pub dirty: bool,
    /// Last processed `ops/<target>/` object key.
    #[serde(default)]
    pub watermark: String,
    /// Locally applied deletes: memory_id -> deletion timestamp. Guards merge
    /// so an older remote upsert cannot resurrect a deleted entry.
    #[serde(default)]
    pub tombstones: HashMap<String, DateTime<Utc>>,
    #[serde(default)]
    pub last_pull: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_push: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_reconcile: Option<DateTime<Utc>>,
    /// Per-client op sequence for unique op keys.
    #[serde(default)]
    pub op_seq: u64,
}

/// Remote-state snapshot: what we believe the remote looks like after our last
/// push/pull. `memories` mirrors `entries/` upserts; `tombstones` mirrors
/// `entries/` tombstone objects.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RemoteState {
    #[serde(default)]
    memories: HashMap<String, MemoryEntry>,
    #[serde(default)]
    tombstones: HashMap<String, DateTime<Utc>>,
}

fn sync_state_path() -> Result<PathBuf> {
    Ok(storage::jcode_dir()?.join("memory").join("sync-state.json"))
}

fn remote_state_path(target: &str) -> Result<PathBuf> {
    Ok(storage::jcode_dir()?
        .join("memory")
        .join("remote-state")
        .join(format!("{}.json", target)))
}

impl SyncState {
    pub fn load() -> Result<Self> {
        let path = sync_state_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        Ok(serde_json::from_slice(&std::fs::read(&path)?)?)
    }

    fn save(&self) -> Result<()> {
        storage::write_json(&sync_state_path()?, self)
    }
}

fn load_remote_state(target: &str) -> Result<RemoteState> {
    let path = remote_state_path(target)?;
    if !path.exists() {
        return Ok(RemoteState::default());
    }
    Ok(serde_json::from_slice(&std::fs::read(&path)?)?)
}

fn save_remote_state(target: &str, state: &RemoteState) -> Result<()> {
    let path = remote_state_path(target)?;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    storage::write_json(&path, state)
}

/// Mark a target dirty after a local save/forget. Best-effort: sync-state
/// corruption is recoverable (a reconcile re-diffs everything).
pub fn mark_dirty(target: &str) {
    let res = (|| -> Result<()> {
        let mut state = SyncState::load()?;
        state.targets.entry(target.to_string()).or_default().dirty = true;
        state.save()
    })();
    if let Err(e) = res {
        crate::logging::warn(&format!("memory sync: failed to mark {target} dirty: {e}"));
    }
}

/// Machine id shared across this host's sync clients; created on first use.
pub fn machine_id() -> Result<String> {
    let path = storage::jcode_dir()?.join("machine_id");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let id = text.trim().to_string();
        if !id.is_empty() {
            return Ok(id);
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    std::fs::write(&path, format!("{}\n", id))?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// Wire bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct TombstoneBody {
    v: u32,
    memory_id: String,
    deleted_at: DateTime<Utc>,
    client_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpBody {
    v: u32,
    op: String, // "upsert" | "delete"
    memory_id: String,
    updated_at: DateTime<Utc>,
    client_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClientMeta {
    v: u32,
    client_id: String,
    label: String,
    last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProjectMeta {
    v: u32,
    project_id: String,
    display_name: String,
    #[serde(default)]
    remotes: Vec<String>,
    #[serde(default)]
    clients: HashMap<String, ProjectClientMeta>,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectClientMeta {
    #[serde(default)]
    paths: Vec<String>,
    last_seen: DateTime<Utc>,
}

fn is_tombstone(body: &[u8]) -> bool {
    serde_json::from_slice::<TombstoneBody>(body)
        .map(|t| t.v == 1 && !t.memory_id.is_empty() && !t.client_id.is_empty())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

pub struct SyncEngine {
    cfg: MemorySyncConfig,
    client: Box<dyn ObjectStore>,
    client_id: String,
}

#[derive(Debug, Clone)]
pub enum Target {
    Global,
    /// `resolved` carries repo metadata (remotes/paths) when the target comes
    /// from a live project dir; catalog-enumerated ids have `None` and skip
    /// `meta/projects` writes (the owning machine maintains them).
    Project {
        id: String,
        resolved: Option<project_id::ResolvedProject>,
    },
}

impl Target {
    pub fn name(&self) -> String {
        match self {
            Self::Global => "global".to_string(),
            Self::Project { id, .. } => id.clone(),
        }
    }
}

#[derive(Debug, Default)]
pub struct PushReport {
    pub upserts: usize,
    pub deletes: usize,
}

#[derive(Debug, Default)]
pub struct PullReport {
    pub applied_upserts: usize,
    pub applied_deletes: usize,
    pub skipped: usize,
}

pub struct CycleReport {
    pub pushed: BTreeMap<String, PushReport>,
    pub pulled: BTreeMap<String, PullReport>,
    pub reconciled: Vec<String>,
    pub errors: Vec<String>,
}

impl std::fmt::Display for CycleReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (t, p) in &self.pushed {
            writeln!(
                f,
                "push {t}: {} upsert(s), {} delete(s)",
                p.upserts, p.deletes
            )?;
        }
        for (t, p) in &self.pulled {
            writeln!(
                f,
                "pull {t}: {} upsert(s), {} delete(s), {} skipped",
                p.applied_upserts, p.applied_deletes, p.skipped
            )?;
        }
        for t in &self.reconciled {
            writeln!(f, "reconcile {t}")?;
        }
        for e in &self.errors {
            writeln!(f, "error: {e}")?;
        }
        if self.pushed.is_empty() && self.pulled.is_empty() && self.errors.is_empty() {
            write!(f, "nothing to do")?;
        }
        Ok(())
    }
}

impl SyncEngine {
    /// Build from config; `store` is injectable for tests.
    pub fn new(cfg: MemorySyncConfig, store: Box<dyn ObjectStore>) -> Result<Self> {
        anyhow::ensure!(
            cfg.backend == "s3",
            "memory.sync.backend '{}' unsupported (only \"s3\")",
            cfg.backend
        );
        store.head_bucket()?;
        Ok(Self {
            cfg,
            client: store,
            client_id: machine_id()?,
        })
    }

    /// Production constructor: resolve secrets, build the S3 client, HEAD the
    /// bucket once (loud error when it does not exist).
    pub fn from_config(cfg: MemorySyncConfig) -> Result<Self> {
        let client = S3Client::from_config(&cfg)?;
        Self::new(cfg, Box::new(client))
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Targets participating in sync for this manager (respects `scopes`).
    pub fn targets(&self, manager: &MemoryManager) -> Vec<Target> {
        let mut out = Vec::new();
        let scope = |s: &str| self.cfg.scopes.iter().any(|x| x == s);
        if scope("global") {
            out.push(Target::Global);
        }
        if scope("project")
            && let Some(dir) = manager.project_dir()
        {
            let resolved = project_id::resolve_project_id(&dir);
            out.push(Target::Project {
                id: resolved.id.clone(),
                resolved: Some(resolved),
            });
        }
        out
    }

    /// Every project id this daemon should carry: local `projects/*.json`
    /// files, ids already tracked in sync-state, and the remote project
    /// catalog (`meta/projects/`). Lets the long-lived daemon sync projects
    /// even when no session is bound to that directory — the daemon's manager
    /// has no project_dir of its own.
    pub fn catalog_targets(&self) -> Vec<Target> {
        let mut ids: Vec<String> = Vec::new();
        if let Ok(projects) = storage::jcode_dir().map(|d| d.join("memory").join("projects"))
            && let Ok(entries) = std::fs::read_dir(&projects)
        {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(id) = name.strip_suffix(".json") {
                    ids.push(id.to_string());
                }
            }
        }
        if let Ok(state) = SyncState::load() {
            for key in state.targets.keys() {
                if key != "global" {
                    ids.push(key.clone());
                }
            }
        }
        if let Ok(keys) = self.client.list("meta/projects/", None) {
            for key in keys {
                if let Some(id) = key
                    .strip_prefix("meta/projects/")
                    .and_then(|s| s.strip_suffix(".json"))
                    && !id.is_empty()
                {
                    ids.push(id.to_string());
                }
            }
        }
        ids.sort();
        ids.dedup();
        ids.into_iter()
            .map(|id| Target::Project { id, resolved: None })
            .collect()
    }

    /// Full target set: caller-bound targets plus every known catalog entry.
    fn all_targets(&self, manager: &MemoryManager) -> Vec<Target> {
        let mut seen: Vec<Target> = self.targets(manager);
        for t in self.catalog_targets() {
            if !seen.iter().any(|x| x.name() == t.name()) {
                seen.push(t);
            }
        }
        seen
    }

    fn op_key(&self, target: &str, state: &mut TargetState, now: DateTime<Utc>) -> String {
        state.op_seq += 1;
        format!(
            "ops/{}/{}-{}-{}.json",
            target,
            now.format("%Y%m%dT%H%M%S%.3fZ"),
            self.client_id,
            state.op_seq
        )
    }

    // --- push -------------------------------------------------------------

    /// Diff the local graph against the remote-state snapshot and upload the
    /// delta (entry objects + op records + tombstones).
    pub fn push(&self, target: &Target, state: &mut TargetState) -> Result<PushReport> {
        let name = target.name();
        let (graph, path) = load_graph(target)?;
        let read_mtime = mtime(&path);
        let mut remote = load_remote_state(&name)?;
        let mut report = PushReport::default();
        let now = Utc::now();

        // Upserts: entries new or changed relative to the snapshot. Serialized
        // equality — any field drift (supersede, counters, tags) counts.
        for (id, entry) in &graph.memories {
            remote.tombstones.remove(id);
            let changed = match remote.memories.get(id) {
                Some(prev) => serde_json::to_vec(prev).ok() != serde_json::to_vec(entry).ok(),
                None => true,
            };
            if !changed {
                continue;
            }
            let body = serde_json::to_vec(entry)?;
            self.client
                .put(&format!("entries/{}/{}.json", name, id), &body)?;
            let op = OpBody {
                v: 1,
                op: "upsert".into(),
                memory_id: id.clone(),
                updated_at: entry.updated_at,
                client_id: self.client_id.clone(),
            };
            self.client
                .put(&self.op_key(&name, state, now), &serde_json::to_vec(&op)?)?;
            remote.memories.insert(id.clone(), entry.clone());
            report.upserts += 1;
        }

        // Deletes: snapshot entries missing locally -> tombstone + delete op.
        for id in remote.memories.keys().cloned().collect::<Vec<_>>() {
            if graph.memories.contains_key(&id) {
                continue;
            }
            let ts = state.tombstones.get(&id).copied().unwrap_or(now);
            let tomb = TombstoneBody {
                v: 1,
                memory_id: id.clone(),
                deleted_at: ts,
                client_id: self.client_id.clone(),
            };
            self.client.put(
                &format!("entries/{}/{}.json", name, id),
                &serde_json::to_vec(&tomb)?,
            )?;
            let op = OpBody {
                v: 1,
                op: "delete".into(),
                memory_id: id.clone(),
                updated_at: ts,
                client_id: self.client_id.clone(),
            };
            self.client
                .put(&self.op_key(&name, state, now), &serde_json::to_vec(&op)?)?;
            remote.memories.remove(&id);
            remote.tombstones.insert(id, ts);
            report.deletes += 1;
        }

        if let Target::Project {
            resolved: Some(resolved),
            ..
        } = target
        {
            self.update_project_meta(resolved)?;
        }
        self.touch_client_meta()?;

        save_remote_state(&name, &remote)?;
        state.last_push = Some(now);
        // A concurrent local save between the graph read and now would be lost
        // from the snapshot; re-check mtime so it stays dirty and re-diffs.
        let still_dirty = match (read_mtime, mtime(&path)) {
            (Some(before), Some(after)) => after != before,
            _ => false,
        };
        state.dirty = still_dirty;
        Ok(report)
    }

    // --- pull -------------------------------------------------------------

    /// Walk `ops/<target>/` past the watermark and apply remote mutations.
    pub fn pull(&self, target: &Target, state: &mut TargetState) -> Result<PullReport> {
        let name = target.name();
        let prefix = format!("ops/{}/", name);
        let start = if state.watermark.is_empty() {
            None
        } else {
            Some(self.full_ops_key(&state.watermark))
        };
        let keys = self.client.list(&prefix, start.as_deref())?;
        let mut report = PullReport::default();
        if keys.is_empty() {
            state.last_pull = Some(Utc::now());
            return Ok(report);
        }

        let (mut graph, _path) = load_graph(target)?;
        let mut remote = load_remote_state(&name)?;
        let mut changed = false;

        for key in keys {
            let op: OpBody = match self.client.get(&key)? {
                Some(body) => match serde_json::from_slice(&body) {
                    Ok(o) => o,
                    Err(e) => {
                        crate::logging::warn(&format!(
                            "memory sync: skipping malformed op {key}: {e}"
                        ));
                        continue;
                    }
                },
                None => continue,
            };
            if op.client_id == self.client_id {
                // Our own echo; just advance the watermark.
            } else {
                match op.op.as_str() {
                    "upsert" => {
                        if self.apply_remote_upsert(&mut graph, &mut remote, &name, &op, state)? {
                            report.applied_upserts += 1;
                            changed = true;
                        } else {
                            report.skipped += 1;
                        }
                    }
                    "delete" => {
                        if self.apply_remote_delete(&mut graph, &mut remote, &op, state) {
                            report.applied_deletes += 1;
                            changed = true;
                        } else {
                            report.skipped += 1;
                        }
                    }
                    other => {
                        crate::logging::warn(&format!(
                            "memory sync: unknown op '{other}' in {key}"
                        ));
                        report.skipped += 1;
                    }
                }
            }
            state.watermark = key;
        }

        if changed {
            save_graph(target, &graph)?;
            save_remote_state(&name, &remote)?;
        }
        state.last_pull = Some(Utc::now());
        Ok(report)
    }

    fn full_ops_key(&self, rel: &str) -> String {
        // watermark stores keys relative to the ops/<target>/ listing prefix.
        rel.to_string()
    }

    /// LWW upsert: remote entry wins iff strictly newer, or equal-time with a
    /// higher client id. Local tombstones shadow older upserts.
    fn apply_remote_upsert(
        &self,
        graph: &mut MemoryGraph,
        remote: &mut RemoteState,
        target: &str,
        op: &OpBody,
        state: &mut TargetState,
    ) -> Result<bool> {
        if let Some(ts) = state.tombstones.get(&op.memory_id)
            && op.updated_at <= *ts
        {
            return Ok(false);
        }
        let local = graph.memories.get(&op.memory_id);
        let remote_wins = match local {
            None => true,
            Some(l) => {
                op.updated_at > l.updated_at
                    || (op.updated_at == l.updated_at
                        && op.client_id.as_str() > self.client_id.as_str())
            }
        };
        if !remote_wins {
            return Ok(false);
        }
        let Some(body) = self
            .client
            .get(&format!("entries/{}/{}.json", target, op.memory_id))?
        else {
            crate::logging::warn(&format!(
                "memory sync: entry object missing for op {}",
                op.memory_id
            ));
            return Ok(false);
        };
        if is_tombstone(&body) {
            // Remote entry was deleted after the op was written: treat as delete.
            let tomb: TombstoneBody = serde_json::from_slice(&body)?;
            return Ok(self.apply_remote_delete(
                graph,
                remote,
                &tomb.into_op(op.client_id.clone()),
                state,
            ));
        }
        let entry: MemoryEntry = serde_json::from_slice(&body)?;
        graph.memories.insert(entry.id.clone(), entry.clone());
        remote.memories.insert(entry.id.clone(), entry);
        remote.tombstones.remove(&op.memory_id);
        Ok(true)
    }

    /// Delete wins iff its timestamp covers the local entry; a strictly newer
    /// local update survives and will be re-pushed (converging to local).
    fn apply_remote_delete(
        &self,
        graph: &mut MemoryGraph,
        remote: &mut RemoteState,
        op: &OpBody,
        state: &mut TargetState,
    ) -> bool {
        let dominated = match graph.memories.get(&op.memory_id) {
            Some(l) => op.updated_at >= l.updated_at,
            None => true,
        };
        if !dominated {
            return false;
        }
        graph.memories.remove(&op.memory_id);
        remote.memories.remove(&op.memory_id);
        remote
            .tombstones
            .insert(op.memory_id.clone(), op.updated_at);
        state.tombstones.insert(op.memory_id.clone(), op.updated_at);
        true
    }

    // --- reconcile + GC ----------------------------------------------------

    /// Cheap safety net: list `entries/<target>/` and fetch only keys the
    /// remote-state snapshot does not know (unknown ids) or tombstone bodies
    /// that arrived while ops were missed. Mutations on already-known keys are
    /// trusted to the ops journal — this keeps the hourly pass O(few GETs)
    /// instead of O(all entries), which matters for metered egress.
    pub fn reconcile(&self, target: &Target, state: &mut TargetState) -> Result<PullReport> {
        let name = target.name();
        let keys = self.client.list(&format!("entries/{}/", name), None)?;
        let mut remote = load_remote_state(&name)?;
        let (mut graph, _path) = load_graph(target)?;
        let mut report = PullReport::default();
        let mut changed = false;

        for key in keys {
            let Some(id) = key
                .strip_prefix(&format!("entries/{}/", name))
                .and_then(|s| s.strip_suffix(".json"))
            else {
                continue;
            };
            if remote.memories.contains_key(id) || remote.tombstones.contains_key(id) {
                continue;
            }
            let Some(body) = self.client.get(&key)? else {
                continue;
            };
            if let Ok(tomb) = serde_json::from_slice::<TombstoneBody>(&body)
                && tomb.v == 1
            {
                let op = tomb.into_op(self.client_id.clone());
                if self.apply_remote_delete(&mut graph, &mut remote, &op, state) {
                    report.applied_deletes += 1;
                    changed = true;
                } else {
                    remote.tombstones.insert(op.memory_id, op.updated_at);
                }
                continue;
            }
            match serde_json::from_slice::<MemoryEntry>(&body) {
                Ok(entry) => {
                    let op = OpBody {
                        v: 1,
                        op: "upsert".into(),
                        memory_id: entry.id.clone(),
                        updated_at: entry.updated_at,
                        client_id: String::new(),
                    };
                    if self.apply_remote_upsert(&mut graph, &mut remote, &name, &op, state)? {
                        report.applied_upserts += 1;
                        changed = true;
                    }
                }
                Err(e) => {
                    crate::logging::warn(&format!(
                        "memory sync: skipping unreadable remote entry {key}: {e}"
                    ));
                }
            }
        }

        if changed {
            save_graph(target, &graph)?;
            save_remote_state(&name, &remote)?;
        }

        self.gc_ops(&name)?;
        self.prune_tombstones(state);
        state.last_reconcile = Some(Utc::now());
        Ok(report)
    }

    /// Delete `ops/<target>/` objects older than the retention window. Any
    /// client may sweep: reconcile covers ops a stale client never saw.
    fn gc_ops(&self, target: &str) -> Result<()> {
        let cutoff = Utc::now() - Duration::days(self.cfg.tombstone_retention_days as i64);
        let keys = self.client.list(&format!("ops/{}/", target), None)?;
        for key in keys {
            let Some(ts) = op_key_timestamp(&key) else {
                continue;
            };
            if ts < cutoff
                && let Err(e) = self.client.delete(&key)
            {
                crate::logging::warn(&format!("memory sync: GC of {key} failed: {e}"));
            }
        }
        Ok(())
    }

    fn prune_tombstones(&self, state: &mut TargetState) {
        let cutoff = Utc::now() - Duration::days(self.cfg.tombstone_retention_days as i64);
        state.tombstones.retain(|_, ts| *ts >= cutoff);
    }

    // --- meta ---------------------------------------------------------------

    fn touch_client_meta(&self) -> Result<()> {
        let meta = ClientMeta {
            v: 1,
            client_id: self.client_id.clone(),
            label: hostname(),
            last_seen: Utc::now(),
        };
        self.client.put(
            &format!("meta/clients/{}.json", self.client_id),
            &serde_json::to_vec(&meta)?,
        )
    }

    fn update_project_meta(&self, resolved: &project_id::ResolvedProject) -> Result<()> {
        let key = format!("meta/projects/{}.json", resolved.id);
        let mut meta: ProjectMeta = match self.client.get(&key)? {
            Some(body) => serde_json::from_slice(&body).unwrap_or_default(),
            None => ProjectMeta::default(),
        };
        meta.v = 1;
        meta.project_id = resolved.id.clone();
        if meta.display_name.is_empty() {
            meta.display_name = resolved
                .canonical_dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| resolved.id.clone());
        }
        for r in &resolved.remotes {
            if !meta.remotes.contains(r) {
                meta.remotes.push(r.clone());
            }
        }
        meta.remotes.sort();
        let client = meta
            .clients
            .entry(self.client_id.clone())
            .or_insert_with(|| ProjectClientMeta {
                paths: Vec::new(),
                last_seen: Utc::now(),
            });
        let path = resolved.canonical_dir.to_string_lossy().to_string();
        if !client.paths.contains(&path) {
            client.paths.push(path);
            client.paths.sort();
        }
        client.last_seen = Utc::now();
        if meta.created_at.is_none() {
            meta.created_at = Some(Utc::now());
        }
        meta.updated_at = Some(Utc::now());
        self.client.put(&key, &serde_json::to_vec(&meta)?)
    }

    // --- cycle --------------------------------------------------------------

    /// One daemon tick: push dirty targets, then pull/reconcile whatever is due.
    pub fn tick(&self, manager: &MemoryManager) -> CycleReport {
        let mut report = CycleReport {
            pushed: BTreeMap::new(),
            pulled: BTreeMap::new(),
            reconciled: Vec::new(),
            errors: Vec::new(),
        };
        let mut state = match SyncState::load() {
            Ok(s) => s,
            Err(e) => {
                report.errors.push(format!("sync-state load failed: {e}"));
                return report;
            }
        };
        let pull_iv = Duration::seconds(self.cfg.pull_interval_secs.max(5) as i64);
        let rec_iv = Duration::seconds(self.cfg.reconcile_interval_secs.max(60) as i64);

        for target in self.all_targets(manager) {
            let name = target.name();
            let tstate = state.targets.entry(name.clone()).or_default();
            if tstate.dirty {
                match self.push(&target, tstate) {
                    Ok(r) => {
                        report.pushed.insert(name.clone(), r);
                    }
                    Err(e) => {
                        report.errors.push(format!("push {name}: {e:#}"));
                    }
                }
            }
            let pull_due = tstate
                .last_pull
                .map(|t| Utc::now() - t >= pull_iv)
                .unwrap_or(true);
            if pull_due {
                match self.pull(&target, tstate) {
                    Ok(r) => {
                        if r.applied_upserts + r.applied_deletes + r.skipped > 0 {
                            report.pulled.insert(name.clone(), r);
                        }
                    }
                    Err(e) => report.errors.push(format!("pull {name}: {e:#}")),
                }
            }
            let rec_due = tstate
                .last_reconcile
                .map(|t| Utc::now() - t >= rec_iv)
                .unwrap_or(true);
            if rec_due {
                match self.reconcile(&target, tstate) {
                    Ok(_) => report.reconciled.push(name.clone()),
                    Err(e) => report.errors.push(format!("reconcile {name}: {e:#}")),
                }
            }
        }

        if let Err(e) = state.save() {
            report.errors.push(format!("sync-state save failed: {e}"));
        }
        report
    }
}

impl TombstoneBody {
    fn into_op(self, client_id: String) -> OpBody {
        OpBody {
            v: 1,
            op: "delete".into(),
            memory_id: self.memory_id,
            updated_at: self.deleted_at,
            client_id,
        }
    }
}

/// `ops/<t>/<yyyymmddThhmmss.mmmZ>-<client>-<seq>.json` -> timestamp.
fn op_key_timestamp(key: &str) -> Option<DateTime<Utc>> {
    let file = key.rsplit('/').next()?;
    let ts = file.split('-').next()?;
    let naive = chrono::NaiveDateTime::parse_from_str(ts, "%Y%m%dT%H%M%S%.3fZ").ok()?;
    Some(naive.and_utc())
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// On-disk file a target maps to.
fn graph_path(target: &Target) -> Result<PathBuf> {
    let memory_dir = storage::jcode_dir()?.join("memory");
    match target {
        Target::Global => Ok(memory_dir.join("global.json")),
        Target::Project { id, .. } => Ok(memory_dir.join("projects").join(format!("{id}.json"))),
    }
}

/// Load the graph file for a target. Missing files yield an empty graph;
/// legacy `MemoryStore` files migrate in place (with .bak backup, mirroring
/// `MemoryManager::load_*_graph`). Returns the path too so callers can stat it.
fn load_graph(target: &Target) -> Result<(MemoryGraph, PathBuf)> {
    let path = graph_path(target)?;
    if !path.exists() {
        return Ok((MemoryGraph::new(), path));
    }
    if let Ok(graph) = storage::read_json::<MemoryGraph>(&path)
        && graph.graph_version == crate::memory::GRAPH_VERSION
    {
        return Ok((graph, path));
    }
    // Fall back to legacy MemoryStore and migrate in place.
    let store: crate::memory_types::MemoryStore = storage::read_json(&path)?;
    let graph = MemoryGraph::from_legacy_store(store);
    let backup = path.with_extension("json.bak");
    if !backup.exists() {
        let _ = std::fs::copy(&path, &backup);
    }
    storage::write_json(&path, &graph)?;
    Ok((graph, path))
}

/// Persist a pull-mutated graph without tripping the dirty flag: sync writes
/// must not look like local edits to the very engine that made them.
fn save_graph(target: &Target, graph: &MemoryGraph) -> Result<()> {
    let path = graph_path(target)?;
    storage::write_json(&path, graph)
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Whether sync is configured well enough to run at all.
pub fn sync_enabled(cfg: &crate::config::Config) -> bool {
    cfg.memory.sync.enabled && cfg.memory.sync.backend == "s3"
}

/// One full sync pass (CLI `jcode memory sync`, daemon `pull_on_start`).
/// Returns a human-readable report; errors abort the pass.
pub async fn run_sync_cycle(manager: &MemoryManager, origin: &str) -> Result<CycleReport> {
    let cfg = crate::config::config();
    if !sync_enabled(cfg) {
        bail!("memory sync is not enabled ([memory.sync] in config.toml)");
    }
    let engine = SyncEngine::from_config(cfg.memory.sync.clone())?;
    let manager = manager.clone();
    let report = tokio::task::spawn_blocking(move || engine.tick(&manager))
        .await
        .context("sync cycle panicked")?;
    crate::logging::info(&format!("memory sync ({origin}) done"));
    Ok(report)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_types::MemoryCategory;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// In-memory object store; BTreeMap gives lexicographic LIST for free.
    struct MemStore {
        objects: Mutex<BTreeMap<String, Vec<u8>>>,
    }

    impl MemStore {
        fn shared() -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                objects: Mutex::new(BTreeMap::new()),
            })
        }
    }

    impl ObjectStore for std::sync::Arc<MemStore> {
        fn head_bucket(&self) -> Result<()> {
            Ok(())
        }
        fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
            Ok(self.objects.lock().unwrap().get(key).cloned())
        }
        fn put(&self, key: &str, body: &[u8]) -> Result<()> {
            self.objects
                .lock()
                .unwrap()
                .insert(key.to_string(), body.to_vec());
            Ok(())
        }
        fn delete(&self, key: &str) -> Result<()> {
            self.objects.lock().unwrap().remove(key);
            Ok(())
        }
        fn list(&self, prefix: &str, start_after: Option<&str>) -> Result<Vec<String>> {
            let objects = self.objects.lock().unwrap();
            Ok(objects
                .range::<String, _>((
                    std::ops::Bound::Included(prefix.to_string()),
                    std::ops::Bound::Unbounded,
                ))
                .filter(|(k, _)| k.starts_with(prefix))
                .filter(|(k, _)| start_after.map(|sa| k.as_str() > sa).unwrap_or(true))
                .map(|(k, _)| k.clone())
                .collect())
        }
    }

    /// Redirect JCODE_HOME to a fresh temp dir for the closure's duration.
    fn with_temp_home<F, T>(f: F) -> T
    where
        F: FnOnce(&Path) -> T,
    {
        let _guard = crate::storage::lock_test_env();
        let old = std::env::var("JCODE_HOME").ok();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("jcode-sync-test-{}", unique));
        std::fs::create_dir_all(&dir).unwrap();
        crate::env::set_var("JCODE_HOME", &dir);
        project_id::invalidate_cache();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&dir)));

        match old {
            Some(v) => crate::env::set_var("JCODE_HOME", v),
            None => crate::env::remove_var("JCODE_HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        match result {
            Ok(v) => v,
            Err(p) => std::panic::resume_unwind(p),
        }
    }

    fn engine(store: &std::sync::Arc<MemStore>) -> SyncEngine {
        SyncEngine::new(MemorySyncConfig::default(), Box::new(store.clone())).unwrap()
    }

    fn entry(id: &str, content: &str, updated_at: DateTime<Utc>) -> MemoryEntry {
        let mut e = MemoryEntry::new(MemoryCategory::Fact, content);
        e.id = id.to_string();
        e.updated_at = updated_at;
        e
    }

    /// A second "machine": same store, different client id.
    fn other_engine(store: &std::sync::Arc<MemStore>, client_id: &str) -> SyncEngine {
        let mut e = engine(store);
        e.client_id = client_id.to_string();
        e
    }

    #[test]
    fn push_uploads_entries_and_ops() {
        with_temp_home(|home| {
            let store = MemStore::shared();
            let eng = engine(&store);
            let manager = MemoryManager::new();
            let mut graph = MemoryGraph::new();
            graph
                .memories
                .insert("m1".into(), entry("m1", "hello", Utc::now()));
            manager.save_global_graph(&graph).unwrap();

            let mut ts = TargetState::default();
            let report = eng.push(&Target::Global, &mut ts).unwrap();
            assert_eq!(report.upserts, 1);
            assert!(!ts.dirty);
            let objects = store.objects.lock().unwrap();
            assert!(objects.contains_key("entries/global/m1.json"));
            assert_eq!(
                objects
                    .keys()
                    .filter(|k| k.starts_with("ops/global/"))
                    .count(),
                1
            );
            assert!(objects.contains_key(&format!("meta/clients/{}.json", eng.client_id())));
            let _ = home;
        });
    }

    #[test]
    fn push_only_uploads_diff() {
        with_temp_home(|_home| {
            let store = MemStore::shared();
            let eng = engine(&store);
            let manager = MemoryManager::new();
            let mut graph = MemoryGraph::new();
            graph.memories.insert(
                "m1".into(),
                entry("m1", "v1", Utc::now() - Duration::hours(1)),
            );
            manager.save_global_graph(&graph).unwrap();
            let mut ts = TargetState::default();
            eng.push(&Target::Global, &mut ts).unwrap();

            // Second push with no changes: no new objects.
            let r2 = eng.push(&Target::Global, &mut ts).unwrap();
            assert_eq!(r2.upserts, 0);
            // meta objects may refresh; entry/ops must not grow.
            assert_eq!(
                store
                    .objects
                    .lock()
                    .unwrap()
                    .keys()
                    .filter(|k| k.starts_with("ops/"))
                    .count(),
                1
            );

            // Change one entry -> exactly one upsert op.
            let mut g2 = manager.load_global_graph().unwrap();
            let e = g2.memories.get_mut("m1").unwrap();
            e.content = "v2".into();
            e.updated_at = Utc::now();
            manager.save_global_graph(&g2).unwrap();
            let r3 = eng.push(&Target::Global, &mut ts).unwrap();
            assert_eq!(r3.upserts, 1);
        });
    }

    #[test]
    fn push_emits_tombstone_for_deleted_entry() {
        with_temp_home(|_home| {
            let store = MemStore::shared();
            let eng = engine(&store);
            let manager = MemoryManager::new();
            let mut graph = MemoryGraph::new();
            graph
                .memories
                .insert("m1".into(), entry("m1", "x", Utc::now()));
            manager.save_global_graph(&graph).unwrap();
            let mut ts = TargetState::default();
            eng.push(&Target::Global, &mut ts).unwrap();

            // Forget locally -> tombstone pushed, entries object stays.
            manager.forget("m1").unwrap();
            let mut state = SyncState::load().unwrap();
            let tstate = state.targets.entry("global".into()).or_default();
            *tstate = ts.clone();
            let r = eng.push(&Target::Global, tstate).unwrap();
            assert_eq!(r.deletes, 1);
            let objects = store.objects.lock().unwrap();
            let body = objects.get("entries/global/m1.json").unwrap();
            assert!(is_tombstone(body), "delete must leave a tombstone object");
            assert!(
                objects
                    .keys()
                    .filter(|k| k.starts_with("ops/global/"))
                    .count()
                    >= 2
            );
        });
    }

    #[test]
    fn pull_applies_remote_ops_and_advances_watermark() {
        let store = MemStore::shared();
        // Machine A pushes (its own home).
        with_temp_home(|_ha| {
            let eng_a = other_engine(&store, "aaaa");
            let manager_a = MemoryManager::new();
            let mut g = MemoryGraph::new();
            g.memories
                .insert("m1".into(), entry("m1", "from A", Utc::now()));
            manager_a.save_global_graph(&g).unwrap();
            let mut ts_a = TargetState::default();
            eng_a.push(&Target::Global, &mut ts_a).unwrap();
        });

        // Machine B pulls into its own home.
        with_temp_home(|_hb| {
            let eng_b = other_engine(&store, "bbbb");
            let manager_b = MemoryManager::new();
            let mut ts_b = TargetState::default();
            let r = eng_b.pull(&Target::Global, &mut ts_b).unwrap();
            assert_eq!(r.applied_upserts, 1);
            assert!(!ts_b.watermark.is_empty());
            let graph = manager_b.load_global_graph().unwrap();
            assert_eq!(graph.memories["m1"].content, "from A");

            // Pull again: watermark covers everything.
            let r2 = eng_b.pull(&Target::Global, &mut ts_b).unwrap();
            assert_eq!(r2.applied_upserts + r2.applied_deletes + r2.skipped, 0);
        });
    }

    #[test]
    fn own_ops_are_skipped_on_pull() {
        with_temp_home(|_home| {
            let store = MemStore::shared();
            let eng = engine(&store);
            let manager = MemoryManager::new();
            let mut g = MemoryGraph::new();
            g.memories
                .insert("m1".into(), entry("m1", "mine", Utc::now()));
            manager.save_global_graph(&g).unwrap();
            let mut ts = TargetState::default();
            eng.push(&Target::Global, &mut ts).unwrap();
            let r = eng.pull(&Target::Global, &mut ts).unwrap();
            assert_eq!(r.applied_upserts, 0);
            assert!(!ts.watermark.is_empty());
        });
    }

    #[test]
    fn newer_local_wins_over_remote_upsert() {
        let store = MemStore::shared();
        // A pushes old version.
        with_temp_home(|_ha| {
            let eng_a = other_engine(&store, "aaaa");
            let manager_a = MemoryManager::new();
            let old_ts = Utc::now() - Duration::hours(2);
            let mut g = MemoryGraph::new();
            g.memories.insert("m1".into(), entry("m1", "old", old_ts));
            manager_a.save_global_graph(&g).unwrap();
            let mut ts_a = TargetState::default();
            eng_a.push(&Target::Global, &mut ts_a).unwrap();
        });

        // B has a strictly newer local version -> pull must not clobber it.
        with_temp_home(|_hb| {
            let eng_b = other_engine(&store, "bbbb");
            let manager_b = MemoryManager::new();
            let mut g = MemoryGraph::new();
            g.memories
                .insert("m1".into(), entry("m1", "new", Utc::now()));
            manager_b.save_global_graph(&g).unwrap();
            let mut ts_b = TargetState::default();
            let r = eng_b.pull(&Target::Global, &mut ts_b).unwrap();
            assert_eq!(r.applied_upserts, 0);
            assert_eq!(r.skipped, 1);
            assert_eq!(
                manager_b.load_global_graph().unwrap().memories["m1"].content,
                "new"
            );
        });
    }

    #[test]
    fn delete_op_beats_newer_remote_but_not_newer_local() {
        let store = MemStore::shared();
        let del_ts = Utc::now();
        // A pushes m1, then deletes it (tombstone + delete op).
        with_temp_home(|_ha| {
            let eng_a = other_engine(&store, "aaaa");
            let manager_a = MemoryManager::new();
            let ts_old = Utc::now() - Duration::hours(2);
            let mut g = MemoryGraph::new();
            g.memories.insert("m1".into(), entry("m1", "x", ts_old));
            manager_a.save_global_graph(&g).unwrap();
            let mut ts_a = TargetState::default();
            eng_a.push(&Target::Global, &mut ts_a).unwrap();
            let tomb = TombstoneBody {
                v: 1,
                memory_id: "m1".into(),
                deleted_at: del_ts,
                client_id: "aaaa".into(),
            };
            eng_a
                .client
                .put(
                    "entries/global/m1.json",
                    &serde_json::to_vec(&tomb).unwrap(),
                )
                .unwrap();
            let op = OpBody {
                v: 1,
                op: "delete".into(),
                memory_id: "m1".into(),
                updated_at: del_ts,
                client_id: "aaaa".into(),
            };
            eng_a
                .client
                .put(
                    &format!(
                        "ops/global/{}-aaaa-{}.json",
                        del_ts.format("%Y%m%dT%H%M%S%.3fZ"),
                        99
                    ),
                    &serde_json::to_vec(&op).unwrap(),
                )
                .unwrap();
        });

        // B pulls m1, re-adds it locally with a NEWER timestamp, then the
        // delete op arrives: delete must not win.
        with_temp_home(|_hb| {
            let eng_b = other_engine(&store, "bbbb");
            let manager_b = MemoryManager::new();
            let mut ts_b = TargetState::default();
            eng_b.pull(&Target::Global, &mut ts_b).unwrap();
            let mut g2 = MemoryGraph::new();
            g2.memories
                .insert("m1".into(), entry("m1", "revived", Utc::now()));
            manager_b.save_global_graph(&g2).unwrap();
            let r = eng_b.pull(&Target::Global, &mut ts_b).unwrap();
            assert_eq!(r.applied_deletes, 0);
            assert!(
                manager_b
                    .load_global_graph()
                    .unwrap()
                    .memories
                    .contains_key("m1")
            );
        });

        // A third machine with an OLD local copy applies the delete.
        with_temp_home(|_hc| {
            let eng_c = other_engine(&store, "cccc");
            let manager_c = MemoryManager::new();
            let mut g = MemoryGraph::new();
            g.memories.insert(
                "m1".into(),
                entry("m1", "stale", Utc::now() - Duration::hours(4)),
            );
            manager_c.save_global_graph(&g).unwrap();
            let mut ts_c = TargetState::default();
            let r = eng_c.pull(&Target::Global, &mut ts_c).unwrap();
            assert_eq!(r.applied_deletes, 1);
            assert!(
                !manager_c
                    .load_global_graph()
                    .unwrap()
                    .memories
                    .contains_key("m1")
            );
        });
    }

    #[test]
    fn reconcile_fetches_unknown_entry_keys() {
        with_temp_home(|_home| {
            let store = MemStore::shared();
            let eng_b = other_engine(&store, "bbbb");
            let manager = MemoryManager::new();
            // Remote holds an entry B never saw (no ops — as if GC'd).
            let e = entry("m9", "stray", Utc::now());
            store
                .put("entries/global/m9.json", &serde_json::to_vec(&e).unwrap())
                .unwrap();
            let mut ts_b = TargetState::default();
            let r = eng_b.reconcile(&Target::Global, &mut ts_b).unwrap();
            assert_eq!(r.applied_upserts, 1);
            assert_eq!(
                manager.load_global_graph().unwrap().memories["m9"].content,
                "stray"
            );
        });
    }

    #[test]
    fn reconcile_applies_remote_tombstone() {
        with_temp_home(|_home| {
            let store = MemStore::shared();
            let eng_b = other_engine(&store, "bbbb");
            let manager = MemoryManager::new();
            // B has the entry locally; remote carries a tombstone for it.
            let old = Utc::now() - Duration::hours(3);
            let mut g = MemoryGraph::new();
            g.memories.insert("m1".into(), entry("m1", "x", old));
            manager.save_global_graph(&g).unwrap();
            let tomb = TombstoneBody {
                v: 1,
                memory_id: "m1".into(),
                deleted_at: Utc::now() - Duration::hours(1),
                client_id: "aaaa".into(),
            };
            store
                .put(
                    "entries/global/m1.json",
                    &serde_json::to_vec(&tomb).unwrap(),
                )
                .unwrap();
            let mut ts_b = TargetState::default();
            let r = eng_b.reconcile(&Target::Global, &mut ts_b).unwrap();
            assert_eq!(r.applied_deletes, 1);
            assert!(
                !manager
                    .load_global_graph()
                    .unwrap()
                    .memories
                    .contains_key("m1")
            );
        });
    }

    #[test]
    fn op_key_parses_timestamp() {
        let key = "ops/global/20260929T051234.123Z-abcdef-7.json";
        let ts = op_key_timestamp(key).expect("parse");
        assert_eq!(ts.format("%Y%m%d").to_string(), "20260929");
        assert!(op_key_timestamp("ops/global/bad-name.json").is_none());
    }

    #[test]
    fn dirty_flag_roundtrip() {
        with_temp_home(|_home| {
            mark_dirty("global");
            let s = SyncState::load().unwrap();
            assert!(s.targets["global"].dirty);
        });
    }
}
