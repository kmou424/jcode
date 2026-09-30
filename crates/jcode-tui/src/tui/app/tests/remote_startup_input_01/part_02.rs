#[test]
fn test_handle_server_event_available_models_updated_replaces_remote_model_catalog() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_remote = true;
    app.remote_available_entries = vec!["old-model".to_string()];
    app.remote_model_options = vec![crate::provider::ModelRoute {
        display_name: None,
        context_window: None,
        model: "old-model".to_string(),
        provider: "OldProvider".to_string(),
        api_method: "old-api".to_string(),
        available: false,
        detail: "old".to_string(),
        usage: None,
        cheapness: None,
    }];

    let needs_redraw = app.handle_server_event(
        crate::protocol::ServerEvent::AvailableModelsUpdated {
            model_display_name: None,
            model_context_window: None,
            available_efforts: None,
            provider_name: Some("OpenAI".to_string()),
            provider_model: Some("new-model".to_string()),
            available_models: vec!["new-model".to_string(), "second-model".to_string()],
            available_model_routes: vec![crate::provider::ModelRoute {
                display_name: None,
                context_window: None,
                model: "new-model".to_string(),
                provider: "OpenAI".to_string(),
                api_method: "openai-oauth".to_string(),
                available: true,
                detail: String::new(),
                usage: None,
                cheapness: None,
            }],
        },
        &mut remote,
    );

    assert!(needs_redraw, "catalog replacement must redraw immediately");
    assert_eq!(
        app.remote_available_entries,
        vec!["new-model".to_string(), "second-model".to_string()]
    );
    assert_eq!(app.remote_model_options.len(), 1);
    assert_eq!(app.remote_model_options[0].model, "new-model");
    assert_eq!(app.remote_model_options[0].provider, "OpenAI");
    assert!(app.remote_model_options[0].available);
    assert_eq!(app.remote_provider_name.as_deref(), Some("OpenAI"));
    assert_eq!(app.remote_provider_model.as_deref(), Some("new-model"));
}

#[test]
fn test_refresh_model_list_command_shows_summary_and_status_notice() {
    let mut app = create_refresh_summary_test_app(crate::provider::ModelCatalogRefreshSummary {
        model_count_before: 12,
        model_count_after: 15,
        models_added: 3,
        models_removed: 0,
        models_added_names: vec![
            "cerebras-fast".to_string(),
            "cerebras-large".to_string(),
            "cerebras-reasoning".to_string(),
        ],
        models_removed_names: Vec::new(),
        route_count_before: 20,
        route_count_after: 29,
        routes_added: 9,
        routes_removed: 0,
        routes_changed: 2,
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut bus_rx = crate::bus::Bus::global().subscribe();
    while bus_rx.try_recv().is_ok() {}

    assert!(super::model_context::handle_model_command(
        &mut app,
        "/refresh-model-list"
    ));

    rt.block_on(async {
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), bus_rx.recv())
                .await
                .expect("timed out waiting for model refresh bus event")
                .expect("bus should stay open");
            let saw_completion = matches!(event, crate::bus::BusEvent::ModelRefreshCompleted(_));
            super::local::handle_bus_event(&mut app, Ok(event));
            if saw_completion {
                break;
            }
        }
    });

    assert_eq!(
        app.status_notice(),
        Some("Model list refreshed: +3 models, +9 routes, ~2 changed".to_string())
    );

    let last = app.display_messages.last().expect("display message");
    assert_eq!(last.role, "system");
    assert!(last.content.contains("Model List Refresh Complete"));
    assert!(last.content.contains("Models: 12 → 15  (+3 / -0)"));
    assert!(last.content.contains("Routes: 20 → 29  (+9 / -0 / ~2)"));
    assert!(last.content.contains("Added models:"));
    assert!(last.content.contains("cerebras-fast"));
    assert!(last.content.contains("cerebras-large"));
    assert!(last.content.contains("cerebras-reasoning"));
    assert!(
        !app.display_messages
            .iter()
            .any(|message| message.role == "background_task")
    );
    assert!(app.background_task_rows_ref().iter().any(|row| {
        row.task_id == "refresh-model-list"
            && row.status == crate::tui::BackgroundTaskRowStatus::Completed
    }));
}

#[test]
fn test_remote_available_models_updated_after_refresh_shows_summary_and_updates_catalog() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_remote = true;
    app.pending_remote_model_refresh_snapshot = Some((
        vec!["old-model".to_string()],
        vec![crate::provider::ModelRoute {
            display_name: None,
            context_window: None,
            model: "old-model".to_string(),
            provider: "OpenAI".to_string(),
            api_method: "responses".to_string(),
            available: true,
            detail: "old detail".to_string(),
            usage: None,
            cheapness: None,
        }],
    ));

    let needs_redraw = app.handle_server_event(
        crate::protocol::ServerEvent::AvailableModelsUpdated {
            model_display_name: None,
            model_context_window: None,
            available_efforts: None,
            provider_name: None,
            provider_model: None,
            available_models: vec!["old-model".to_string(), "new-model".to_string()],
            available_model_routes: vec![
                crate::provider::ModelRoute {
                    display_name: None,
                    context_window: None,
                    model: "old-model".to_string(),
                    provider: "OpenAI".to_string(),
                    api_method: "responses".to_string(),
                    available: true,
                    detail: "new detail".to_string(),
                    usage: None,
                    cheapness: None,
                },
                crate::provider::ModelRoute {
                    display_name: None,
                    context_window: None,
                    model: "new-model".to_string(),
                    provider: "OpenRouter".to_string(),
                    api_method: "chat".to_string(),
                    available: true,
                    detail: String::new(),
                    usage: None,
                    cheapness: None,
                },
            ],
        },
        &mut remote,
    );

    assert!(
        needs_redraw,
        "model refresh completion must redraw immediately"
    );
    assert_eq!(
        app.status_notice(),
        Some("Model list refreshed: +1 models, +1 routes, ~1 changed".to_string())
    );
    assert_eq!(
        app.remote_available_entries,
        vec!["old-model".to_string(), "new-model".to_string()]
    );
    assert_eq!(app.remote_model_options.len(), 2);
    assert!(app.pending_remote_model_refresh_snapshot.is_none());

    let last = app.display_messages.last().expect("display message");
    assert_eq!(last.role, "system");
    assert!(last.content.contains("Model List Refresh Complete"));
    assert!(last.content.contains("Models: 1 → 2  (+1 / -0)"));
    assert!(last.content.contains("Routes: 1 → 2  (+1 / -0 / ~1)"));
    assert!(last.content.contains("Added models: new-model"));
}

#[test]
fn test_remote_runtime_activity_notification_renders_as_system_message() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.handle_server_event(
        crate::protocol::ServerEvent::Notification {
            from_session: "jcode".to_string(),
            from_name: Some("Jcode".to_string()),
            notification_type: crate::protocol::NotificationType::Message {
                scope: Some("auth_activity".to_string()),
                channel: None,
                tldr: None,
            },
            message: "**Auth Change Received**\n\nThe server is refreshing provider credentials."
                .to_string(),
        },
        &mut remote,
    );

    let last = app.display_messages.last().expect("display message");
    assert_eq!(last.role, "system");
    assert!(last.content.contains("Auth Change Received"));
    assert_eq!(
        app.status_notice(),
        Some("Auth Change Received".to_string())
    );
}

#[test]
fn test_remote_auth_activity_notification_is_status_only_during_onboarding() {
    let mut app = create_test_app();
    let mut flow = crate::tui::app::onboarding_flow::OnboardingFlow::begin();
    flow.phase = crate::tui::app::onboarding_flow::OnboardingPhase::Login { import: None };
    app.onboarding_flow = Some(flow);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.handle_server_event(
        crate::protocol::ServerEvent::Notification {
            from_session: "jcode".to_string(),
            from_name: Some("Jcode".to_string()),
            notification_type: crate::protocol::NotificationType::Message {
                scope: Some("auth_activity".to_string()),
                channel: None,
                tldr: None,
            },
            message: "**Auth Change Received**\n\nThe server is refreshing provider credentials."
                .to_string(),
        },
        &mut remote,
    );

    assert!(
        app.display_messages.is_empty(),
        "onboarding should keep auth runtime activity out of chat"
    );
    assert_eq!(
        app.status_notice(),
        Some("Auth Change Received".to_string())
    );
}

#[test]
fn test_remote_final_catalog_activity_is_two_lines_and_completes_model_setup() {
    let mut app = create_test_app();
    app.auth_catalog_refresh_pending = true;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    let message = "**Model ready:** `gpt-5.6-sol`\nOpenAI catalog changed: models +14/-10, routes +24/-19/~3. Use `/model`.";

    app.handle_server_event(
        crate::protocol::ServerEvent::Notification {
            from_session: "jcode".to_string(),
            from_name: Some("Jcode".to_string()),
            notification_type: crate::protocol::NotificationType::Message {
                scope: Some("catalog_activity".to_string()),
                channel: None,
                tldr: None,
            },
            message: message.to_string(),
        },
        &mut remote,
    );

    assert!(!app.auth_catalog_refresh_pending);
    let last = app
        .display_messages
        .last()
        .expect("compact catalog message");
    assert_eq!(last.role, "system");
    assert_eq!(last.content.lines().count(), 2);
    assert_eq!(last.content, message);
}

#[test]
fn test_remote_auth_model_change_does_not_add_a_third_visible_line() {
    let mut app = create_test_app();
    app.auth_catalog_refresh_pending = true;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.handle_server_event(
        crate::protocol::ServerEvent::ModelChanged {
            model_display_name: None,
            model_context_window: None,
            available_efforts: None,
            id: 91,
            model: "gpt-5.6-sol".to_string(),
            provider_name: Some("OpenAI".to_string()),
            error: None,
            resolved_credential: None,
            reasoning_effort: None,
        },
        &mut remote,
    );

    assert_eq!(app.remote_provider_model.as_deref(), Some("gpt-5.6-sol"));
    assert!(app.display_messages.is_empty());
}

#[test]
fn test_remote_onboarding_catalog_activity_completes_model_setup_without_chat_noise() {
    let mut app = create_test_app();
    let mut flow = crate::tui::app::onboarding_flow::OnboardingFlow::begin();
    flow.phase = crate::tui::app::onboarding_flow::OnboardingPhase::Login { import: None };
    app.onboarding_flow = Some(flow);
    app.auth_catalog_refresh_pending = true;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.handle_server_event(
        crate::protocol::ServerEvent::Notification {
            from_session: "jcode".to_string(),
            from_name: Some("Jcode".to_string()),
            notification_type: crate::protocol::NotificationType::Message {
                scope: Some("catalog_activity".to_string()),
                channel: None,
                tldr: None,
            },
            message: "**Model ready:** `gpt-5.6-sol`\nOpenAI catalog changed: models +14/-10, routes +24/-19/~3. Use `/model`.".to_string(),
        },
        &mut remote,
    );

    assert!(!app.auth_catalog_refresh_pending);
    assert!(app.display_messages.is_empty());
}

#[test]
fn test_remote_catalog_activity_notification_upserts_compact_row() {
    let mut app = create_test_app();
    app.auth_catalog_refresh_pending = true;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    for message in [
        crate::message::format_model_refresh_progress_markdown(
            "Starting provider model catalog refresh",
            Some(5),
        ),
        crate::message::format_model_refresh_progress_markdown(
            "Waiting on provider APIs (2s elapsed)",
            Some(20),
        ),
    ] {
        app.handle_server_event(
            crate::protocol::ServerEvent::Notification {
                from_session: "jcode".to_string(),
                from_name: Some("Jcode".to_string()),
                notification_type: crate::protocol::NotificationType::Message {
                    scope: Some("catalog_activity".to_string()),
                    channel: None,
                    tldr: None,
                },
                message,
            },
            &mut remote,
        );
    }

    assert!(app.display_messages.is_empty());
    assert_eq!(app.background_task_rows_ref().len(), 1);
    assert!(app.auth_catalog_refresh_pending);
    assert_eq!(
        app.background_task_rows_ref()[0],
        crate::tui::BackgroundTaskRow {
            task_id: "refresh-model-list".to_string(),
            label: "Model list refresh".to_string(),
            percent: Some(20.0),
            status: crate::tui::BackgroundTaskRowStatus::Running,
            completed_at: None,
        }
    );
    let status = app.status_notice().expect("status notice");
    assert!(
        status.contains("Waiting on provider APIs (2s elapsed)"),
        "status should summarize latest catalog progress, got: {status}"
    );
}

#[test]
fn test_model_picker_copilot_models_have_copilot_route() {
    // Temp home: the synthesized fallback routes consult configured
    // openai-compatible profiles from ambient config/env state, and hydrate
    // from the persisted catalog cache, so a shared home lets stray routes
    // suppress the copilot fallback this test asserts on.
    with_temp_jcode_home(|| {
        // A leaked `JCODE_NAMED_PROVIDER_PROFILE` marks every
        // openai-compatible profile configured; drop it for the fallback.
        let _profile_env = EnvRestoreGuard::capture(["JCODE_NAMED_PROVIDER_PROFILE"]);
        crate::env::remove_var("JCODE_NAMED_PROVIDER_PROFILE");
        let mut app = create_test_app();
        configure_test_remote_models_with_copilot(&mut app);

        app.open_model_picker();

        let picker = app
            .inline_interactive_state
            .as_ref()
            .expect("model picker should be open");

        // grok-code-fast-1 is NOT in ALL_CLAUDE_MODELS or ALL_OPENAI_MODELS,
        // so it should get a copilot route
        let grok_entry = picker
            .entries
            .iter()
            .find(|m| m.name == "grok-code-fast-1")
            .expect("grok-code-fast-1 should be in picker");

        assert!(
            grok_entry.options.iter().any(|r| r.api_method == "copilot"),
            "grok-code-fast-1 should have a copilot route, got: {:?}",
            grok_entry.options
        );
    });
}

#[test]
fn test_model_picker_remote_comtegra_model_uses_comtegra_route_not_copilot() {
    let prev_key = std::env::var("COMTEGRA_API_KEY").ok();
    crate::env::set_var("COMTEGRA_API_KEY", "test-key");

    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_available_entries = vec!["glm-51-nvfp4".to_string()];

    app.open_model_picker();

    match prev_key {
        Some(value) => crate::env::set_var("COMTEGRA_API_KEY", value),
        None => crate::env::remove_var("COMTEGRA_API_KEY"),
    }

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should be open");
    let glm_entry = picker
        .entries
        .iter()
        .find(|m| m.name == "glm-51-nvfp4")
        .expect("glm-51-nvfp4 should be in picker");

    assert!(
        glm_entry.options.iter().any(|r| {
            r.provider == "Comtegra GPU Cloud"
                && r.api_method == "openai-compatible:comtegra"
                && r.available
        }),
        "glm route should be Comtegra/api key, got: {:?}",
        glm_entry.options
    );
    assert!(
        !glm_entry.options.iter().any(|r| r.api_method == "copilot"),
        "glm route should not fall back to Copilot, got: {:?}",
        glm_entry.options
    );
}

#[test]
fn test_model_picker_remote_bedrock_model_has_bedrock_route_when_configured() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var("JCODE_HOME").ok();
    let prev_key = std::env::var(crate::provider::bedrock::API_KEY_ENV).ok();
    let prev_region = std::env::var(crate::provider::bedrock::REGION_ENV).ok();
    let temp = tempfile::tempdir().expect("tempdir");
    crate::env::set_var("JCODE_HOME", temp.path().display().to_string());
    crate::env::set_var(crate::provider::bedrock::API_KEY_ENV, "test-bedrock-key");
    crate::env::set_var(crate::provider::bedrock::REGION_ENV, "us-east-2");
    crate::auth::AuthStatus::invalidate_cache();

    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_available_entries = vec!["us.amazon.nova-micro-v1:0".to_string()];

    app.open_model_picker();

    match prev_home {
        Some(value) => crate::env::set_var("JCODE_HOME", value),
        None => crate::env::remove_var("JCODE_HOME"),
    }
    match prev_key {
        Some(value) => crate::env::set_var(crate::provider::bedrock::API_KEY_ENV, value),
        None => crate::env::remove_var(crate::provider::bedrock::API_KEY_ENV),
    }
    match prev_region {
        Some(value) => crate::env::set_var(crate::provider::bedrock::REGION_ENV, value),
        None => crate::env::remove_var(crate::provider::bedrock::REGION_ENV),
    }
    crate::auth::AuthStatus::invalidate_cache();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should be open");
    let nova_entry = picker
        .entries
        .iter()
        .find(|m| m.name == "us.amazon.nova-micro-v1:0")
        .expect("Bedrock Nova model should be in picker");

    assert!(
        nova_entry
            .options
            .iter()
            .any(|r| { r.provider == "AWS Bedrock" && r.api_method == "bedrock" && r.available }),
        "Bedrock route should be available with credentials, got: {:?}",
        nova_entry.options
    );
}

#[test]
fn test_model_picker_preserves_recommendation_priority_order() {
    let mut app = create_test_app();
    configure_test_remote_models_with_openai_recommendations(&mut app);

    app.open_model_picker();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should be open");

    let model_names: Vec<&str> = picker.entries.iter().map(|m| m.name.as_str()).collect();

    let gpt55 = picker
        .entries
        .iter()
        .position(|model| {
            model.name == "gpt-5.5 (high)"
                && model
                    .active_option()
                    .map(|route| route.api_method == "openai-oauth" && route.provider == "OpenAI")
                    .unwrap_or(false)
        })
        .expect("gpt-5.5 should be present");
    let gpt54 = picker
        .entries
        .iter()
        .position(|model| model.name.starts_with("gpt-5.4 "))
        .expect("gpt-5.4 should be present");
    let gpt54_pro = picker
        .entries
        .iter()
        .position(|model| model.name.starts_with("gpt-5.4-pro "))
        .expect("gpt-5.4-pro should be present");
    let claude_oauth = picker
        .entries
        .iter()
        .position(|model| {
            model.name == "claude-opus-4-8 (high)"
                && model
                    .active_option()
                    .map(|route| route.api_method == "claude-oauth")
                    .unwrap_or(false)
        })
        .expect("claude-opus-4-8 oauth should be present");
    let claude_api = picker
        .entries
        .iter()
        .position(|model| {
            model.name == "claude-opus-4-8 (high)"
                && model
                    .active_option()
                    .map(|route| route.api_method == "claude-api")
                    .unwrap_or(false)
        })
        .expect("claude-opus-4-8 api key should be present");
    let spark = picker
        .entries
        .iter()
        .position(|model| model.name.starts_with("gpt-5.3-codex-spark "))
        .expect("gpt-5.3-codex-spark should be present");
    let codex = picker
        .entries
        .iter()
        .position(|model| model.name.starts_with("gpt-5.3-codex "))
        .expect("gpt-5.3-codex should be present");

    assert!(
        gpt55 < claude_oauth,
        "gpt-5.5 should rank ahead of claude-opus-4-8, got {:?}",
        model_names
    );
    assert!(
        claude_oauth < gpt54,
        "claude-opus-4-8 should rank ahead of unrecommended gpt-5.4, got {:?}",
        model_names
    );
    assert!(
        claude_api < gpt54_pro,
        "claude-opus-4-8 api key should rank ahead of unrecommended gpt-5.4-pro, got {:?}",
        model_names
    );
    assert!(
        picker.entries[gpt55].recommended,
        "gpt-5.5 high over OpenAI OAuth should be recommended"
    );
    assert!(
        picker.entries[claude_oauth].recommended,
        "claude-opus-4-8 oauth should be recommended"
    );
    assert!(
        picker.entries[claude_api].recommended,
        "claude-opus-4-8 api key should be recommended"
    );
    assert!(
        !picker.entries[gpt54].recommended,
        "gpt-5.4 should not be recommended"
    );
    assert!(
        !picker.entries[gpt54_pro].recommended,
        "gpt-5.4-pro should not be recommended"
    );
    assert!(
        !picker.entries[spark].recommended,
        "gpt-5.3-codex-spark should not be recommended"
    );
    assert!(
        !picker.entries[codex].recommended,
        "gpt-5.3-codex should not be recommended"
    );
    let recommended_routes: Vec<_> = picker
        .entries
        .iter()
        .filter(|entry| entry.recommended)
        .map(|entry| {
            let route = entry.active_option().expect("recommended entry has route");
            (
                entry.name.as_str(),
                route.provider.as_str(),
                route.api_method.as_str(),
            )
        })
        .collect();
    assert_eq!(
        recommended_routes,
        vec![
            ("gpt-5.5 (high)", "OpenAI", "openai-oauth"),
            ("claude-opus-4-8 (high)", "Anthropic", "claude-api"),
            ("claude-opus-4-8 (high)", "Anthropic", "claude-oauth"),
        ],
        "only the exact requested routes should be recommended; got {:?}",
        recommended_routes
    );
}

fn remote_catalog_route(model: &str, provider: &str, api_method: &str) -> crate::provider::ModelRoute {
    crate::provider::ModelRoute {
        display_name: None,
        context_window: None,
        model: model.to_string(),
        provider: provider.to_string(),
        api_method: api_method.to_string(),
        available: true,
        detail: String::new(),
        usage: None,
        cheapness: None,
    }
}

/// A `History` event shaped like the persisted-session fallback: provider
/// identity present, catalog fields empty. The server emits exactly this
/// while the agent is busy or still initializing its provider.
fn empty_catalog_history_event(
    provider_name: &str,
    provider_model: &str,
) -> crate::protocol::ServerEvent {
    crate::protocol::ServerEvent::History {
        id: 1,
        session_id: "session-remote-catalog".to_string(),
        messages: vec![],
        images: vec![],
        provider_name: Some(provider_name.to_string()),
        provider_model: Some(provider_model.to_string()),
        model_display_name: None,
        model_context_window: None,
        available_efforts: None,
        subagent_model: None,
        autoreview_enabled: None,
        autojudge_enabled: None,
        available_models: vec![],
        available_model_routes: vec![],
        mcp_servers: vec![],
        skills: vec![],
        total_tokens: None,
        token_usage_totals: None,
        all_sessions: vec![],
        client_count: None,
        is_canary: None,
        reload_recovery: None,
        server_version: None,
        server_name: None,
        server_icon: None,
        server_has_update: None,
        was_interrupted: None,
        connection_type: None,
        status_detail: None,
        upstream_provider: None,
        resolved_credential: None,
        reasoning_effort: None,
        service_tier: None,
        compaction_mode: crate::config::CompactionMode::Reactive,
        activity: None,
        applets: Default::default(),
        side_panel: crate::side_panel::SidePanelSnapshot::default(),
    }
}

#[test]
fn test_history_with_empty_catalog_fields_preserves_remote_models() {
    // Busy-agent History fallbacks ship `available_models`/`available_model_routes`
    // as empty vectors — "no catalog data", not "empty catalog". Applying one
    // must not wipe the route catalog a previous push already installed.
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    app.is_remote = true;
    app.handle_server_event(
        crate::protocol::ServerEvent::AvailableModelsUpdated {
            model_display_name: None,
            model_context_window: None,
            available_efforts: None,
            provider_name: Some("claude".to_string()),
            provider_model: Some("claude-sonnet-4-20250514".to_string()),
            available_models: vec![
                "claude-sonnet-4-20250514".to_string(),
                "gpt-6-astra".to_string(),
            ],
            available_model_routes: vec![
                remote_catalog_route("claude-sonnet-4-20250514", "Anthropic", "claude-api"),
                remote_catalog_route("gpt-6-astra", "Sub2API OpenAI", "openai-compatible:sub2api"),
            ],
        },
        &mut remote,
    );
    assert_eq!(app.remote_available_entries.len(), 2);
    assert_eq!(app.remote_model_options.len(), 2);

    // Same provider/model, empty catalog fields: exactly what the
    // persisted-history fallback emits while the provider is still
    // initializing (or the agent is mid-turn).
    let history = empty_catalog_history_event("claude", "claude-sonnet-4-20250514");
    app.handle_server_event(history, &mut remote);

    assert_eq!(
        app.remote_available_entries.len(),
        2,
        "a catalog-less History event must not wipe known remote model names"
    );
    assert_eq!(
        app.remote_model_options.len(),
        2,
        "a catalog-less History event must not wipe known remote model routes"
    );

    // The picker must still list every previously known model afterwards.
    app.open_model_picker();
    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should be open");
    assert!(
        picker
            .entries
            .iter()
            .any(|entry| entry.name == "gpt-6-astra"),
        "picker lost remote models after a catalog-less History event"
    );
}

#[test]
fn test_history_with_empty_catalog_and_provider_change_clears_routes() {
    // Same empty-catalog shape, but the provider actually switched: keeping
    // the old provider's routes would be stale, so they must be dropped.
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    app.is_remote = true;
    app.handle_server_event(
        crate::protocol::ServerEvent::AvailableModelsUpdated {
            model_display_name: None,
            model_context_window: None,
            available_efforts: None,
            provider_name: Some("claude".to_string()),
            provider_model: Some("claude-sonnet-4-20250514".to_string()),
            available_models: vec!["claude-sonnet-4-20250514".to_string()],
            available_model_routes: vec![remote_catalog_route(
                "claude-sonnet-4-20250514",
                "Anthropic",
                "claude-api",
            )],
        },
        &mut remote,
    );
    assert_eq!(app.remote_model_options.len(), 1);

    let history = empty_catalog_history_event("OpenAI", "gpt-6-astra");
    app.handle_server_event(history, &mut remote);

    assert!(
        app.remote_model_options.is_empty(),
        "provider switch with an empty catalog must drop the old provider's routes"
    );
    assert_eq!(app.remote_provider_name.as_deref(), Some("OpenAI"));
    assert_eq!(app.remote_provider_model.as_deref(), Some("gpt-6-astra"));
}

#[test]
fn test_remote_model_picker_with_no_routes_requests_catalog() {
    // Names may be known (live pushes arrive names-only past the size cap, or
    // the cache was never persisted) while the route catalog stays empty. The
    // picker must queue a one-shot `GetModelCatalog` — drained by the remote
    // poll loop — instead of permanently rendering placeholder routes.
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        remote.mark_history_loaded();

        app.is_remote = true;
        app.remote_provider_name = Some("Sub2API OpenAI".to_string());
        app.remote_provider_model = Some("swe-2".to_string());
        app.remote_available_entries = vec!["swe-2".to_string(), "gpt-6-astra".to_string()];
        app.remote_model_options = Vec::new();

        app.open_model_picker();

        assert!(
            app.pending_remote_model_catalog_request,
            "route-less remote picker must queue a catalog request"
        );
    });
}

#[test]
fn test_remote_model_picker_with_routes_does_not_request_catalog() {
    // When real routes are already known the picker must not burn a
    // `GetModelCatalog` round-trip.
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        remote.mark_history_loaded();

        app.is_remote = true;
        app.remote_provider_name = Some("Sub2API OpenAI".to_string());
        app.remote_provider_model = Some("swe-2".to_string());
        app.remote_available_entries = vec!["swe-2".to_string()];
        app.remote_model_options = vec![remote_catalog_route(
            "swe-2",
            "Sub2API OpenAI",
            "openai-compatible:sub2api",
        )];

        app.open_model_picker();

        assert!(
            !app.pending_remote_model_catalog_request,
            "picker with real routes must not request the catalog again"
        );
    });
}

#[test]
fn test_remote_loading_picker_labels_remote_provider() {
    // While the startup catalog is still in flight the picker renders a
    // single loading row. That row must name the remote provider, not the
    // local provider (which is a different, unrelated identity over SSH).
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    app.is_remote = true;
    app.set_remote_startup_phase(crate::tui::app::RemoteStartupPhase::LoadingSession);
    app.remote_provider_name = Some("Sub2API OpenAI".to_string());
    app.remote_provider_model = Some("swe-2".to_string());

    app.open_model_picker();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("loading picker should be open");
    assert_eq!(picker.entries.len(), 1);
    assert_eq!(
        picker.entries[0].options[0].provider, "Sub2API OpenAI",
        "loading row must carry the remote provider label"
    );

    app.clear_remote_startup_phase();
}

fn remote_catalog_route_with_availability(
    model: &str,
    provider: &str,
    api_method: &str,
    available: bool,
) -> crate::provider::ModelRoute {
    crate::provider::ModelRoute {
        display_name: None,
        context_window: None,
        model: model.to_string(),
        provider: provider.to_string(),
        api_method: api_method.to_string(),
        available,
        detail: String::new(),
        usage: None,
        cheapness: None,
    }
}

fn picker_entry_names(app: &App) -> Vec<String> {
    app.inline_interactive_state
        .as_ref()
        .expect("model picker should be open")
        .entries
        .iter()
        .map(|entry| entry.name.clone())
        .collect()
}

#[test]
fn test_model_picker_hides_models_reachable_only_through_dead_channels() {
    // A channel (`api_method` = one credential slot) with zero available
    // routes has no usable credential. Its routes — and the models reachable
    // only through it — must not list in `/model`.
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();

        app.is_remote = true;
        app.remote_provider_name = Some("Anthropic".to_string());
        app.remote_provider_model = Some("claude-opus-4-8".to_string());
        app.remote_model_options = vec![
            remote_catalog_route("claude-opus-4-8", "Anthropic", "claude-api"),
            remote_catalog_route("gpt-6-astra", "Sub2API", "openai-compatible:sub2api"),
            remote_catalog_route_with_availability(
                "grok-code-fast-1",
                "Copilot",
                "copilot",
                false,
            ),
        ];

        app.open_model_picker();

        let names = picker_entry_names(&app);
        // Effort-capable routes expand to one row per effort level
        // (`claude-opus-4-8 (high)`, ...), so match on the bare model prefix.
        assert!(
            names.iter().any(|name| name.starts_with("claude-opus-4-8")),
            "{names:?}"
        );
        assert!(
            names.iter().any(|name| name.starts_with("gpt-6-astra")),
            "{names:?}"
        );
        assert!(
            !names.iter().any(|name| name.starts_with("grok-code-fast-1")),
            "model reachable only through the dead copilot channel must be hidden: {names:?}"
        );
    });
}

#[test]
fn test_model_picker_strips_dead_channel_options_but_keeps_shared_model() {
    // A model reachable through a live channel stays listed; only the dead
    // channel's option rows disappear.
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();

        app.is_remote = true;
        app.remote_provider_name = Some("Anthropic".to_string());
        // A different current model: `route.model == current_model` keeps every
        // route for the current row, so the filter under test must apply to a
        // non-current model.
        app.remote_provider_model = Some("claude-sonnet-4".to_string());
        app.remote_model_options = vec![
            remote_catalog_route("claude-sonnet-4", "Anthropic", "claude-api"),
            remote_catalog_route("claude-opus-4-8", "Anthropic", "claude-api"),
            remote_catalog_route_with_availability(
                "claude-opus-4-8",
                "Anthropic",
                "claude-oauth",
                false,
            ),
        ];

        app.open_model_picker();

        let picker = app
            .inline_interactive_state
            .as_ref()
            .expect("model picker should be open");
        let entries: Vec<_> = picker
            .entries
            .iter()
            .filter(|entry| entry.name.starts_with("claude-opus-4-8"))
            .collect();
        assert!(
            !entries.is_empty(),
            "model with a live channel must stay listed"
        );
        for entry in &entries {
            assert!(
                entry
                    .options
                    .iter()
                    .all(|option| option.api_method == "claude-api"),
                "the dead claude-oauth channel must not appear under {:?}: {:?}",
                entry.name,
                entry.options
            );
        }
    });
}

#[test]
fn test_model_picker_keeps_current_model_when_its_channel_died() {
    // The current model always keeps its row — hiding it would leave the
    // picker unable to show where the session is.
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();

        app.is_remote = true;
        app.remote_provider_name = Some("Copilot".to_string());
        app.remote_provider_model = Some("grok-code-fast-1".to_string());
        app.remote_model_options = vec![
            remote_catalog_route("claude-opus-4-8", "Anthropic", "claude-api"),
            remote_catalog_route_with_availability(
                "grok-code-fast-1",
                "Copilot",
                "copilot",
                false,
            ),
        ];

        app.open_model_picker();

        let names = picker_entry_names(&app);
        assert!(
            names.iter().any(|name| name.starts_with("grok-code-fast-1")),
            "current model must survive the dead-channel filter: {names:?}"
        );
        assert!(
            names.iter().any(|name| name.starts_with("claude-opus-4-8")),
            "{names:?}"
        );
    });
}

#[test]
fn test_model_picker_fully_unavailable_catalog_stays_visible() {
    // Every channel dead (e.g. nothing is logged in) must not collapse the
    // picker to zero rows — showing the dead channels beats showing nothing.
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();

        app.is_remote = true;
        app.remote_provider_name = Some("Anthropic".to_string());
        app.remote_provider_model = Some("claude-opus-4-8".to_string());
        app.remote_model_options = vec![
            remote_catalog_route_with_availability(
                "claude-opus-4-8",
                "Anthropic",
                "claude-api",
                false,
            ),
            remote_catalog_route_with_availability(
                "grok-code-fast-1",
                "Copilot",
                "copilot",
                false,
            ),
        ];

        app.open_model_picker();

        let names = picker_entry_names(&app);
        assert!(
            names.len() >= 2,
            "fully-dead catalog must still list its rows: {names:?}"
        );
    });
}
