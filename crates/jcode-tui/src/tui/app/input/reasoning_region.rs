//! Streaming reasoning region: the live "thinking" block rendered as dim,
//! italic text in the streaming buffer.
//!
//! Extracted from `input.rs`, which is over the code-size budget. Grouped here
//! because these methods share one fragile invariant: `reasoning_partial_len`
//! and `reasoning_block_start` are **byte offsets into
//! `streaming.streaming_text`**, recorded at one point and used to slice the
//! buffer later. If the buffer is replaced in between, those offsets describe
//! nothing, and slicing at a non-boundary offset panics and kills the process
//! (issues #632/#633/#635). Every slice here therefore snaps to a character
//! boundary, and every buffer replacement clears the offsets.
//!
//! Display data is mode-agnostic: every closed block anchors a `reasoning`
//! [`DisplayMessage`] carrying the full sentinel-wrapped markup and the
//! measured duration, no matter which `reasoning_display` mode was active
//! while it streamed. `off` mode accumulates the raw text into
//! `reasoning_hidden_block` instead of the live stream; the anchored row is
//! still produced, it just renders to nothing while the mode stays `off`.
//! Visibility (full text / `✻ thought for Ns` label / live-only / hidden) is
//! decided at render time by `render_reasoning_message` and the `reasoning`
//! arm in `ui_prepare.rs` off the *current* mode, so toggling the mode
//! re-renders all captured history correctly.

use super::floor_char_boundary;
use crate::tui::app::{App, DisplayMessage};

/// Live header shown at the top of a `compact` reasoning block while thinking
/// streams. Claude Code-style glyph (`✻`, U+273B) with the lowercase phrasing
/// this mode uses throughout (`thinking…`/`thought for`). It is emitted through
/// the same sentinel-wrapped dim+italic markup as the reasoning body so it
/// renders identically and collapses away with the rest of the block. It is a
/// live-only decoration: `close_reasoning_region` strips it from the anchored
/// block so the stored reasoning row contains only model-produced text.
const COMPACT_REASONING_LIVE_HEADER: &str = "✻ thinking…";

/// Build the sentinel-wrapped dim+italic markup for a raw reasoning block's
/// text (one markup line per source line). Mirrors what the live stream path
/// produces incrementally via `append_reasoning_text`.
fn reasoning_block_markup(raw: &str) -> String {
    let mut markup = String::with_capacity(raw.len() + raw.len() / 4);
    // `str::lines` covers a trailing unterminated line too.
    for line in raw.lines() {
        markup.push_str(&jcode_tui_markdown::reasoning_line_markup(line));
    }
    markup.trim_end_matches('\n').to_string()
}

/// Strip the live-only `✻ thinking…` header line from a compact-mode block's
/// markup, if present as the first sentinel line.
fn strip_compact_live_header(block: &str) -> String {
    let header = jcode_tui_markdown::reasoning_line_markup(COMPACT_REASONING_LIVE_HEADER);
    if let Some(rest) = block.strip_prefix(header.as_str()) {
        rest.to_string()
    } else {
        // The header can lose its trailing hard break when the block was
        // trimmed; match a bare `*sentinel✻ thinking…sentinel*` prefix too.
        let bare = header.trim_end();
        match block.strip_prefix(bare) {
            Some(rest) => rest.trim_start_matches('\n').to_string(),
            None => block.to_string(),
        }
    }
}

impl App {
    /// Begin a reasoning region. Reasoning renders as dim, italic text (no
    /// blockquote gutter, no footer). `compact` mode prepends the Claude
    /// Code-style `✻ thinking…` header line; `off` mode records nothing in the
    /// live stream (raw text accumulates in `reasoning_hidden_block` instead)
    /// and other modes have no header. Idempotent while open.
    pub(in crate::tui::app) fn open_reasoning_region(&mut self) {
        if self.reasoning_streaming {
            return;
        }
        self.reasoning_streaming = true;
        self.reasoning_pending_line.clear();
        self.reasoning_hidden_block.clear();
        self.reasoning_partial_len = 0;
        // NOTE: `reasoning_close_duration_secs` is intentionally not reset
        // here. The paced buffer can apply the buffered reasoning characters
        // that open this region *after* ThinkingDone/ReasoningDone already
        // recorded the duration for this block, so clearing on open would
        // clobber it. The close path drains the field instead.
        if self.reasoning_off_mode() {
            // Hidden accumulation: nothing lands in the live stream, so there
            // is no block offset to track.
            self.reasoning_block_start = None;
            return;
        }
        // Separate the reasoning block from any prior content with a blank line.
        self.begin_stream_block();
    }

    /// Open the live-stream half of a reasoning block: blank-line separator,
    /// `reasoning_block_start`, and (compact mode) the `✻ thinking…` header.
    /// Called from `open_reasoning_region` for visible modes, and lazily from
    /// `append_reasoning_text` when a mid-block mode toggle (`off` → visible)
    /// makes a previously hidden region start streaming — the block must still
    /// be sliced out and anchored at close, so the offset has to exist.
    fn begin_stream_block(&mut self) {
        if !self.streaming.streaming_text.is_empty() {
            if self.streaming.streaming_text.ends_with("\n\n") {
                // already separated
            } else if self.streaming.streaming_text.ends_with('\n') {
                self.append_streaming_text("\n");
            } else {
                self.append_streaming_text("\n\n");
            }
        }
        // Remember where this reasoning block starts in the stream so the close
        // path can slice it back out in place (without disturbing any preceding
        // answer text) once the block ends.
        self.reasoning_block_start = Some(self.streaming.streaming_text.len());
        if self.reasoning_compact_mode() {
            // The header is part of the block (recorded after `block_start`), so
            // it is sliced out with the reasoning text when the block closes and
            // stripped before anchoring.
            self.streaming
                .streaming_text
                .push_str(&jcode_tui_markdown::reasoning_line_markup(
                    COMPACT_REASONING_LIVE_HEADER,
                ));
            self.refresh_split_view_if_needed();
        }
    }

    /// Remove the live partial-reasoning tail (the rendered, not-yet-committed
    /// in-progress line) from the streaming buffer so it can be rebuilt. No-op
    /// when there is no live partial.
    pub(in crate::tui::app) fn strip_reasoning_partial_tail(&mut self) {
        if self.reasoning_partial_len > 0 {
            let new_len = self
                .streaming
                .streaming_text
                .len()
                .saturating_sub(self.reasoning_partial_len);
            // `String::truncate` panics when `new_len` is not a UTF-8 boundary.
            // The tail length is normally exact, but the buffer can be replaced
            // out from under us (reconnect/resume replays a server snapshot), so
            // snap to the nearest boundary at or below `new_len` instead of
            // trusting the recorded length (see issues #632/#633/#635).
            let new_len = floor_char_boundary(&self.streaming.streaming_text, new_len);
            self.streaming.streaming_text.truncate(new_len);
            self.reasoning_partial_len = 0;
        }
    }

    /// Append streamed reasoning text. With a visible mode active, complete
    /// lines (terminated by `\n`) are committed to the live stream as
    /// dim+italic markdown and the trailing partial line renders as a live tail
    /// that is re-emitted in place on each delta, so reasoning trickles in
    /// token-by-token. In `off` mode the raw text accumulates into
    /// `reasoning_hidden_block` untouched; the whole-line emphasis run is still
    /// produced at close so a later mode toggle can reveal the block.
    pub(in crate::tui::app) fn append_reasoning_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.reasoning_streaming {
            self.open_reasoning_region();
        }
        if self.reasoning_off_mode() {
            self.reasoning_hidden_block.push_str(text);
            return;
        }
        // The region may have opened under `off` (hidden accumulation) and the
        // mode toggled to a visible one mid-block: lazily open the live-stream
        // block now so this text is still sliced out and anchored at close.
        if self.reasoning_block_start.is_none() {
            self.begin_stream_block();
        }
        // Drop the previous live tail; we rebuild committed lines + a fresh tail.
        self.strip_reasoning_partial_tail();
        let mut committed = String::new();
        for ch in text.chars() {
            if ch == '\n' {
                let line = std::mem::take(&mut self.reasoning_pending_line);
                committed.push_str(&jcode_tui_markdown::reasoning_line_markup(&line));
            } else {
                self.reasoning_pending_line.push(ch);
            }
        }
        if !committed.is_empty() {
            self.streaming.streaming_text.push_str(&committed);
        }
        // Re-append the live tail for the in-progress (partial) line.
        let partial = jcode_tui_markdown::reasoning_partial_markup(&self.reasoning_pending_line);
        self.reasoning_partial_len = partial.len();
        self.streaming.streaming_text.push_str(&partial);
        self.refresh_split_view_if_needed();
    }

    /// Promote the live partial line to a committed line and end the region.
    /// `duration_secs` is the thinking time bundled with the paced close op
    /// (if any); the value recorded at ThinkingEnd/ThinkingDone/ReasoningDone
    /// and finally the still-ticking `thinking_start` are the fallbacks for
    /// closes that carry no payload.
    ///
    /// In every mode the closed block leaves the live stream and anchors
    /// *in place* as a `reasoning` display message carrying the full
    /// sentinel-wrapped markup plus the resolved duration. Whether that row
    /// renders (and as full text vs. the compact `✻ thought for Ns` label) is
    /// decided at render time by `render_reasoning_message`, so mode toggles
    /// re-render the same captured data.
    pub(in crate::tui::app) fn close_reasoning_region(&mut self, duration_secs: Option<f64>) {
        // Drain the recorded duration up-front so it belongs to this close and
        // cannot leak into a later block, no matter which path ends the region.
        let recorded_duration = self.reasoning_close_duration_secs.take();
        if !self.reasoning_streaming {
            return;
        }
        self.reasoning_streaming = false;
        // Duration priority: the value bundled with the paced close op, then
        // what was recorded at ThinkingEnd/ThinkingDone/ReasoningDone, then a
        // still-ticking `thinking_start` estimate.
        let secs = duration_secs.or(recorded_duration).or_else(|| {
            self.thinking_start
                .map(|start| start.elapsed().as_secs_f64())
        });

        // Hidden (`off`-mode) accumulation and the live-stream block can both
        // hold data when the mode toggled mid-block; merge them in order.
        let mut block = String::new();
        if !self.reasoning_hidden_block.is_empty() {
            block.push_str(&reasoning_block_markup(&self.reasoning_hidden_block));
            self.reasoning_hidden_block.clear();
        }
        // Replace the live tail with the committed (newline-terminated) line.
        self.strip_reasoning_partial_tail();
        let pending = std::mem::take(&mut self.reasoning_pending_line);
        if !pending.is_empty() {
            self.streaming
                .streaming_text
                .push_str(&jcode_tui_markdown::reasoning_line_markup(&pending));
        }
        if let Some(block_start) = self.reasoning_block_start.take() {
            // Same hazard as `strip_reasoning_partial_tail`: this is a byte
            // offset recorded against an earlier state of the buffer, so
            // `split_off` panics on a non-boundary offset — snap it to a
            // character boundary (see issues #632/#633/#635).
            let block_start = floor_char_boundary(&self.streaming.streaming_text, block_start);
            // Everything from the block start onward is the reasoning markup.
            // Split it off so the preceding answer text (if any) stays in the
            // live stream.
            let mut streamed = self.streaming.streaming_text.split_off(block_start);
            // Drop the separator the open path added before the reasoning block
            // so the surrounding answer text rejoins cleanly.
            while self.streaming.streaming_text.ends_with('\n') {
                self.streaming.streaming_text.pop();
            }
            let streamed = {
                let trimmed = streamed.trim_matches('\n').to_string();
                streamed.clear();
                // The `✻ thinking…` live header is a status decoration, not
                // reasoning content — strip it from the anchored block. Applied
                // unconditionally (not just under `compact`) because a mid-block
                // mode toggle can close a compact-opened block under any mode.
                strip_compact_live_header(&trimmed)
            };
            if !streamed.is_empty() {
                if !block.is_empty() {
                    block.push('\n');
                }
                block.push_str(&streamed);
            }
        }

        if block.trim().is_empty() {
            self.refresh_split_view_if_needed();
            return;
        }
        self.anchor_reasoning_block(block, secs);
    }

    /// True when the active reasoning-display mode is `compact` (Claude
    /// Code-style: live `✻ thinking…` header + dim/italic reasoning, then a
    /// one-line `✻ thought for Ns` trace once the block closes).
    pub(in crate::tui::app) fn reasoning_compact_mode(&self) -> bool {
        matches!(
            crate::tui::ui::reasoning_display_mode(),
            crate::config::ReasoningDisplayMode::Compact
        )
    }

    /// True when the active reasoning-display mode is `off` (no reasoning in
    /// the live stream; blocks accumulate hidden and anchor as rows that only
    /// render once the mode changes).
    pub(in crate::tui::app) fn reasoning_off_mode(&self) -> bool {
        matches!(
            crate::tui::ui::reasoning_display_mode(),
            crate::config::ReasoningDisplayMode::Off
        )
    }

    /// Anchor a closed reasoning block — full sentinel-wrapped markup plus its
    /// measured duration — as a `reasoning` display message exactly where it
    /// streamed. Used in every mode: the trace keeps its position (content
    /// below it can only be appended, never inserted above) and its visibility
    /// is governed at render time by the current `reasoning_display` mode.
    /// Every anchored row registers in `turn_reasoning_traces` so `current`
    /// mode knows which rows still belong to the in-flight turn.
    pub(in crate::tui::app) fn anchor_reasoning_block(
        &mut self,
        block: String,
        duration_secs: Option<f64>,
    ) {
        // Answer text that streamed *before* the block must commit first so the
        // anchored trace lands after it in the transcript (chronological order).
        if !self.streaming.streaming_text.trim().is_empty() {
            let preceding = self.take_streaming_text();
            let preceding = self.collapse_reasoning_for_commit(preceding);
            if !preceding.trim().is_empty() {
                self.push_display_message(DisplayMessage::assistant(preceding));
            }
        }
        self.turn_reasoning_traces
            .push(crate::tui::app::TurnReasoningTrace {
                display_index: self.display_messages.len(),
                // Snapshot the transcript height when this trace anchors. The trace
                // begins life at the viewport tail; once the transcript grows a
                // full viewport beyond this point the trace is provably off-screen
                // (while tail-following) and its live membership can be dropped.
                wrapped_lines_at_anchor: crate::tui::ui::last_total_wrapped_lines(),
            });
        self.push_display_message(DisplayMessage::reasoning_with_duration(
            block,
            duration_secs,
        ));
        self.refresh_split_view_if_needed();
    }

    /// End the current turn's live-reasoning membership: anchored rows stay in
    /// the transcript (they carry the data `full`/`compact` modes render), but
    /// under `current` mode they stop rendering once the next prompt makes them
    /// historical. Called when the next user prompt is submitted.
    pub(in crate::tui::app) fn clear_turn_reasoning_traces(&mut self) {
        if self.turn_reasoning_traces.is_empty() {
            return;
        }
        self.turn_reasoning_traces.clear();
        // Live rows became historical: under `current` mode that hides them, so
        // the prepared transcript changes even though no message was removed.
        self.bump_display_messages_version();
        self.refresh_split_view_if_needed();
    }
}
