//! `tool_apply_patch_compat` experimental: a compatibility variant of the
//! `apply_patch` surface for models that cannot drive the standard tool.
//! Until [15] lands, the tag exposes the standard implementation so the
//! surface already exists.

use super::{ToolMap, tool_apply_patch};

/// Config tag enabling the compat `apply_patch` surface for a model.
pub const TAG: &str = "tool_apply_patch_compat";

/// [15] substitutes the compat implementation here; until then the tag keeps
/// the standard `apply_patch` tool on the surface.
pub(crate) fn apply(tools: &mut ToolMap) -> bool {
    tool_apply_patch::apply(tools)
}
