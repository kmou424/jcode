//! `tool_ask_user_question` experimental ([16]): an interactive user-prompt
//! tool. Reserved here so configs can already declare the tag without
//! warnings while [16] lands the implementation.

use super::ToolMap;

/// Config tag enabling the `ask_user_question` tool for a model.
pub const TAG: &str = "tool_ask_user_question";

/// [16] registers the tool here; until then the tag is a recognized no-op.
pub(crate) fn apply(_tools: &mut ToolMap) -> bool {
    false
}
