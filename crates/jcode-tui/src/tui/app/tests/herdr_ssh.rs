// Client-side herdr reporting under `--ssh`. The remote server cannot
// spawn `herdr` for the local pane, so `handle_server_event` re-derives the
// lifecycle from client state and reports it from the TUI process. These
// tests point `HERDR_BIN_PATH` at a stub script that records its argv.

struct RestoreEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl RestoreEnv {
    fn capture(keys: &[&'static str]) -> Self {
        Self(
            keys.iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect(),
        )
    }
}

impl Drop for RestoreEnv {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..) {
            if let Some(value) = value {
                crate::env::set_var(key, value);
            } else {
                crate::env::remove_var(key);
            }
        }
    }
}

/// Install a `herdr` stub that appends its argv to `$HERDR_STUB_OUT`, one
/// invocation per line. Returns the output path.
fn install_herdr_stub() -> std::path::PathBuf {
    let dir = tempfile::tempdir().expect("stub dir");
    let bin = dir.path().join("herdr-stub.sh");
    let out = dir.path().join("herdr-calls.log");
    std::fs::write(&bin, "#!/bin/sh\necho \"$@\" >> \"$HERDR_STUB_OUT\"\n")
        .expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
    }
    crate::env::set_var("HERDR_BIN_PATH", &bin);
    crate::env::set_var("HERDR_STUB_OUT", &out);
    // The tempdir must outlive the spawned stub; leak it for the test scope.
    std::mem::forget(dir);
    out
}

/// Block until the stub log contains `needle` (or a short timeout passes).
/// Reports are spawned detached, so the write lands asynchronously.
fn stub_log_waited(out: &std::path::Path, needle: &str) -> String {
    for _ in 0..100 {
        let log = std::fs::read_to_string(out).unwrap_or_default();
        if log.contains(needle) {
            return log;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::fs::read_to_string(out).unwrap_or_default()
}

fn ask_question_event() -> crate::protocol::ServerEvent {
    crate::protocol::ServerEvent::AskUserQuestion {
        request_id: "req_1".to_string(),
        session_id: "s_ssh".to_string(),
        questions: vec![jcode_session_types::AskUserQuestion {
            question: "pick one".to_string(),
            header: None,
            options: vec![jcode_session_types::AskUserQuestionOption {
                label: "yes".to_string(),
                description: None,
                preview: None,
                recommended: None,
            }],
            multi_select: None,
        }],
    }
}

#[test]
fn ssh_remote_reports_lifecycle_to_local_herdr() {
    let _restore = RestoreEnv::capture(&[
        "HERDR_ENV",
        "HERDR_PANE_ID",
        "HERDR_BIN_PATH",
        "HERDR_STUB_OUT",
    ]);
    with_ssh_remote_test_home(|| {
        crate::env::set_var("HERDR_ENV", "1");
        crate::env::set_var("HERDR_PANE_ID", "w1:p9");
        let out = install_herdr_stub();

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _rt_guard = rt.enter();
        let mut app = App::new_for_remote(None);
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        app.remote_session_id = Some("s_ssh".to_string());

        // Attach: publishes the session identity and the idle state.
        app.handle_server_event(crate::protocol::ServerEvent::Done { id: 0 }, &mut remote);
        let log = stub_log_waited(&out, "--state idle");
        assert!(
            log.contains("report-agent-session --source custom:jcode --agent jcode --agent-session-id s_ssh w1:p9"),
            "identity report missing: {log}"
        );
        assert!(
            log.contains("report-agent --source custom:jcode --agent jcode --state idle --agent-session-id s_ssh w1:p9"),
            "idle report missing: {log}"
        );

        // An adopted remote turn reports working; its end reports idle.
        app.handle_server_event(
            crate::protocol::ServerEvent::TextDelta {
                text: "hi".to_string(),
            },
            &mut remote,
        );
        stub_log_waited(&out, "--state working");
        app.handle_server_event(crate::protocol::ServerEvent::Interrupted, &mut remote);
        stub_log_waited(&out, "report-agent --source custom:jcode --agent jcode --state idle");

        // A pending ask_user_question reports blocked, then working once the
        // answer lets the remote turn resume.
        app.handle_server_event(
            crate::protocol::ServerEvent::TextDelta {
                text: "again".to_string(),
            },
            &mut remote,
        );
        stub_log_waited(&out, "--state working");
        app.handle_server_event(ask_question_event(), &mut remote);
        let log = stub_log_waited(&out, "--state blocked");
        assert!(
            log.contains("--message awaiting answer"),
            "blocked report should carry the awaiting message: {log}"
        );
        app.inline_ask_user_question_state = None;
        app.handle_server_event(
            crate::protocol::ServerEvent::TextDelta {
                text: "resumed".to_string(),
            },
            &mut remote,
        );
        stub_log_waited(&out, "--state working");

        // Attaching a different session re-publishes identity.
        app.remote_session_id = Some("s_other".to_string());
        app.handle_server_event(
            crate::protocol::ServerEvent::TextDelta {
                text: "other".to_string(),
            },
            &mut remote,
        );
        let log = stub_log_waited(&out, "--agent-session-id s_other w1:p9");
        assert!(
            log.contains("report-agent-session --source custom:jcode --agent jcode --agent-session-id s_other w1:p9"),
            "identity for new session missing: {log}"
        );
    });
}

#[test]
fn ssh_remote_reports_nothing_outside_herdr() {
    let _restore = RestoreEnv::capture(&[
        "HERDR_ENV",
        "HERDR_PANE_ID",
        "HERDR_BIN_PATH",
        "HERDR_STUB_OUT",
    ]);
    with_ssh_remote_test_home(|| {
        crate::env::remove_var("HERDR_ENV");
        crate::env::remove_var("HERDR_PANE_ID");
        let out = install_herdr_stub();

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _rt_guard = rt.enter();
        let mut app = App::new_for_remote(None);
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        app.remote_session_id = Some("s_ssh".to_string());
        app.handle_server_event(
            crate::protocol::ServerEvent::TextDelta {
                text: "hi".to_string(),
            },
            &mut remote,
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !out.exists(),
            "no reports should spawn outside herdr: {:?}",
            std::fs::read_to_string(&out)
        );
    });
}

#[test]
fn non_ssh_remote_does_not_client_report() {
    let _restore = RestoreEnv::capture(&[
        "JCODE_SSH_REMOTE",
        "HERDR_ENV",
        "HERDR_PANE_ID",
        "HERDR_BIN_PATH",
        "HERDR_STUB_OUT",
    ]);
    with_temp_jcode_home(|| {
        // A plain `jcode connect` client still reports through the daemon's
        // forwarded env snapshot, so the client path must stay silent.
        crate::env::remove_var("JCODE_SSH_REMOTE");
        crate::env::set_var("HERDR_ENV", "1");
        crate::env::set_var("HERDR_PANE_ID", "w1:p9");
        let out = install_herdr_stub();

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _rt_guard = rt.enter();
        let mut app = App::new_for_remote(None);
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        app.handle_server_event(
            crate::protocol::ServerEvent::TextDelta {
                text: "hi".to_string(),
            },
            &mut remote,
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !out.exists(),
            "non-SSH clients must not double-report: {:?}",
            std::fs::read_to_string(&out)
        );
    });
}
