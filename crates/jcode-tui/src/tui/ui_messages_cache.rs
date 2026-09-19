use super::*;

pub(super) use jcode_tui_messages::{centered_wrap_width, left_pad_lines_for_centered_mode};

pub(crate) fn get_cached_message_lines<F>(
    msg: &DisplayMessage,
    width: u16,
    diff_mode: crate::config::DiffDisplayMode,
    render: F,
) -> Vec<Line<'static>>
where
    F: FnOnce(&DisplayMessage, u16, crate::config::DiffDisplayMode) -> Vec<Line<'static>>,
{
    jcode_tui_messages::get_cached_message_lines(
        msg,
        width,
        diff_mode,
        jcode_tui_messages::MessageCacheContext {
            diagram_mode: crate::config::config().display.diagram_mode,
            centered: markdown::center_code_blocks(),
            // Message lines contain Mermaid placeholder rows. Size clicks must
            // invalidate this cache just like a completed deferred render does.
            mermaid_epoch: crate::tui::mermaid::deferred_render_epoch()
                .wrapping_add(crate::tui::mermaid::mermaid_inline_expand_epoch()),
            mermaid_aspect_bucket: crate::tui::mermaid::current_preferred_aspect_ratio_bucket(),
            show_agentgrep_output: crate::config::config().display.show_agentgrep_output,
            // Source these two from the same accessors the renderers consult so
            // the key flips exactly when the rendered output would (in tests the
            // accessors honor thread-local overrides).
            show_bash_output: tools_ui::show_bash_output(),
            tool_call_details: tools_ui::show_tool_call_details(),
            reasoning_display: crate::config::config().display.reasoning_display(),
            // Any transcript-affecting `display.*` change bumps this epoch so
            // history re-renders under the new setting.
            display_epoch: super::display_epoch(),
        },
        render,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mermaid_expand_transition_invalidates_cached_message_lines() {
        let hash = 0xf4ce_a123_u64;
        let msg = DisplayMessage::assistant(format!(
            "```mermaid\nflowchart LR\nA[cache-{hash}] --> B\n```"
        ));

        crate::tui::mermaid::set_mermaid_inline_expand_level(hash, 0);
        let first =
            get_cached_message_lines(&msg, 80, crate::config::DiffDisplayMode::Off, |_, _, _| {
                vec![Line::from("fit")]
            });
        assert_eq!(first.len(), 1);

        crate::tui::mermaid::set_mermaid_inline_expand_level(hash, 1);
        let second =
            get_cached_message_lines(&msg, 80, crate::config::DiffDisplayMode::Off, |_, _, _| {
                vec![Line::from("large-1"), Line::from("large-2")]
            });
        crate::tui::mermaid::set_mermaid_inline_expand_level(hash, 0);

        assert_eq!(
            second.len(),
            2,
            "the message cache must rerender after a Mermaid size transition"
        );
    }

    /// A display toggle change bumps the epoch baked into the per-message
    /// cache context. The same message must re-render under the new epoch
    /// instead of serving lines cached against the old one.
    #[test]
    fn display_epoch_bump_invalidates_cached_message_lines() {
        let msg = DisplayMessage::assistant("display epoch probe");

        let first =
            get_cached_message_lines(&msg, 80, crate::config::DiffDisplayMode::Off, |_, _, _| {
                vec![Line::from("before")]
            });
        assert_eq!(first.len(), 1);

        crate::tui::ui::bump_display_epoch();
        let second =
            get_cached_message_lines(&msg, 80, crate::config::DiffDisplayMode::Off, |_, _, _| {
                vec![Line::from("after-1"), Line::from("after-2")]
            });

        assert_eq!(
            second.len(),
            2,
            "a display epoch bump must invalidate the per-message line cache"
        );
    }
}
