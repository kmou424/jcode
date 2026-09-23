// `copy_selection_mode` could be toggled on but no keys were routed to it
// on the remote key path — the only path production runs — so the mode
// swallowed nothing and Esc could not leave it.

#[test]
fn copy_selection_mode_routes_keys_on_remote_path() {
    let (mut app, _terminal) = create_scroll_test_app(100, 30, 1, 20);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.enter_copy_selection_mode();
    assert!(app.copy_selection_mode);

    // A plain key is consumed by copy mode — it must not reach the input
    // line. 'a' selects all in copy mode (and would type into the draft
    // otherwise).
    rt.block_on(app.handle_remote_key(
        KeyCode::Char('a'),
        KeyModifiers::empty(),
        &mut remote,
    ))
    .unwrap();
    assert_eq!(app.input(), "", "copy mode must consume plain keys");

    // Esc exits copy mode.
    rt.block_on(app.handle_remote_key(
        KeyCode::Esc,
        KeyModifiers::empty(),
        &mut remote,
    ))
    .unwrap();
    assert!(
        !app.copy_selection_mode,
        "Esc must exit copy selection mode"
    );
}
