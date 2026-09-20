//! `tool_apply_patch` experimental: exposes the standard `apply_patch` tool to
//! models that opt in via `experimentals = ["tool_apply_patch"]`.

use super::ToolMap;
use crate::tool::apply_patch::ApplyPatchTool;
use std::sync::Arc;

/// Config tag enabling the standard `apply_patch` tool for a model.
pub const TAG: &str = "tool_apply_patch";

/// Registry name the standard apply-patch implementation is filed under.
pub const TOOL_NAME: &str = "apply_patch";

/// Ensure the standard apply-patch tool is present on the session surface.
/// Returns whether the map changed.
pub(crate) fn apply(tools: &mut ToolMap) -> bool {
    if tools.contains_key(TOOL_NAME) {
        return false;
    }
    tools.insert(TOOL_NAME.to_string(), Arc::new(ApplyPatchTool::new()));
    true
}
