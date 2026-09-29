//! Repo-stable project identity for memory buckets.
//!
//! Resolution order (first hit wins):
//!   1. `~/.jcode/memory/project-map.json` — per-machine dir -> id overrides
//!      (written by `jcode memory project set` for non-repo dirs, and later by
//!      the TUI picker).
//!   2. `<dir>/.jcode/memory-project-id` — marker file that travels with the repo.
//!   3. `proj-<hash>` of the normalized git remote URL (origin, else first remote).
//!   4. `path-<hash>` of the project dir — the legacy scheme, kept as fallback.
//!
//! Hashing uses `DefaultHasher` throughout so all project keys share one hash
//! family. DefaultHasher has no cross-version stability guarantee: a Rust
//! upgrade could remap keys. `meta/projects/<id>.json` records carry the
//! project_id so a remap is recoverable.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

/// Where a resolved project id came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectIdSource {
    /// `~/.jcode/memory/project-map.json` override.
    Map,
    /// `<dir>/.jcode/memory-project-id` marker file.
    Marker,
    /// Derived from the normalized git remote URL.
    Remote,
    /// Legacy DefaultHasher(dir) fallback.
    PathHash,
}

impl ProjectIdSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Map => "project-map.json",
            Self::Marker => ".jcode/memory-project-id",
            Self::Remote => "git remote",
            Self::PathHash => "path hash (legacy)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedProject {
    pub id: String,
    pub source: ProjectIdSource,
    /// Canonical directory used as the map key.
    pub canonical_dir: PathBuf,
    /// All normalized remotes (for meta/projects payloads).
    pub remotes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectMapFile {
    #[serde(default = "default_map_version")]
    v: u32,
    #[serde(default)]
    map: HashMap<String, String>,
}

fn default_map_version() -> u32 {
    1
}

fn hash_hex<T: Hash>(value: &T) -> String {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The legacy filename stem (`<hash>.json`) used before project ids existed.
pub fn legacy_path_hash(dir: &Path) -> String {
    hash_hex(&dir.to_path_buf())
}

fn cache() -> &'static Mutex<HashMap<PathBuf, ResolvedProject>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, ResolvedProject>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Drop cached resolutions (e.g. after `memory project set`).
pub fn invalidate_cache() {
    if let Ok(mut c) = cache().lock() {
        c.clear();
    }
}

pub fn project_map_path() -> Result<PathBuf> {
    Ok(crate::storage::jcode_dir()?
        .join("memory")
        .join("project-map.json"))
}

pub fn marker_path(dir: &Path) -> PathBuf {
    dir.join(".jcode").join("memory-project-id")
}

fn read_project_map() -> HashMap<String, String> {
    let Ok(path) = project_map_path() else {
        return HashMap::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    match serde_json::from_str::<ProjectMapFile>(&text) {
        Ok(f) => f.map,
        Err(err) => {
            crate::logging::warn(&format!(
                "Ignoring malformed project map {}: {}",
                path.display(),
                err
            ));
            HashMap::new()
        }
    }
}

/// Persist a dir -> id binding. Repo roots get the marker file (travels with
/// the repo); everything else lands in project-map.json.
pub fn set_project_id(dir: &Path, id: &str) -> Result<PathBuf> {
    let clean = sanitize_project_id(id);
    let canonical = canonical_dir(dir);
    let marker_dir = canonical.join(".jcode");
    if marker_dir.is_dir() {
        let path = marker_path(&canonical);
        std::fs::write(&path, format!("{}\n", clean))
            .with_context(|| format!("write {}", path.display()))?;
        invalidate_cache();
        return Ok(path);
    }
    let path = project_map_path()?;
    let mut map = read_project_map();
    map.insert(canonical.to_string_lossy().to_string(), clean);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::storage::write_json(&path, &ProjectMapFile { v: 1, map })?;
    invalidate_cache();
    Ok(path)
}

/// Normalize a user-supplied id: lowercase, `[a-z0-9._-]`, `-` for anything else.
pub fn sanitize_project_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '.');
    if trimmed.is_empty() {
        "unnamed".to_string()
    } else {
        trimmed.chars().take(64).collect()
    }
}

/// Normalize a git remote URL to `host[:port]/path`:
/// lowercase, scheme/userinfo stripped, trailing `.git` and `/` removed.
/// Accepts `ssh://git@h:8222/a/b.git`, `git@h:a/b.git`, `https://h/a/b`, and
/// bare paths (returned as-is, lowercased).
pub fn normalize_git_remote(url: &str) -> Option<String> {
    let mut s = url.trim().to_string();
    if s.is_empty() {
        return None;
    }
    if let Some(idx) = s.find("://") {
        s = s[idx + 3..].to_string();
    }
    // Strip userinfo: only when '@' precedes the first '/'.
    let slash = s.find('/').unwrap_or(s.len());
    if let Some(at) = s[..slash].find('@') {
        s = s[at + 1..].to_string();
    }
    let mut s = s.trim().to_lowercase();
    while s.ends_with('/') {
        s.pop();
    }
    if let Some(stripped) = s.strip_suffix(".git") {
        s = stripped.trim_end_matches('/').to_string();
    }
    if s.is_empty() { None } else { Some(s) }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// All normalized remotes for the repo containing `dir`, deduped.
pub fn normalized_remotes(dir: &Path) -> Vec<String> {
    let Some(names) = git(dir, &["remote"]) else {
        return Vec::new();
    };
    let mut seen = Vec::new();
    for name in names.lines().map(str::trim).filter(|l| !l.is_empty()) {
        for url in git(dir, &["remote", "get-url", name]).into_iter() {
            if let Some(n) = normalize_git_remote(&url)
                && !seen.contains(&n)
            {
                seen.push(n);
            }
        }
    }
    seen
}

/// Preferred remote for id derivation: `origin` if present, else the first
/// remote name git reports.
fn primary_remote(dir: &Path) -> Option<String> {
    for name in git(dir, &["remote"]).into_iter().flat_map(|n| {
        n.lines()
            .map(str::trim)
            .map(str::to_string)
            .collect::<Vec<_>>()
    }) {
        if name == "origin" {
            return git(dir, &["remote", "get-url", "origin"]);
        }
        if let Some(url) = git(dir, &["remote", "get-url", &name]) {
            return Some(url);
        }
    }
    None
}

fn canonical_dir(dir: &Path) -> PathBuf {
    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
}

/// Resolve the stable project id for `dir` (cached per canonical path).
pub fn resolve_project_id(dir: &Path) -> ResolvedProject {
    let canonical = canonical_dir(dir);
    if let Ok(c) = cache().lock()
        && let Some(hit) = c.get(&canonical)
    {
        return hit.clone();
    }
    let resolved = resolve_uncached(&canonical);
    if let Ok(mut c) = cache().lock() {
        c.insert(canonical.clone(), resolved.clone());
    }
    resolved
}

fn resolve_uncached(canonical: &Path) -> ResolvedProject {
    let remotes = normalized_remotes(canonical);
    let key = canonical.to_string_lossy().to_string();

    if let Some(id) = read_project_map().get(&key) {
        return ResolvedProject {
            id: sanitize_project_id(id),
            source: ProjectIdSource::Map,
            canonical_dir: canonical.to_path_buf(),
            remotes,
        };
    }

    let marker = marker_path(canonical);
    if let Ok(text) = std::fs::read_to_string(&marker) {
        let id = sanitize_project_id(&text);
        if !id.is_empty() && id != "unnamed" {
            return ResolvedProject {
                id,
                source: ProjectIdSource::Marker,
                canonical_dir: canonical.to_path_buf(),
                remotes,
            };
        }
    }

    if let Some(url) = primary_remote(canonical)
        && let Some(normalized) = normalize_git_remote(&url)
    {
        return ResolvedProject {
            id: format!("proj-{}", hash_hex(&normalized)),
            source: ProjectIdSource::Remote,
            canonical_dir: canonical.to_path_buf(),
            remotes,
        };
    }

    ResolvedProject {
        id: format!("path-{}", legacy_path_hash(canonical)),
        source: ProjectIdSource::PathHash,
        canonical_dir: canonical.to_path_buf(),
        remotes,
    }
}

/// Rename the legacy `<path_hash>.json` store to `<new_id>.json` once, so a
/// newly-derived `proj-*`/`path-*` key keeps existing memories. `.bak` travels
/// too. Returns the new store path when a rename happened.
pub fn migrate_legacy_store(dir: &Path, new_id: &str) -> Result<Option<PathBuf>> {
    let projects = crate::storage::jcode_dir()?.join("memory").join("projects");
    let new = projects.join(format!("{}.json", new_id));
    if new.exists() {
        return Ok(None);
    }
    // The legacy file was keyed by DefaultHasher over the project dir PathBuf
    // exactly as stored — which may be canonical or not depending on the
    // caller. Check both spellings.
    let mut candidates = vec![legacy_path_hash(dir)];
    let canonical = canonical_dir(dir);
    if canonical != dir {
        candidates.push(legacy_path_hash(&canonical));
    }
    let legacy = candidates
        .iter()
        .filter(|h| *h != new_id)
        .map(|h| projects.join(format!("{}.json", h)))
        .find(|p| p.exists());
    let Some(legacy) = legacy else {
        return Ok(None);
    };
    std::fs::create_dir_all(&projects)?;
    std::fs::rename(&legacy, &new).with_context(|| {
        format!(
            "migrate memory store {} -> {}",
            legacy.display(),
            new.display()
        )
    })?;
    let legacy_bak = legacy.with_extension("bak");
    if legacy_bak.exists() {
        let _ = std::fs::rename(&legacy_bak, new.with_extension("bak"));
    }
    crate::logging::info(&format!(
        "Migrated project memory store {} -> {}",
        legacy.display(),
        new.display()
    ));
    Ok(Some(new))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_scp_syntax() {
        assert_eq!(
            normalize_git_remote("git@git.kmou424.moe:8222/configs/jcode.git"),
            Some("git.kmou424.moe:8222/configs/jcode".to_string())
        );
        assert_eq!(
            normalize_git_remote("git@github.com:kmou424/jcode"),
            Some("github.com:kmou424/jcode".to_string())
        );
    }

    #[test]
    fn normalizes_scheme_urls() {
        assert_eq!(
            normalize_git_remote("ssh://git@git.kmou424.moe:8222/configs/jcode.git"),
            Some("git.kmou424.moe:8222/configs/jcode".to_string())
        );
        assert_eq!(
            normalize_git_remote("https://user:pw@GitHub.com/Kmou424/JCode.git/"),
            Some("github.com/kmou424/jcode".to_string())
        );
        assert_eq!(
            normalize_git_remote("https://github.com/kmou424/jcode/"),
            Some("github.com/kmou424/jcode".to_string())
        );
    }

    #[test]
    fn scp_and_ssh_forms_converge() {
        assert_eq!(
            normalize_git_remote("git@git.kmou424.moe:8222/configs/jcode.git"),
            normalize_git_remote("ssh://git@git.kmou424.moe:8222/configs/jcode.git")
        );
    }

    #[test]
    fn rejects_empty_and_trims() {
        assert_eq!(normalize_git_remote(""), None);
        assert_eq!(normalize_git_remote("   "), None);
        assert_eq!(normalize_git_remote(":///"), None);
    }

    #[test]
    fn sanitize_ids() {
        assert_eq!(sanitize_project_id(" My_Project! "), "my_project");
        assert_eq!(sanitize_project_id("..."), "unnamed");
        assert_eq!(sanitize_project_id("proj-AbC_01"), "proj-abc_01");
    }

    #[test]
    fn id_is_stable_for_same_normalized_remote() {
        let a = format!("proj-{}", hash_hex(&"github.com/kmou424/jcode".to_string()));
        let b = format!("proj-{}", hash_hex(&"github.com/kmou424/jcode".to_string()));
        assert_eq!(a, b);
        assert!(a.starts_with("proj-"));
    }
}
