//! `git_commit` and `git_checkpoint` tools.
//!
//! `git_commit` takes a commit as structured fields (type/scope/description/
//! body/closes/refs plus `paths` or `checkpoint`) and assembles the
//! Conventional-Commits message server-side, so a malformed message is
//! impossible. The `Co-Authored-By` trailer is injected from `[tools.git]`
//! config — the model never writes it — and three gates are enforced: refuse
//! on the default branch unless `allowDefaultBranch`, refuse unrelated
//! pre-staged content unless `includePreStaged`, refuse an empty stage.
//!
//! `git_checkpoint` snapshots index+worktree into a recoverable `refs/stash`
//! commit WITHOUT clearing the working tree (`git stash create` + `store`), so
//! the agent keeps editing; `git_commit`'s `checkpoint` field promotes one
//! into a real commit.
//!
//! Git operations shell out to the `git` CLI rather than linking libgit2 so
//! repo hooks and config are honored.

use super::{Tool, ToolContext, ToolOutput};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
use tokio::process::Command;

/// Literal `${model}` config token for `[tools.git]` sign-off fields.
const MODEL_PLACEHOLDER: &str = "${model}";
/// Literal `${user}` config token for `[tools.git]` sign-off fields.
const USER_PLACEHOLDER: &str = "${user}";

const COMMIT_TYPES: &[&str] = &[
    "feat", "fix", "refactor", "test", "docs", "chore", "perf", "build", "ci", "style", "revert",
];

pub struct GitCommitTool;

impl GitCommitTool {
    pub fn new() -> Self {
        Self
    }
}

pub struct GitCheckpointTool;

impl GitCheckpointTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Deserialize)]
struct GitCommitInput {
    /// Paths to stage (repo-root-relative). Mutually exclusive with checkpoint.
    #[serde(default, deserialize_with = "super::serde_coerce::opt_vec_nonempty")]
    paths: Option<Vec<String>>,
    /// Promote a checkpoint (`stash@{N}` or sha) into this commit.
    #[serde(
        default,
        deserialize_with = "super::serde_coerce::opt_string_blank_as_none"
    )]
    checkpoint: Option<String>,
    #[serde(rename = "type")]
    commit_type: String,
    #[serde(
        default,
        deserialize_with = "super::serde_coerce::opt_string_blank_as_none"
    )]
    scope: Option<String>,
    description: String,
    #[serde(default, deserialize_with = "super::serde_coerce::null_default")]
    body: Vec<String>,
    #[serde(default, deserialize_with = "super::serde_coerce::null_default")]
    closes: Vec<u64>,
    #[serde(default, deserialize_with = "super::serde_coerce::null_default")]
    refs: Vec<u64>,
    #[serde(
        default,
        rename = "repoPath",
        deserialize_with = "super::serde_coerce::opt_string_blank_as_none"
    )]
    repo_path: Option<String>,
    #[serde(
        default,
        rename = "allowDefaultBranch",
        deserialize_with = "super::serde_coerce::null_default"
    )]
    allow_default_branch: bool,
    #[serde(
        default,
        rename = "includePreStaged",
        deserialize_with = "super::serde_coerce::null_default"
    )]
    include_pre_staged: bool,
    /// Append the configured co-author trailer. Defaults to true.
    #[serde(default)]
    signoff: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GitCheckpointInput {
    message: String,
    /// Capture untracked files in the snapshot. Defaults to true.
    #[serde(default, rename = "includeUntracked")]
    include_untracked: Option<bool>,
    #[serde(
        default,
        rename = "repoPath",
        deserialize_with = "super::serde_coerce::opt_string_blank_as_none"
    )]
    repo_path: Option<String>,
}

#[async_trait]
impl Tool for GitCommitTool {
    fn name(&self) -> &str {
        "git_commit"
    }

    fn description(&self) -> &str {
        "Create a git commit from structured fields. The tool assembles the \
         Conventional-Commits message and injects the configured co-author \
         trailer; do not hand-write either."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["type", "description"],
            "additionalProperties": false,
            "properties": {
                "intent": super::intent_schema_property(),
                "paths": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Paths to stage, resolved relative to repoPath. Mutually exclusive with checkpoint."
                },
                "checkpoint": {
                    "type": "string",
                    "description": "Promote a checkpoint ('stash@{0}' or sha) into this commit."
                },
                "type": {
                    "type": "string",
                    "enum": COMMIT_TYPES,
                },
                "scope": {
                    "type": "string",
                    "pattern": "^[a-z0-9._/-]+$",
                    "description": "Scope without parentheses; the tool adds them."
                },
                "description": {
                    "type": "string",
                    "description": "Imperative summary, lowercase first letter, no trailing period. One phrase; detail goes in body."
                },
                "body": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Each item becomes one '- …' bullet explaining what and why."
                },
                "closes": {
                    "type": "array",
                    "items": {"type": "integer"},
                    "description": "Issue numbers fully satisfied by this commit → 'Closes #n'."
                },
                "refs": {
                    "type": "array",
                    "items": {"type": "integer"},
                    "description": "Partial/preparatory issue links → 'Refs: #n'."
                },
                "repoPath": {
                    "type": "string",
                    "description": "Base dir to locate the repo (rev-parse --show-toplevel); paths resolve relative to it. Defaults to session working dir."
                },
                "allowDefaultBranch": {
                    "type": "boolean",
                    "default": false,
                    "description": "Permit committing on the default branch. Omit unless a deliberate main-line commit is intended."
                },
                "includePreStaged": {
                    "type": "boolean",
                    "default": false,
                    "description": "Include already-staged content outside `paths`. Omit unless that content is part of this commit."
                },
                "signoff": {
                    "type": "boolean",
                    "default": true,
                    "description": "Append the configured co-author trailer. Leave unset unless an explicit requirement asks to omit the sign-off."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let input: GitCommitInput =
            serde_json::from_value(input).context("Invalid git_commit input")?;

        // Mutual exclusion: a checkpoint supplies its own file set.
        if input.checkpoint.is_some() && input.paths.is_some() {
            bail!("git_commit: pass either `paths` or `checkpoint`, not both");
        }
        if let Some(error) = commit_type_error(&input.commit_type) {
            bail!("git_commit: {error}");
        }
        if let Some(error) = scope_error(input.scope.as_deref()) {
            bail!("git_commit: {error}");
        }
        if let Some(error) = description_error(&input.description) {
            bail!("git_commit: {error}");
        }

        let base = repo_base(input.repo_path.as_deref(), &ctx)?;
        let root = repo_root(&base).await?;
        let branch = current_branch(&root).await?;
        let default_branch = default_branch(&root).await?;
        if branch == default_branch && !input.allow_default_branch {
            bail!(
                "git_commit: refusing to commit on default branch \"{default_branch}\"; \
                 create a task branch or pass allowDefaultBranch"
            );
        }

        let paths_to_stage: Vec<String> = match input.checkpoint.as_deref() {
            Some(reference) => {
                // Promote: materialize the snapshot, then stage exactly its paths.
                stash_apply(&root, reference).await?;
                stash_paths(&root, reference).await?
            }
            None => input
                .paths
                .unwrap_or_default()
                .iter()
                .map(|path| path.trim())
                .filter(|path| !path.is_empty())
                .map(|path| normalize_repo_path(&root, &base, path))
                .collect::<Result<Vec<_>>>()?,
        };

        // Pre-staged gate: anything already staged outside our set is a blocker
        // unless the caller declared it part of this commit.
        let staged = staged_paths(&root).await?;
        let foreign: Vec<&String> = staged
            .iter()
            .filter(|path| !paths_to_stage.contains(path))
            .collect();
        if !foreign.is_empty() && !input.include_pre_staged {
            bail!(
                "git_commit: refusing to sweep unrelated staged paths ({}); \
                 unstage them or pass includePreStaged",
                foreign
                    .iter()
                    .map(|path| path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        if !paths_to_stage.is_empty() {
            stage(&root, &paths_to_stage).await?;
        }
        if !has_staged(&root).await? {
            bail!("git_commit: nothing staged to commit");
        }

        let trailer = match input.signoff {
            Some(false) => None,
            _ => resolve_trailer(&root, &ctx).await?,
        };

        let message = assemble_message(
            &input.commit_type,
            input.scope.as_deref(),
            &input.description,
            &input.body,
            &input.closes,
            &input.refs,
            trailer.as_deref(),
        );
        let files_committed = staged_paths(&root).await?.len();
        let sha = commit(&root, &message).await?;
        let subject = message.lines().next().unwrap_or_default().to_string();
        let short_sha: String = sha.chars().take(10).collect();

        Ok(ToolOutput::new(format!(
            "Committed {short_sha} on {branch}\n{subject}\n{files_committed} file(s)"
        ))
        .with_title("git_commit")
        .with_metadata(json!({
            "sha": sha,
            "branch": branch,
            "subject": subject,
            "filesCommitted": files_committed,
        })))
    }
}

#[async_trait]
impl Tool for GitCheckpointTool {
    fn name(&self) -> &str {
        "git_checkpoint"
    }

    fn description(&self) -> &str {
        "Snapshot the current index+worktree into a recoverable stash commit \
         without clearing the working tree, so work can continue. The \
         snapshot can later be promoted into a real commit via git_commit's \
         `checkpoint`."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["message"],
            "additionalProperties": false,
            "properties": {
                "intent": super::intent_schema_property(),
                "message": {
                    "type": "string",
                    "description": "Checkpoint label, e.g. 'wip: parser done, before refactor'."
                },
                "includeUntracked": {
                    "type": "boolean",
                    "default": true,
                    "description": "Capture untracked files in the snapshot."
                },
                "repoPath": {
                    "type": "string",
                    "description": "Base dir to locate the repo; defaults to session working dir."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let input: GitCheckpointInput =
            serde_json::from_value(input).context("Invalid git_checkpoint input")?;
        if input.message.trim().is_empty() {
            bail!("git_checkpoint: message must be a non-empty string");
        }

        let base = repo_base(input.repo_path.as_deref(), &ctx)?;
        let root = repo_root(&base).await?;
        let branch = current_branch(&root).await?;
        let sha = checkpoint(
            &root,
            &input.message,
            input.include_untracked != Some(false),
        )
        .await?;
        let selector = stash_selector(&root).await?;
        let short_sha: String = sha.chars().take(10).collect();

        Ok(ToolOutput::new(format!(
            "Checkpoint {selector} ({short_sha}) on {branch}\n{}",
            input.message
        ))
        .with_title("git_checkpoint")
        .with_metadata(json!({
            "checkpoint": selector,
            "sha": sha,
            "branch": branch,
            "message": input.message,
        })))
    }
}

/// Locate the directory `git -C` should start from: the explicit `repoPath`
/// resolved against the session working dir, else the session working dir,
/// else the process cwd.
fn repo_base(repo_path: Option<&str>, ctx: &ToolContext) -> Result<PathBuf> {
    match repo_path {
        Some(path) if !path.trim().is_empty() => Ok(ctx.resolve_path(Path::new(path.trim()))),
        _ => match ctx.working_dir.clone() {
            Some(dir) => Ok(dir),
            None => std::env::current_dir().context("no session working dir or repoPath"),
        },
    }
}

/// Run one git subcommand inside `dir`, returning trimmed stdout on success.
async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim_end());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

/// Resolve the repository top-level for a base directory.
async fn repo_root(base: &Path) -> Result<PathBuf> {
    let root = git(base, &["rev-parse", "--show-toplevel"]).await?;
    Ok(PathBuf::from(root))
}

/// Current branch name (`HEAD` when detached).
///
/// `symbolic-ref` still resolves the target branch while HEAD is unborn (no
/// commits yet), where `rev-parse --abbrev-ref HEAD` fails; on a detached
/// HEAD it is symbolic-ref that fails, so try it first and fall back.
async fn current_branch(root: &Path) -> Result<String> {
    if let Ok(branch) = git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await
        && !branch.is_empty()
    {
        return Ok(branch);
    }
    git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).await
}

/// The repository's default branch. The remote's HEAD is authoritative for a
/// clone; otherwise fall back to `init.defaultBranch`, then `main`.
async fn default_branch(root: &Path) -> Result<String> {
    if let Ok(head) = git(
        root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .await
    {
        if let Some(branch) = head
            .strip_prefix("origin/")
            .map(str::trim)
            .filter(|branch| !branch.is_empty())
        {
            return Ok(branch.to_string());
        }
    }
    if let Some(branch) = git_config(root, "init.defaultBranch").await {
        return Ok(branch);
    }
    Ok("main".to_string())
}

/// `git config --get <key>` or `None` when unset.
async fn git_config(root: &Path, key: &str) -> Option<String> {
    git(root, &["config", "--get", key])
        .await
        .ok()
        .filter(|value| !value.is_empty())
}

/// Paths staged in the index, relative to the repo root.
async fn staged_paths(root: &Path) -> Result<Vec<String>> {
    let out = git(root, &["diff", "--cached", "--name-only", "-z"]).await?;
    Ok(out
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect())
}

/// Whether the index has anything staged.
async fn has_staged(root: &Path) -> Result<bool> {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .await
        .context("failed to run git diff --cached --quiet")?;
    // exit 0: nothing staged; non-zero: staged content present
    Ok(!status.success())
}

/// Stage the given repo-root-relative paths.
async fn stage(root: &Path, paths: &[String]) -> Result<()> {
    let mut args = vec!["add", "--"];
    args.extend(paths.iter().map(String::as_str));
    git(root, &args).await?;
    Ok(())
}

/// Commit the staged index, returning the new HEAD sha.
async fn commit(root: &Path, message: &str) -> Result<String> {
    git(root, &["commit", "-m", message]).await?;
    git(root, &["rev-parse", "HEAD"]).await
}

/// Package the current index+worktree into a dangling commit and hang it on
/// `refs/stash` without clearing the working tree — the checkpoint primitive.
/// `stash create` only packages tracked content, so untracked files are first
/// staged for real, then their index entries are surgically reset
/// (`reset -- <paths>`) so previously staged work is left untouched and the
/// files return to untracked. Returns the new commit sha.
async fn checkpoint(root: &Path, message: &str, include_untracked: bool) -> Result<String> {
    let mut untracked = Vec::new();
    if include_untracked {
        let out = git(root, &["ls-files", "--others", "--exclude-standard", "-z"]).await?;
        untracked = out
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        if !untracked.is_empty() {
            let mut args = vec!["add", "--"];
            args.extend(untracked.iter().map(String::as_str));
            git(root, &args).await?;
        }
    }
    let sha = git(root, &["stash", "create", message]).await?;
    if sha.is_empty() {
        bail!("git_checkpoint: git stash create produced no commit (nothing to snapshot)");
    }
    git(root, &["stash", "store", "-m", message, &sha]).await?;
    if !untracked.is_empty() {
        let mut args = vec!["reset", "-q", "--"];
        args.extend(untracked.iter().map(String::as_str));
        // Best-effort: leaving the files staged is recoverable, the snapshot
        // already captured them.
        let _ = git(root, &args).await;
    }
    Ok(sha)
}

/// The `stash@{N}` selector of the newest stash entry.
async fn stash_selector(root: &Path) -> Result<String> {
    let top = git(root, &["stash", "list", "-1", "--format=%gd"]).await?;
    Ok(if top.is_empty() {
        "stash@{0}".to_string()
    } else {
        top
    })
}

/// Paths a stash/sha touches, relative to the repo root.
async fn stash_paths(root: &Path, reference: &str) -> Result<Vec<String>> {
    let out = git(root, &["stash", "show", "--name-only", "-z", reference]).await?;
    Ok(out
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect())
}

/// Materialize a stash/sha's changes into index+worktree.
async fn stash_apply(root: &Path, reference: &str) -> Result<()> {
    git(root, &["checkout", reference, "--", "."]).await?;
    Ok(())
}

/// Normalize a user-supplied path to repo-root-relative form, so `git add`
/// and the pre-staged gate compare like with like: `diff --cached` reports
/// root-relative names even when the caller passed an absolute or `./`- or
/// `..`-laden path. Relative paths resolve against `base` (the `repoPath`
/// anchor). Errors when the path escapes the repository.
fn normalize_repo_path(root: &Path, base: &Path, path: &str) -> Result<String> {
    // Canonicalize the two anchors (they must exist) so a symlinked prefix
    // like /tmp -> /private/tmp cannot fake a strip_prefix mismatch.
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let canonical_base = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    let raw = Path::new(path);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        canonical_base.join(raw)
    };
    // Lexical normalization: fold `.`/`..` without touching the filesystem,
    // so paths to deleted files (staged deletions) still normalize.
    let absolute = normalize_components(&joined);
    let normalized_root = normalize_components(&canonical_root);
    let relative = absolute
        .strip_prefix(&normalized_root)
        .with_context(|| format!("git_commit: path is outside repository root: {path}"))?;
    let relative = relative.to_string_lossy();
    Ok(if relative.is_empty() {
        ".".to_string()
    } else {
        relative.into_owned()
    })
}

/// Fold `.`/`..` path components lexically, without filesystem access.
fn normalize_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Validate the commit type; returns an error string or `None`.
fn commit_type_error(commit_type: &str) -> Option<String> {
    if COMMIT_TYPES.contains(&commit_type) {
        None
    } else {
        Some(format!(
            "type must be one of: {} (got \"{commit_type}\")",
            COMMIT_TYPES.join(", ")
        ))
    }
}

/// Validate the optional scope; returns an error string or `None`.
fn scope_error(scope: Option<&str>) -> Option<String> {
    let scope = scope?;
    if !scope.is_empty()
        && scope.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '/' | '-')
        })
    {
        None
    } else {
        Some(format!(
            "scope must match ^[a-z0-9._/-]+$ without parentheses (got \"{scope}\")"
        ))
    }
}

/// Validate the description line; returns an error string or `None`.
fn description_error(description: &str) -> Option<String> {
    if description.is_empty() {
        return Some("description must be a non-empty string".to_string());
    }
    if description != description.trim() {
        return Some("description must not have leading/trailing whitespace".to_string());
    }
    if description
        .chars()
        .last()
        .is_some_and(|c| matches!(c, '.' | '!' | '?' | '。' | '！' | '？'))
    {
        return Some("description must not end with terminal punctuation".to_string());
    }
    if description
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase())
    {
        return Some("description must start lowercase (imperative mood)".to_string());
    }
    if description.chars().count() > 72 {
        return Some("description must be at most 72 characters".to_string());
    }
    None
}

/// Assemble the Conventional-Commits message from structured fields.
fn assemble_message(
    commit_type: &str,
    scope: Option<&str>,
    description: &str,
    body: &[String],
    closes: &[u64],
    refs: &[u64],
    trailer: Option<&str>,
) -> String {
    let head = match scope {
        Some(scope) if !scope.is_empty() => format!("{commit_type}({scope}): {description}"),
        _ => format!("{commit_type}: {description}"),
    };
    let mut parts = vec![head];
    let body_lines: Vec<&String> = body.iter().filter(|line| !line.trim().is_empty()).collect();
    if !body_lines.is_empty() {
        parts.push(String::new());
        parts.extend(body_lines.iter().map(|line| format!("- {line}")));
    }
    let mut issue_footers = Vec::new();
    for number in closes.iter().filter(|number| **number > 0) {
        issue_footers.push(format!("Closes #{number}"));
    }
    for number in refs.iter().filter(|number| **number > 0) {
        issue_footers.push(format!("Refs: #{number}"));
    }
    if !issue_footers.is_empty() {
        parts.push(String::new());
        parts.extend(issue_footers);
    }
    if let Some(trailer) = trailer {
        parts.push(String::new());
        parts.push(trailer.to_string());
    }
    format!("{}\n", parts.join("\n"))
}

/// Resolve the `Co-Authored-By` trailer from `[tools.git]` config. Returns
/// `None` when either field is unset or resolves empty — the trailer is then
/// simply omitted. Unresolvable placeholders are hard errors: a configured
/// `${model}`/`${user}` that cannot produce a value must not silently sign
/// with a placeholder.
async fn resolve_trailer(root: &Path, ctx: &ToolContext) -> Result<Option<String>> {
    let config = &crate::config::config().tools.git;
    let name = resolve_signoff_field(root, ctx, "name", config.signoff_name.as_deref()).await?;
    let email = resolve_signoff_field(root, ctx, "email", config.signoff_email.as_deref()).await?;
    match (name, email) {
        (Some(name), Some(email)) if !name.is_empty() && !email.is_empty() => {
            Ok(Some(format!("Co-Authored-By: {name} <{email}>")))
        }
        _ => Ok(None),
    }
}

/// Resolve one sign-off field's placeholder to a concrete string.
async fn resolve_signoff_field(
    root: &Path,
    ctx: &ToolContext,
    field: &str,
    value: Option<&str>,
) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        MODEL_PLACEHOLDER => {
            let Some(identity) = crate::session_model::session_model(&ctx.session_id) else {
                bail!(
                    "git_commit cannot sign off: ${{model}} has not resolved yet \
                     (no model identity recorded for this session)"
                );
            };
            crate::provider_catalog::named_provider_model_display_name_for_provider_key(
                identity.provider_key.as_deref(),
                &identity.model,
            )
            .map(Some)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "git_commit cannot sign off: model \"{}\" has no resolvable \
                     display name for ${{model}}",
                    identity.model
                )
            })
        }
        USER_PLACEHOLDER => {
            let key = if field == "name" {
                "user.name"
            } else {
                "user.email"
            };
            match git_config(root, key).await {
                Some(value) => Ok(Some(value)),
                None => bail!("git_commit cannot sign off: git config {key} is unset"),
            }
        }
        literal => Ok(Some(literal.to_string())),
    }
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod git_tests;
