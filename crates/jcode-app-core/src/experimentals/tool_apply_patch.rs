//! `tool_apply_patch` experimental: exposes the freeform `apply_patch` tool to
//! models that opt in via `experimentals = ["tool_apply_patch"]`. On the
//! Responses wire it is declared as a `custom` tool with a Lark grammar so
//! capable models emit raw patch text; on other wires it degrades to a
//! function tool with a single `input` string parameter.

use super::ToolMap;
use crate::tool::apply_patch::ApplyPatchFreeformTool;
use std::sync::Arc;

/// Config tag enabling the freeform `apply_patch` tool for a model.
pub const TAG: &str = "tool_apply_patch";

/// Registry name the apply-patch implementation is filed under.
pub const TOOL_NAME: &str = "apply_patch";

/// Ensure the freeform apply-patch tool is present on the session surface.
/// Returns whether the map changed.
pub(crate) fn apply(tools: &mut ToolMap) -> bool {
    // Both variants register under TOOL_NAME; the freeform grammar marker in
    // the advertised definition distinguishes which one is installed.
    let existing_freeform = tools
        .get(TOOL_NAME)
        .is_some_and(|tool| tool.to_definition().freeform_format().is_some());
    if existing_freeform {
        return false;
    }
    tools.insert(
        TOOL_NAME.to_string(),
        Arc::new(ApplyPatchFreeformTool::new()),
    );
    true
}
