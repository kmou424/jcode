//! `tool_apply_patch_compat` experimental: the JSON-schema `apply_patch`
//! variant for distilled models that cannot drive a freeform `custom` tool.
//! It is declared as an ordinary function tool with a `patch_text` string
//! parameter and shares the same parse/exec engine as the freeform variant.
//!
//! Mutually exclusive with `tool_apply_patch`: both tags together are a
//! configuration error surfaced by the dispatcher, which then enables neither.

use super::ToolMap;
use crate::tool::apply_patch::ApplyPatchTool;
use std::sync::Arc;

/// Config tag enabling the compat `apply_patch` surface for a model.
pub const TAG: &str = "tool_apply_patch_compat";

/// Ensure the JSON-schema apply-patch tool is present on the session surface.
/// Returns whether the map changed.
pub(crate) fn apply(tools: &mut ToolMap) -> bool {
    const TOOL_NAME: &str = super::tool_apply_patch::TOOL_NAME;
    // Both variants register under TOOL_NAME; the freeform grammar marker in
    // the advertised definition distinguishes which one is installed.
    let existing_json = tools
        .get(TOOL_NAME)
        .is_some_and(|tool| tool.to_definition().freeform_format().is_none());
    if existing_json {
        return false;
    }
    tools.insert(TOOL_NAME.to_string(), Arc::new(ApplyPatchTool::new()));
    true
}
