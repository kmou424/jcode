#[tokio::test]
async fn resume_restored_session_emits_session_id_with_anchor() -> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let (_runtime, prev_runtime) = setup_runtime_dir()?;

    let target_session_id = "session_resume_anchor_target";
    let temp_session_id = "session_resume_anchor_temp";
    let anchor_dir = "/tmp/jcode-resume-anchor-restored";

    let mut persisted = crate::session::Session::create_with_id(
        target_session_id.to_string(),
        None,
        Some("Resume Anchor".to_string()),
    );
    persisted.working_dir = Some(anchor_dir.to_string());
    persisted.save()?;

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::new(provider.clone()).await;
    let agent = Arc::new(Mutex::new(build_test_agent_with_id(
        provider.clone(),
        registry.clone(),
        temp_session_id,
        Vec::new(),
    )));

    let sessions = Arc::new(RwLock::new(HashMap::from([(
        temp_session_id.to_string(),
        Arc::clone(&agent),
    )])));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::<String, InterruptSignal>::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let now = Instant::now();
    let client_connections = Arc::new(RwLock::new(HashMap::from([(
        "conn_restore".to_string(),
        ClientConnectionInfo {
            client_id: "conn_restore".to_string(),
            session_id: temp_session_id.to_string(),
            client_instance_id: None,
            debug_client_id: Some("debug_restore".to_string()),
            connected_at: now,
            last_seen: now,
            is_processing: false,
            current_tool_name: None,
            terminal_env: Vec::new(),
            disconnect_tx: mpsc::unbounded_channel().0,
        },
    )])));
    let client_debug_state = Arc::new(RwLock::new(ClientDebugState::default()));
    let (placeholder_event_tx, _placeholder_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        temp_session_id.to_string(),
        SwarmMember {
            session_id: temp_session_id.to_string(),
            event_tx: placeholder_event_tx,
            event_txs: HashMap::new(),
            working_dir: None,
            swarm_id: None,
            swarm_enabled: false,
            status: "ready".to_string(),
            detail: None,
            task_label: None,
            friendly_name: Some("restore".to_string()),
            report_back_to_session_id: None,
            latest_completion_report: None,
            role: "agent".to_string(),
            joined_at: now,
            last_status_change: now,
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: crate::protocol::SwarmMemberRuntime::default(),
        },
    )])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::<String, HashSet<String>>::new()));
    let file_touch = FileTouchService::new();
    let channel_subscriptions = Arc::new(RwLock::new(HashMap::<
        String,
        HashMap<String, HashSet<String>>,
    >::new()));
    let channel_subscriptions_by_session = Arc::new(RwLock::new(HashMap::<
        String,
        HashMap<String, HashSet<String>>,
    >::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::<String, String>::new()));
    let client_count = Arc::new(RwLock::new(1usize));
    let (writer, _peer_stream) = test_writer()?;
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let event_history = Arc::new(RwLock::new(VecDeque::<SwarmEvent>::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel::<SwarmEvent>(8);
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let mut client_selfdev = false;
    let mut client_session_id = temp_session_id.to_string();

    handle_resume_session(
        47,
        target_session_id.to_string(),
        None,
        None,
        false,
        false,
        &mut client_selfdev,
        &mut client_session_id,
        "conn_restore",
        &agent,
        &provider,
        &registry,
        &sessions,
        &shutdown_signals,
        &soft_interrupt_queues,
        &client_connections,
        &client_debug_state,
        &swarm_members,
        &swarms_by_id,
        &file_touch,
        &channel_subscriptions,
        &channel_subscriptions_by_session,
        &swarm_plans,
        &swarm_coordinators,
        &client_count,
        &writer,
        "test-server",
        "🌿",
        &client_event_tx,
        &mcp_pool,
        &event_history,
        &event_counter,
        &swarm_event_tx,
        false,
    )
    .await?;

    let events = collect_events_until_done(&mut client_event_rx, 47).await;
    let session_id_event = events.iter().find_map(|event| match event {
        ServerEvent::SessionId {
            session_id,
            working_dir,
        } => Some((session_id.clone(), working_dir.clone())),
        _ => None,
    });
    assert_eq!(
        session_id_event,
        Some((target_session_id.to_string(), Some(anchor_dir.to_string()))),
        "resume must emit SessionId with the restored session's anchor, got {events:?}"
    );

    restore_runtime_dir(prev_runtime);
    Ok(())
}
