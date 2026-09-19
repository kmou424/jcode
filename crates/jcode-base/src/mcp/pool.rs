//! Shared MCP Server Pool
//!
//! Manages a global pool of MCP server processes that are shared across
//! all jcode sessions. Instead of each session spawning its own set of
//! MCP servers (N sessions × M servers = N×M processes), sessions share
//! a single pool (M processes total).
//!
//! Sessions get lightweight `McpHandle` clones that can send concurrent
//! requests to shared server processes. Request/response correlation by
//! ID ensures no interference between sessions.

use super::client::{McpClient, McpHandle};
use super::protocol::{McpConfig, McpServerConfig, McpToolDef};
use anyhow::{Context, Result};
use futures::FutureExt;
use futures::future::Shared;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, oneshot};

const FAILED_CONNECT_RETRY_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct FailedConnectRecord {
    message: String,
    failed_at: Instant,
}

/// Resolution signal for an in-flight connect attempt. A `Shared` future over
/// a oneshot, chosen deliberately over `Notify`:
/// - `notify_waiters` only wakes waiters that already registered; a caller
///   that clones the signal but awaits it after the attempt finished would
///   hang forever. A resolved `Shared` stays resolved for late awaiters.
/// - Dropping/cancelling one waiter's future never consumes the signal for
///   the others.
type ConnectWait = Shared<oneshot::Receiver<()>>;

/// Bookkeeping for one in-flight connect attempt.
struct ConnectSlot {
    /// Slot generation. `disconnect_server`/`disconnect_all` remove the slot,
    /// so a `finish_connect` that lands after them can detect (by seq
    /// mismatch) that its attempt was superseded and discard the
    /// freshly-connected client instead of resurrecting a dropped server.
    seq: u64,
    wait: ConnectWait,
}

enum ConnectAttempt {
    Connected,
    /// This caller owns the attempt: it must spawn the detached connect task
    /// that resolves `wait` via `send`, then await `wait` like any waiter.
    Leader {
        seq: u64,
        wait: ConnectWait,
        send: oneshot::Sender<()>,
    },
    /// Another attempt is in flight; await its shared resolution.
    Wait(ConnectWait),
}

/// Global shared pool of MCP server processes.
///
/// Only one pool exists per jcode daemon. It owns the child processes
/// and hands out cheap `McpHandle` clones to sessions.
pub struct SharedMcpPool {
    clients: Mutex<HashMap<String, McpClient>>,
    handles: RwLock<HashMap<String, McpHandle>>,
    config: RwLock<McpConfig>,
    /// Directory against which the pool's default config was resolved. Keep it
    /// stable across reloads because the daemon may serve sessions in many dirs.
    config_dir: Option<std::path::PathBuf>,
    ref_counts: Mutex<HashMap<String, usize>>,
    /// In-flight connect attempts. An entry is only removed by its owning
    /// `finish_connect` or by a disconnect superseding it, so a waiter that
    /// observed an entry is always resolved.
    connecting: Mutex<HashMap<String, ConnectSlot>>,
    connect_seq: AtomicU64,
    last_errors: RwLock<HashMap<String, FailedConnectRecord>>,
}

impl SharedMcpPool {
    /// Create a new shared pool with the given config
    pub fn new(config: McpConfig) -> Self {
        Self::new_for_dir(config, std::env::current_dir().ok())
    }

    fn new_for_dir(config: McpConfig, config_dir: Option<std::path::PathBuf>) -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
            handles: RwLock::new(HashMap::new()),
            config: RwLock::new(config),
            config_dir,
            ref_counts: Mutex::new(HashMap::new()),
            connecting: Mutex::new(HashMap::new()),
            connect_seq: AtomicU64::new(1),
            last_errors: RwLock::new(HashMap::new()),
        }
    }

    /// Create pool loading config from default locations
    pub fn from_default_config() -> Self {
        let config_dir = std::env::current_dir().ok();
        let config = McpConfig::load_for_dir(config_dir.as_deref());
        Self::new_for_dir(config, config_dir)
    }

    /// Connect to all configured servers.
    /// Returns (successes, failures).
    pub async fn connect_all(self: &Arc<Self>) -> (usize, Vec<(String, String)>) {
        let config = self.config.read().await;
        let mut connect_futures = Vec::new();

        for (name, server_config) in &config.servers {
            // Disabled servers stay configured but are never auto-spawned
            // (issue #436); they can still be connected on demand by name.
            if !server_config.is_enabled() {
                continue;
            }
            // Non-shared servers are owned per-session (with the session cwd)
            // and must never be spawned in the daemon-global pool (issue #557).
            if !server_config.shared {
                continue;
            }
            let name = name.clone();
            let server_config = server_config.clone();
            let pool = Arc::clone(self);
            connect_futures.push(async move {
                let result = pool.ensure_connected(name.clone(), server_config).await;
                (name, result)
            });
        }
        drop(config);

        let mut successes = 0;
        let mut failures = Vec::new();

        for (name, result) in futures::future::join_all(connect_futures).await {
            match result {
                Ok(new_connection) => {
                    if new_connection {
                        successes += 1;
                    }
                }
                Err(error_msg) => {
                    crate::logging::error(&format!(
                        "Failed to connect to MCP server '{}': {}",
                        name, error_msg
                    ));
                    failures.push((name, error_msg));
                }
            }
        }

        if successes == 0 {
            successes = self.handles.read().await.len();
        }

        (successes, failures)
    }

    /// Connect to a specific server by name and config
    pub async fn connect_server(
        self: &Arc<Self>,
        name: &str,
        config: &McpServerConfig,
    ) -> Result<()> {
        self.ensure_connected(name.to_string(), config.clone())
            .await
            .map(|_| ())
            .map_err(|error_msg| anyhow::anyhow!(error_msg))
            .with_context(|| format!("Failed to connect to MCP server '{}'", name))
    }

    /// Disconnect a specific server
    pub async fn disconnect_server(&self, name: &str) {
        {
            // Drop the in-flight connect slot first: a connect task that
            // finishes after this sees a stale generation and discards its
            // client instead of resurrecting the disconnected server. Its
            // waiters still resolve via the oneshot the task owns.
            let mut connecting = self.connecting.lock().await;
            connecting.remove(name);
        }
        {
            let mut handles = self.handles.write().await;
            handles.remove(name);
        }
        {
            let mut clients = self.clients.lock().await;
            if let Some(mut client) = clients.remove(name) {
                client.shutdown().await;
            }
        }
        {
            let mut refs = self.ref_counts.lock().await;
            refs.remove(name);
        }
        {
            let mut errors = self.last_errors.write().await;
            errors.remove(name);
        }
    }

    /// Disconnect all servers
    pub async fn disconnect_all(&self) {
        {
            // Supersede all in-flight connects (see disconnect_server).
            let mut connecting = self.connecting.lock().await;
            connecting.clear();
        }
        {
            let mut handles = self.handles.write().await;
            handles.clear();
        }
        {
            let mut clients = self.clients.lock().await;
            for (_, mut client) in clients.drain() {
                client.shutdown().await;
            }
        }
        {
            let mut refs = self.ref_counts.lock().await;
            refs.clear();
        }
        {
            let mut errors = self.last_errors.write().await;
            errors.clear();
        }
    }

    /// Get handles for all connected servers (for a new session).
    /// Increments reference counts.
    pub async fn acquire_handles(&self, session_id: &str) -> HashMap<String, McpHandle> {
        let handles = self.handles.read().await;
        let result = handles.clone();

        let mut refs = self.ref_counts.lock().await;
        for name in result.keys() {
            *refs.entry(name.clone()).or_insert(0) += 1;
        }

        if !result.is_empty() {
            crate::logging::info(&format!(
                "MCP pool: session '{}' acquired {} server handle(s)",
                session_id,
                result.len()
            ));
        }

        result
    }

    /// Release handles when a session disconnects.
    /// Decrements reference counts.
    pub async fn release_handles(&self, session_id: &str, server_names: &[String]) {
        let mut refs = self.ref_counts.lock().await;
        for name in server_names {
            if let Some(count) = refs.get_mut(name) {
                *count = count.saturating_sub(1);
            }
        }

        if !server_names.is_empty() {
            crate::logging::info(&format!(
                "MCP pool: session '{}' released {} server handle(s)",
                session_id,
                server_names.len()
            ));
        }
    }

    /// Get a handle for a specific server
    pub async fn get_handle(&self, name: &str) -> Option<McpHandle> {
        let handles = self.handles.read().await;
        handles.get(name).cloned()
    }

    /// Get all available tools from all connected servers
    pub async fn all_tools(&self) -> Vec<(String, McpToolDef)> {
        let handles = self.handles.read().await;
        let mut tools = Vec::new();
        for (server_name, handle) in handles.iter() {
            for tool in handle.tools() {
                tools.push((server_name.clone(), tool));
            }
        }
        tools
    }

    /// Live tool counts per connected server (unfiltered by `direct`).
    pub async fn tool_counts(&self) -> std::collections::BTreeMap<String, usize> {
        let mut counts = std::collections::BTreeMap::new();
        for (server, _) in self.all_tools().await {
            *counts.entry(server).or_insert(0) += 1;
        }
        counts
    }

    /// Get list of connected server names
    pub async fn connected_servers(&self) -> Vec<String> {
        let handles = self.handles.read().await;
        handles.keys().cloned().collect()
    }

    /// Call a tool on a specific server
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<super::protocol::ToolCallResult> {
        let handles = self.handles.read().await;
        let handle = handles
            .get(server)
            .with_context(|| format!("MCP server '{}' not connected", server))?;
        handle.call_tool(tool, arguments).await
    }

    /// Reload config and reconnect all servers
    pub async fn reload(self: &Arc<Self>) -> (usize, Vec<(String, String)>) {
        self.disconnect_all().await;
        *self.config.write().await = McpConfig::load_for_dir(self.config_dir.as_deref());
        self.connect_all().await
    }

    /// Get current config
    pub async fn config(&self) -> McpConfig {
        self.config.read().await.clone()
    }

    /// Check if any servers are connected
    pub async fn has_connections(&self) -> bool {
        let handles = self.handles.read().await;
        !handles.is_empty()
    }

    /// Get reference counts (for debugging)
    pub async fn ref_counts(&self) -> HashMap<String, usize> {
        self.ref_counts.lock().await.clone()
    }

    async fn begin_connect(&self, name: &str) -> ConnectAttempt {
        let mut connecting = self.connecting.lock().await;
        if let Some(slot) = connecting.get(name) {
            return ConnectAttempt::Wait(slot.wait.clone());
        }

        if self.handles.read().await.contains_key(name) {
            return ConnectAttempt::Connected;
        }

        let (send, recv) = oneshot::channel::<()>();
        let wait = recv.shared();
        let seq = self.connect_seq.fetch_add(1, Ordering::Relaxed);
        connecting.insert(
            name.to_string(),
            ConnectSlot {
                seq,
                wait: wait.clone(),
            },
        );
        ConnectAttempt::Leader { seq, wait, send }
    }

    /// Record the outcome of a connect attempt and release the slot.
    ///
    /// `seq` is the slot generation the attempt was started under. When a
    /// disconnect/reload removed the slot mid-connect the attempt is no longer
    /// admitted: its freshly-connected client is dropped (which kills the
    /// child) rather than resurrecting a server the pool was told to drop.
    async fn finish_connect(
        &self,
        name: &str,
        seq: u64,
        send: oneshot::Sender<()>,
        result: Result<McpClient>,
    ) {
        let current = {
            let connecting = self.connecting.lock().await;
            connecting
                .get(name)
                .map(|slot| slot.seq == seq)
                .unwrap_or(false)
        };

        if current {
            match result {
                Ok(client) => {
                    let handle = client.handle();
                    {
                        let mut handles = self.handles.write().await;
                        handles.insert(name.to_string(), handle);
                    }
                    {
                        let mut clients = self.clients.lock().await;
                        clients.insert(name.to_string(), client);
                    }
                    {
                        let mut errors = self.last_errors.write().await;
                        errors.remove(name);
                    }
                }
                Err(error) => {
                    let mut errors = self.last_errors.write().await;
                    errors.insert(
                        name.to_string(),
                        FailedConnectRecord {
                            message: format!("{:#}", error),
                            failed_at: Instant::now(),
                        },
                    );
                }
            }
        } else {
            // Superseded attempt. Dropping an Ok(client) kills the child.
            drop(result);
        }

        // Resolve every waiter only after the outcome is fully recorded, then
        // release the slot. Clones of `wait` obtained before removal resolve
        // immediately, so late waiters also observe the result.
        let _ = send.send(());
        let mut connecting = self.connecting.lock().await;
        if connecting
            .get(name)
            .map(|slot| slot.seq == seq)
            .unwrap_or(false)
        {
            connecting.remove(name);
        }
    }

    async fn ensure_connected(
        self: &Arc<Self>,
        name: String,
        config: McpServerConfig,
    ) -> std::result::Result<bool, String> {
        if let Some(record) = self.recent_failure(&name).await {
            let retry_after = FAILED_CONNECT_RETRY_COOLDOWN
                .saturating_sub(record.failed_at.elapsed())
                .as_secs()
                .max(1);
            crate::logging::info(&format!(
                "MCP: Skipping reconnect to '{}' for {}s after recent failure",
                name, retry_after
            ));
            return Err(format!(
                "{} (retry suppressed for ~{}s after recent failure)",
                record.message, retry_after
            ));
        }

        match self.begin_connect(&name).await {
            ConnectAttempt::Connected => Ok(false),
            ConnectAttempt::Wait(wait) => {
                // The detached connect task always resolves this, even if the
                // caller that spawned it has been cancelled.
                let _ = wait.await;
                self.connect_outcome(&name, false).await
            }
            ConnectAttempt::Leader { seq, wait, send } => {
                // Run the connect in a detached task: cancelling this caller
                // (dropped request, closed session, reload racing in) must not
                // strand the connect slot or the waiters queued behind it.
                let pool = Arc::clone(self);
                let task_name = name.clone();
                tokio::spawn(async move {
                    let result = McpClient::connect(task_name.clone(), &config).await;
                    pool.finish_connect(&task_name, seq, send, result).await;
                });
                let _ = wait.await;
                self.connect_outcome(&name, true).await
            }
        }
    }

    /// Read back the recorded outcome of a finished connect attempt.
    async fn connect_outcome(
        &self,
        name: &str,
        is_leader: bool,
    ) -> std::result::Result<bool, String> {
        if self.handles.read().await.contains_key(name) {
            Ok(is_leader)
        } else {
            let error = self
                .last_errors
                .read()
                .await
                .get(name)
                .map(|record| record.message.clone())
                .unwrap_or_else(|| "Connection attempt did not produce a handle".to_string());
            Err(error)
        }
    }

    async fn recent_failure(&self, name: &str) -> Option<FailedConnectRecord> {
        if self.handles.read().await.contains_key(name) {
            return None;
        }

        self.last_errors
            .read()
            .await
            .get(name)
            .filter(|record| record.failed_at.elapsed() < FAILED_CONNECT_RETRY_COOLDOWN)
            .cloned()
    }
}

/// Global pool singleton
static SHARED_POOL: tokio::sync::OnceCell<Arc<SharedMcpPool>> = tokio::sync::OnceCell::const_new();

/// Initialize the global shared MCP pool. Call once at daemon startup.
pub async fn init_shared_pool() -> Arc<SharedMcpPool> {
    SHARED_POOL
        .get_or_init(|| async {
            let pool = SharedMcpPool::from_default_config();
            Arc::new(pool)
        })
        .await
        .clone()
}

/// Get the global shared pool, if initialized.
pub fn get_shared_pool() -> Option<Arc<SharedMcpPool>> {
    SHARED_POOL.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::{ConnectAttempt, SharedMcpPool};
    use crate::mcp::protocol::McpConfig;
    use std::sync::Arc;

    #[tokio::test]
    async fn issue_790_reload_reuses_default_config_directory() {
        let _guard = crate::storage::lock_test_env();
        let original_cwd = std::env::current_dir().expect("current cwd");
        let previous_home = std::env::var_os("JCODE_HOME");
        let home = tempfile::tempdir().expect("home tempdir");
        let first_project = tempfile::tempdir().expect("first project tempdir");
        let second_project = tempfile::tempdir().expect("second project tempdir");
        crate::env::set_var("JCODE_HOME", home.path());
        std::fs::write(
            first_project.path().join(".mcp.json"),
            r#"{"mcpServers":{"first":{"command":"first-server","shared":false}}}"#,
        )
        .expect("write first project config");
        std::fs::write(
            second_project.path().join(".mcp.json"),
            r#"{"mcpServers":{"second":{"command":"second-server","shared":false}}}"#,
        )
        .expect("write second project config");

        std::env::set_current_dir(first_project.path()).expect("set first project cwd");
        let pool = Arc::new(SharedMcpPool::from_default_config());
        let initially_loaded_first = pool.config().await.servers.contains_key("first");

        std::fs::write(
            first_project.path().join(".mcp.json"),
            r#"{"mcpServers":{"first-reloaded":{"command":"first-reloaded-server","shared":false}}}"#,
        )
        .expect("update first project config");

        std::env::set_current_dir(second_project.path()).expect("set second project cwd");
        let _ = pool.reload().await;
        let reloaded = pool.config().await;

        std::env::set_current_dir(original_cwd).expect("restore cwd");
        if let Some(previous_home) = previous_home {
            crate::env::set_var("JCODE_HOME", previous_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }

        assert!(initially_loaded_first);
        assert!(!reloaded.servers.contains_key("first"));
        assert!(reloaded.servers.contains_key("first-reloaded"));
        assert!(!reloaded.servers.contains_key("second"));
    }

    #[tokio::test]
    async fn begin_connect_deduplicates_concurrent_attempts() {
        let pool = Arc::new(SharedMcpPool::new(McpConfig::default()));

        let first = pool.begin_connect("demo").await;
        let second = pool.begin_connect("demo").await;

        let (first_wait, send) = match first {
            ConnectAttempt::Leader { wait, send, .. } => (wait, send),
            _ => panic!("first attempt should lead"),
        };
        let second_wait = match second {
            ConnectAttempt::Wait(wait) => wait,
            _ => panic!("second attempt should wait"),
        };

        // Both waiters resolve when the leader's attempt resolves — and a
        // waiter cloned late (after resolution) still observes the result.
        send.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), first_wait)
            .await
            .expect("leader wait should resolve")
            .expect("leader send should succeed");
        tokio::time::timeout(std::time::Duration::from_secs(1), second_wait)
            .await
            .expect("follower wait should resolve")
            .expect("follower wait should observe send");
    }

    #[tokio::test]
    async fn cancelled_leader_connect_does_not_strand_followup_connects() {
        // If `ensure_connected`'s Leader branch is cancelled while
        // `McpClient::connect` is in flight (server reload, request drop, task
        // abort), `finish_connect` never runs and the `connecting` entry is
        // stranded: every later attempt parks on a Notify that can never fire,
        // making the server unreachable until daemon restart.
        use crate::mcp::protocol::McpServerConfig;
        use std::time::Duration;

        let pool = Arc::new(SharedMcpPool::new(McpConfig::default()));
        let config = McpServerConfig {
            // A process that spawns fine but never answers `initialize`, so the
            // connect attempt stays in flight long enough to abort mid-connect.
            command: "sleep".to_string(),
            args: vec!["30".to_string()],
            env: Default::default(),
            shared: true,
            transport: None,
            url: None,
            headers: Default::default(),
            enabled: None,
            disabled: None,
            timeout_secs: Some(2),
            request_timeout_ms: None,
            direct: None,
        };

        // Leader attempt in a background task so we can cancel it.
        let leader = {
            let pool = Arc::clone(&pool);
            let config = config.clone();
            tokio::spawn(async move { pool.ensure_connected("srv".to_string(), config).await })
        };

        // Wait until the leader has registered its connecting entry.
        let mut registered = false;
        for _ in 0..200 {
            if pool.connecting.lock().await.contains_key("srv") {
                registered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(registered, "leader never registered its connect attempt");

        leader.abort();

        // A follow-up connect must not hang on the stranded entry. With the bug
        // this times out; the fix makes it join (or supersede) the still-running
        // attempt, which then fails fast via timeout_secs.
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            pool.ensure_connected("srv".to_string(), config),
        )
        .await;

        assert!(
            result.is_ok(),
            "follow-up connect hung on stranded leader entry"
        );
    }

    #[tokio::test]
    async fn notify_waiters_misses_late_registered_waiters() {
        // Documents the primitive hazard behind the stranded-connect bug:
        // `Notify::notify_waiters` wakes only waiters that have already created
        // (and thus epoch-registered) their `notified()` future. A task that
        // obtained the Arc<Notify> in `begin_connect` but calls `.notified()`
        // after the leader's wake captures the post-wake epoch, registers, and
        // sleeps forever. The fix must use a primitive whose result is
        // observable by late subscribers (Shared future), not Notify.
        let notify = Arc::new(tokio::sync::Notify::new());
        notify.notify_waiters(); // fires before any waiter future exists
        let missed =
            tokio::time::timeout(std::time::Duration::from_millis(50), notify.notified()).await;
        assert!(
            missed.is_err(),
            "notify_waiters unexpectedly woke a future created after the wake"
        );
    }

    #[tokio::test]
    async fn connect_all_skips_non_shared_servers() {
        // Issue #557: shared:false servers are owned per-session and must not
        // be spawned in the daemon-global pool. If the pool tried to connect
        // this nonexistent command it would show up as a failure.
        let mut config = McpConfig::default();
        config.servers.insert(
            "owned-only".to_string(),
            crate::mcp::protocol::McpServerConfig {
                command: "/nonexistent/jcode-test-mcp-557".to_string(),
                args: vec![],
                env: Default::default(),
                shared: false,
                transport: None,
                url: None,
                headers: std::collections::HashMap::new(),
                enabled: None,
                disabled: None,
                timeout_secs: None,
                request_timeout_ms: None,
                direct: None,
            },
        );
        let pool = Arc::new(SharedMcpPool::new(config));

        let (successes, failures) = pool.connect_all().await;
        assert_eq!(successes, 0);
        assert!(
            failures.is_empty(),
            "non-shared server must be skipped, got failures: {failures:?}"
        );
    }
}
