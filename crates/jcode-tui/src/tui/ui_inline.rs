use super::*;
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph};
use unicode_width::UnicodeWidthStr;

fn inline_view_display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub(super) fn inline_ui_height(app: &dyn TuiState, width: u16, window_height: u16) -> u16 {
    match app.inline_ui_state() {
        Some(crate::tui::InlineUiStateRef::Interactive(picker)) => {
            // Model choices replace the command suggestions, without reflowing chat.
            if picker.kind == crate::tui::PickerKind::Model {
                return 0;
            }
            let visible_rows = picker.filtered.len() as u16;
            let rows_needed = visible_rows + 1 + 2; // header + rounded border
            rows_needed.min(20)
        }
        Some(crate::tui::InlineUiStateRef::AskUserQuestion(state)) => {
            // The composer is hidden while the questionnaire is open, so the
            // panel may use up to 60% of the window height (bounded by a
            // fixed cap on tall terminals).
            ask_user_question_height(state, width)
                .min(28)
                .min((window_height.saturating_mul(3) / 5).max(6))
        }
        Some(crate::tui::InlineUiStateRef::View(view)) => {
            let visible_rows = view.lines.len().max(1) as u16;
            let rows_needed = visible_rows + 1 + 2; // header + rounded border
            rows_needed.min(10)
        }
        None => 0,
    }
}

/// Rows needed to render the questionnaire panel: the full wrapped content
/// plus the top and bottom borders.
fn ask_user_question_height(
    state: &crate::tui::InlineAskUserQuestionState,
    area_width: u16,
) -> u16 {
    let content_width = ask_panel_content_width(area_width);
    let layout = ask_panel_layout(state, content_width);
    (layout.header.len() + layout.body.len() + layout.footer.len()) as u16 + 2
}

pub(super) fn draw_inline_ui(frame: &mut Frame, app: &dyn TuiState, area: Rect) {
    match app.inline_ui_state() {
        Some(crate::tui::InlineUiStateRef::Interactive(picker))
            if picker.kind != crate::tui::PickerKind::Model =>
        {
            super::inline_interactive_ui::draw_inline_interactive(frame, app, area)
        }
        Some(crate::tui::InlineUiStateRef::AskUserQuestion(state)) => {
            draw_ask_user_question(frame, app, state, area)
        }
        Some(crate::tui::InlineUiStateRef::View(view)) => draw_inline_view(frame, app, view, area),
        _ => {}
    }
}

fn draw_inline_view(
    frame: &mut Frame,
    app: &dyn TuiState,
    view: &crate::tui::InlineViewState,
    area: Rect,
) {
    let height = area.height as usize;
    let width = area.width as usize;
    if height <= 2 || width <= 2 {
        return;
    }

    let mut content_width = inline_view_display_width(view.title.as_str());
    if let Some(status) = view.status.as_ref() {
        content_width = content_width.max(inline_view_display_width(status.as_str()) + 2);
    }
    for line in &view.lines {
        content_width = content_width.max(inline_view_display_width(line.as_str()));
    }
    let content_width = content_width.min(width.saturating_sub(2)).max(1);
    let outer_width = content_width.saturating_add(2).min(width);
    let horizontal_offset = if app.centered_mode() {
        area.width.saturating_sub(outer_width as u16) / 2
    } else {
        0
    };
    let render_area = Rect {
        x: area.x + horizontal_offset,
        y: area.y,
        width: outer_width as u16,
        height: area.height,
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(rgb(85, 85, 110)))
        .style(Style::default().bg(rgb(18, 18, 26)));
    frame.render_widget(block.clone(), render_area);

    let inner = block.inner(render_area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    let mut header_spans = vec![Span::styled(
        view.title.clone(),
        Style::default().fg(Color::White).bold(),
    )];
    if let Some(status) = view.status.as_ref() {
        header_spans.push(Span::styled(
            format!("  {}", status),
            Style::default().fg(dim_color()).italic(),
        ));
    }
    lines.push(Line::from(header_spans));

    for line in &view.lines {
        lines.push(Line::from(Span::styled(
            line.clone(),
            Style::default().fg(rgb(200, 200, 220)),
        )));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Render the blocking ask_user_question questionnaire: a
/// full-width bordered panel with question tabs across the top (plus a
/// Submit tab on multi-question forms), the markdown question body pinned
/// under them, a scrollable option list on the left, a markdown preview
/// box for the focused option on the right (or stacked below on narrow
/// panes), key hints pinned at the bottom, and the notes editor drawn as
/// an overlay so it stays reachable when the list overflows.
fn draw_ask_user_question(
    frame: &mut Frame,
    _app: &dyn TuiState,
    state: &crate::tui::InlineAskUserQuestionState,
    area: Rect,
) {
    let height = area.height as usize;
    let width = area.width as usize;
    if height <= 2 || width <= 2 {
        return;
    }

    // Full-width panel with rounded borders and one column of inner
    // horizontal padding (wow-pi parity: the dialog spans the pane).
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(rgb(120, 90, 200)))
        .padding(Padding::horizontal(1))
        .style(Style::default().bg(rgb(18, 18, 26)));
    frame.render_widget(block.clone(), area);

    let inner = block.inner(area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let layout = ask_panel_layout(state, inner.width as usize);

    // Pinned header (tab bar/chip + question body) at the top.
    let header_h = layout.header.len().min(inner.height as usize) as u16;
    if header_h > 0 {
        frame.render_widget(
            Paragraph::new(layout.header),
            Rect {
                y: inner.y,
                height: header_h,
                ..inner
            },
        );
    }

    // Pinned footer (blank + key hints) at the bottom; the scrollable body
    // gets whatever remains between them.
    let remaining = (inner.height - header_h) as usize;
    let body_min = usize::from(!layout.body.is_empty());
    let footer_h = layout.footer.len().min(remaining.saturating_sub(body_min)) as u16;
    let body_h = remaining - footer_h as usize;
    if body_h > 0 && !layout.body.is_empty() {
        let scroll = if layout.body.len() <= body_h {
            0
        } else {
            // Keep the focused row inside the viewport, moving the window
            // only as far as needed (standard follow-the-selection scroll).
            let max_scroll = layout.body.len() - body_h;
            let mut scroll = state.scroll.get().min(max_scroll);
            let (focus_start, focus_end) = layout.focus;
            if focus_start < scroll {
                scroll = focus_start;
            }
            if focus_end > scroll + body_h {
                scroll = focus_end.saturating_sub(body_h);
            }
            scroll.min(max_scroll)
        };
        state.scroll.set(scroll);
        let visible: Vec<Line> = layout.body.into_iter().skip(scroll).take(body_h).collect();
        frame.render_widget(
            Paragraph::new(visible),
            Rect {
                y: inner.y + header_h,
                height: body_h as u16,
                ..inner
            },
        );
    }
    if footer_h > 0 {
        frame.render_widget(
            Paragraph::new(layout.footer),
            Rect {
                y: inner.y + inner.height - footer_h,
                height: footer_h,
                ..inner
            },
        );
    }

    // The notes editor is an overlay so it never scrolls off-screen when
    // the option list is taller than the panel.
    if state.editing_notes && !state.on_submit_tab() {
        draw_notes_overlay(frame, state, inner, footer_h);
    }
}

/// The `n` notes editor as a floating box anchored above the footer: a
/// bordered overlay that clears the rows beneath it, keeping the draft
/// (and its cursor) visible regardless of how tall the option list is.
fn draw_notes_overlay(
    frame: &mut Frame,
    state: &crate::tui::InlineAskUserQuestionState,
    inner: Rect,
    footer_h: u16,
) {
    let text_width = (inner.width as usize).saturating_sub(2).max(1);
    let mut lines = vec![Line::from(Span::styled("Note:", muted_style()))];
    lines.extend(edit_lines(
        &state.notes_text,
        state.notes_cursor,
        text_width,
    ));
    let box_h = (lines.len() as u16 + 2).min(inner.height);
    if box_h < 3 {
        return;
    }
    // Sit just above the footer hints when they fit, else at the bottom.
    let bottom = (inner.y + inner.height).saturating_sub(footer_h);
    let y = if bottom >= inner.y + box_h {
        bottom - box_h
    } else {
        inner.y + inner.height - box_h
    };
    let rect = Rect {
        x: inner.x,
        y,
        width: inner.width,
        height: box_h,
    };
    frame.render_widget(ratatui::widgets::Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(rgb(120, 90, 200)))
        .style(Style::default().bg(rgb(18, 18, 26)));
    frame.render_widget(block.clone(), rect);
    frame.render_widget(Paragraph::new(lines), block.inner(rect));
}

/// Content width inside the panel chrome (border + horizontal padding).
fn ask_panel_content_width(area_width: u16) -> usize {
    (area_width as usize).saturating_sub(4).max(1)
}

const ACCENT: (u8, u8, u8) = (140, 180, 255);

fn accent_style() -> Style {
    Style::default().fg(rgb(ACCENT.0, ACCENT.1, ACCENT.2))
}

fn label_style() -> Style {
    Style::default().fg(rgb(200, 200, 220))
}

fn muted_style() -> Style {
    Style::default().fg(dim_color())
}

/// The panel content split into regions: the pinned `header` (tab bar or
/// chip plus the markdown question body), the scrollable `body` (options
/// plus preview, or the submit-tab summary), the pinned `footer` (blank
/// row plus key hints), and `focus`, the focused row's line range inside
/// `body` so the draw pass can keep it in view.
struct AskPanelLayout {
    header: Vec<Line<'static>>,
    body: Vec<Line<'static>>,
    footer: Vec<Line<'static>>,
    focus: (usize, usize),
}

fn ask_panel_layout(
    state: &crate::tui::InlineAskUserQuestionState,
    width: usize,
) -> AskPanelLayout {
    let width = width.max(1);
    let mut header: Vec<Line<'static>> = Vec::new();
    let mut focus = (0, 0);

    // Tab bar across the top on multi-question forms; single questions get
    // a compact header chip instead.
    if state.is_multi() {
        header.push(tab_bar_line(state));
        header.push(Line::default());
    } else if let Some(question) = state.current_question() {
        header.push(Line::from(Span::styled(
            format!(" {} ", question.display_header(0)),
            Style::default().fg(Color::White).bg(rgb(120, 90, 200)),
        )));
        header.push(Line::default());
    }

    let body: Vec<Line<'static>>;
    if state.on_submit_tab() {
        body = submit_tab_lines(state, width);
        // The Submit/Cancel chooser is the last two lines.
        let focused = body.len().saturating_sub(2) + state.submit_choice.min(1);
        focus = (focused, (focused + 1).min(body.len()));
    } else if let Some(question) = state.current_question() {
        // Question body as markdown, wrapped to the content width.
        let markdown =
            jcode_tui_markdown::render_markdown_with_width(&question.question, Some(width));
        header.extend(markdown);
        header.push(Line::default());
        let (lines, range) = options_and_preview_lines(state, question, width);
        body = lines;
        focus = range;
    } else {
        body = Vec::new();
    }

    let footer = vec![
        Line::default(),
        Line::from(Span::styled(help_text(state), muted_style())),
    ];
    AskPanelLayout {
        header,
        body,
        footer,
        focus,
    }
}

/// The tab bar: `← □ Q1  ■ Q2 … ✓ Submit →` with the active tab
/// highlighted and answered tabs boxed.
fn tab_bar_line(state: &crate::tui::InlineAskUserQuestionState) -> Line<'static> {
    let mut spans = vec![Span::styled("← ", muted_style())];
    for (index, question) in state.questions.iter().enumerate() {
        let active = index == state.question_index;
        let answered = state.answers.get(index).is_some_and(|a| a.is_some());
        let text = format!(
            " {} {} ",
            if answered { "■" } else { "□" },
            question.display_header(index)
        );
        let style = if active {
            Style::default().fg(Color::White).bg(rgb(60, 60, 90))
        } else if answered {
            Style::default().fg(rgb(120, 200, 140))
        } else {
            muted_style()
        };
        spans.push(Span::styled(text, style));
        spans.push(Span::raw(" "));
    }
    let all_answered = state.answers.iter().all(|a| a.is_some());
    let submit_active = state.on_submit_tab();
    let submit_style = if submit_active {
        Style::default().fg(Color::White).bg(rgb(60, 60, 90))
    } else if all_answered {
        Style::default().fg(rgb(120, 200, 140))
    } else {
        muted_style()
    };
    spans.push(Span::styled(" ✓ Submit ", submit_style));
    spans.push(Span::styled("→", muted_style()));
    Line::from(spans)
}

/// Submit tab body: "Ready to submit", one summary row per question, and
/// the Submit/Cancel chooser.
fn submit_tab_lines(
    state: &crate::tui::InlineAskUserQuestionState,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled("Ready to submit", accent_style().bold())),
        Line::default(),
    ];
    for (index, question) in state.questions.iter().enumerate() {
        let header = question.display_header(index);
        let line = match state.answers.get(index).and_then(|a| a.as_ref()) {
            Some(answer) => {
                let (wrote, scalar) = answer_scalar(answer);
                Line::from(vec![
                    Span::styled(format!("{header}: "), muted_style()),
                    Span::styled(if wrote { "(wrote) " } else { "" }, muted_style()),
                    Span::styled(scalar, label_style()),
                ])
            }
            None => Line::from(vec![
                Span::styled(format!("{header}: "), muted_style()),
                Span::styled("unanswered", Style::default().fg(rgb(220, 170, 90))),
            ]),
        };
        lines.extend(wrap_with_prefix(" ", line, width));
    }
    lines.push(Line::default());
    let submit_style = if state.submit_choice == 0 {
        accent_style().bold()
    } else {
        label_style()
    };
    let cancel_style = if state.submit_choice == 1 {
        accent_style().bold()
    } else {
        label_style()
    };
    lines.push(Line::from(Span::styled(
        if state.submit_choice == 0 {
            "> Submit"
        } else {
            "  Submit"
        },
        submit_style,
    )));
    lines.push(Line::from(Span::styled(
        if state.submit_choice == 1 {
            "> Cancel"
        } else {
            "  Cancel"
        },
        cancel_style,
    )));
    lines
}

/// Scalar display form of an answer: "N. label", "1. A, 2. B", or the raw
/// custom text, with a merged note appended in parens.
fn answer_scalar(answer: &jcode_session_types::AskUserQuestionAnswer) -> (bool, String) {
    use jcode_session_types::AskUserQuestionAnswer as A;
    let (wrote, scalar) = match answer {
        A::Option { index, label, .. } => (false, format!("{}. {}", index + 1, label)),
        A::Multi { selected, .. } => (
            false,
            selected
                .iter()
                .map(|entry| format!("{}. {}", entry.index + 1, entry.label))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        A::Custom { text, .. } => (true, text.clone()),
    };
    match answer.notes() {
        Some(notes) if !notes.is_empty() => (
            wrote,
            format!("{scalar} (note: {})", notes.replace('\n', " ")),
        ),
        _ => (wrote, scalar),
    }
}

/// Min terminal width for the side-by-side options/preview layout.
const PREVIEW_MIN_WIDTH: usize = 100;
/// Visual gap between the options column and the preview column.
const PREVIEW_COLUMN_GAP: usize = 2;
/// Floor for the preview box's inner content width.
const MIN_PREVIEW_WIDTH: usize = 45;
/// Floor for the adaptive left column width.
const MIN_LEFT: usize = 30;
/// Preview block height cap.
const MAX_PREVIEW_HEIGHT: usize = 15;

const NO_PREVIEW_TEXT: &str = "No preview available";
const NOTES_AFFORDANCE_TEXT: &str = "Notes: press n to add notes";

/// Whether the question renders an option preview pane (wow-pi parity:
/// single-select only, free-text editor closed, at least one preview).
fn question_shows_preview(
    question: &jcode_session_types::AskUserQuestion,
    state: &crate::tui::InlineAskUserQuestionState,
) -> bool {
    !question.is_multi_select()
        && !state.editing_custom
        && question
            .options
            .iter()
            .any(|option| option.preview.as_deref().is_some_and(|p| !p.is_empty()))
}

/// Adaptive left column width: longest label + numbering prefix, floored
/// at MIN_LEFT, capped at half the pane, and never squeezing the preview
/// column below MIN_PREVIEW_WIDTH.
fn adaptive_left_width(labels: &[String], count: usize, pane_width: usize) -> usize {
    let prefix_w = count.max(1).to_string().len() + 4;
    let max_label = labels
        .iter()
        .map(|label| inline_view_display_width(label))
        .max()
        .unwrap_or(0);
    let desired = max_label + prefix_w + 2;
    let ratio_capped = desired.min(pane_width / 2);
    let available = pane_width.saturating_sub(PREVIEW_COLUMN_GAP + MIN_PREVIEW_WIDTH);
    MIN_LEFT.max(ratio_capped.min(available.max(1)))
}

/// The options column plus the focused option's preview box, either
/// side-by-side on wide panes or stacked below the options. Returns the
/// merged lines and the focused row's line range within them (the left
/// column's indices carry over into both merge modes, since the left
/// lines always come first).
fn options_and_preview_lines(
    state: &crate::tui::InlineAskUserQuestionState,
    question: &jcode_session_types::AskUserQuestion,
    width: usize,
) -> (Vec<Line<'static>>, (usize, usize)) {
    let show_preview = question_shows_preview(question, state);
    let side_by_side = show_preview && width >= PREVIEW_MIN_WIDTH;
    let labels: Vec<String> = question.options.iter().map(|o| o.label.clone()).collect();
    let left_width = if side_by_side {
        adaptive_left_width(&labels, question.options.len(), width)
    } else {
        width
    };
    let (left, ranges) = option_list_lines(state, question, left_width);
    let focus = ranges
        .get(state.row_index)
        .copied()
        .unwrap_or((0, left.len().min(1)));
    if !show_preview {
        return (left, focus);
    }
    let focused_preview = match state.row_kind(state.row_index) {
        crate::tui::AskUserQuestionRow::Option => question
            .options
            .get(state.row_index)
            .and_then(|option| option.preview.as_deref()),
        _ => None,
    };
    let preview_width = if side_by_side {
        width.saturating_sub(left_width + PREVIEW_COLUMN_GAP).max(1)
    } else {
        width
    };
    let right = preview_block_lines(
        focused_preview,
        preview_width,
        MAX_PREVIEW_HEIGHT,
        !state.editing_notes,
    );
    if !side_by_side {
        return ([left, vec![Line::default()], right].concat(), focus);
    }
    (
        merge_columns(left, right, left_width, PREVIEW_COLUMN_GAP),
        focus,
    )
}

/// Zip two wrapped columns into one line set, padding the left column to
/// its width so the right column aligns.
fn merge_columns(
    left: Vec<Line<'static>>,
    right: Vec<Line<'static>>,
    left_width: usize,
    gap: usize,
) -> Vec<Line<'static>> {
    let rows = left.len().max(right.len());
    let mut lines = Vec::with_capacity(rows);
    for i in 0..rows {
        let mut line = left.get(i).cloned().unwrap_or_default();
        let pad = left_width.saturating_sub(line.width()) + gap;
        line.spans.push(Span::raw(" ".repeat(pad)));
        if let Some(right_line) = right.get(i) {
            line.spans.extend(right_line.spans.iter().cloned());
        }
        lines.push(line);
    }
    lines
}

/// The option list of one question: author options with `N. label` and an
/// indented description line, plus the auto-appended "Type something."
/// row. All rows wrap at `width`. Returns the lines plus each row's line
/// range (start, end) so the draw pass can scroll the focused row into
/// view.
fn option_list_lines(
    state: &crate::tui::InlineAskUserQuestionState,
    question: &jcode_session_types::AskUserQuestion,
    width: usize,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let multi = question.is_multi_select();
    for index in 0..state.row_count() {
        let row_start = lines.len();
        let focused = state.row_index == index;
        let prefix = if focused { "> " } else { "  " };
        let prefix_style = if focused {
            accent_style()
        } else {
            label_style()
        };
        match state.row_kind(index) {
            crate::tui::AskUserQuestionRow::Option => {
                let Some(option) = question.options.get(index) else {
                    continue;
                };
                let mut spans: Vec<Span<'static>> = Vec::new();
                if multi {
                    let toggled = state
                        .multi_selected
                        .get(state.question_index)
                        .is_some_and(|set| set.contains(&index));
                    spans.push(Span::styled(
                        if toggled { "[✔] " } else { "[ ] " },
                        if toggled {
                            accent_style()
                        } else {
                            muted_style()
                        },
                    ));
                }
                let text_style = if focused {
                    accent_style().bold()
                } else {
                    label_style()
                };
                spans.push(Span::styled(
                    format!("{}. {}", index + 1, option.label),
                    text_style,
                ));
                if option.recommended == Some(true) {
                    spans.push(Span::styled(
                        jcode_session_types::RECOMMENDED_SUFFIX,
                        muted_style(),
                    ));
                }
                lines.extend(wrap_with_prefix_styled(
                    prefix,
                    prefix_style,
                    Line::from(spans),
                    width,
                ));
                if let Some(description) = option.description.as_deref() {
                    lines.extend(wrap_with_prefix(
                        "     ",
                        Line::from(Span::styled(description.to_string(), muted_style())),
                        width,
                    ));
                }
            }
            crate::tui::AskUserQuestionRow::Custom => {
                if state.editing_custom && focused {
                    // Keep the `N.` label while editing so the row does not
                    // lose its position in the list.
                    let mut spans = vec![Span::styled(
                        format!("{}. ", index + 1),
                        accent_style().bold(),
                    )];
                    spans.extend(edit_spans(&state.custom_text, state.custom_cursor));
                    lines.extend(wrap_with_prefix_styled(
                        prefix,
                        accent_style(),
                        Line::from(spans),
                        width,
                    ));
                } else {
                    let draft = if state.custom_text.is_empty() {
                        jcode_session_types::FREE_TEXT_LABEL.to_string()
                    } else {
                        state.custom_text.clone()
                    };
                    let text_style = if focused {
                        accent_style().bold()
                    } else {
                        label_style()
                    };
                    lines.extend(wrap_with_prefix_styled(
                        prefix,
                        prefix_style,
                        Line::from(Span::styled(
                            format!("{}. {}", index + 1, draft),
                            text_style,
                        )),
                        width,
                    ));
                }
            }
        }
        ranges.push((row_start, lines.len()));
    }
    (lines, ranges)
}

/// Wrap a styled line at `width - prefix`, then prefix the first row with
/// `prefix` and continuations with matching spaces.
fn wrap_with_prefix(prefix: &str, line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    wrap_with_prefix_styled(prefix, Style::default(), line, width)
}

fn wrap_with_prefix_styled(
    prefix: &str,
    prefix_style: Style,
    line: Line<'static>,
    width: usize,
) -> Vec<Line<'static>> {
    let prefix_width = inline_view_display_width(prefix);
    if prefix_width >= width {
        let mut merged = Line::from(Span::styled(prefix.to_string(), prefix_style));
        merged.spans.extend(line.spans);
        return jcode_tui_markdown::wrap_line(merged, width.max(1));
    }
    let wrapped = jcode_tui_markdown::wrap_line(line, width - prefix_width);
    let continuation = " ".repeat(prefix_width);
    wrapped
        .into_iter()
        .enumerate()
        .map(|(i, segment)| {
            let mut spans = vec![Span::styled(
                if i == 0 {
                    prefix.to_string()
                } else {
                    continuation.clone()
                },
                prefix_style,
            )];
            spans.extend(segment.spans);
            Line::from(spans)
        })
        .collect()
}

/// Spans for an editable draft with a reverse-video cursor cell at the
/// char-offset cursor (a block space at end-of-buffer).
fn edit_spans(text: &str, cursor: usize) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let before: String = chars[..cursor].iter().collect();
    // End-of-buffer shows a `█` glyph, not a reversed space: whitespace-only
    // content is dropped by the markdown wrapper as a blank line, which
    // would hide the cursor on an empty draft.
    let (at, at_style) = match chars.get(cursor) {
        Some(c) => (
            c.to_string(),
            accent_style().add_modifier(Modifier::REVERSED),
        ),
        None => ("█".to_string(), accent_style()),
    };
    let after: String = chars[cursor.saturating_add(1).min(chars.len())..]
        .iter()
        .collect();
    vec![
        Span::styled(before, label_style()),
        Span::styled(at, at_style),
        Span::styled(after, label_style()),
    ]
}

/// The editable draft as wrapped lines with its cursor cell.
fn edit_lines(text: &str, cursor: usize, width: usize) -> Vec<Line<'static>> {
    jcode_tui_markdown::wrap_line(Line::from(edit_spans(text, cursor)), width.max(1))
}

/// The bordered preview block for the focused option: the markdown body
/// (or the empty-state placeholder), height-capped, boxed in accent, with
/// the notes affordance row below. Overflow rows collapse into a
/// `N lines hidden` indicator on the bottom border.
fn preview_block_lines(
    preview: Option<&str>,
    width: usize,
    max_height: usize,
    show_affordance: bool,
) -> Vec<Line<'static>> {
    if width < 6 || max_height < 3 {
        return Vec::new();
    }
    let inner_width = width.saturating_sub(4).max(1);
    let has_preview = preview.is_some_and(|p| !p.is_empty());
    let content: Vec<Line<'static>> = match preview.filter(|p| !p.is_empty()) {
        Some(text) => jcode_tui_markdown::render_markdown_with_width(text, Some(inner_width)),
        None => vec![Line::from(Span::styled(NO_PREVIEW_TEXT, muted_style()))],
    };
    // Rows the box can hold: two border rows, then the blank + affordance
    // pair below.
    let content_budget = max_height.saturating_sub(4).max(1);
    let hidden = content.len().saturating_sub(content_budget);
    let content = &content[..content.len().min(content_budget)];

    let accent = Style::default().fg(rgb(120, 90, 200));
    let dash = width.saturating_sub(2);
    let mut lines = vec![Line::from(Span::styled(
        format!("┌{}┐", "─".repeat(dash)),
        accent,
    ))];
    for line in content {
        let pad = inner_width.saturating_sub(line.width());
        let mut spans = vec![Span::styled("│ ", accent)];
        spans.extend(line.spans.iter().cloned());
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(" │", accent));
        lines.push(Line::from(spans));
    }
    if hidden > 0 {
        let label = format!(" ✂ ── {hidden} lines hidden ── ");
        let label_width = inline_view_display_width(&label).min(dash);
        let clipped: String = label.chars().take(label_width).collect();
        let space = dash.saturating_sub(inline_view_display_width(&clipped));
        let left_fill = "─".repeat(space / 2);
        let right_fill =
            "─".repeat(dash.saturating_sub(space / 2 + inline_view_display_width(&clipped)));
        lines.push(Line::from(Span::styled(
            format!("└{left_fill}{clipped}{right_fill}┘"),
            accent,
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!("└{}┘", "─".repeat(dash)),
            accent,
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        if has_preview && show_affordance {
            NOTES_AFFORDANCE_TEXT
        } else {
            ""
        },
        muted_style(),
    )));
    lines
}

/// The key-hint row text for the current inner state.
fn help_text(state: &crate::tui::InlineAskUserQuestionState) -> &'static str {
    if state.editing_notes {
        return "type · Enter or Esc to save · ←→ move · Home/End line edges · Backspace to delete";
    }
    if state.editing_custom {
        return "Enter to submit · ←→ move · Esc to go back";
    }
    if state.on_submit_tab() {
        return "↑↓ choose · Enter confirm · Tab to review · Esc cancel";
    }
    let multi_select = state
        .current_question()
        .is_some_and(|question| question.is_multi_select());
    if state.is_multi() {
        return if multi_select {
            "Tab/←→ switch · ↑↓ select · Space toggle · Enter confirm · n note · Esc cancel"
        } else {
            "Tab/←→ switch · ↑↓ select · Enter confirm · n note · Esc cancel"
        };
    }
    if multi_select {
        "↑↓ select · Space toggle · Enter confirm · n note · Esc cancel"
    } else {
        "↑↓ select · Enter confirm · n note · Esc cancel"
    }
}
