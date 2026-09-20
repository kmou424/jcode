//! `tool_ask_user_question` experimental: exposes the blocking
//! `ask_user_question` tool to models that opt in via
//! `experimentals = ["tool_ask_user_question"]`.

use super::ToolMap;
use crate::tool::ask_user_question::AskUserQuestionTool;
use std::sync::Arc;

/// Config tag enabling the `ask_user_question` tool for a model.
pub const TAG: &str = "tool_ask_user_question";

/// Registry name the tool is filed under.
pub const TOOL_NAME: &str = "ask_user_question";

/// Ensure the ask-user-question tool is present on the session surface.
/// Returns whether the map changed.
pub(crate) fn apply(tools: &mut ToolMap) -> bool {
    if tools.contains_key(TOOL_NAME) {
        return false;
    }
    tools.insert(TOOL_NAME.to_string(), Arc::new(AskUserQuestionTool::new()));
    true
}
