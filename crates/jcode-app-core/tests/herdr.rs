//! Env-gating and state-mapping tests for the herdr lifecycle reporter.
//! These live in an integration target so they exercise the public surface
//! exactly as a consumer would, and stay runnable even when unrelated lib
//! test modules fail to compile.

use std::collections::HashMap;
use std::sync::Mutex;

use jcode_app_core::herdr::{self, AgentState};

// Tests that mutate HERDR_* env or depend on the process-global REGISTERED
// flag must not interleave: env is process-wide and a concurrent spawn would
// pollute the stub log or flip the flag for a neighbor test.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    move |key| map.get(key).cloned()
}

#[test]
fn disabled_without_herdr_env() {
    assert!(herdr::resolve_target(env(&[("HERDR_PANE_ID", "w1:p1")])).is_none());
    assert!(herdr::resolve_target(env(&[])).is_none());
    assert!(
        herdr::resolve_target(env(&[("HERDR_ENV", "0"), ("HERDR_PANE_ID", "w1:p1"),])).is_none()
    );
}

#[test]
fn disabled_without_pane_id() {
    assert!(herdr::resolve_target(env(&[("HERDR_ENV", "1")])).is_none());
}

#[test]
fn resolves_pane_and_default_binary() {
    let target = herdr::resolve_target(env(&[("HERDR_ENV", "1"), ("HERDR_PANE_ID", "w1:p1")]))
        .expect("target");
    assert_eq!(target.pane_id, "w1:p1");
    assert_eq!(target.bin, "herdr");
}

#[test]
fn prefers_herdr_bin_path() {
    let target = herdr::resolve_target(env(&[
        ("HERDR_ENV", "1"),
        ("HERDR_PANE_ID", "w1:p1"),
        ("HERDR_BIN_PATH", "/usr/local/bin/herdr"),
    ]))
    .expect("target");
    assert_eq!(target.bin, "/usr/local/bin/herdr");
}

#[test]
fn agent_state_names_match_herdr_enum() {
    assert_eq!(AgentState::Idle.as_str(), "idle");
    assert_eq!(AgentState::Working.as_str(), "working");
    assert_eq!(AgentState::Blocked.as_str(), "blocked");
}

#[test]
fn report_is_noop_outside_herdr() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    // The test harness never sets HERDR_ENV; reporting must silently no-op
    // instead of spawning anything.
    herdr::report(AgentState::Working);
    herdr::report_for_session(AgentState::Blocked, Some("s_test"), Some("awaiting answer"));
    herdr::report_session_identity("s_test");
    herdr::release();
    herdr::release_if_registered();
}

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
                jcode_core::env::set_var(key, value);
            } else {
                jcode_core::env::remove_var(key);
            }
        }
    }
}

/// Point HERDR_* at a stub that appends its argv to $HERDR_STUB_OUT.
/// Returns the log path; the tempdir is leaked so the stub outlives spawns.
fn stub_bin() -> std::path::PathBuf {
    let dir = tempfile::tempdir().expect("stub dir");
    let bin = dir.path().join("herdr-stub.sh");
    let out = dir.path().join("calls.log");
    std::fs::write(&bin, "#!/bin/sh\necho \"$@\" >> \"$HERDR_STUB_OUT\"\n").expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
    }
    jcode_core::env::set_var("HERDR_ENV", "1");
    jcode_core::env::set_var("HERDR_PANE_ID", "w1:p9");
    jcode_core::env::set_var("HERDR_BIN_PATH", &bin);
    jcode_core::env::set_var("HERDR_STUB_OUT", &out);
    std::mem::forget(dir);
    out
}

fn log_waited(out: &std::path::Path, needle: &str) -> String {
    for _ in 0..100 {
        let log = std::fs::read_to_string(out).unwrap_or_default();
        if log.contains(needle) {
            return log;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::fs::read_to_string(out).unwrap_or_default()
}

#[test]
fn release_spawns_release_agent_with_pane() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let _restore = RestoreEnv::capture(&[
        "HERDR_ENV",
        "HERDR_PANE_ID",
        "HERDR_BIN_PATH",
        "HERDR_STUB_OUT",
    ]);
    let out = stub_bin();
    herdr::release();
    let log = log_waited(&out, "release-agent");
    assert!(
        log.contains("release-agent --source custom:jcode --agent jcode w1:p9"),
        "unexpected release argv: {log}"
    );
}

#[test]
fn release_if_registered_requires_a_prior_report() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let _restore = RestoreEnv::capture(&[
        "HERDR_ENV",
        "HERDR_PANE_ID",
        "HERDR_BIN_PATH",
        "HERDR_STUB_OUT",
    ]);
    let out = stub_bin();
    herdr::__test_reset_registered();
    // A process that never reported must not release another agent's pane.
    herdr::release_if_registered();
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(!out.exists(), "release fired without any report");

    // After a successful report the exit path releases the pane agent.
    herdr::report(AgentState::Idle);
    log_waited(&out, "--state idle");
    herdr::release_if_registered();
    let log = log_waited(&out, "release-agent");
    assert!(
        log.contains("release-agent --source custom:jcode --agent jcode w1:p9"),
        "release argv missing after report: {log}"
    );
}
