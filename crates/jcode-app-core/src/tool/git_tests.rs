use super::*;
use std::path::Path;

fn make_ctx(working_dir: &Path) -> ToolContext {
    ToolContext {
        session_id: "git-tool-test".to_string(),
        message_id: "test-msg".to_string(),
        tool_call_id: "test-call".to_string(),
        working_dir: Some(working_dir.to_path_buf()),
        stdin_request_tx: None,
        ask_user_question_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    }
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

/// A repo on `main` with local identity and a deterministic default branch.
fn init_repo() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    run_git(root, &["init", "-b", "main"]);
    run_git(root, &["config", "user.name", "Test User"]);
    run_git(root, &["config", "user.email", "test@example.com"]);
    // Pin the default branch locally so detection does not depend on the
    // host's global init.defaultBranch.
    run_git(root, &["config", "init.defaultBranch", "main"]);
    std::fs::write(root.join("base.txt"), "base\n").unwrap();
    run_git(root, &["add", "base.txt"]);
    run_git(root, &["commit", "-m", "chore: initial"]);
    temp
}

/// Point `JCODE_HOME` at an empty directory so a developer's real
/// `[tools.git]` signoff config cannot leak into these tests (they assert
/// the unconfigured behavior). Restores the previous value on drop while
/// still holding the shared env-mutation lock.
struct EmptyJcodeHome {
    _guard: std::sync::MutexGuard<'static, ()>,
    _dir: tempfile::TempDir,
    prev: Option<std::ffi::OsString>,
}

impl EmptyJcodeHome {
    fn new() -> Self {
        let guard = jcode_base::storage::lock_test_env();
        let prev = std::env::var_os("JCODE_HOME");
        let dir = tempfile::tempdir().expect("temp JCODE_HOME");
        jcode_base::env::set_var("JCODE_HOME", dir.path());
        Self {
            _guard: guard,
            _dir: dir,
            prev,
        }
    }
}

impl Drop for EmptyJcodeHome {
    fn drop(&mut self) {
        match &self.prev {
            Some(value) => jcode_base::env::set_var("JCODE_HOME", value),
            None => jcode_base::env::remove_var("JCODE_HOME"),
        }
    }
}

// ---------- pure validation / assembly ----------

#[test]
fn description_error_accepts_valid() {
    assert_eq!(description_error("add a git commit tool"), None);
}

#[test]
fn description_error_rejects_invalid() {
    assert_eq!(
        description_error(""),
        Some("description must be a non-empty string".to_string())
    );
    assert!(
        description_error("  spaced")
            .unwrap()
            .contains("whitespace")
    );
    assert!(
        description_error("ends with period.")
            .unwrap()
            .contains("punctuation")
    );
    assert!(
        description_error("ends with bang!")
            .unwrap()
            .contains("punctuation")
    );
    assert!(
        description_error("Uppercase start")
            .unwrap()
            .contains("lowercase")
    );
    let long = "a".repeat(73);
    assert!(description_error(&long).unwrap().contains("72"));
    assert_eq!(description_error(&"a".repeat(72)), None);
    // Non-ASCII first letters are not flagged as uppercase.
    assert_eq!(description_error("中文描述"), None);
}

#[test]
fn scope_error_validates_pattern() {
    assert_eq!(scope_error(None), None);
    assert_eq!(scope_error(Some("tools")), None);
    assert_eq!(scope_error(Some("app-core/git")), None);
    assert!(scope_error(Some("")).is_some());
    assert!(scope_error(Some("Tools")).is_some());
    assert!(scope_error(Some("with space")).is_some());
    assert!(scope_error(Some("(parens)")).is_some());
}

#[test]
fn commit_type_error_validates_enum() {
    assert_eq!(commit_type_error("feat"), None);
    assert_eq!(commit_type_error("revert"), None);
    assert!(commit_type_error("feature").is_some());
    assert!(commit_type_error("").is_some());
    assert!(commit_type_error("FEAT").is_some());
}

#[test]
fn assemble_message_minimal() {
    let message = assemble_message("feat", None, "add thing", &[], &[], &[], None);
    assert_eq!(message, "feat: add thing\n");
}

#[test]
fn assemble_message_full() {
    let message = assemble_message(
        "fix",
        Some("tools"),
        "correct path staging",
        &["stage relative to repoPath".to_string()],
        &[12],
        &[34, 56],
        Some("Co-Authored-By: Model <model@example.com>"),
    );
    assert_eq!(
        message,
        "fix(tools): correct path staging\n\n- stage relative to repoPath\n\n\
         Closes #12\nRefs: #34\nRefs: #56\n\n\
         Co-Authored-By: Model <model@example.com>\n"
    );
}

#[test]
fn assemble_message_body_only() {
    let message = assemble_message(
        "docs",
        None,
        "note the gate order",
        &[
            "branch gate first".to_string(),
            "then staged gate".to_string(),
        ],
        &[],
        &[],
        None,
    );
    assert_eq!(
        message,
        "docs: note the gate order\n\n- branch gate first\n- then staged gate\n"
    );
}

#[test]
fn assemble_message_skips_blank_lines_and_zero_issues() {
    let message = assemble_message(
        "feat",
        None,
        "add thing",
        &["real bullet".to_string(), "   ".to_string(), String::new()],
        &[0, 7],
        &[0],
        None,
    );
    assert_eq!(message, "feat: add thing\n\n- real bullet\n\nCloses #7\n");
}

#[test]
fn assemble_message_trailer_without_issues() {
    let message = assemble_message(
        "chore",
        None,
        "tidy things",
        &[],
        &[],
        &[],
        Some("Co-Authored-By: Model <model@example.com>"),
    );
    assert_eq!(
        message,
        "chore: tidy things\n\nCo-Authored-By: Model <model@example.com>\n"
    );
}

#[test]
fn assemble_message_issues_without_trailer() {
    let message = assemble_message("fix", None, "correct thing", &[], &[3], &[4], None);
    assert_eq!(message, "fix: correct thing\n\nCloses #3\nRefs: #4\n");
}

#[test]
fn normalize_repo_path_variants() {
    let root = Path::new("/repo");
    let base = Path::new("/repo/sub");
    assert_eq!(
        normalize_repo_path(root, root, "src/a.rs").unwrap(),
        "src/a.rs"
    );
    assert_eq!(normalize_repo_path(root, base, "a.rs").unwrap(), "sub/a.rs");
    assert_eq!(
        normalize_repo_path(root, root, "./src/a.rs").unwrap(),
        "src/a.rs"
    );
    assert_eq!(
        normalize_repo_path(root, base, "../src/a.rs").unwrap(),
        "src/a.rs"
    );
    assert_eq!(
        normalize_repo_path(root, root, "/repo/src/a.rs").unwrap(),
        "src/a.rs"
    );
    assert_eq!(normalize_repo_path(root, root, "").unwrap(), ".");
    assert!(normalize_repo_path(root, root, "../outside.rs").is_err());
    assert!(normalize_repo_path(root, root, "/etc/passwd").is_err());
}

// ---------- sign-off resolution ----------

#[tokio::test]
async fn signoff_field_literal_and_user() {
    let repo = init_repo();
    let ctx = make_ctx(repo.path());
    assert_eq!(
        resolve_signoff_field(repo.path(), &ctx, "name", Some("Literal Name"))
            .await
            .unwrap(),
        Some("Literal Name".to_string())
    );
    assert_eq!(
        resolve_signoff_field(repo.path(), &ctx, "name", Some(USER_PLACEHOLDER))
            .await
            .unwrap(),
        Some("Test User".to_string())
    );
    assert_eq!(
        resolve_signoff_field(repo.path(), &ctx, "email", Some(USER_PLACEHOLDER))
            .await
            .unwrap(),
        Some("test@example.com".to_string())
    );
    assert_eq!(
        resolve_signoff_field(repo.path(), &ctx, "name", None)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn signoff_field_user_unset_errors() {
    let _guard = jcode_base::storage::lock_test_env();
    // Hide system/global git config so `git config --get user.name` really is
    // unset even on hosts that configure a global identity.
    let prev_global = std::env::var_os("GIT_CONFIG_GLOBAL");
    let prev_nosystem = std::env::var_os("GIT_CONFIG_NOSYSTEM");
    let empty = tempfile::NamedTempFile::new().unwrap();
    jcode_base::env::set_var("GIT_CONFIG_GLOBAL", empty.path());
    jcode_base::env::set_var("GIT_CONFIG_NOSYSTEM", "1");

    let temp = tempfile::tempdir().unwrap();
    run_git(temp.path(), &["init", "-b", "main"]);
    let ctx = make_ctx(temp.path());
    let err = resolve_signoff_field(temp.path(), &ctx, "name", Some(USER_PLACEHOLDER))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("git config user.name is unset"));

    match prev_global {
        Some(value) => jcode_base::env::set_var("GIT_CONFIG_GLOBAL", value),
        None => jcode_base::env::remove_var("GIT_CONFIG_GLOBAL"),
    }
    match prev_nosystem {
        Some(value) => jcode_base::env::set_var("GIT_CONFIG_NOSYSTEM", value),
        None => jcode_base::env::remove_var("GIT_CONFIG_NOSYSTEM"),
    }
}

#[tokio::test]
async fn signoff_field_model_unresolved_errors() {
    let repo = init_repo();
    let ctx = make_ctx(repo.path());
    let err = resolve_signoff_field(repo.path(), &ctx, "name", Some(MODEL_PLACEHOLDER))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("${model} has not resolved"));
}

#[tokio::test]
async fn trailer_is_none_when_unconfigured() {
    let _jcode_home = EmptyJcodeHome::new();
    let repo = init_repo();
    let ctx = make_ctx(repo.path());
    // The test environment's config has no [tools.git] signoff configured.
    assert_eq!(resolve_trailer(repo.path(), &ctx).await.unwrap(), None);
}

// ---------- git_commit gates ----------

#[tokio::test]
async fn git_commit_rejects_paths_and_checkpoint() {
    let repo = init_repo();
    let ctx = make_ctx(repo.path());
    let err = GitCommitTool::new()
        .execute(
            json!({
                "type": "feat",
                "description": "x",
                "paths": ["a.rs"],
                "checkpoint": "stash@{0}",
            }),
            ctx,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not both"));
}

#[tokio::test]
async fn git_commit_rejects_bad_description_and_type_and_scope() {
    let repo = init_repo();
    for input in [
        json!({"type": "feat", "description": "Trailing period."}),
        json!({"type": "feat", "description": "Uppercase start"}),
        json!({"type": "feature", "description": "ok"}),
        json!({"type": "feat", "scope": "Bad Scope", "description": "ok"}),
    ] {
        let err = GitCommitTool::new()
            .execute(input, make_ctx(repo.path()))
            .await
            .unwrap_err();
        assert!(err.to_string().starts_with("git_commit: "), "{err}");
    }
}

#[tokio::test]
async fn git_commit_refuses_default_branch() {
    let _jcode_home = EmptyJcodeHome::new();
    let repo = init_repo();
    std::fs::write(repo.path().join("a.rs"), "fn a() {}\n").unwrap();
    let err = GitCommitTool::new()
        .execute(
            json!({
                "type": "feat",
                "description": "add a",
                "paths": ["a.rs"],
            }),
            make_ctx(repo.path()),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("default branch"),
        "unexpected error: {err}"
    );
    // allowDefaultBranch bypasses the gate.
    let out = GitCommitTool::new()
        .execute(
            json!({
                "type": "feat",
                "description": "add a",
                "paths": ["a.rs"],
                "allowDefaultBranch": true,
            }),
            make_ctx(repo.path()),
        )
        .await
        .unwrap();
    assert!(out.output.contains("on main"));
    assert_eq!(
        run_git(repo.path(), &["log", "-1", "--format=%s"]),
        "feat: add a"
    );
}

#[tokio::test]
async fn git_commit_refuses_foreign_staged() {
    let _jcode_home = EmptyJcodeHome::new();
    let repo = init_repo();
    run_git(repo.path(), &["checkout", "-b", "feature"]);
    std::fs::write(repo.path().join("foreign.rs"), "fn f() {}\n").unwrap();
    run_git(repo.path(), &["add", "foreign.rs"]);
    std::fs::write(repo.path().join("ours.rs"), "fn o() {}\n").unwrap();

    let err = GitCommitTool::new()
        .execute(
            json!({
                "type": "feat",
                "description": "add ours",
                "paths": ["ours.rs"],
            }),
            make_ctx(repo.path()),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("unrelated staged paths"),
        "unexpected error: {err}"
    );
    assert!(err.to_string().contains("foreign.rs"));

    // includePreStaged sweeps both into the commit.
    GitCommitTool::new()
        .execute(
            json!({
                "type": "feat",
                "description": "add ours and foreign",
                "paths": ["ours.rs"],
                "includePreStaged": true,
            }),
            make_ctx(repo.path()),
        )
        .await
        .unwrap();
    let files = run_git(repo.path(), &["show", "--name-only", "--format=", "HEAD"]);
    assert!(files.contains("ours.rs"));
    assert!(files.contains("foreign.rs"));
}

#[tokio::test]
async fn git_commit_refuses_empty_stage() {
    let repo = init_repo();
    run_git(repo.path(), &["checkout", "-b", "feature"]);
    let err = GitCommitTool::new()
        .execute(
            json!({"type": "feat", "description": "nothing here"}),
            make_ctx(repo.path()),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("nothing staged"));
}

#[tokio::test]
async fn git_commit_tolerates_null_and_blank_optionals() {
    let _jcode_home = EmptyJcodeHome::new();
    let repo = init_repo();
    run_git(repo.path(), &["checkout", "-b", "feature"]);
    std::fs::write(repo.path().join("a.rs"), "fn a() {}\n").unwrap();
    // Strict-normalized schemas mark every property required, so models fill
    // unused fields with null/"" — none may trip deserialization or the
    // paths/checkpoint mutual-exclusion gate.
    let out = GitCommitTool::new()
        .execute(
            json!({
                "type": "feat",
                "description": "add a",
                "paths": ["a.rs", "", "  "],
                "checkpoint": "",
                "scope": null,
                "body": null,
                "closes": null,
                "refs": null,
                "repoPath": null,
                "allowDefaultBranch": null,
                "includePreStaged": null,
                "signoff": null,
            }),
            make_ctx(repo.path()),
        )
        .await
        .unwrap();
    assert!(out.output.contains("on feature"), "{}", out.output);
    assert_eq!(
        run_git(repo.path(), &["log", "-1", "--format=%s"]),
        "feat: add a"
    );
    // Blank path elements must not normalize to "." and stage the whole repo.
    let files = run_git(repo.path(), &["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(files, "a.rs");
}

#[tokio::test]
async fn git_commit_empty_paths_do_not_conflict_with_checkpoint() {
    let _jcode_home = EmptyJcodeHome::new();
    let repo = init_repo();
    run_git(repo.path(), &["checkout", "-b", "feature"]);
    std::fs::write(repo.path().join("wip.rs"), "fn wip() {}\n").unwrap();
    let checkpoint_out = GitCheckpointTool::new()
        .execute(json!({"message": "wip"}), make_ctx(repo.path()))
        .await
        .unwrap();
    let selector = checkpoint_out.metadata.unwrap()["checkpoint"]
        .as_str()
        .unwrap()
        .to_string();

    let out = GitCommitTool::new()
        .execute(
            json!({
                "type": "feat",
                "description": "promote checkpoint",
                "paths": [],
                "checkpoint": selector,
            }),
            make_ctx(repo.path()),
        )
        .await
        .unwrap();
    assert_eq!(out.metadata.unwrap()["filesCommitted"], json!(1));
    assert_eq!(
        run_git(repo.path(), &["log", "-1", "--format=%s"]),
        "feat: promote checkpoint"
    );
}

#[tokio::test]
async fn git_commit_works_on_unborn_head() {
    let _jcode_home = EmptyJcodeHome::new();
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    run_git(root, &["init", "-b", "main"]);
    run_git(root, &["config", "user.name", "Test User"]);
    run_git(root, &["config", "user.email", "test@example.com"]);
    run_git(root, &["config", "init.defaultBranch", "main"]);
    std::fs::write(root.join("seed.rs"), "fn seed() {}\n").unwrap();

    let out = GitCommitTool::new()
        .execute(
            json!({
                "type": "chore",
                "description": "bootstrap",
                "paths": ["seed.rs"],
                "allowDefaultBranch": true,
            }),
            make_ctx(root),
        )
        .await
        .unwrap();
    assert!(out.output.contains("on main"), "{}", out.output);
    assert_eq!(
        run_git(root, &["log", "-1", "--format=%s"]),
        "chore: bootstrap"
    );
    assert_eq!(run_git(root, &["rev-list", "--count", "HEAD"]), "1");
}

#[tokio::test]
async fn git_commit_assembles_structured_message() {
    let _jcode_home = EmptyJcodeHome::new();
    let repo = init_repo();
    run_git(repo.path(), &["checkout", "-b", "feature"]);
    std::fs::write(repo.path().join("a.rs"), "fn a() {}\n").unwrap();
    let out = GitCommitTool::new()
        .execute(
            json!({
                "type": "feat",
                "scope": "tools",
                "description": "add structured commit tool",
                "paths": ["a.rs"],
                "body": ["structured fields in", "message assembled server-side"],
                "closes": [18],
                "refs": [1],
            }),
            make_ctx(repo.path()),
        )
        .await
        .unwrap();
    assert!(out.metadata.as_ref().unwrap()["filesCommitted"] == json!(1));
    let message = run_git(repo.path(), &["log", "-1", "--format=%B"]);
    assert_eq!(
        message,
        "feat(tools): add structured commit tool\n\n- structured fields in\n\
         - message assembled server-side\n\nCloses #18\nRefs: #1"
    );
}

// ---------- git_checkpoint ----------

#[tokio::test]
async fn git_checkpoint_snapshots_without_clearing() {
    let repo = init_repo();
    std::fs::write(repo.path().join("wip.rs"), "fn wip() {}\n").unwrap();
    let out = GitCheckpointTool::new()
        .execute(json!({"message": "wip: half done"}), make_ctx(repo.path()))
        .await
        .unwrap();
    assert!(out.output.contains("stash@{0}"));
    // Working tree is NOT cleared: the file stays on disk.
    assert!(repo.path().join("wip.rs").exists());
    // And it is back to untracked, not left staged.
    let staged = run_git(repo.path(), &["diff", "--cached", "--name-only"]);
    assert!(staged.is_empty(), "staged after checkpoint: {staged}");
}

#[tokio::test]
async fn git_checkpoint_then_commit_promotes() {
    let _jcode_home = EmptyJcodeHome::new();
    let repo = init_repo();
    run_git(repo.path(), &["checkout", "-b", "feature"]);
    std::fs::write(repo.path().join("new.rs"), "fn new() {}\n").unwrap();
    std::fs::write(repo.path().join("base.txt"), "base v2\n").unwrap();

    let checkpoint_out = GitCheckpointTool::new()
        .execute(json!({"message": "wip: work"}), make_ctx(repo.path()))
        .await
        .unwrap();
    let selector = checkpoint_out.metadata.as_ref().unwrap()["checkpoint"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(selector, "stash@{0}");

    // Diverge the worktree from the snapshot before promoting: new.rs gains
    // content the checkpoint never saw, base.txt goes back to its v1.
    std::fs::write(repo.path().join("new.rs"), "fn new() { /* diverged */ }\n").unwrap();
    run_git(repo.path(), &["checkout", "--", "base.txt"]);
    assert_eq!(
        std::fs::read_to_string(repo.path().join("new.rs")).unwrap(),
        "fn new() { /* diverged */ }\n"
    );

    let out = GitCommitTool::new()
        .execute(
            json!({
                "type": "feat",
                "description": "promote checkpoint",
                "checkpoint": selector,
            }),
            make_ctx(repo.path()),
        )
        .await
        .unwrap();
    let metadata = out.metadata.unwrap();
    assert_eq!(metadata["filesCommitted"], json!(2));
    assert_eq!(
        run_git(repo.path(), &["log", "-1", "--format=%s"]),
        "feat: promote checkpoint"
    );
    // The snapshot content — not the later v2 edit — was committed.
    assert_eq!(
        run_git(repo.path(), &["show", "HEAD:new.rs"]),
        "fn new() {}"
    );
    assert_eq!(run_git(repo.path(), &["show", "HEAD:base.txt"]), "base v2");
}

#[tokio::test]
async fn git_checkpoint_empty_message_rejected() {
    let repo = init_repo();
    let err = GitCheckpointTool::new()
        .execute(json!({"message": "  "}), make_ctx(repo.path()))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("non-empty"));
}

#[tokio::test]
async fn git_checkpoint_nothing_to_snapshot_errors() {
    let repo = init_repo();
    let err = GitCheckpointTool::new()
        .execute(json!({"message": "empty"}), make_ctx(repo.path()))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("nothing to snapshot"), "{err}");
}
