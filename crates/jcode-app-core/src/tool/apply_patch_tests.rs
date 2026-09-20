use super::*;
use std::io::Write;
use tempfile::NamedTempFile;

fn write_temp(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f
}

#[test]
fn test_parse_add_file() {
    let patch =
        "*** Begin Patch\n*** Add File: hello.txt\n+Hello world\n+Second line\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    assert_eq!(hunks.len(), 1);
    match &hunks[0] {
        PatchHunk::AddFile { path, contents } => {
            assert_eq!(path, "hello.txt");
            assert_eq!(contents, "Hello world\nSecond line\n");
        }
        _ => panic!("Expected AddFile"),
    }
}

#[test]
fn test_parse_delete_file() {
    let patch = "*** Begin Patch\n*** Delete File: old.txt\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    assert_eq!(hunks.len(), 1);
    match &hunks[0] {
        PatchHunk::DeleteFile { path } => {
            assert_eq!(path, "old.txt");
        }
        _ => panic!("Expected DeleteFile"),
    }
}

#[test]
fn test_parse_update_file_simple() {
    let patch = "*** Begin Patch\n*** Update File: test.py\n@@\n foo\n-bar\n+baz\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    assert_eq!(hunks.len(), 1);
    match &hunks[0] {
        PatchHunk::UpdateFile { path, chunks, .. } => {
            assert_eq!(path, "test.py");
            assert_eq!(chunks.len(), 1);
            assert_eq!(chunks[0].old_lines, vec!["foo", "bar"]);
            assert_eq!(chunks[0].new_lines, vec!["foo", "baz"]);
        }
        _ => panic!("Expected UpdateFile"),
    }
}

#[test]
fn test_parse_update_with_context() {
    let patch = "*** Begin Patch\n*** Update File: test.py\n@@ def my_func():\n-    pass\n+    return 42\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    match &hunks[0] {
        PatchHunk::UpdateFile { chunks, .. } => {
            assert_eq!(chunks[0].change_context, Some("def my_func():".to_string()));
            assert_eq!(chunks[0].old_lines, vec!["    pass"]);
            assert_eq!(chunks[0].new_lines, vec!["    return 42"]);
        }
        _ => panic!("Expected UpdateFile"),
    }
}

#[test]
fn test_parse_update_with_move() {
    let patch = "*** Begin Patch\n*** Update File: old.py\n*** Move to: new.py\n@@\n-old_line\n+new_line\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    match &hunks[0] {
        PatchHunk::UpdateFile {
            path,
            move_to,
            chunks,
        } => {
            assert_eq!(path, "old.py");
            assert_eq!(move_to, &Some("new.py".to_string()));
            assert_eq!(chunks.len(), 1);
        }
        _ => panic!("Expected UpdateFile"),
    }
}

#[test]
fn test_parse_multiple_chunks() {
    let patch = "*** Begin Patch\n*** Update File: test.py\n@@\n foo\n-bar\n+BAR\n@@\n baz\n-qux\n+QUX\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    match &hunks[0] {
        PatchHunk::UpdateFile { chunks, .. } => {
            assert_eq!(chunks.len(), 2);
            assert_eq!(chunks[0].old_lines, vec!["foo", "bar"]);
            assert_eq!(chunks[0].new_lines, vec!["foo", "BAR"]);
            assert_eq!(chunks[1].old_lines, vec!["baz", "qux"]);
            assert_eq!(chunks[1].new_lines, vec!["baz", "QUX"]);
        }
        _ => panic!("Expected UpdateFile"),
    }
}

#[test]
fn test_parse_end_of_file() {
    let patch = "*** Begin Patch\n*** Update File: test.py\n@@\n last_line\n+new_last_line\n*** End of File\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    match &hunks[0] {
        PatchHunk::UpdateFile { chunks, .. } => {
            assert!(chunks[0].is_end_of_file);
        }
        _ => panic!("Expected UpdateFile"),
    }
}

#[tokio::test]
async fn test_apply_update_simple() {
    let f = write_temp("foo\nbar\n");
    let chunks = vec![UpdateFileChunk {
        change_context: None,
        old_lines: vec!["foo".to_string(), "bar".to_string()],
        new_lines: vec!["foo".to_string(), "baz".to_string()],
        is_end_of_file: false,
    }];
    let (old_result, new_result) = apply_update_chunks(f.path(), &chunks).await.unwrap();
    assert_eq!(old_result, "foo\nbar\n");
    assert_eq!(new_result, "foo\nbaz\n");
}

#[tokio::test]
async fn test_apply_update_multiple_chunks() {
    let f = write_temp("foo\nbar\nbaz\nqux\n");
    let chunks = vec![
        UpdateFileChunk {
            change_context: None,
            old_lines: vec!["foo".to_string(), "bar".to_string()],
            new_lines: vec!["foo".to_string(), "BAR".to_string()],
            is_end_of_file: false,
        },
        UpdateFileChunk {
            change_context: None,
            old_lines: vec!["baz".to_string(), "qux".to_string()],
            new_lines: vec!["baz".to_string(), "QUX".to_string()],
            is_end_of_file: false,
        },
    ];
    let (old_result, new_result) = apply_update_chunks(f.path(), &chunks).await.unwrap();
    assert_eq!(old_result, "foo\nbar\nbaz\nqux\n");
    assert_eq!(new_result, "foo\nBAR\nbaz\nQUX\n");
}

#[tokio::test]
async fn test_apply_update_with_context_header() {
    let f = write_temp(
        "class Foo:\n    def bar(self):\n        pass\n    def baz(self):\n        pass\n",
    );
    let chunks = vec![UpdateFileChunk {
        change_context: Some("def baz(self):".to_string()),
        old_lines: vec!["        pass".to_string()],
        new_lines: vec!["        return 42".to_string()],
        is_end_of_file: false,
    }];
    let (_old_result, new_result) = apply_update_chunks(f.path(), &chunks).await.unwrap();
    assert_eq!(
        new_result,
        "class Foo:\n    def bar(self):\n        pass\n    def baz(self):\n        return 42\n"
    );
}

#[tokio::test]
async fn test_apply_update_append_at_eof() {
    let f = write_temp("foo\nbar\nbaz\n");
    let chunks = vec![UpdateFileChunk {
        change_context: None,
        old_lines: vec![],
        new_lines: vec!["quux".to_string()],
        is_end_of_file: false,
    }];
    let (_old_result, new_result) = apply_update_chunks(f.path(), &chunks).await.unwrap();
    assert_eq!(new_result, "foo\nbar\nbaz\nquux\n");
}

#[test]
fn test_generate_diff_summary_compact_format() {
    let old = "line one\nline two\nline three\n";
    let new = "line one\nchanged two\nline three\n";
    let diff = generate_diff_summary(old, new);

    assert!(diff.contains("2- line two"));
    assert!(diff.contains("2+ changed two"));
    assert!(!diff.contains("line one"));
}

#[test]
fn test_seek_sequence_exact() {
    let lines: Vec<String> = vec!["foo", "bar", "baz"]
        .into_iter()
        .map(String::from)
        .collect();
    let pattern: Vec<String> = vec!["bar", "baz"].into_iter().map(String::from).collect();
    assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(1));
}

#[test]
fn test_seek_sequence_whitespace_tolerant() {
    let lines: Vec<String> = vec!["foo   ", "bar\t"]
        .into_iter()
        .map(String::from)
        .collect();
    let pattern: Vec<String> = vec!["foo", "bar"].into_iter().map(String::from).collect();
    assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
}

#[test]
fn test_seek_sequence_eof() {
    let lines: Vec<String> = vec!["a", "b", "c", "d"]
        .into_iter()
        .map(String::from)
        .collect();
    let pattern: Vec<String> = vec!["c", "d"].into_iter().map(String::from).collect();
    assert_eq!(seek_sequence(&lines, &pattern, 0, true), Some(2));
}

#[test]
fn test_parse_no_begin() {
    let result = parse_apply_patch("random text");
    assert!(result.is_err());
}

#[test]
fn test_parse_heredoc_wrapper() {
    let patch = "<<'EOF'\n*** Begin Patch\n*** Add File: test.txt\n+hello\n*** End Patch\nEOF";
    let hunks = parse_apply_patch(patch).unwrap();
    assert_eq!(hunks.len(), 1);
}

#[test]
fn test_parse_update_without_explicit_at() {
    let patch = "*** Begin Patch\n*** Update File: file.py\n import foo\n+bar\n*** End Patch";
    let hunks = parse_apply_patch(patch).unwrap();
    match &hunks[0] {
        PatchHunk::UpdateFile { chunks, .. } => {
            assert_eq!(chunks.len(), 1);
            assert!(chunks[0].change_context.is_none());
        }
        _ => panic!("Expected UpdateFile"),
    }
}

// Issue #604: apply_patch can delete by absolute path, so it is a second route
// to the same damage the bash gate blocks. `ToolContext::resolve_path` passes
// absolute paths through unchanged, so nothing else bounds it.

#[tokio::test]
async fn apply_patch_refuses_to_delete_a_protected_path() {
    let temp = tempfile::tempdir().expect("temp home");
    let home = temp.path().to_path_buf();
    let previous = std::env::var("HOME").ok();
    // SAFETY: single-threaded test setup; restored below.
    unsafe { std::env::set_var("HOME", &home) };

    // A credential file inside the protected ~/.ssh directory.
    let ssh = home.join(".ssh");
    std::fs::create_dir_all(&ssh).expect("ssh dir");
    let key = ssh.join("id_ed25519");
    std::fs::write(&key, "PRIVATE KEY").expect("key");

    let patch = format!(
        "*** Begin Patch\n*** Delete File: {}\n*** End Patch",
        key.display()
    );
    let result = ApplyPatchTool
        .execute(
            serde_json::json!({ "patch_text": patch }),
            ToolContext {
                session_id: "patch-gate".to_string(),
                message_id: "m".to_string(),
                tool_call_id: "c".to_string(),
                working_dir: Some(std::path::PathBuf::from("/tmp")),
                stdin_request_tx: None,
                ask_user_question_tx: None,
                graceful_shutdown_signal: None,
                execution_mode: crate::tool::ToolExecutionMode::Direct,
            },
        )
        .await;

    match previous {
        Some(value) => unsafe { std::env::set_var("HOME", value) },
        None => unsafe { std::env::remove_var("HOME") },
    }

    let output = result.expect("the tool should report, not error out");
    assert!(
        format!("{output:?}").contains("refused"),
        "expected a refusal in the output: {output:?}"
    );
    assert!(
        key.exists(),
        "apply_patch must not delete a protected credential file"
    );
}

#[tokio::test]
async fn apply_patch_still_deletes_ordinary_files() {
    // The guard must not break the tool's normal job.
    let temp = tempfile::tempdir().expect("temp dir");
    let target = temp.path().join("obsolete.rs");
    std::fs::write(&target, "fn old() {}\n").expect("file");

    let patch = format!(
        "*** Begin Patch\n*** Delete File: {}\n*** End Patch",
        target.display()
    );
    ApplyPatchTool
        .execute(
            serde_json::json!({ "patch_text": patch }),
            ToolContext {
                session_id: "patch-ok".to_string(),
                message_id: "m".to_string(),
                tool_call_id: "c".to_string(),
                working_dir: Some(temp.path().to_path_buf()),
                stdin_request_tx: None,
                ask_user_question_tx: None,
                graceful_shutdown_signal: None,
                execution_mode: crate::tool::ToolExecutionMode::Direct,
            },
        )
        .await
        .expect("ordinary delete should succeed");

    assert!(!target.exists(), "an ordinary file should still be deleted");
}

#[test]
fn freeform_tool_declares_the_lark_grammar_marker() {
    let def = ApplyPatchFreeformTool::new().to_definition();
    assert_eq!(def.name, "apply_patch");
    let format = def
        .freeform_format()
        .expect("freeform variant must declare a grammar format");
    assert_eq!(format["type"], serde_json::json!("grammar"));
    assert_eq!(format["syntax"], serde_json::json!("lark"));
    assert!(
        format["definition"]
            .as_str()
            .is_some_and(|grammar| grammar.contains("*** Begin Patch")),
        "grammar must constrain the patch envelope: {format}"
    );
}

#[tokio::test]
async fn freeform_tool_executes_the_wrapped_raw_input() {
    // The stream layer wraps a custom_tool_call's raw payload into
    // `{"input": <raw>}`; the tool must apply that patch text directly.
    let temp = tempfile::tempdir().expect("temp dir");
    let target = temp.path().join("created.txt");
    let patch = format!(
        "*** Begin Patch\n*** Add File: {}\n+hello freeform\n*** End Patch",
        target.display()
    );
    let output = ApplyPatchFreeformTool::new()
        .execute(
            serde_json::json!({ "input": patch }),
            ToolContext {
                session_id: "patch-freeform".to_string(),
                message_id: "m".to_string(),
                tool_call_id: "c".to_string(),
                working_dir: Some(temp.path().to_path_buf()),
                stdin_request_tx: None,
                ask_user_question_tx: None,
                graceful_shutdown_signal: None,
                execution_mode: crate::tool::ToolExecutionMode::Direct,
            },
        )
        .await
        .expect("freeform apply_patch should succeed");
    assert!(
        std::fs::read_to_string(&target).unwrap() == "hello freeform\n",
        "patch content should be applied: {output:?}"
    );
}

#[tokio::test]
async fn freeform_tool_also_accepts_patch_text() {
    let temp = tempfile::tempdir().expect("temp dir");
    let target = temp.path().join("created.txt");
    let patch = format!(
        "*** Begin Patch\n*** Add File: {}\n+via patch_text\n*** End Patch",
        target.display()
    );
    ApplyPatchFreeformTool::new()
        .execute(
            serde_json::json!({ "patch_text": patch }),
            ToolContext {
                session_id: "patch-freeform-alias".to_string(),
                message_id: "m".to_string(),
                tool_call_id: "c".to_string(),
                working_dir: Some(temp.path().to_path_buf()),
                stdin_request_tx: None,
                ask_user_question_tx: None,
                graceful_shutdown_signal: None,
                execution_mode: crate::tool::ToolExecutionMode::Direct,
            },
        )
        .await
        .expect("patch_text fallback should succeed");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "via patch_text\n"
    );
}

#[tokio::test]
async fn freeform_tool_reports_missing_patch_text() {
    let temp = tempfile::tempdir().expect("temp dir");
    let err = ApplyPatchFreeformTool::new()
        .execute(
            serde_json::json!({}),
            ToolContext {
                session_id: "patch-freeform-empty".to_string(),
                message_id: "m".to_string(),
                tool_call_id: "c".to_string(),
                working_dir: Some(temp.path().to_path_buf()),
                stdin_request_tx: None,
                ask_user_question_tx: None,
                graceful_shutdown_signal: None,
                execution_mode: crate::tool::ToolExecutionMode::Direct,
            },
        )
        .await
        .expect_err("missing input must error");
    assert!(err.to_string().contains("input"), "{err}");
}

#[tokio::test]
async fn freeform_tool_prints_codex_summary_for_gpt_models() {
    // Codex's apply_patch ends with `Success. Updated the following
    // files:` plus one `A`/`M`/`D` line per path — no per-hunk lines, no
    // `File diff:` block. GPT-family sessions get that text verbatim so
    // the model sees the response it was trained on.
    let temp = tempfile::tempdir().expect("temp dir");
    std::fs::write(temp.path().join("mod.txt"), "alpha\nbeta\n").unwrap();
    std::fs::write(temp.path().join("del.txt"), "gone\n").unwrap();

    let session = "patch-codex-summary";
    crate::session_model::record_session_model(session, None, "GPT-5.4-Codex");
    let patch = "*** Begin Patch\n*** Add File: add.txt\n+new\n*** Update File: \
                 mod.txt\n@@\n-alpha\n+ALPHA\n*** Delete File: del.txt\n*** End Patch";
    let output = ApplyPatchFreeformTool::new()
        .execute(
            serde_json::json!({ "input": patch }),
            ToolContext {
                session_id: session.to_string(),
                message_id: "m".to_string(),
                tool_call_id: "c".to_string(),
                working_dir: Some(temp.path().to_path_buf()),
                stdin_request_tx: None,
                ask_user_question_tx: None,
                graceful_shutdown_signal: None,
                execution_mode: crate::tool::ToolExecutionMode::Direct,
            },
        )
        .await
        .expect("patch should apply");
    crate::session_model::forget_session_model(session);

    assert_eq!(
        output.output,
        "Success. Updated the following files:\nA add.txt\nM mod.txt\nD del.txt"
    );
}

#[tokio::test]
async fn non_gpt_freeform_and_compat_keep_legacy_summary() {
    // The byte-identical guarantee: a non-GPT freeform session and the
    // compat variant (even under a GPT session) both keep the ✓ lines
    // and the `File diff:` block.
    let temp = tempfile::tempdir().expect("temp dir");
    let patch = "*** Begin Patch\n*** Add File: add.txt\n+new\n*** End Patch";

    let ctx = |session: &str| ToolContext {
        session_id: session.to_string(),
        message_id: "m".to_string(),
        tool_call_id: "c".to_string(),
        working_dir: Some(temp.path().to_path_buf()),
        stdin_request_tx: None,
        ask_user_question_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    };

    crate::session_model::record_session_model("patch-legacy-model", None, "swe-2");
    let freeform = ApplyPatchFreeformTool::new()
        .execute(
            serde_json::json!({ "input": patch }),
            ctx("patch-legacy-model"),
        )
        .await
        .expect("freeform patch should apply");
    crate::session_model::forget_session_model("patch-legacy-model");
    assert!(
        freeform.output.contains("✓ add.txt: created"),
        "non-GPT sessions keep the legacy summary: {}",
        freeform.output
    );
    assert!(
        freeform.output.contains("File diff:"),
        "non-GPT sessions keep the diff block"
    );

    // Same model id check never runs on the compat variant at all.
    std::fs::remove_file(temp.path().join("add.txt")).unwrap();
    crate::session_model::record_session_model("patch-compat-gpt", None, "gpt-5.4");
    let compat = ApplyPatchTool
        .execute(
            serde_json::json!({ "patch_text": patch }),
            ctx("patch-compat-gpt"),
        )
        .await
        .expect("compat patch should apply");
    crate::session_model::forget_session_model("patch-compat-gpt");
    assert!(
        compat.output.contains("✓ add.txt: created"),
        "the compat variant keeps the legacy summary even for GPT models: {}",
        compat.output
    );
}
