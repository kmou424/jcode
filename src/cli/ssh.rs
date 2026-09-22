//! Native SSH attach: the TUI is local; its daemon and workspace are remote.

use anyhow::{Context, Result, bail};

use super::args::{Args, Command};
use super::provider_init::ProviderChoice;

fn validate(args: &Args) -> Result<()> {
    match args.command {
        None | Some(Command::SelfDev { build: false }) | Some(Command::Open { .. }) => {}
        Some(Command::SelfDev { build: true }) => {
            bail!(
                "--ssh self-dev --build is not supported: run builds on the remote host, then reconnect"
            )
        }
        _ => bail!("--ssh supports the interactive client, open, and self-dev only"),
    }
    if matches!(args.command, Some(Command::Open { .. })) && args.resume.is_some() {
        bail!("--ssh open picks a remote directory for a new launch; --resume does not apply")
    }
    if args.resume.as_deref().is_some_and(|id| {
        id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    }) {
        bail!(
            "--ssh --resume requires an explicit remote session ID; local session lookup is not used"
        )
    }
    if args.provider != ProviderChoice::Auto
        || args.model.is_some()
        || args.provider_profile.is_some()
        || args.tool_profile.is_some()
        || args.tools.is_some()
        || args.disabled_tools.is_some()
        || args.disable_base_tools
        || args.mcp_tools.is_some()
        || args.mcp_tools_token_threshold.is_some()
    {
        bail!(
            "provider/tool startup flags cannot configure an existing SSH server; use the remote /model command or configure the remote host"
        )
    }
    if args.onboarding_sim || args.update_sim {
        bail!("local onboarding/update simulators cannot run in an SSH session")
    }
    Ok(())
}

pub(crate) async fn run(args: Args) -> Result<()> {
    validate(&args)?;
    #[cfg(unix)]
    {
        run_unix(args).await
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        bail!("native SSH TUI attach currently requires a Unix client")
    }
}

#[cfg(unix)]
async fn run_unix(args: Args) -> Result<()> {
    let mut args = args;
    let host = args.ssh.as_deref().expect("SSH dispatch requires a host");
    let binary = args.ssh_binary.as_deref().unwrap_or("jcode");
    super::output::stderr_info(format!("Connecting local Jcode UI to {host} over SSH..."));
    let open_start = match &args.command {
        Some(Command::Open { path }) => Some(
            path.clone()
                .or_else(|| args.remote_working_dir.clone())
                .or_else(|| std::env::var("JCODE_SSH_OPEN_DIR").ok()),
        ),
        _ => None,
    };
    if let Some(start) = open_start {
        // Remote directory picker before the workspace bridge exists: a
        // probe bridge answers browse_dir ops, Enter's pick becomes the
        // workspace --cwd for the real connection, Esc exits here.
        let Some(dir) = pick_remote_dir(
            host,
            binary,
            args.ssh_server_socket.as_deref(),
            start.as_deref(),
        )
        .await?
        else {
            return Ok(());
        };
        args.remote_working_dir = Some(dir);
        args.command = None;
    }
    // `--remote-working-dir` is deprecated; `/open`-spawned clients pass the
    // picked remote dir through JCODE_SSH_OPEN_DIR instead. The flag still
    // wins when both are present.
    let requested_dir = args
        .remote_working_dir
        .clone()
        .or_else(|| std::env::var("JCODE_SSH_OPEN_DIR").ok())
        .map(|dir| dir.trim().to_string())
        .filter(|dir| !dir.is_empty());
    let mut connection = super::ssh_transport::NativeSsh::connect_with_workspace(
        host,
        binary,
        args.ssh_server_socket.as_deref(),
        requested_dir.as_deref(),
    )
    .await?;
    let working_dir = connection.remote_working_dir().to_owned();
    crate::env::set_var("JCODE_SSH_REMOTE", host);
    crate::env::set_var("JCODE_SSH_BINARY", binary);
    crate::env::set_var("JCODE_SSH_WORKING_DIR", &working_dir);
    // sideband ops capability, advertised by the remote bridge's
    // handshake. Older remote binaries omit it and TUI client ops degrade
    // to a fast, explicit failure instead of waiting on a reply.
    if connection.handshake().sideband_ops {
        crate::env::set_var("JCODE_SSH_OPS", "1");
    } else {
        crate::env::remove_var("JCODE_SSH_OPS");
    }
    if let Some(socket) = args.ssh_server_socket.as_deref() {
        crate::env::set_var("JCODE_SSH_SERVER_SOCKET", socket);
    } else {
        crate::env::remove_var("JCODE_SSH_SERVER_SOCKET");
    }
    if matches!(args.command, Some(Command::SelfDev { .. })) {
        crate::env::set_var(super::selfdev::CLIENT_SELFDEV_ENV, "1");
    } else {
        crate::env::remove_var(super::selfdev::CLIENT_SELFDEV_ENV);
    }
    crate::server::set_socket_path(
        connection
            .socket_path()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("SSH adapter socket is not UTF-8"))?,
    );
    super::output::stderr_info(format!(
        "Remote: {} | workspace: {} | {}",
        connection.host(),
        working_dir,
        connection.handshake().version,
    ));
    // The guard remains alive for the whole UI, including reconnect attempts.
    // Each local connection gets its own owned SSH bridge to the shared daemon.
    use tokio::signal::unix::{SignalKind, signal};
    // Keep the transport guard outside the cancellable UI future and explicitly
    // reap its SSH children before returning to runtime shutdown.
    let mut hup = signal(SignalKind::hangup())?;
    let mut term = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut quit = signal(SignalKind::quit())?;
    let result = tokio::select! {
        result = super::tui_launch::run_tui_client(
            args.resume, None, false, true, Some(working_dir), false, false,
        ) => result,
        _ = hup.recv() => Ok(()),
        _ = term.recv() => Ok(()),
        _ = interrupt.recv() => Ok(()),
        _ = quit.recv() => Ok(()),
    };
    let cleanup = connection.close().await;
    result.and(cleanup)
}

/// `jcode --ssh <host> open`: hold a probe bridge open while the directory
/// picker runs against `browse_dir` sideband replies, starting at `start`
/// (the `open` arg, else `--remote-working-dir`, else the remote login
/// HOME). Returns the Enter pick; None means the user quit with Esc and
/// no session should launch.
#[cfg(unix)]
async fn pick_remote_dir(
    host: &str,
    remote_binary: &str,
    daemon_socket: Option<&str>,
    start: Option<&str>,
) -> Result<Option<String>> {
    use super::ssh_transport::RemoteBrowser;
    use crate::ssh_ops::SshOpResult;
    use crate::tui::dir_browser::DirListing;

    let mut browser = RemoteBrowser::connect(host, remote_binary, daemon_socket, start).await?;
    if !browser.sideband_ops() {
        browser.close().await;
        bail!("remote bridge does not support sideband client ops; update the remote jcode binary");
    }
    let start_dir = browser.working_dir().to_owned();

    // The picker is a blocking crossterm loop on a dedicated thread; this
    // task owns the probe bridge and answers each browse_dir request it
    // forwards over the channel pair.
    let (req_tx, mut req_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (res_tx, res_rx) = std::sync::mpsc::channel::<Result<DirListing, String>>();
    let bridge = tokio::spawn(async move {
        while let Some(path) = req_rx.recv().await {
            let listing = browser
                .browse(&path)
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| match result {
                    SshOpResult::BrowseDir { path, entries, git } => {
                        Ok(DirListing::from_ssh(path, entries, git))
                    }
                    _ => Err("unexpected remote reply to browse_dir".to_string()),
                });
            if res_tx.send(listing).is_err() {
                break;
            }
        }
        browser.close().await;
    });

    let picked = tokio::task::spawn_blocking(move || {
        super::tui_launch::run_open_picker_loop(start_dir, true, move |path| {
            if req_tx.send(path.to_string()).is_err() {
                return Err("remote browse connection closed".to_string());
            }
            res_rx
                .recv()
                .unwrap_or_else(|_| Err("remote browse connection closed".to_string()))
        })
    })
    .await
    .context("open picker task failed")??;
    // `req_tx` dropped with the picker closure, so the bridge task is
    // already finishing; wait for the SSH child to be reaped.
    let _ = bridge.await;
    Ok(picked)
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Return a remote-aware resume command, never a local session lookup.
pub(crate) fn resume_hint(session_id: &str) -> Option<String> {
    let host = std::env::var("JCODE_SSH_REMOTE").ok()?;
    let mut args = vec!["jcode".to_string(), "--ssh".to_string(), quote(&host)];
    for (flag, variable) in [
        ("--ssh-binary", "JCODE_SSH_BINARY"),
        ("--ssh-server-socket", "JCODE_SSH_SERVER_SOCKET"),
    ] {
        if let Ok(value) = std::env::var(variable) {
            args.extend([flag.to_owned(), quote(&value)]);
        }
    }
    args.extend(["--resume".to_string(), quote(session_id)]);
    if super::selfdev::client_selfdev_requested() {
        args.push("self-dev".to_string());
    }
    Some(args.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn remote_modes_accept_explicit_remote_ids_without_local_lookup() {
        for argv in [
            vec!["jcode", "--ssh", "dev"],
            vec![
                "jcode",
                "--ssh",
                "dev",
                "--resume",
                "session_remote_123",
                "self-dev",
            ],
        ] {
            validate(&Args::try_parse_from(argv).unwrap()).unwrap();
        }
    }

    #[test]
    fn open_is_allowed_but_not_with_resume() {
        for argv in [
            vec!["jcode", "--ssh", "dev", "open"],
            vec!["jcode", "--ssh", "dev", "open", "/srv/repo"],
            vec![
                "jcode",
                "--ssh",
                "dev",
                "--remote-working-dir",
                "/srv",
                "open",
            ],
        ] {
            validate(&Args::try_parse_from(argv).unwrap()).unwrap();
        }
        assert!(
            validate(
                &Args::try_parse_from(vec![
                    "jcode",
                    "--ssh",
                    "dev",
                    "--resume",
                    "session_remote_1",
                    "open"
                ])
                .unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn remote_modes_reject_local_only_operations_before_connecting() {
        for tail in [
            vec!["--resume"],
            vec!["self-dev", "--build"],
            vec!["run", "test"],
            vec!["--model", "local-model"],
            vec!["--onboarding-sim"],
            vec!["--tools", "bash"],
        ] {
            let mut argv = vec!["jcode", "--ssh", "dev"];
            argv.extend(tail);
            assert!(validate(&Args::try_parse_from(argv).unwrap()).is_err());
        }
    }

    #[test]
    fn resume_hint_retains_remote_identity_and_quotes_paths() {
        let _lock = crate::storage::lock_test_env();
        let names = [
            "JCODE_SSH_REMOTE",
            "JCODE_SSH_BINARY",
            "JCODE_SSH_WORKING_DIR",
            "JCODE_SSH_SERVER_SOCKET",
            super::super::selfdev::CLIENT_SELFDEV_ENV,
        ];
        let previous: Vec<_> = names.iter().map(std::env::var_os).collect();
        for name in names {
            crate::env::remove_var(name);
        }
        crate::env::set_var("JCODE_SSH_REMOTE", "dev");
        crate::env::set_var("JCODE_SSH_WORKING_DIR", "/srv/sam's repo");
        let hint = resume_hint("session_remote_1").unwrap();
        assert!(hint.contains("--ssh 'dev'"));
        assert!(hint.contains("--resume 'session_remote_1'"));
        // A resume binds the session's stored remote dir, so the hint must
        // not carry the deprecated --remote-working-dir flag.
        assert!(!hint.contains("--remote-working-dir"));
        for (name, value) in names.into_iter().zip(previous) {
            match value {
                Some(value) => crate::env::set_var(name, value),
                None => crate::env::remove_var(name),
            }
        }
    }
}
