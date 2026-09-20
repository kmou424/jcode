//! Process-global, deadlock-free registry of each session's active model
//! identity.
//!
//! Tools executing inside an agent's own turn cannot take the agent lock (the
//! turn loop already holds it), so they cannot ask the agent for the current
//! model. This side-table — the same pattern as `session_effort` — is updated
//! whenever an agent's model is selected or switched, letting tools resolve
//! the session's `provider_key` + model id by session id alone.
//!
//! `git_commit` uses it to resolve the `${model}` sign-off placeholder to the
//! configured model display name.

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

/// A session's active model identity at last record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionModel {
    /// Persisted runtime provider key (e.g. `openai-compatible:<profile>` or a
    /// bare session vocabulary like `deepseek`), used to resolve the named
    /// provider profile for display-name lookup.
    pub provider_key: Option<String>,
    /// Provider-facing model id currently selected for the session.
    pub model: String,
}

static SESSION_MODELS: LazyLock<RwLock<HashMap<String, SessionModel>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Record a session's active model identity.
pub fn record_session_model(session_id: &str, provider_key: Option<&str>, model: &str) {
    let Ok(mut map) = SESSION_MODELS.write() else {
        return;
    };
    map.insert(
        session_id.to_string(),
        SessionModel {
            provider_key: provider_key.map(str::to_string),
            model: model.to_string(),
        },
    );
}

/// Look up a session's last-recorded model identity, if any.
pub fn session_model(session_id: &str) -> Option<SessionModel> {
    SESSION_MODELS.read().ok()?.get(session_id).cloned()
}

/// Drop a session's entry entirely (called on session teardown to bound growth).
pub fn forget_session_model(session_id: &str) {
    if let Ok(mut map) = SESSION_MODELS.write() {
        map.remove(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_reads_back_model() {
        let sid = "session-model-roundtrip";
        forget_session_model(sid);
        assert_eq!(session_model(sid), None);

        record_session_model(sid, Some("openai-compatible:nim"), "model-a");
        assert_eq!(
            session_model(sid),
            Some(SessionModel {
                provider_key: Some("openai-compatible:nim".to_string()),
                model: "model-a".to_string(),
            })
        );

        // Overwrite with a new value.
        record_session_model(sid, None, "model-b");
        assert_eq!(
            session_model(sid),
            Some(SessionModel {
                provider_key: None,
                model: "model-b".to_string(),
            })
        );

        forget_session_model(sid);
        assert_eq!(session_model(sid), None);
    }
}
