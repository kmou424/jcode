//! Human-friendly model names live in `jcode-provider-core` so the TUI and
//! Jcode Desktop render identical labels.
//!
//! `pretty_model_display_name` is wrapped here so configured `display_name`
//! labels (remote wire labels, named-provider profiles, per-model entries)
//! win over the built-in prettification. `pretty_known_model_family` is a
//! plain re-export.

pub(crate) use jcode_provider_core::model_names::pretty_known_model_family;

/// Turn a raw model id into a friendlier display name. A configured
/// `display_name` wins over every heuristic.
pub(crate) fn pretty_model_display_name(model: &str) -> String {
    // `display_name` wins over every heuristic. This is how custom
    // provider/model labels reach the /model picker, header, and status line.
    if let Some(label) = crate::provider_catalog::remote_model_display_name(model)
        .or_else(|| crate::provider_catalog::active_named_provider_model_display_name(model))
        .or_else(|| crate::provider_catalog::unique_named_provider_model_display_name(model))
        && !label.trim().is_empty()
    {
        return label;
    }
    jcode_provider_core::model_names::pretty_model_display_name(model)
}

#[cfg(test)]
mod tests {
    #[test]
    fn pretty_model_display_name_prefers_remote_wire_label() {
        let _guard = crate::storage::lock_test_env();
        let model = "remote-label-model";
        crate::provider_catalog::set_remote_model_display_name(
            model,
            Some("Remote Label".to_string()),
        );
        let resolved = super::pretty_model_display_name(model);
        crate::provider_catalog::set_remote_model_display_name(model, None);
        assert_eq!(resolved, "Remote Label");
    }
}
