use crate::{message::ToolCall, tui::ui::tools_ui};
use ratatui::prelude::*;

pub(super) fn diff_add_color() -> Color {
    Color::Rgb(100, 200, 100)
}

pub(super) fn diff_del_color() -> Color {
    Color::Rgb(200, 100, 100)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DiffLineKind {
    Add,
    Del,
    /// Non-change row separating hunks inside one file's group (the `@@`
    /// position header), rendered dim.
    Sep,
    /// Failed patch section, rendered red with an `Error:` message.
    Error,
}

#[derive(Clone, Debug)]
pub(super) struct ParsedDiffLine {
    pub kind: DiffLineKind,
    pub prefix: String,
    pub content: String,
    pub file_path: Option<String>,
}

pub(super) fn diff_change_counts(content: &str) -> (usize, usize) {
    let lines = collect_diff_lines(content);
    let additions = lines
        .iter()
        .filter(|line| line.kind == DiffLineKind::Add)
        .count();
    let deletions = lines
        .iter()
        .filter(|line| line.kind == DiffLineKind::Del)
        .count();
    (additions, deletions)
}

pub(super) fn diff_change_counts_for_tool(tool: &ToolCall, content: &str) -> (usize, usize) {
    // Count the rows actually rendered so the badge can never disagree with
    // the diff box — the input patch and the effective result can differ
    // when a hunk fails or the tool reports partial edits.
    if tools_ui::canonical_tool_name(&tool.name) == "apply_patch" {
        return diff_lines_for_tool_message(tool, content).iter().fold(
            (0usize, 0usize),
            |(adds, dels), line| match line.kind {
                DiffLineKind::Add => (adds + 1, dels),
                DiffLineKind::Del => (adds, dels + 1),
                DiffLineKind::Sep | DiffLineKind::Error => (adds, dels),
            },
        );
    }

    let (additions, deletions) = diff_change_counts(content);
    if additions > 0 || deletions > 0 {
        return (additions, deletions);
    }

    match tools_ui::canonical_tool_name(&tool.name) {
        "edit" => {
            diff_counts_from_input_pair(&tool.input, "old_string", "new_string").unwrap_or((0, 0))
        }
        "write" => {
            let content = tool
                .input
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            diff_counts_from_strings("", content)
        }
        "multiedit" => diff_counts_from_multiedit(&tool.input).unwrap_or((0, 0)),
        "patch" => diff_counts_from_unified_patch_input(&tool.input).unwrap_or((0, 0)),
        _ => (additions, deletions),
    }
}

fn diff_counts_from_input_pair(
    input: &serde_json::Value,
    old_key: &str,
    new_key: &str,
) -> Option<(usize, usize)> {
    let old = input.get(old_key)?.as_str()?;
    let new = input.get(new_key)?.as_str()?;
    Some(diff_counts_from_strings(old, new))
}

fn diff_counts_from_multiedit(input: &serde_json::Value) -> Option<(usize, usize)> {
    let edits = input.get("edits")?.as_array()?;
    let mut additions = 0usize;
    let mut deletions = 0usize;

    for edit in edits {
        let old = edit
            .get("old_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let new = edit
            .get("new_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if old.is_empty() && new.is_empty() {
            continue;
        }
        let (add, del) = diff_counts_from_strings(old, new);
        additions += add;
        deletions += del;
    }

    Some((additions, deletions))
}

fn diff_counts_from_unified_patch_input(input: &serde_json::Value) -> Option<(usize, usize)> {
    let patch_text = input.get("patch_text")?.as_str()?;
    let mut additions = 0usize;
    let mut deletions = 0usize;

    for line in patch_text.lines() {
        if line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("@@")
            || line.starts_with("diff --git")
            || line.starts_with("index ")
            || line.starts_with("\\ No newline")
        {
            continue;
        }
        if line.starts_with('+') {
            additions += 1;
        } else if line.starts_with('-') {
            deletions += 1;
        }
    }

    Some((additions, deletions))
}

/// The apply_patch tool accepts its patch under `input` (freeform variant)
/// or `patch_text` (JSON variant).
fn apply_patch_input_text(input: &serde_json::Value) -> Option<&str> {
    input
        .get("input")
        .and_then(|v| v.as_str())
        .or_else(|| input.get("patch_text").and_then(|v| v.as_str()))
}

fn diff_counts_from_strings(old: &str, new: &str) -> (usize, usize) {
    use similar::ChangeTag;

    let diff = similar::TextDiff::from_lines(old, new);
    let mut additions = 0usize;
    let mut deletions = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => additions += 1,
            ChangeTag::Delete => deletions += 1,
            ChangeTag::Equal => {}
        }
    }
    (additions, deletions)
}

pub(super) fn generate_diff_lines_from_tool_input(tool: &ToolCall) -> Vec<ParsedDiffLine> {
    match tools_ui::canonical_tool_name(&tool.name) {
        "edit" => {
            let old = tool
                .input
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new = tool
                .input
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            generate_diff_lines_from_strings(old, new)
        }
        "multiedit" => {
            let Some(edits) = tool.input.get("edits").and_then(|v| v.as_array()) else {
                return Vec::new();
            };
            let mut all_lines = Vec::new();
            for edit in edits {
                let old = edit
                    .get("old_string")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let new = edit
                    .get("new_string")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                all_lines.extend(generate_diff_lines_from_strings(old, new));
            }
            all_lines
        }
        "write" => {
            let content = tool
                .input
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            generate_diff_lines_from_strings("", content)
        }
        "patch" => {
            let patch_text = tool
                .input
                .get("patch_text")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            collect_diff_lines(patch_text)
        }
        "apply_patch" => {
            let patch_text = apply_patch_input_text(&tool.input).unwrap_or("");
            collect_apply_patch_diff_lines(patch_text)
        }
        _ => Vec::new(),
    }
}

/// Diff rows for one tool message. `apply_patch` input is grammar-exact, so
/// it is preferred over re-parsing the generated unified diff in the tool
/// result (whose `--- `/`+++ ` headers can collide with hunk content lines,
/// producing a bogus file boundary mid-block and dropping the row). Other
/// tools keep the result-text-first order.
pub(super) fn diff_lines_for_tool_message(tool: &ToolCall, content: &str) -> Vec<ParsedDiffLine> {
    if tools_ui::canonical_tool_name(&tool.name) == "apply_patch" {
        // Preferred source: the post-edit result text. Its `File diff:`
        // fenced block is the effective diff — real filenames from `+++`
        // headers and real line numbers from `@@` positions — so the display
        // matches what was actually applied. The numbered `N±` echo and the
        // input patch are fallbacks for results without a fenced block.
        let from_result = collect_apply_patch_result_diff_lines(content);
        if !from_result.is_empty() {
            return from_result;
        }
    }
    let from_content = collect_diff_lines(content);
    if !from_content.is_empty() {
        return from_content;
    }
    generate_diff_lines_from_tool_input(tool)
}

/// Parse an apply_patch (`*** Begin Patch`/`*** End Patch`) document into
/// display rows. Modeled on codex's grammar: `***` markers only count at
/// column 0, so an indented ` *** Update File:` inside a hunk is context,
/// and a `-`/`+` content line is never mistaken for a file boundary — the
/// failure mode of running this through the generic unified-diff scanner.
pub(super) fn collect_apply_patch_diff_lines(patch_text: &str) -> Vec<ParsedDiffLine> {
    let mut lines = Vec::new();
    let mut in_patch = false;
    let mut file_path: Option<String> = None;
    // Inside an Add File hunk every `+` line is file content until the next
    // column-0 `***` marker; inside Update hunks `+`/`-` rows are collected
    // and context lines (` ` or anything else) are skipped.
    let mut in_add_file = false;

    for raw_line in patch_text.lines() {
        let line = raw_line.trim_end();
        if line.trim() == "*** Begin Patch" {
            in_patch = true;
            continue;
        }
        if !in_patch {
            continue;
        }
        if line.trim() == "*** End Patch" {
            break;
        }

        if let Some(path) = line.strip_prefix("*** Add File: ") {
            file_path = non_empty_diff_path(path.trim());
            in_add_file = true;
            continue;
        }
        if let Some(path) = line
            .strip_prefix("*** Delete File: ")
            .or_else(|| line.strip_prefix("*** Update File: "))
        {
            file_path = non_empty_diff_path(path.trim());
            in_add_file = false;
            continue;
        }
        if line == "*** End of File" {
            file_path = None;
            in_add_file = false;
            continue;
        }
        // `*** Move to:` and unknown `***` markers at column 0, plus `@@`
        // section headers, carry no display rows.
        if line.starts_with("*** ") || line.starts_with("@@") {
            continue;
        }

        if in_add_file {
            if let Some(content) = raw_line.strip_prefix('+') {
                lines.push(ParsedDiffLine {
                    kind: DiffLineKind::Add,
                    prefix: "+".to_string(),
                    content: trim_diff_content(content),
                    file_path: file_path.clone(),
                });
            }
            continue;
        }

        if let Some(content) = raw_line.strip_prefix('+') {
            lines.push(ParsedDiffLine {
                kind: DiffLineKind::Add,
                prefix: "+".to_string(),
                content: trim_diff_content(content),
                file_path: file_path.clone(),
            });
        } else if let Some(content) = raw_line.strip_prefix('-') {
            lines.push(ParsedDiffLine {
                kind: DiffLineKind::Del,
                prefix: "-".to_string(),
                content: trim_diff_content(content),
                file_path: file_path.clone(),
            });
        }
        // Context lines (` ` prefix or bare) produce no display row.
    }

    lines
}

/// Parse apply_patch's post-edit result text into display rows. Every change
/// is emitted exactly once: the `File diff:` fenced unified diff is the
/// preferred per-file source (real `+++` filenames, `@@` line numbers and
/// hunk boundaries), while `✓ path:` gates supply the `N±` numbered echo for
/// files the fenced diff omits. `✗` gates become Error rows in their own
/// file group so a failed hunk keeps its place and its diff window.
pub(super) fn collect_apply_patch_result_diff_lines(content: &str) -> Vec<ParsedDiffLine> {
    struct Section {
        path: String,
        error: Option<String>,
        numbered: Vec<ParsedDiffLine>,
    }

    let mut sections: Vec<Section> = Vec::new();
    let mut fenced: Vec<(String, Vec<ParsedDiffLine>)> = Vec::new();

    // Fence state for ```diff blocks.
    let mut in_fence = false;
    let mut cur_path: Option<String> = None;
    let mut cur_rows: Vec<ParsedDiffLine> = Vec::new();
    let mut old_ln = 0usize;
    let mut new_ln = 0usize;
    let mut hunks_in_file = 0usize;
    // Numbering state for legacy bare-`@@` hunks under a `✓` gate.
    let mut gate_old_ln = 0usize;
    let mut gate_new_ln = 0usize;
    let mut gate_has_hunk = false;

    macro_rules! flush_fence_file {
        () => {{
            if let Some(path) = cur_path.take() {
                if !cur_rows.is_empty() {
                    fenced.push((path, std::mem::take(&mut cur_rows)));
                }
            }
            cur_rows.clear();
        }};
    }

    for raw_line in content.lines() {
        let trimmed = raw_line.trim();

        if in_fence {
            if trimmed.starts_with("```") {
                flush_fence_file!();
                in_fence = false;
                continue;
            }
            if let Some(path) = trimmed.strip_prefix("--- ") {
                // New file section inside the same fenced block.
                flush_fence_file!();
                cur_path = unified_diff_path(path);
                hunks_in_file = 0;
                continue;
            }
            if let Some(path) = trimmed.strip_prefix("+++ ") {
                // `+++` overrides `---` (dev/null on the other side).
                if let Some(path) = unified_diff_path(path) {
                    cur_path = Some(path);
                }
                continue;
            }
            if trimmed.starts_with("@@") {
                // `@@ -old_start[,len] +new_start[,len] @@` starts a hunk;
                // emit a separator before the second and later hunks of the
                // same file.
                if let Some((old, new)) = parse_hunk_header(trimmed) {
                    if hunks_in_file > 0 {
                        cur_rows.push(ParsedDiffLine {
                            kind: DiffLineKind::Sep,
                            prefix: String::new(),
                            content: trimmed.to_string(),
                            file_path: cur_path.clone(),
                        });
                    }
                    hunks_in_file += 1;
                    old_ln = old;
                    new_ln = new;
                }
                continue;
            }
            if cur_path.is_none() {
                continue;
            }
            if let Some(rest) = raw_line.strip_prefix('-') {
                cur_rows.push(ParsedDiffLine {
                    kind: DiffLineKind::Del,
                    prefix: format!("{}- ", old_ln),
                    content: trim_diff_content(rest),
                    file_path: cur_path.clone(),
                });
                old_ln += 1;
            } else if let Some(rest) = raw_line.strip_prefix('+') {
                cur_rows.push(ParsedDiffLine {
                    kind: DiffLineKind::Add,
                    prefix: format!("{}+ ", new_ln),
                    content: trim_diff_content(rest),
                    file_path: cur_path.clone(),
                });
                new_ln += 1;
            } else {
                // Context line advances both counters.
                old_ln += 1;
                new_ln += 1;
            }
            continue;
        }

        if trimmed.starts_with("```") {
            in_fence = true;
            cur_path = None;
            cur_rows.clear();
            hunks_in_file = 0;
            continue;
        }

        // Stored tool messages prefix the result text once with the tool
        // name (`[apply_patch] ✗ path: ...`), so the status glyph is not
        // always at column 0. Strip one optional `[name] ` wrapper before
        // testing for the gate.
        let gate_text = trimmed
            .strip_prefix('[')
            .and_then(|rest| rest.split_once("] "))
            .map(|(_, rest)| rest)
            .unwrap_or(trimmed);
        if let Some(status) = gate_text
            .strip_prefix('✓')
            .or_else(|| gate_text.strip_prefix('✗'))
        {
            let status = status.trim_start();
            if let Some((path, rest)) = status.split_once(": ")
                && let Some(path) = non_empty_diff_path(path)
            {
                sections.push(Section {
                    path,
                    error: gate_text.starts_with('✗').then(|| rest.trim().to_string()),
                    numbered: Vec::new(),
                });
                gate_old_ln = 0;
                gate_new_ln = 0;
                gate_has_hunk = false;
                continue;
            }
        }

        // Legacy result shape: `✓ file: updated` followed by a bare `@@`
        // hunk (no fenced block, no `N±` echo). Number its plain `+`/`-`
        // rows from the hunk header so they look like the standard display.
        if trimmed.starts_with("@@") {
            if let Some((old, new)) = parse_hunk_header(trimmed) {
                if gate_has_hunk && let Some(section) = sections.last_mut() {
                    section.numbered.push(ParsedDiffLine {
                        kind: DiffLineKind::Sep,
                        prefix: String::new(),
                        content: trimmed.to_string(),
                        file_path: Some(section.path.clone()),
                    });
                }
                gate_old_ln = old;
                gate_new_ln = new;
                gate_has_hunk = true;
            }
            continue;
        }
        if gate_has_hunk && raw_line.starts_with(' ') {
            // Context line inside a bare hunk advances both counters.
            gate_old_ln += 1;
            gate_new_ln += 1;
            continue;
        }

        if let Some(mut line) = parse_diff_line(raw_line)
            && let Some(section) = sections.last_mut()
        {
            if gate_has_hunk {
                match line.kind {
                    DiffLineKind::Del if line.prefix == "-" => {
                        line.prefix = format!("{}- ", gate_old_ln);
                        gate_old_ln += 1;
                    }
                    DiffLineKind::Add if line.prefix == "+" => {
                        line.prefix = format!("{}+ ", gate_new_ln);
                        gate_new_ln += 1;
                    }
                    _ => {}
                }
            }
            line.file_path = Some(section.path.clone());
            section.numbered.push(line);
        }
    }
    flush_fence_file!();

    if sections.is_empty() && fenced.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    for section in &sections {
        if let Some(error) = &section.error {
            lines.push(ParsedDiffLine {
                kind: DiffLineKind::Error,
                prefix: "✗ ".to_string(),
                content: format!("Error: {error}"),
                file_path: Some(section.path.clone()),
            });
            continue;
        }
        if let Some((_, rows)) = fenced.iter().find(|(path, _)| *path == section.path) {
            lines.extend(rows.iter().cloned());
        } else {
            lines.extend(section.numbered.iter().cloned());
        }
    }
    // Fenced files no `✓` gate covered (foreign result shapes) still show.
    for (path, rows) in &fenced {
        if sections.iter().all(|section| section.path != *path) {
            lines.extend(rows.iter().cloned());
        }
    }

    insert_gap_separators(&mut lines);
    lines
}

/// Insert a dim `⋯` row wherever a file's displayed line numbers jump, so
/// context collapsed inside one emitted hunk is visibly separated — the
/// same signal an `@@` hunk boundary gives between separate hunks.
fn insert_gap_separators(lines: &mut Vec<ParsedDiffLine>) {
    use std::collections::HashMap;
    // (file_path, is_del) -> last displayed line number on that side.
    let mut last: HashMap<(String, bool), usize> = HashMap::new();
    let mut out: Vec<ParsedDiffLine> = Vec::with_capacity(lines.len() + 4);

    for line in lines.drain(..) {
        let side_num = |prefix: &str| -> Option<(bool, usize)> {
            let prefix = prefix.trim_end();
            if let Some(rest) = prefix.strip_suffix('-') {
                rest.trim().parse().ok().map(|n| (true, n))
            } else if let Some(rest) = prefix.strip_suffix('+') {
                rest.trim().parse().ok().map(|n| (false, n))
            } else {
                None
            }
        };

        match line.kind {
            DiffLineKind::Add | DiffLineKind::Del => {
                if let (Some(file), Some((is_del, num))) =
                    (line.file_path.clone(), side_num(&line.prefix))
                {
                    let key = (file.clone(), is_del);
                    if let Some(&prev) = last.get(&key)
                        && num > prev + 1
                    {
                        out.push(ParsedDiffLine {
                            kind: DiffLineKind::Sep,
                            prefix: String::new(),
                            content: "⋯".to_string(),
                            file_path: Some(file),
                        });
                        // One `⋯` per gap: a del/add pair shares the jump,
                        // so reset both sides and the companion row does not
                        // emit a second separator for the same discontinuity.
                        last.remove(&(key.0.clone(), true));
                        last.remove(&(key.0.clone(), false));
                    }
                    last.insert(key, num);
                }
            }
            DiffLineKind::Sep | DiffLineKind::Error => {
                // A real `@@` boundary (or an error group) already separates;
                // restart numbering so the next row does not trigger a
                // redundant `⋯` on top of it.
                if let Some(file) = &line.file_path {
                    last.remove(&(file.clone(), true));
                    last.remove(&(file.clone(), false));
                }
            }
        }
        out.push(line);
    }

    *lines = out;
}

/// Parse `@@ -old_start[,len] +new_start[,len] @@` into starting line numbers.
fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let inner = line.strip_prefix("@@")?.trim().strip_suffix("@@")?.trim();
    let mut old = None;
    let mut new = None;
    for part in inner.split_whitespace() {
        if let Some(rest) = part.strip_prefix('-') {
            old = rest.split(',').next()?.parse().ok();
        } else if let Some(rest) = part.strip_prefix('+') {
            new = rest.split(',').next()?.parse().ok();
        }
    }
    Some((old?, new?))
}

fn generate_diff_lines_from_strings(old: &str, new: &str) -> Vec<ParsedDiffLine> {
    use similar::ChangeTag;

    let diff = similar::TextDiff::from_lines(old, new);
    let mut lines = Vec::new();

    for change in diff.iter_all_changes() {
        let content = change.value().trim();
        if content.is_empty() {
            continue;
        }

        match change.tag() {
            ChangeTag::Delete => {
                lines.push(ParsedDiffLine {
                    kind: DiffLineKind::Del,
                    prefix: format!("{}- ", change.old_index().unwrap_or(0) + 1),
                    content: content.to_string(),
                    file_path: None,
                });
            }
            ChangeTag::Insert => {
                lines.push(ParsedDiffLine {
                    kind: DiffLineKind::Add,
                    prefix: format!("{}+ ", change.new_index().unwrap_or(0) + 1),
                    content: content.to_string(),
                    file_path: None,
                });
            }
            ChangeTag::Equal => {}
        }
    }

    lines
}

pub(super) fn collect_diff_lines(content: &str) -> Vec<ParsedDiffLine> {
    let mut file_path = None;
    let mut lines = Vec::new();

    for raw_line in content.lines() {
        if let Some(path) = diff_file_path(raw_line) {
            file_path = Some(path);
            continue;
        }
        if let Some(mut line) = parse_diff_line(raw_line) {
            line.file_path = file_path.clone();
            lines.push(line);
        }
    }

    lines
}

fn diff_file_path(raw_line: &str) -> Option<String> {
    let trimmed = raw_line.trim();
    if let Some(path) = trimmed
        .strip_prefix("*** Add File: ")
        .or_else(|| trimmed.strip_prefix("*** Update File: "))
        .or_else(|| trimmed.strip_prefix("*** Delete File: "))
    {
        return non_empty_diff_path(path);
    }

    if let Some(path) = trimmed.strip_prefix("+++ ") {
        return unified_diff_path(path);
    }
    if let Some(path) = trimmed.strip_prefix("--- ") {
        return unified_diff_path(path);
    }

    let status = trimmed
        .strip_prefix('✓')
        .or_else(|| trimmed.strip_prefix('✗'))?
        .trim_start();
    let (path, _) = status.split_once(": ")?;
    non_empty_diff_path(path)
}

fn unified_diff_path(raw_path: &str) -> Option<String> {
    let path = raw_path
        .split('\t')
        .next()
        .unwrap_or(raw_path)
        .split_whitespace()
        .next()
        .unwrap_or("");
    let path = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path);
    non_empty_diff_path(path)
}

fn non_empty_diff_path(path: &str) -> Option<String> {
    let path = path.trim();
    (!path.is_empty() && path != "/dev/null").then(|| path.to_string())
}

fn parse_diff_line(raw_line: &str) -> Option<ParsedDiffLine> {
    let trimmed = raw_line.trim();
    if trimmed.is_empty() || trimmed == "..." {
        return None;
    }
    if trimmed.starts_with("diff --git ")
        || trimmed.starts_with("index ")
        || trimmed.starts_with("--- ")
        || trimmed.starts_with("+++ ")
        || trimmed.starts_with("@@ ")
        || trimmed.starts_with("\\ No newline")
    {
        return None;
    }

    if let Some(pos) = trimmed.find("- ") {
        let (prefix, content) = trimmed.split_at(pos + 2);
        if !prefix.is_empty() && prefix[..pos].chars().all(|c| c.is_ascii_digit()) {
            return Some(ParsedDiffLine {
                kind: DiffLineKind::Del,
                prefix: prefix.to_string(),
                content: trim_diff_content(content),
                file_path: None,
            });
        }
    }
    if let Some(pos) = trimmed.find("+ ") {
        let (prefix, content) = trimmed.split_at(pos + 2);
        if !prefix.is_empty() && prefix[..pos].chars().all(|c| c.is_ascii_digit()) {
            return Some(ParsedDiffLine {
                kind: DiffLineKind::Add,
                prefix: prefix.to_string(),
                content: trim_diff_content(content),
                file_path: None,
            });
        }
    }

    if let Some(rest) = raw_line.strip_prefix('+') {
        return Some(ParsedDiffLine {
            kind: DiffLineKind::Add,
            prefix: "+".to_string(),
            content: trim_diff_content(rest),
            file_path: None,
        });
    }
    if let Some(rest) = raw_line.strip_prefix('-') {
        return Some(ParsedDiffLine {
            kind: DiffLineKind::Del,
            prefix: "-".to_string(),
            content: trim_diff_content(rest),
            file_path: None,
        });
    }

    None
}

fn trim_diff_content(content: &str) -> String {
    content.trim_start_matches([' ', '\t']).to_string()
}

pub(super) fn tint_span_with_diff_color(span: Span<'static>, diff_color: Color) -> Span<'static> {
    let (dr, dg, db) = match diff_color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(n) => super::color_support::indexed_to_rgb(n),
        _ => return span,
    };

    let fg = span.style.fg.unwrap_or(Color::White);
    let (sr, sg, sb) = match fg {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(n) => super::color_support::indexed_to_rgb(n),
        Color::White => (255, 255, 255),
        Color::Black => (0, 0, 0),
        _ => return span,
    };

    // Diff color dominates the blend: a syntax-colored token must not flip
    // the row's add/del readout (a markdown `- ` bullet is `markup.list`
    // red in base16-ocean.dark — at 30% diff weight an added list line
    // rendered as a deletion).
    let blend = |s: u8, d: u8| -> u8 { ((s as u16 * 30 + d as u16 * 70) / 100) as u8 };

    let tinted = Color::Rgb(blend(sr, dr), blend(sg, dg), blend(sb, db));
    Span::styled(span.content, span.style.fg(tinted))
}

#[cfg(test)]
mod tests {
    use super::{
        DiffLineKind, apply_patch_input_text, collect_apply_patch_diff_lines,
        collect_apply_patch_result_diff_lines, collect_diff_lines, diff_change_counts_for_tool,
        generate_diff_lines_from_strings, tint_span_with_diff_color,
    };
    use crate::message::ToolCall;
    use ratatui::prelude::{Color, Span, Style};
    use serde_json::json;

    /// The row's diff color must dominate the tint: markdown `- ` bullets
    /// are `markup.list` red in base16-ocean.dark, and a syntax-dominant
    /// blend kept added list lines looking like deletions.
    #[test]
    fn diff_tint_keeps_row_kind_dominant_over_red_syntax_tokens() {
        let syntax_red = Color::Indexed(131);
        let green = super::diff_add_color();
        let span = Span::styled("- item", Style::default().fg(syntax_red));

        let tinted = tint_span_with_diff_color(span, green);
        let Some(Color::Rgb(r, g, _)) = tinted.style.fg else {
            panic!("expected rgb tint, got {:?}", tinted.style.fg)
        };
        assert!(
            g > r,
            "added markdown list line should read green, got r={r} g={g}"
        );

        let red = super::diff_del_color();
        let tinted = tint_span_with_diff_color(
            Span::styled("item", Style::default().fg(Color::Indexed(144))),
            red,
        );
        let Some(Color::Rgb(r, g, _)) = tinted.style.fg else {
            panic!("expected rgb tint, got {:?}", tinted.style.fg)
        };
        assert!(r > g, "deleted row should read red, got r={r} g={g}");
    }

    #[test]
    fn apply_patch_content_line_looking_like_file_header_stays_a_deletion() {
        // A deleted line whose content is "-- a/fake.rs" appears in the patch
        // as "--- a/fake.rs". The generic unified-diff scanner reads that as a
        // file boundary (mid-block filename, dropped row); the dedicated
        // apply_patch parser keeps it a deletion under the real file.
        let patch = "*** Begin Patch\n*** Update File: crates/real.rs\n@@\n ctx\n-    \"notify\": false,\n--- a/fake.rs\n+    \"notify\": true,\n+++ b/fake.rs\n ctx\n*** End Patch\n";

        let lines = collect_apply_patch_diff_lines(patch);

        assert_eq!(lines.len(), 4);
        assert!(
            lines
                .iter()
                .all(|line| line.file_path.as_deref() == Some("crates/real.rs")),
            "{lines:?}"
        );
        assert_eq!(lines[1].kind, DiffLineKind::Del);
        assert_eq!(lines[1].content, "-- a/fake.rs");
        assert_eq!(lines[3].kind, DiffLineKind::Add);
        assert_eq!(lines[3].content, "++ b/fake.rs");
    }

    #[test]
    fn apply_patch_indented_update_marker_is_context_not_a_boundary() {
        // `***` markers only count at column 0 (codex grammar); an indented
        // ` *** Update File:` inside a hunk is a context line.
        let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n-old\n+new\n  *** Update File: fake.txt\n+after\n*** End Patch\n";

        let lines = collect_apply_patch_diff_lines(patch);

        assert_eq!(lines.len(), 3);
        assert!(
            lines
                .iter()
                .all(|line| line.file_path.as_deref() == Some("a.txt")),
            "{lines:?}"
        );
    }

    #[test]
    fn apply_patch_add_file_lines_and_multi_file_boundaries() {
        let patch = "*** Begin Patch\n*** Add File: new.txt\n+line one\n+line two\n*** Update File: old.txt\n@@\n-gone\n*** Delete File: dead.txt\n*** End Patch\n";

        let lines = collect_apply_patch_diff_lines(patch);

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].file_path.as_deref(), Some("new.txt"));
        assert_eq!(lines[0].kind, DiffLineKind::Add);
        assert_eq!(lines[2].file_path.as_deref(), Some("old.txt"));
        assert_eq!(lines[2].kind, DiffLineKind::Del);
    }

    #[test]
    fn apply_patch_freeform_input_field_is_accepted() {
        let input = json!({
            "input": "*** Begin Patch\n*** Update File: a.txt\n@@\n-old\n+new\n*** End Patch\n"
        });

        let lines = collect_apply_patch_diff_lines(apply_patch_input_text(&input).unwrap());
        let adds = lines.iter().filter(|l| l.kind == DiffLineKind::Add).count();
        let dels = lines.iter().filter(|l| l.kind == DiffLineKind::Del).count();
        assert_eq!((adds, dels), (1, 1));
    }

    #[test]
    fn apply_patch_counts_skip_indented_marker_lookalikes() {
        // " *** Update File:" as hunk content: counted via `+`/`-` prefix
        // only, never as a boundary that resets attribution.
        let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n-a\n+b\n+*** not a marker\n*** End Patch\n";
        let lines = collect_apply_patch_diff_lines(patch);
        let adds = lines.iter().filter(|l| l.kind == DiffLineKind::Add).count();
        let dels = lines.iter().filter(|l| l.kind == DiffLineKind::Del).count();
        assert_eq!((adds, dels), (2, 1));
    }

    #[test]
    fn apply_patch_counts_ignore_context_lines_with_plus_or_minus_prefixes() {
        let patch = "*** Begin Patch\n*** Update File: demo.txt\n@@\n  +context line\n  -context line\n+added line\n-deleted line\n*** End Patch\n";
        let lines = collect_apply_patch_diff_lines(patch);
        let adds = lines.iter().filter(|l| l.kind == DiffLineKind::Add).count();
        let dels = lines.iter().filter(|l| l.kind == DiffLineKind::Del).count();
        assert_eq!((adds, dels), (1, 1));
    }

    #[test]
    fn collected_apply_patch_lines_retain_file_boundaries() {
        let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n-old a\n+new a\n*** Update File: b.txt\n@@\n-old b\n+new b\n*** End Patch\n";

        let lines = collect_diff_lines(patch);

        assert_eq!(lines.len(), 4);
        assert!(
            lines[..2]
                .iter()
                .all(|line| line.file_path.as_deref() == Some("a.txt"))
        );
        assert!(
            lines[2..]
                .iter()
                .all(|line| line.file_path.as_deref() == Some("b.txt"))
        );
    }

    #[test]
    fn write_tool_falls_back_to_content_diff_counts() {
        let tool = ToolCall {
            id: "tool_1".to_string(),
            name: "write".to_string(),
            input: json!({
                "file_path": "demo.txt",
                "content": "first line\nsecond line\n"
            }),
            intent: None,
            thought_signature: None,
        };

        assert_eq!(diff_change_counts_for_tool(&tool, ""), (2, 0));
    }

    #[test]
    fn multiedit_pascal_case_falls_back_to_input_diff_counts() {
        let tool = ToolCall {
            id: "tool_2".to_string(),
            name: "MultiEdit".to_string(),
            input: json!({
                "file_path": "demo.txt",
                "edits": [
                    {"old_string": "two\n", "new_string": "TWO\n"},
                    {"old_string": "three\n", "new_string": "THREE\n"}
                ]
            }),
            intent: None,
            thought_signature: None,
        };

        assert_eq!(diff_change_counts_for_tool(&tool, ""), (2, 2));
    }

    #[test]
    fn generated_diff_lines_use_old_and_new_line_numbers() {
        let lines =
            generate_diff_lines_from_strings("one\ntwo\nthree\n", "one\nthree\nfour\nfive\n");

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].kind, DiffLineKind::Del);
        assert_eq!(lines[0].prefix, "2- ");
        assert_eq!(lines[1].kind, DiffLineKind::Add);
        assert_eq!(lines[1].prefix, "3+ ");
        assert_eq!(lines[2].kind, DiffLineKind::Add);
        assert_eq!(lines[2].prefix, "4+ ");
    }

    #[test]
    fn apply_patch_result_parse_uses_fenced_diff_once_with_real_numbers() {
        // The result echoes every change twice — numbered `N±` lines and the
        // `File diff:` fenced block — but the display must emit each once,
        // numbered from `@@` positions and filed under the `+++` path.
        let result = "✓ crates/x.rs: modified (1 hunks)\n\
                      10- old a\n\
                      10+ new a\n\
                      \n\
                      File diff:\n\
                      ```diff\n\
                      --- a/crates/x.rs\n\
                      +++ b/crates/x.rs\n\
                      @@ -8,3 +8,3 @@\n\
                       ctx\n\
                      -old a\n\
                      +new a\n\
                      ```\n";

        let lines = collect_apply_patch_result_diff_lines(result);

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].kind, DiffLineKind::Del);
        assert_eq!(lines[0].prefix, "9- ");
        assert_eq!(lines[0].content, "old a");
        assert_eq!(lines[0].file_path.as_deref(), Some("crates/x.rs"));
        assert_eq!(lines[1].kind, DiffLineKind::Add);
        assert_eq!(lines[1].prefix, "9+ ");
        assert_eq!(lines[1].file_path.as_deref(), Some("crates/x.rs"));
    }

    #[test]
    fn apply_patch_result_parse_multihunk_separates_with_hunk_headers() {
        let result = "✓ f.rs: modified (2 hunks)\n\
                      File diff:\n\
                      ```diff\n\
                      --- a/f.rs\n\
                      +++ b/f.rs\n\
                      @@ -1,2 +1,2 @@\n\
                      -a\n\
                      +b\n\
                      @@ -10,2 +10,2 @@\n\
                      -c\n\
                      +d\n\
                      ```\n";

        let lines = collect_apply_patch_result_diff_lines(result);

        assert_eq!(lines.len(), 5);
        assert_eq!(lines[2].kind, DiffLineKind::Sep);
        assert_eq!(lines[2].content, "@@ -10,2 +10,2 @@");
        assert_eq!(lines[2].file_path.as_deref(), Some("f.rs"));
        assert_eq!(lines[3].prefix, "10- ");
        assert_eq!(lines[4].prefix, "10+ ");
        // One file group: every row carries f.rs.
        assert!(lines.iter().all(|l| l.file_path.as_deref() == Some("f.rs")));
    }

    #[test]
    fn apply_patch_result_parse_error_gate_emits_error_row() {
        let result = "✓ a.rs: modified (1 hunks)\n\
                      File diff:\n\
                      ```diff\n\
                      --- a/a.rs\n\
                      +++ b/a.rs\n\
                      @@ -1,1 +1,1 @@\n\
                      -x\n\
                      +y\n\
                      ```\n\
                      ✗ b.rs: hunk 2 did not apply\n";

        let lines = collect_apply_patch_result_diff_lines(result);

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2].kind, DiffLineKind::Error);
        assert_eq!(lines[2].content, "Error: hunk 2 did not apply");
        assert_eq!(lines[2].file_path.as_deref(), Some("b.rs"));
    }

    #[test]
    fn apply_patch_result_parse_error_gate_after_tool_name_prefix() {
        // Stored tool messages wrap the result text with `[apply_patch] `,
        // so the `✗` gate is not at column 0. The empty trailing `File diff:`
        // fence contributes no rows; the error row alone must render.
        let result = "[apply_patch] ✗ f.rs: Failed to find expected lines in /abs/f.rs:\n\
                      line 05 echo\n\
                      line 11 kilo\n\
                      \n\
                      File diff:\n\
                      ```diff\n\
                      --- a/f.rs\n\
                      +++ b/f.rs\n\
                      ```\n";

        let lines = collect_apply_patch_result_diff_lines(result);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].kind, DiffLineKind::Error);
        assert_eq!(lines[0].prefix, "✗ ");
        assert!(lines[0].content.contains("Failed to find expected lines"));
        assert_eq!(lines[0].file_path.as_deref(), Some("f.rs"));
    }

    #[test]
    fn apply_patch_result_parse_numbered_echo_fallback_without_fence() {
        // Older/foreign result shape without `File diff:`: the `✓` gate's
        // numbered lines still render, filed under the gate path.
        let result = "✓ f.rs: modified (1 hunks)\n42- old\n42+ new\n";

        let lines = collect_apply_patch_result_diff_lines(result);

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].prefix, "42- ");
        assert_eq!(lines[0].file_path.as_deref(), Some("f.rs"));
        assert_eq!(lines[1].prefix, "42+ ");
    }

    #[test]
    fn apply_patch_result_parse_empty_on_foreign_content() {
        assert!(collect_apply_patch_result_diff_lines("Applied successfully").is_empty());
        assert!(collect_apply_patch_result_diff_lines("").is_empty());
    }

    #[test]
    fn apply_patch_result_parse_inserts_gap_sep_on_line_jumps() {
        // One emitted `@@ -1,10 +1,10 @@` hunk with changes at lines 2 and
        // 9: the collapsed context between them needs a `⋯` separator.
        let result = "✓ f.txt: modified (1 hunks)\n\
                      File diff:\n\
                      ```diff\n\
                      --- a/f.txt\n\
                      +++ b/f.txt\n\
                      @@ -1,10 +1,10 @@\n\
                       alpha\n\
                      -bravo\n\
                      +BRAVO\n\
                       charlie\n\
                       delta\n\
                       echo\n\
                       foxtrot\n\
                       golf\n\
                       hotel\n\
                      -india\n\
                      +INDIA\n\
                       juliet\n\
                      ```\n";

        let lines = collect_apply_patch_result_diff_lines(result);

        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0].prefix, "2- ");
        assert_eq!(lines[1].prefix, "2+ ");
        assert_eq!(lines[2].kind, DiffLineKind::Sep);
        assert_eq!(lines[2].content, "⋯");
        assert_eq!(lines[2].file_path.as_deref(), Some("f.txt"));
        assert_eq!(lines[3].prefix, "9- ");
        assert_eq!(lines[4].prefix, "9+ ");
    }

    #[test]
    fn apply_patch_result_parse_no_gap_sep_for_sequential_lines() {
        let result = "✓ f.txt: modified (1 hunks)\n\
                      File diff:\n\
                      ```diff\n\
                      --- a/f.txt\n\
                      +++ b/f.txt\n\
                      @@ -1,3 +1,3 @@\n\
                      -a\n\
                      -b\n\
                      +c\n\
                      +d\n\
                      ```\n";

        let lines = collect_apply_patch_result_diff_lines(result);

        assert_eq!(lines.len(), 4);
        assert!(lines.iter().all(|l| l.kind != DiffLineKind::Sep));
    }

    #[test]
    fn apply_patch_result_parse_hunk_sep_not_doubled_by_gap() {
        // A real `@@` boundary already separates hunks — no extra `⋯`
        // on top of it even though the line numbers jump.
        let result = "✓ f.txt: modified (2 hunks)\n\
                      File diff:\n\
                      ```diff\n\
                      --- a/f.txt\n\
                      +++ b/f.txt\n\
                      @@ -1,1 +1,1 @@\n\
                      -a\n\
                      +b\n\
                      @@ -9,1 +9,1 @@\n\
                      -c\n\
                      +d\n\
                      ```\n";

        let lines = collect_apply_patch_result_diff_lines(result);

        let seps: Vec<_> = lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Sep)
            .collect();
        assert_eq!(seps.len(), 1);
        assert!(seps[0].content.starts_with("@@"));
    }
}
