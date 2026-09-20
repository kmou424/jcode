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
    for tag in KNOWN_TAGS.iter().copied().filter(|tag| tags.contains(*tag)) {
        changed |= match tag {
            tool_apply_patch::TAG => tool_apply_patch::apply(tools),
            tool_apply_patch_compat::TAG => tool_apply_patch_compat::apply(tools),
            tool_ask_user_question::TAG => tool_ask_user_question::apply(tools),
            _ => unreachable!("KNOWN_TAGS only lists dispatched tags"),
        };
    }
    for tag in tags {
        if !KNOWN_TAGS.contains(&tag.as_str()) {
            warn_unknown_tag(tag);
        }
    }
    if !apply_patch_surface_enabled(tags) {
        changed |= tools.remove(tool_apply_patch::TOOL_NAME).is_some();
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
    fn ask_user_question_tag_is_a_recognized_no_op_until_landed() {
        let mut tools = map_with_apply_patch();
        let tags = tags_of(&[tool_ask_user_question::TAG]);
        // Recognized (no warning), but nothing is registered yet; the
        // apply_patch removal still applies since no apply_patch tag is set.
        assert!(apply(&mut tools, &tags));
        assert!(!tools.contains_key("apply_patch"));
    }
}
