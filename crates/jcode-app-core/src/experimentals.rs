//! Per-model experimental feature gating (`experimentals = [...]` on a
//! `[providers.<profile>]` `[[models]]` config entry).
//!
//! Each experimental feature lives in its own submodule exporting a
//! `pub const TAG: &str` plus a hook that transforms the session's live tool
//! map. The dispatcher imports the tag constants so a missing or renamed
//! feature fails at compile time rather than silently drifting from config.
//!
//! The transform is idempotent and runs against the session registry's live
//! map (see `Registry::apply_experimentals`), so both the advertised tool
//! definitions and execution dispatch observe the same gated surface, and a
//! model switch takes effect on the next request.

use crate::tool::Tool;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, RwLock as StdRwLock};

pub mod tool_apply_patch;
pub mod tool_apply_patch_compat;
pub mod tool_ask_user_question;

/// Session tool map shape, shared with `tool::Registry`.
pub type ToolMap = HashMap<String, Arc<dyn Tool>>;

/// Experimental tags the dispatcher knows, in fixed application order.
/// Iterating this list rather than the caller's set keeps overlapping hooks
/// deterministic: a later hook wins a tool name it shares with an earlier one.
const KNOWN_TAGS: &[&str] = &[
    tool_apply_patch::TAG,
    tool_apply_patch_compat::TAG,
    tool_ask_user_question::TAG,
];

/// Resolve the `experimentals` tag set configured for the active model.
/// `provider_key` is the session's persisted route identity (e.g.
/// `openai-compatible:<profile>`); non-named routes cannot declare per-model
/// experimentals and resolve to an empty set.
pub(crate) fn tags_for_model(provider_key: Option<&str>, model_id: &str) -> HashSet<String> {
    crate::provider_catalog::named_provider_model_experimentals_for_provider_key(
        provider_key,
        model_id,
    )
    .into_iter()
    .collect()
}

/// Whether an apply_patch-family tag keeps the `apply_patch` tool on the
/// session surface. Without one, `apply_patch` is removed from the map.
fn apply_patch_surface_enabled(tags: &HashSet<String>) -> bool {
    tags.contains(tool_apply_patch::TAG) || tags.contains(tool_apply_patch_compat::TAG)
}

/// Built-in editing tools the apply_patch surface replaces when enabled.
/// The two families are mutually exclusive: a model that can emit patches
/// must not also see `edit`/`multiedit`/`write`/`patch`.
pub const EDIT_FAMILY_TOOLS: &[&str] = &["edit", "multiedit", "write", "patch"];

/// A fresh instance of a built-in editing tool by registry name. The base
/// tools are stateless, so restoring the family after the apply_patch tags
/// are removed is just a re-insert.
fn edit_family_tool(name: &str) -> Option<Arc<dyn Tool>> {
    let tool: Arc<dyn Tool> = match name {
        "edit" => Arc::new(crate::tool::edit::EditTool::new()),
        "multiedit" => Arc::new(crate::tool::multiedit::MultiEditTool::new()),
        "write" => Arc::new(crate::tool::write::WriteTool::new()),
        "patch" => Arc::new(crate::tool::patch::PatchTool::new()),
        _ => return None,
    };
    Some(tool)
}

/// Detect the invalid configuration of both apply_patch tags on one model:
/// `tool_apply_patch` (freeform) and `tool_apply_patch_compat` (JSON) are
/// mutually exclusive. Returns a user-facing error message when both are set.
pub fn patch_tag_conflict(tags: &HashSet<String>) -> Option<String> {
    if tags.contains(tool_apply_patch::TAG) && tags.contains(tool_apply_patch_compat::TAG) {
        Some(format!(
            "Configuration error: `experimentals` lists both '{}' and '{}' on the active model. \
             The two apply_patch variants are mutually exclusive; neither was enabled and the \
             built-in editing tools remain available. Remove one tag to fix this.",
            tool_apply_patch::TAG,
            tool_apply_patch_compat::TAG
        ))
    } else {
        None
    }
}

/// Warn once per process about a tag this build does not implement. Called on
/// every `apply` pass, so unknown tags must not spam the log on each request.
fn warn_unknown_tag(tag: &str) {
    static WARNED: LazyLock<StdRwLock<HashSet<String>>> =
        LazyLock::new(|| StdRwLock::new(HashSet::new()));
    let mut warned = WARNED
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if warned.insert(tag.to_string()) {
        crate::logging::warn(&format!(
            "Ignoring unknown experimental tag '{tag}' configured on the active model"
        ));
    }
}

/// Transform the session's live tool map for the active model's
/// `experimentals` tags. Returns whether the map changed so callers can
/// invalidate cached tool-definition snapshots.
pub fn apply(tools: &mut ToolMap, tags: &HashSet<String>) -> bool {
    let mut changed = false;
    // Both apply_patch tags configured is an error: enable neither variant
    // and leave the built-in editing family untouched.
    let patch_conflict = patch_tag_conflict(tags).is_some();
    for tag in KNOWN_TAGS.iter().copied().filter(|tag| tags.contains(*tag)) {
        changed |= match tag {
            tool_apply_patch::TAG => !patch_conflict && tool_apply_patch::apply(tools),
            tool_apply_patch_compat::TAG => {
                !patch_conflict && tool_apply_patch_compat::apply(tools)
            }
            tool_ask_user_question::TAG => tool_ask_user_question::apply(tools),
            _ => unreachable!("KNOWN_TAGS only lists dispatched tags"),
        };
    }
    for tag in tags {
        if !KNOWN_TAGS.contains(&tag.as_str()) {
            warn_unknown_tag(tag);
        }
    }
    if apply_patch_surface_enabled(tags) && !patch_conflict {
        // The apply_patch surface replaces the built-in editing family.
        for name in EDIT_FAMILY_TOOLS {
            changed |= tools.remove(*name).is_some();
        }
    } else {
        changed |= tools.remove(tool_apply_patch::TOOL_NAME).is_some();
        // Restore the built-in editing family when the apply_patch surface is
        // not active (idempotent on a converged map).
        for name in EDIT_FAMILY_TOOLS {
            if !tools.contains_key(*name)
                && let Some(tool) = edit_family_tool(name)
            {
                tools.insert((*name).to_string(), tool);
                changed = true;
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_with_apply_patch() -> ToolMap {
        let mut tools = ToolMap::new();
        tools.insert(
            tool_apply_patch::TOOL_NAME.to_string(),
            Arc::new(crate::tool::apply_patch::ApplyPatchTool::new()) as Arc<dyn Tool>,
        );
        tools
    }

    fn map_with_edit_family() -> ToolMap {
        let mut tools = ToolMap::new();
        for name in EDIT_FAMILY_TOOLS {
            tools.insert(
                (*name).to_string(),
                edit_family_tool(name).expect("known edit family tool"),
            );
        }
        tools
    }

    fn tags_of(tags: &[&str]) -> HashSet<String> {
        tags.iter().map(|tag| tag.to_string()).collect()
    }

    #[test]
    fn apply_patch_is_removed_without_a_tag() {
        let mut tools = map_with_apply_patch();
        assert!(apply(&mut tools, &HashSet::new()));
        assert!(!tools.contains_key("apply_patch"));
        // Converged: a second pass reports no change.
        assert!(!apply(&mut tools, &HashSet::new()));
    }

    #[test]
    fn apply_patch_tag_restores_the_tool() {
        let mut tools = ToolMap::new();
        let tags = tags_of(&[tool_apply_patch::TAG]);
        assert!(apply(&mut tools, &tags));
        assert!(tools.contains_key("apply_patch"));
        assert!(!apply(&mut tools, &tags));
    }

    #[test]
    fn freeform_tag_registers_the_freeform_variant() {
        let mut tools = ToolMap::new();
        let tags = tags_of(&[tool_apply_patch::TAG]);
        apply(&mut tools, &tags);
        let tool = tools.get("apply_patch").expect("apply_patch registered");
        let def = tool.to_definition();
        assert!(
            def.freeform_format().is_some(),
            "freeform variant must declare a grammar format"
        );
    }

    #[test]
    fn compat_tag_registers_the_json_variant() {
        let mut tools = ToolMap::new();
        let tags = tags_of(&[tool_apply_patch_compat::TAG]);
        apply(&mut tools, &tags);
        let tool = tools.get("apply_patch").expect("apply_patch registered");
        let def = tool.to_definition();
        assert!(def.freeform_format().is_none());
        assert!(
            def.input_schema
                .get("properties")
                .and_then(|p| p.get("patch_text"))
                .is_some(),
            "compat variant keeps the patch_text JSON parameter"
        );
    }

    #[test]
    fn any_apply_patch_tag_removes_the_edit_family() {
        for tag in [tool_apply_patch::TAG, tool_apply_patch_compat::TAG] {
            let mut tools = map_with_edit_family();
            let tags = tags_of(&[tag]);
            assert!(apply(&mut tools, &tags));
            assert!(tools.contains_key("apply_patch"));
            for name in EDIT_FAMILY_TOOLS {
                assert!(
                    !tools.contains_key(*name),
                    "{name} must be removed while {tag} is active"
                );
            }
            // Converged on a second pass.
            assert!(!apply(&mut tools, &tags));
        }
    }

    #[test]
    fn edit_family_returns_when_tags_are_removed() {
        let mut tools = map_with_edit_family();
        let tags = tags_of(&[tool_apply_patch::TAG]);
        assert!(apply(&mut tools, &tags));
        for name in EDIT_FAMILY_TOOLS {
            assert!(!tools.contains_key(*name));
        }
        // Switching to a model without the tag restores the family and drops
        // apply_patch.
        assert!(apply(&mut tools, &HashSet::new()));
        assert!(!tools.contains_key("apply_patch"));
        for name in EDIT_FAMILY_TOOLS {
            assert!(tools.contains_key(*name), "{name} must be restored");
        }
        assert!(!apply(&mut tools, &HashSet::new()));
    }

    #[test]
    fn both_patch_tags_is_a_conflict_enabling_neither() {
        let mut tools = map_with_edit_family();
        tools.insert(
            tool_apply_patch::TOOL_NAME.to_string(),
            Arc::new(crate::tool::apply_patch::ApplyPatchTool::new()) as Arc<dyn Tool>,
        );
        let tags = tags_of(&[tool_apply_patch::TAG, tool_apply_patch_compat::TAG]);
        assert!(patch_tag_conflict(&tags).is_some());
        assert!(apply(&mut tools, &tags));
        // Neither patch variant is enabled; the edit family stays.
        assert!(!tools.contains_key("apply_patch"));
        for name in EDIT_FAMILY_TOOLS {
            assert!(tools.contains_key(*name));
        }
        assert!(!apply(&mut tools, &tags));
    }

    #[test]
    fn compat_tag_also_keeps_apply_patch_on_the_surface() {
        let mut tools = map_with_apply_patch();
        let tags = tags_of(&[tool_apply_patch_compat::TAG]);
        assert!(!apply(&mut tools, &tags));
        assert!(tools.contains_key("apply_patch"));
    }

    #[test]
    fn unknown_tags_are_ignored_not_fatal() {
        let mut tools = map_with_apply_patch();
        let tags = tags_of(&["tool_future_thing"]);
        // Only the apply_patch removal applies; the unknown tag is a no-op.
        assert!(apply(&mut tools, &tags));
        assert!(!tools.contains_key("apply_patch"));
    }

    #[test]
    fn ask_user_question_tag_registers_the_tool() {
        let mut tools = map_with_apply_patch();
        let tags = tags_of(&[tool_ask_user_question::TAG]);
        assert!(apply(&mut tools, &tags));
        assert!(tools.contains_key(tool_ask_user_question::TOOL_NAME));
        // A second pass is idempotent.
        assert!(!apply(&mut tools, &tags));
        // The apply_patch removal still applies since no apply_patch tag is set.
        assert!(!tools.contains_key("apply_patch"));
    }
}
