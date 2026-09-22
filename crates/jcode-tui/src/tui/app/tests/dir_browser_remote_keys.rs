// Regression for the /open overlay on the remote key path:
// `handle_remote_key_internal` routed every overlay except the directory
// browser, so Up/Down fell through to prompt-history navigation and the
// picker highlight never moved. The production TUI only runs the remote
// path, so this arm is the whole feature's key delivery.

#[test]
fn dir_browser_overlay_routes_arrow_keys_on_remote_path() {
    let (mut app, _terminal) = create_scroll_test_app(100, 30, 1, 20);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.dir_browser_overlay = Some(std::cell::RefCell::new(
        crate::tui::dir_browser::DirBrowser::new("/".to_string(), false),
    ));
    app.dir_browser_overlay
        .as_ref()
        .unwrap()
        .borrow_mut()
        .apply_listing(Ok(crate::tui::dir_browser::DirListing {
            path: "/".to_string(),
            entries: vec![
                crate::tui::dir_browser::DirEntry {
                    name: "alpha".to_string(),
                    is_dir: true,
                    is_symlink: false,
                },
                crate::tui::dir_browser::DirEntry {
                    name: "beta".to_string(),
                    is_dir: true,
                    is_symlink: false,
                },
            ],
            git: None,
        }));

    // Down must reach the overlay (not prompt history): the highlight moves
    // from `alpha` to `beta`.
    rt.block_on(app.handle_remote_key(
        KeyCode::Down,
        KeyModifiers::empty(),
        &mut remote,
    ))
    .unwrap();
    assert_eq!(
        app.dir_browser_overlay
            .as_ref()
            .unwrap()
            .borrow()
            .selected_dir()
            .as_deref(),
        Some("/beta"),
        "Down must move the picker selection"
    );

    // Up moves it back.
    rt.block_on(app.handle_remote_key(
        KeyCode::Up,
        KeyModifiers::empty(),
        &mut remote,
    ))
    .unwrap();
    assert_eq!(
        app.dir_browser_overlay
            .as_ref()
            .unwrap()
            .borrow()
            .selected_dir()
            .as_deref(),
        Some("/alpha"),
        "Up must move the picker selection back"
    );

    // Esc closes the overlay.
    rt.block_on(app.handle_remote_key(
        KeyCode::Esc,
        KeyModifiers::empty(),
        &mut remote,
    ))
    .unwrap();
    assert!(
        app.dir_browser_overlay.is_none(),
        "Esc must close the directory browser"
    );
}
