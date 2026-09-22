use super::{Tool, ToolContext, ToolOutput};
use crate::bus::{Bus, BusEvent, FileOp, FileTouch};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};
use std::path::Path;

const FILE_TOUCH_PREVIEW_MAX_LINES: usize = 6;
const FILE_TOUCH_PREVIEW_MAX_BYTES: usize = 240;

pub struct ApplyPatchTool;

/// Freeform (Responses `custom` tool) variant of `apply_patch`: on the wire
/// it is declared with a Lark grammar so capable models emit the patch as raw
/// text instead of JSON. The raw payload arrives wrapped as `{"input": ...}`.
pub struct ApplyPatchFreeformTool;

impl ApplyPatchTool {
    pub fn new() -> Self {
        Self
    }
}

impl ApplyPatchFreeformTool {
    pub fn new() -> Self {
        Self
    }
}

/// Lark grammar constraining freeform apply_patch output, matching the
/// grammar codex-rs declares for its apply_patch custom tool.
const APPLY_PATCH_LARK_GRAMMAR: &str = r#"start: begin_patch hunk+ end_patch
begin_patch: "*** Begin Patch" LF
end_patch: "*** End Patch" LF?

hunk: add_hunk | delete_hunk | update_hunk
add_hunk: "*** Add File: " filename LF add_line+
delete_hunk: "*** Delete File: " filename LF
update_hunk: "*** Update File: " filename LF change_move? change?

filename: /(.+)/
add_line: "+" /(.*)/ LF -> line

change_move: "*** Move to: " filename LF
change: (change_context | change_line)+ eof_line?
change_context: ("@@" | "@@ " /(.+)/) LF
change_line: ("+" | "-" | " ") /(.*)/ LF
eof_line: "*** End of File" LF

%import common.LF
"#;

#[derive(Deserialize)]
struct ApplyPatchInput {
    #[serde(default)]
    intent: Option<String>,
    patch_text: String,
}

#[derive(Deserialize)]
struct ApplyPatchFreeformInput {
    #[serde(default)]
    intent: Option<String>,
    /// Raw patch text. Freeform calls arrive wrapped as `{"input": ...}`;
    /// `patch_text` is accepted so the same tool body works if the payload
    /// was normalized through the JSON path.
    #[serde(default)]
    input: Option<String>,
    #[serde(default)]
    patch_text: Option<String>,
}

#[derive(Debug, Clone)]
struct UpdateFileChunk {
    change_context: Option<String>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    is_end_of_file: bool,
}

#[derive(Debug)]
#[expect(
    clippy::enum_variant_names,
    reason = "patch variants intentionally mirror unified diff file-level operations for readability"
)]
enum PatchHunk {
    AddFile {
        path: String,
        contents: String,
    },
    DeleteFile {
        path: String,
    },
    UpdateFile {
        path: String,
        move_to: Option<String>,
        chunks: Vec<UpdateFileChunk>,
    },
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply a Codex-style *** Begin Patch / *** End Patch patch. Prefer over patch."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["patch_text"],
            "properties": {
                "intent": super::intent_schema_property(),
                "patch_text": {
                    "type": "string",
                    "description": "Patch text."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: ApplyPatchInput = serde_json::from_value(input)?;
        apply_patch_text(&params.patch_text, params.intent.as_deref(), &ctx, false).await
    }
}

#[async_trait]
impl Tool for ApplyPatchFreeformTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "The `apply_patch` tool can be used to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["input"],
            "properties": {
                "intent": super::intent_schema_property(),
                "input": {
                    "type": "string",
                    "description": "Patch text (freeform)."
                }
            },
            jcode_message_types::TOOL_FREEFORM_FORMAT_KEY: {
                "type": "grammar",
                "syntax": "lark",
                "definition": APPLY_PATCH_LARK_GRAMMAR,
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: ApplyPatchFreeformInput = serde_json::from_value(input)?;
        let patch_text = params
            .input
            .or(params.patch_text)
            .ok_or_else(|| anyhow::anyhow!("missing patch text: expected 'input' string"))?;
        // GPT models trained on codex's apply_patch expect its exact result
        // text: `Success. Updated the following files:` + A/M/D path lines,
        // no per-file hunks, no diff block. Detect by session model id.
        let codex_style = crate::session_model::session_model(&ctx.session_id)
            .is_some_and(|model| model.model.to_ascii_lowercase().contains("gpt"));
        apply_patch_text(&patch_text, params.intent.as_deref(), &ctx, codex_style).await
    }
}

async fn apply_patch_text(
    patch_text: &str,
    intent: Option<&str>,
    ctx: &ToolContext,
    codex_style: bool,
) -> Result<ToolOutput> {
    let hunks = parse_apply_patch(patch_text)?;

    // A patch can reach config.toml through any hunk kind (add, update,
    // move), so watch the file across the whole invocation rather than
    // threading before/after content through each branch.
    let config_watch = super::config_edit_notice::ConfigEditWatch::begin();

    // Capture whole-file states, including move destinations and AddFile
    // overwrites. Diff the final state so repeated hunks share one coordinate
    // system and failed operations cannot produce a speculative preview.
    let mut before = std::collections::BTreeMap::new();
    for hunk in &hunks {
        let (path, destination) = match hunk {
            PatchHunk::AddFile { path, .. } | PatchHunk::DeleteFile { path } => (path, None),
            PatchHunk::UpdateFile { path, move_to, .. } => (path, move_to.as_ref()),
        };
        for path in std::iter::once(path).chain(destination) {
            if !before.contains_key(path) {
                let resolved = ctx.resolve_path(Path::new(path));
                before.insert(path.clone(), super::file_diff::snapshot(&resolved).await);
            }
        }
    }

    let mut results = Vec::new();
    let mut touched_paths = Vec::new();
    // Codex-style summary bookkeeping: per-path A/M/D groups plus plain
    // failure lines (codex reports failures as errors, never `✗`).
    let mut added_paths: Vec<String> = Vec::new();
    let mut modified_paths: Vec<String> = Vec::new();
    let mut deleted_paths: Vec<String> = Vec::new();
    let mut failure_lines: Vec<String> = Vec::new();

    for hunk in &hunks {
        match hunk {
            PatchHunk::AddFile { path, contents } => {
                let resolved = ctx.resolve_path(Path::new(path));
                if let Some(parent) = resolved.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                let existed = resolved.exists();
                let old = tokio::fs::read_to_string(&resolved).await.ok();
                tokio::fs::write(&resolved, contents).await?;
                super::edit_stats::record(
                    &ctx,
                    old.as_deref().unwrap_or(""),
                    contents,
                    existed && old.is_none(),
                )
                .await;
                let diff = generate_diff_summary("", contents);
                publish_file_touch(&ctx, &resolved, path, "created", &diff, intent);
                touched_paths.push(path.clone());
                if diff.is_empty() {
                    results.push(format!("✓ {}: created", path));
                } else {
                    results.push(format!("✓ {}: created\n{}", path, diff));
                }
                added_paths.push(path.clone());
            }
            PatchHunk::DeleteFile { path } => {
                let resolved = ctx.resolve_path(Path::new(path));
                // `resolve_path` passes absolute paths through unchanged, so
                // a patch can name any file on disk. The bash gate does not
                // cover this path, so apply the same absolute deny here
                // (#604). Only the catastrophic tier: ordinary file deletes
                // are this tool's normal job.
                let risk_ctx = jcode_command_risk::RiskContext::from_env(ctx.working_dir.clone());
                if jcode_command_risk::is_catastrophic_target(&resolved, &risk_ctx) {
                    failure_lines.push(format!(
                        "{}: refused, this path is protected and must never \
                             be deleted by an agent",
                        path
                    ));
                    results.push(format!(
                        "✗ {}: refused, this path is protected and must never \
                             be deleted by an agent",
                        path
                    ));
                    continue;
                }
                let old = tokio::fs::read_to_string(&resolved).await.ok();
                let old_contents = old.as_deref().unwrap_or("");
                if tokio::fs::remove_file(&resolved).await.is_ok() {
                    super::edit_stats::record(&ctx, old_contents, "", old.is_none()).await;
                    let diff = generate_diff_summary(&old_contents, "");
                    publish_file_touch(&ctx, &resolved, path, "deleted", &diff, intent);
                    touched_paths.push(path.clone());
                    if diff.is_empty() {
                        results.push(format!("✓ {}: deleted", path));
                    } else {
                        results.push(format!("✓ {}: deleted\n{}", path, diff));
                    }
                    deleted_paths.push(path.clone());
                } else {
                    failure_lines.push(format!("{}: failed to delete", path));
                    results.push(format!("✗ {}: failed to delete", path));
                }
            }
            PatchHunk::UpdateFile {
                path,
                move_to,
                chunks,
            } => {
                let resolved = ctx.resolve_path(Path::new(path));
                match apply_update_chunks(&resolved, chunks).await {
                    Ok((old_contents, new_contents)) => {
                        let diff = generate_diff_summary(&old_contents, &new_contents);
                        if let Some(dest) = move_to {
                            let dest_resolved = ctx.resolve_path(Path::new(dest));
                            if let Some(parent) = dest_resolved.parent() {
                                tokio::fs::create_dir_all(parent).await?;
                            }
                            let dest_existed = dest_resolved.exists();
                            let dest_old = tokio::fs::read_to_string(&dest_resolved).await.ok();
                            tokio::fs::write(&dest_resolved, &new_contents).await?;
                            if tokio::fs::remove_file(&resolved).await.is_ok() {
                                super::edit_stats::record(
                                    &ctx,
                                    &old_contents,
                                    &new_contents,
                                    false,
                                )
                                .await;
                                if dest_existed {
                                    super::edit_stats::record(
                                        &ctx,
                                        dest_old.as_deref().unwrap_or(""),
                                        "",
                                        dest_old.is_none(),
                                    )
                                    .await;
                                }
                                publish_file_touch(
                                    &ctx, &resolved, path, "modified", &diff, intent,
                                );
                                publish_file_touch(
                                    &ctx,
                                    &dest_resolved,
                                    dest,
                                    "modified",
                                    &diff,
                                    intent,
                                );
                                touched_paths.push(path.clone());
                                touched_paths.push(dest.clone());
                                if diff.is_empty() {
                                    results.push(format!(
                                        "✓ {}: modified ({} hunks), moved to {}",
                                        path,
                                        chunks.len(),
                                        dest
                                    ));
                                } else {
                                    results.push(format!(
                                        "✓ {}: modified ({} hunks), moved to {}\n{}",
                                        path,
                                        chunks.len(),
                                        dest,
                                        diff
                                    ));
                                }
                                // Codex classifies a move as `M <source>` — the
                                // source path is pushed for both branches.
                                modified_paths.push(path.clone());
                            } else {
                                // The destination write already happened, so
                                // the stats reflect it, but the move is a
                                // failure: codex reports "Failed to remove
                                // original" rather than claiming a move that
                                // left a duplicate behind.
                                super::edit_stats::record(
                                    &ctx,
                                    dest_old.as_deref().unwrap_or(""),
                                    &new_contents,
                                    dest_existed && dest_old.is_none(),
                                )
                                .await;
                                publish_file_touch(
                                    &ctx,
                                    &dest_resolved,
                                    dest,
                                    "modified",
                                    &diff,
                                    intent,
                                );
                                touched_paths.push(dest.clone());
                                let failure = format!(
                                    "{path}: failed to remove original after writing {dest}"
                                );
                                failure_lines.push(failure.clone());
                                results.push(format!("✗ {failure}"));
                            }
                        } else {
                            tokio::fs::write(&resolved, &new_contents).await?;
                            super::edit_stats::record(&ctx, &old_contents, &new_contents, false)
                                .await;
                            publish_file_touch(&ctx, &resolved, path, "modified", &diff, intent);
                            touched_paths.push(path.clone());
                            if diff.is_empty() {
                                results.push(format!(
                                    "✓ {}: modified ({} hunks)",
                                    path,
                                    chunks.len()
                                ));
                            } else {
                                results.push(format!(
                                    "✓ {}: modified ({} hunks)\n{}",
                                    path,
                                    chunks.len(),
                                    diff
                                ));
                            }
                            // Codex classifies a plain update as `M <path>`.
                            modified_paths.push(path.clone());
                        }
                    }
                    Err(e) => {
                        failure_lines.push(format!("{}: {}", path, e));
                        results.push(format!("✗ {}: {}", path, e));
                    }
                }
            }
        }
    }

    if results.is_empty() {
        Ok(ToolOutput::new("No changes applied"))
    } else {
        let mut body = if codex_style {
            // Codex's apply_patch prints a git-style summary only:
            // `Success. Updated the following files:` then one `A`/`M`/`D`
            // line per affected path (moves count as `M` on the source
            // path). Failures surface as plain error lines; the per-file
            // ✓/✗ lines, hunk counts and the unified `File diff:` block
            // are dropped entirely.
            let mut lines = Vec::new();
            if !added_paths.is_empty() || !modified_paths.is_empty() || !deleted_paths.is_empty() {
                lines.push("Success. Updated the following files:".to_string());
                for path in &added_paths {
                    lines.push(format!("A {path}"));
                }
                for path in &modified_paths {
                    lines.push(format!("M {path}"));
                }
                for path in &deleted_paths {
                    lines.push(format!("D {path}"));
                }
            }
            for failure in &failure_lines {
                lines.push(format!("error: {failure}"));
            }
            lines.join("\n")
        } else {
            results.join("\n")
        };
        config_watch.finish(&mut body);
        if codex_style {
            let output = ToolOutput::new(body);
            return Ok(if touched_paths.len() == 1 {
                output.with_title(touched_paths[0].clone())
            } else {
                output.with_title(format!("{} files", touched_paths.len()))
            });
        }
        let mut unified = String::new();
        let mut after = std::collections::BTreeMap::new();
        for path in before.keys() {
            after.insert(
                path.clone(),
                super::file_diff::snapshot(&ctx.resolve_path(Path::new(path))).await,
            );
        }
        let mut combined = std::collections::BTreeSet::new();
        // A simple successful move to a new path can retain the source's
        // coordinates. For overwrites or move chains, keep net per-path
        // diffs instead of hiding destination text that was overwritten.
        for hunk in &hunks {
            if let PatchHunk::UpdateFile {
                path,
                move_to: Some(dest),
                ..
            } = hunk
                && let (
                    Some(Some((true, old))),
                    Some(Some((false, _))),
                    Some(Some((false, _))),
                    Some(Some((true, new))),
                ) = (
                    before.get(path),
                    before.get(dest),
                    after.get(path),
                    after.get(dest),
                )
                && !combined.contains(path)
                && !combined.contains(dest)
            {
                unified.push_str(&super::file_diff::unified(path, dest, old, new));
                combined.insert(path.clone());
                combined.insert(dest.clone());
            }
        }
        for (path, old) in before {
            if combined.contains(&path) {
                continue;
            }
            if let (Some((old_exists, old)), Some(Some((new_exists, new)))) =
                (old, after.remove(&path))
            {
                unified.push_str(&super::file_diff::unified(
                    if old_exists || !new_exists {
                        &path
                    } else {
                        "/dev/null"
                    },
                    if new_exists || !old_exists {
                        &path
                    } else {
                        "/dev/null"
                    },
                    &old,
                    &new,
                ));
            }
        }
        let output = super::file_diff::attach(ToolOutput::new(body), unified);
        if touched_paths.len() == 1 {
            Ok(output.with_title(touched_paths[0].clone()))
        } else {
            Ok(output.with_title(format!("{} files", touched_paths.len())))
        }
    }
}

fn publish_file_touch(
    ctx: &ToolContext,
    resolved: &Path,
    display_path: &str,
    verb: &str,
    diff: &str,
    intent: Option<&str>,
) {
    let detail = build_file_touch_preview(diff);
    Bus::global().publish(BusEvent::FileTouch(FileTouch {
        session_id: ctx.session_id.clone(),
        path: resolved.to_path_buf(),
        op: FileOp::Edit,
        intent: intent
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        summary: Some(format!("{} via apply_patch", verb)),
        detail,
    }));
    let _ = display_path;
}

fn build_file_touch_preview(diff: &str) -> Option<String> {
    let trimmed = diff.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut lines = trimmed.lines();
    let mut preview = lines
        .by_ref()
        .take(FILE_TOUCH_PREVIEW_MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    let mut truncated = lines.next().is_some();

    if preview.len() > FILE_TOUCH_PREVIEW_MAX_BYTES {
        preview = crate::util::truncate_str(&preview, FILE_TOUCH_PREVIEW_MAX_BYTES)
            .trim_end()
            .to_string();
        truncated = true;
    }

    if truncated {
        preview.push_str("\n…");
    }

    Some(preview)
}

async fn apply_update_chunks(path: &Path, chunks: &[UpdateFileChunk]) -> Result<(String, String)> {
    let original_contents = tokio::fs::read_to_string(path).await?;
    let mut original_lines: Vec<String> = original_contents.split('\n').map(String::from).collect();

    if original_lines.last().is_some_and(String::is_empty) {
        original_lines.pop();
    }

    let replacements = compute_replacements(&original_lines, path, chunks)?;
    let mut new_lines = apply_replacements(original_lines, &replacements);

    if !new_lines.last().is_some_and(String::is_empty) {
        new_lines.push(String::new());
    }
    Ok((original_contents, new_lines.join("\n")))
}

/// Generate a compact diff with line numbers (max 30 lines).
fn generate_diff_summary(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();
    let mut line_count = 0;
    const MAX_LINES: usize = 30;

    let mut old_line = 1usize;
    let mut new_line = 1usize;

    for change in diff.iter_all_changes() {
        if line_count >= MAX_LINES {
            output.push_str("... (diff truncated)\n");
            break;
        }

        let content = change.value().trim_end_matches('\n');
        let (prefix, line_num) = match change.tag() {
            ChangeTag::Delete => {
                let num = old_line;
                old_line += 1;
                if content.trim().is_empty() {
                    continue;
                }
                ("-", num)
            }
            ChangeTag::Insert => {
                let num = new_line;
                new_line += 1;
                if content.trim().is_empty() {
                    continue;
                }
                ("+", num)
            }
            ChangeTag::Equal => {
                old_line += 1;
                new_line += 1;
                continue;
            }
        };

        output.push_str(&format!("{}{} {}\n", line_num, prefix, content));
        line_count += 1;
    }

    output.trim_end().to_string()
}

fn compute_replacements(
    original_lines: &[String],
    path: &Path,
    chunks: &[UpdateFileChunk],
) -> Result<Vec<(usize, usize, Vec<String>)>> {
    let mut replacements: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut line_index: usize = 0;

    for chunk in chunks {
        if let Some(ctx_line) = &chunk.change_context {
            if let Some(idx) = seek_sequence(
                original_lines,
                std::slice::from_ref(ctx_line),
                line_index,
                false,
            ) {
                line_index = idx + 1;
            } else {
                anyhow::bail!(
                    "Failed to find context '{}' in {}",
                    ctx_line,
                    path.display()
                );
            }
        }

        if chunk.old_lines.is_empty() {
            let insertion_idx = if original_lines.last().is_some_and(String::is_empty) {
                original_lines.len() - 1
            } else {
                original_lines.len()
            };
            replacements.push((insertion_idx, 0, chunk.new_lines.clone()));
            continue;
        }

        let mut pattern: &[String] = &chunk.old_lines;
        let mut found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);

        let mut new_slice: &[String] = &chunk.new_lines;

        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new_slice.last().is_some_and(String::is_empty) {
                new_slice = &new_slice[..new_slice.len() - 1];
            }
            found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
        }

        if let Some(start_idx) = found {
            replacements.push((start_idx, pattern.len(), new_slice.to_vec()));
            line_index = start_idx + pattern.len();
        } else {
            anyhow::bail!(
                "Failed to find expected lines in {}:\n{}",
                path.display(),
                chunk.old_lines.join("\n"),
            );
        }
    }

    replacements.sort_by(|(a, _, _), (b, _, _)| a.cmp(b));
    Ok(replacements)
}

fn apply_replacements(
    mut lines: Vec<String>,
    replacements: &[(usize, usize, Vec<String>)],
) -> Vec<String> {
    for (start_idx, old_len, new_segment) in replacements.iter().rev() {
        let start_idx = *start_idx;
        let old_len = *old_len;

        for _ in 0..old_len {
            if start_idx < lines.len() {
                lines.remove(start_idx);
            }
        }

        for (offset, new_line) in new_segment.iter().enumerate() {
            lines.insert(start_idx + offset, new_line.clone());
        }
    }

    lines
}

fn seek_sequence(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }

    if pattern.len() > lines.len() {
        return None;
    }

    let search_start = if eof && lines.len() >= pattern.len() {
        lines.len() - pattern.len()
    } else {
        start
    };

    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        if lines[i..i + pattern.len()] == *pattern {
            return Some(i);
        }
    }

    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut ok = true;
        for (p_idx, pat) in pattern.iter().enumerate() {
            if lines[i + p_idx].trim_end() != pat.trim_end() {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }

    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut ok = true;
        for (p_idx, pat) in pattern.iter().enumerate() {
            if lines[i + p_idx].trim() != pat.trim() {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }

    None
}

/// Parser modes, mirroring codex's `StreamingParserMode` so the accepted
/// grammar and the rejection behavior match codex's apply_patch exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatchParseMode {
    NotStarted,
    StartedPatch,
    AddFile,
    DeleteFile,
    UpdateFile { hunk_line_number: usize },
    EndedPatch,
}

const BEGIN_PATCH_MARKER: &str = "*** Begin Patch";
const END_PATCH_MARKER: &str = "*** End Patch";
const ADD_FILE_MARKER: &str = "*** Add File: ";
const DELETE_FILE_MARKER: &str = "*** Delete File: ";
const UPDATE_FILE_MARKER: &str = "*** Update File: ";
const MOVE_TO_MARKER: &str = "*** Move to: ";
const EOF_MARKER: &str = "*** End of File";
const ENVIRONMENT_ID_MARKER: &str = "*** Environment ID:";
const CHANGE_CONTEXT_MARKER: &str = "@@ ";
const EMPTY_CHANGE_CONTEXT_MARKER: &str = "@@";

fn invalid_hunk(line_number: usize, message: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("invalid hunk at line {line_number}, {message}")
}

fn invalid_hunk_header(line_number: usize, line: &str) -> anyhow::Error {
    invalid_hunk(
        line_number,
        format!(
            "'{line}' is not a valid hunk header. Valid hunk headers: \
             '*** Add File: {{path}}', '*** Delete File: {{path}}', \
             '*** Update File: {{path}}'"
        ),
    )
}

fn unexpected_update_line(line_number: usize, line: &str) -> anyhow::Error {
    invalid_hunk(
        line_number,
        format!(
            "Unexpected line found in update hunk: '{line}'. Every line should \
             start with ' ' (context line), '+' (added line), or '-' (removed line)"
        ),
    )
}

/// A hunk header or `*** End Patch` arriving while the previous update hunk
/// has no chunks (or an empty trailing chunk) is a parse error in codex, not
/// a silent hunk boundary.
fn ensure_update_hunk_is_not_empty(
    hunks: &[PatchHunk],
    mode: PatchParseMode,
    line: &str,
    line_number: usize,
) -> Result<()> {
    if let Some(PatchHunk::UpdateFile { path, chunks, .. }) = hunks.last() {
        if chunks.is_empty()
            && let PatchParseMode::UpdateFile { hunk_line_number } = mode
        {
            return Err(invalid_hunk(
                hunk_line_number,
                format!("Update file hunk for path '{path}' is empty"),
            ));
        }
        if chunks
            .last()
            .is_some_and(|chunk| chunk.old_lines.is_empty() && chunk.new_lines.is_empty())
        {
            if line == END_PATCH_MARKER {
                return Err(invalid_hunk(
                    line_number,
                    "Update hunk does not contain any lines",
                ));
            }
            return Err(unexpected_update_line(line_number, line));
        }
    }
    Ok(())
}

/// Returns true when `trimmed` was consumed as a hunk header or `*** End
/// Patch`, pushing the new hunk / updating `mode` as codex does.
fn handle_hunk_headers_and_end_patch(
    hunks: &mut Vec<PatchHunk>,
    mode: &mut PatchParseMode,
    environment_id: &mut Option<String>,
    trimmed: &str,
    line_number: usize,
) -> Result<bool> {
    // Codex parity: `*** Environment ID:` is legal once, right after
    // `*** Begin Patch`. jcode is a single-environment tool so the value is
    // validated then discarded; rejecting a legal codex patch here would be a
    // parse regression.
    if matches!(*mode, PatchParseMode::StartedPatch)
        && let Some(value) = trimmed.strip_prefix(ENVIRONMENT_ID_MARKER)
    {
        if environment_id.is_some() {
            anyhow::bail!(
                "invalid patch: apply_patch environment_id cannot be specified more than once"
            );
        }
        let value = value.trim();
        if value.is_empty() {
            anyhow::bail!("invalid patch: apply_patch environment_id cannot be empty");
        }
        *environment_id = Some(value.to_string());
        return Ok(true);
    }
    if trimmed == END_PATCH_MARKER {
        ensure_update_hunk_is_not_empty(hunks, *mode, trimmed, line_number)?;
        *mode = PatchParseMode::EndedPatch;
        return Ok(true);
    }
    if let Some(path) = trimmed.strip_prefix(ADD_FILE_MARKER) {
        ensure_update_hunk_is_not_empty(hunks, *mode, trimmed, line_number)?;
        hunks.push(PatchHunk::AddFile {
            path: path.trim().to_string(),
            contents: String::new(),
        });
        *mode = PatchParseMode::AddFile;
        return Ok(true);
    }
    if let Some(path) = trimmed.strip_prefix(DELETE_FILE_MARKER) {
        ensure_update_hunk_is_not_empty(hunks, *mode, trimmed, line_number)?;
        hunks.push(PatchHunk::DeleteFile {
            path: path.trim().to_string(),
        });
        *mode = PatchParseMode::DeleteFile;
        return Ok(true);
    }
    if let Some(path) = trimmed.strip_prefix(UPDATE_FILE_MARKER) {
        ensure_update_hunk_is_not_empty(hunks, *mode, trimmed, line_number)?;
        hunks.push(PatchHunk::UpdateFile {
            path: path.trim().to_string(),
            move_to: None,
            chunks: Vec::new(),
        });
        *mode = PatchParseMode::UpdateFile {
            hunk_line_number: line_number,
        };
        return Ok(true);
    }
    Ok(false)
}

/// Verifies the first and last lines are the patch envelope markers, like
/// codex's strict boundary check. On failure, retries once after unwrapping
/// a `<<EOF`/`EOF` heredoc, matching codex's lenient mode for shell-invoked
/// patches.
fn check_patch_boundaries<'a>(lines: &'a [&'a str]) -> Result<&'a [&'a str]> {
    match check_start_and_end_lines(lines) {
        Ok(()) => Ok(lines),
        Err(original_error) => {
            if let [first, .., last] = lines
                && (*first == "<<EOF" || *first == "<<'EOF'" || *first == "<<\"EOF\"")
                && last.ends_with("EOF")
                && lines.len() >= 4
            {
                let inner = &lines[1..lines.len() - 1];
                return check_start_and_end_lines(inner).map(|()| inner);
            }
            Err(original_error)
        }
    }
}

fn check_start_and_end_lines(lines: &[&str]) -> Result<()> {
    let first = lines.first().map(|line| line.trim());
    let last = lines.last().map(|line| line.trim());
    match (first, last) {
        (Some(first), Some(last)) if first == BEGIN_PATCH_MARKER && last == END_PATCH_MARKER => {
            Ok(())
        }
        (Some(first), _) if first != BEGIN_PATCH_MARKER => {
            anyhow::bail!("invalid patch: The first line of the patch must be '*** Begin Patch'")
        }
        _ => anyhow::bail!("invalid patch: The last line of the patch must be '*** End Patch'"),
    }
}

fn parse_apply_patch(input: &str) -> Result<Vec<PatchHunk>> {
    let all_lines: Vec<&str> = input.trim().lines().collect();
    let lines = check_patch_boundaries(&all_lines)?;

    let mut hunks: Vec<PatchHunk> = Vec::new();
    let mut mode = PatchParseMode::NotStarted;
    // Parsed for codex parity and discarded: jcode applies to one environment.
    let mut environment_id: Option<String> = None;

    for (index, raw_line) in lines.iter().enumerate() {
        let line_number = index + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let trimmed = line.trim();

        match mode {
            PatchParseMode::NotStarted => {
                if trimmed == BEGIN_PATCH_MARKER {
                    mode = PatchParseMode::StartedPatch;
                } else {
                    return Err(anyhow::anyhow!(
                        "invalid patch: The first line of the patch must be '*** Begin Patch'"
                    ));
                }
            }
            PatchParseMode::StartedPatch => {
                if !handle_hunk_headers_and_end_patch(
                    &mut hunks,
                    &mut mode,
                    &mut environment_id,
                    trimmed,
                    line_number,
                )? {
                    return Err(invalid_hunk_header(line_number, trimmed));
                }
            }
            PatchParseMode::AddFile => {
                if handle_hunk_headers_and_end_patch(
                    &mut hunks,
                    &mut mode,
                    &mut environment_id,
                    trimmed,
                    line_number,
                )? {
                    continue;
                }
                if let Some(added) = line.strip_prefix('+')
                    && let Some(PatchHunk::AddFile { contents, .. }) = hunks.last_mut()
                {
                    contents.push_str(added);
                    contents.push('\n');
                    continue;
                }
                // A bare content line used to be skipped silently, which
                // turned a malformed patch into an empty file that still
                // reported success.
                if trimmed.starts_with("*** ") {
                    return Err(invalid_hunk_header(line_number, trimmed));
                }
                return Err(invalid_hunk(
                    line_number,
                    format!(
                        "'{trimmed}' is not a valid line in an Add File hunk: \
                         every added line must start with '+'"
                    ),
                ));
            }
            PatchParseMode::DeleteFile => {
                if !handle_hunk_headers_and_end_patch(
                    &mut hunks,
                    &mut mode,
                    &mut environment_id,
                    trimmed,
                    line_number,
                )? {
                    return Err(invalid_hunk_header(line_number, trimmed));
                }
            }
            PatchParseMode::UpdateFile { hunk_line_number } => {
                let update_line = line.trim_end();
                if handle_hunk_headers_and_end_patch(
                    &mut hunks,
                    &mut mode,
                    &mut environment_id,
                    update_line,
                    line_number,
                )? {
                    continue;
                }

                let Some(PatchHunk::UpdateFile {
                    move_to, chunks, ..
                }) = hunks.last_mut()
                else {
                    return Err(invalid_hunk(line_number, "update hunk state is missing"));
                };

                if chunks.last().is_some_and(|chunk| chunk.is_end_of_file) {
                    if update_line.is_empty() {
                        continue;
                    }
                    if update_line != EMPTY_CHANGE_CONTEXT_MARKER
                        && !update_line.starts_with(CHANGE_CONTEXT_MARKER)
                    {
                        return Err(invalid_hunk(
                            line_number,
                            format!(
                                "Expected update hunk to start with a @@ context marker, \
                                 got: '{line}'"
                            ),
                        ));
                    }
                }

                if chunks.is_empty()
                    && move_to.is_none()
                    && let Some(dest) = update_line.strip_prefix(MOVE_TO_MARKER)
                {
                    *move_to = Some(dest.trim().to_string());
                    mode = PatchParseMode::UpdateFile { hunk_line_number };
                    continue;
                }

                if (update_line == EMPTY_CHANGE_CONTEXT_MARKER
                    || update_line.starts_with(CHANGE_CONTEXT_MARKER))
                    && chunks.last().is_some_and(|chunk| {
                        chunk.old_lines.is_empty() && chunk.new_lines.is_empty()
                    })
                {
                    return Err(unexpected_update_line(line_number, line));
                }

                if update_line == EMPTY_CHANGE_CONTEXT_MARKER {
                    chunks.push(UpdateFileChunk {
                        change_context: None,
                        old_lines: Vec::new(),
                        new_lines: Vec::new(),
                        is_end_of_file: false,
                    });
                    continue;
                }

                if let Some(change_context) = update_line.strip_prefix(CHANGE_CONTEXT_MARKER) {
                    chunks.push(UpdateFileChunk {
                        change_context: Some(change_context.to_string()),
                        old_lines: Vec::new(),
                        new_lines: Vec::new(),
                        is_end_of_file: false,
                    });
                    continue;
                }

                if update_line == EOF_MARKER {
                    if chunks.last().is_some_and(|chunk| {
                        chunk.old_lines.is_empty() && chunk.new_lines.is_empty()
                    }) {
                        return Err(invalid_hunk(
                            line_number,
                            "Update hunk does not contain any lines",
                        ));
                    }
                    if let Some(chunk) = chunks.last_mut() {
                        chunk.is_end_of_file = true;
                    }
                    continue;
                }

                if line.is_empty() {
                    if chunks.is_empty() {
                        chunks.push(UpdateFileChunk {
                            change_context: None,
                            old_lines: Vec::new(),
                            new_lines: Vec::new(),
                            is_end_of_file: false,
                        });
                    }
                    if let Some(chunk) = chunks.last_mut() {
                        chunk.old_lines.push(String::new());
                        chunk.new_lines.push(String::new());
                    }
                    continue;
                }

                if let Some(content) = line.strip_prefix(' ') {
                    if chunks.is_empty() {
                        chunks.push(UpdateFileChunk {
                            change_context: None,
                            old_lines: Vec::new(),
                            new_lines: Vec::new(),
                            is_end_of_file: false,
                        });
                    }
                    if let Some(chunk) = chunks.last_mut() {
                        chunk.old_lines.push(content.to_string());
                        chunk.new_lines.push(content.to_string());
                    }
                    continue;
                }

                if let Some(content) = line.strip_prefix('+') {
                    if chunks.is_empty() {
                        chunks.push(UpdateFileChunk {
                            change_context: None,
                            old_lines: Vec::new(),
                            new_lines: Vec::new(),
                            is_end_of_file: false,
                        });
                    }
                    if let Some(chunk) = chunks.last_mut() {
                        chunk.new_lines.push(content.to_string());
                    }
                    continue;
                }

                if let Some(content) = line.strip_prefix('-') {
                    if chunks.is_empty() {
                        chunks.push(UpdateFileChunk {
                            change_context: None,
                            old_lines: Vec::new(),
                            new_lines: Vec::new(),
                            is_end_of_file: false,
                        });
                    }
                    if let Some(chunk) = chunks.last_mut() {
                        chunk.old_lines.push(content.to_string());
                    }
                    continue;
                }

                if chunks
                    .last()
                    .is_some_and(|chunk| !chunk.old_lines.is_empty() || !chunk.new_lines.is_empty())
                {
                    return Err(invalid_hunk(
                        line_number,
                        format!(
                            "Expected update hunk to start with a @@ context marker, \
                             got: '{line}'"
                        ),
                    ));
                }
                return Err(unexpected_update_line(line_number, line));
            }
            PatchParseMode::EndedPatch => {
                if !trimmed.is_empty() {
                    return Err(anyhow::anyhow!(
                        "invalid patch: The last line of the patch must be '*** End Patch'"
                    ));
                }
            }
        }
    }

    if !matches!(mode, PatchParseMode::EndedPatch) {
        anyhow::bail!("invalid patch: The last line of the patch must be '*** End Patch'");
    }

    if hunks.is_empty() {
        anyhow::bail!("invalid patch: no hunks found");
    }

    Ok(hunks)
}

#[cfg(test)]
#[path = "apply_patch_tests.rs"]
mod apply_patch_tests;
