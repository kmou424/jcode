//! HTTP transports for MCP servers (Streamable HTTP + legacy HTTP+SSE),
//! with spec-defined era auto-detection.
//!
//! Supported shapes, per the MCP spec revisions:
//!
//! - **Modern (2026-07-28+)**: stateless POSTs; every request carries
//!   `_meta.io.modelcontextprotocol/protocolVersion` (+ clientInfo /
//!   clientCapabilities) and the `MCP-Protocol-Version`/`Mcp-Method`/
//!   `Mcp-Name` headers; no initialize, no sessions.
//! - **Legacy streamable (2025-03-26..2025-11-25)**: `initialize` handshake,
//!   optional `Mcp-Session-Id` session, `MCP-Protocol-Version` = negotiated
//!   version, optional standalone GET SSE stream (unused here).
//! - **Legacy HTTP+SSE (2024-11-05, deprecated)**: GET opens an SSE stream
//!   whose first event is `endpoint`; requests POST to that endpoint and
//!   responses arrive as `message` events on the stream.
//!
//! Detection: POST `initialize` first. A 2xx means a legacy-streamable
//! server. A 4xx whose body is a recognized *modern* JSON-RPC error means a
//! modern server. Any other 4xx falls back to the legacy SSE `GET` probe,
//! matching the spec's backwards-compatibility path.

use super::client::{McpClient, McpHandle, PendingMap, request_timeout_for};
use super::protocol::*;
use anyhow::{Context, Result};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, watch};

/// The newest per-request-metadata protocol revision we claim.
const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";

/// Interval between background `tools/list` refreshes for network servers —
/// doubles as the health check (a transport failure marks the connection
/// dead so the next tool call reconnects).
const HEALTH_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Which protocol era a connection negotiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HttpEra {
    /// 2026-07-28+ per-request-metadata era: no initialize, no sessions.
    Modern,
    /// 2025-03-26..2025-11-25 streamable HTTP: initialize + sessions.
    LegacyStreamable,
    /// 2024-11-05 HTTP+SSE (deprecated).
    LegacySse,
}

/// Owns the background tasks of an HTTP connection; dropping aborts them.
/// For legacy session-based connections `shutdown` also best-effort DELETEs
/// the session endpoint.
pub(crate) struct HttpConnection {
    tasks: Vec<tokio::task::JoinHandle<()>>,
    delete: Option<DeleteContext>,
    #[allow(dead_code)]
    pub(crate) era: HttpEra,
}

struct DeleteContext {
    client: reqwest::Client,
    url: String,
    session_id: Arc<Mutex<Option<String>>>,
}

impl Drop for HttpConnection {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl HttpConnection {
    pub(crate) async fn shutdown(self) {
        if let Some(ctx) = &self.delete {
            if let Some(session_id) = ctx.session_id.lock().await.clone() {
                let _ = ctx
                    .client
                    .delete(&ctx.url)
                    .header("mcp-session-id", session_id)
                    .send()
                    .await;
            }
        }
        // Drop aborts the tasks.
    }
}

// ---------------------------------------------------------------------------
// SSE decoding
// ---------------------------------------------------------------------------

/// One decoded SSE event.
#[derive(Debug, Default)]
pub(crate) struct SseEvent {
    pub event: String,
    pub data: String,
}

/// Incremental SSE parser. Feed raw bytes, get complete events out.
/// Handles multi-line `data:` (joined with `\n`), `:` comment lines, and
/// chunks split mid-line.
#[derive(Default)]
pub(crate) struct SseDecoder {
    buf: String,
    /// Fields of the in-progress event (cleared on dispatch).
    pending: Option<SseEvent>,
}

impl SseDecoder {
    fn feed(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut events = Vec::new();
        // Events are separated by a blank line. Process complete lines.
        while let Some(pos) = self.buf.find('\n') {
            let line = self.buf[..pos].trim_end_matches('\r').to_string();
            self.buf.drain(..pos + 1);
            if line.is_empty() {
                // End of event — but only emit if we buffered data (pending
                // fields are stored in `self.pending` to survive the loop).
                if let Some(ev) = self.pending.take() {
                    events.push(ev);
                }
                continue;
            }
            if line.starts_with(':') {
                continue; // comment / heartbeat
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line.as_str(), ""),
            };
            let pending = self.pending.get_or_insert_with(SseEvent::default);
            match field {
                "data" => {
                    if !pending.data.is_empty() {
                        pending.data.push('\n');
                    }
                    pending.data.push_str(value);
                }
                "event" => pending.event = value.to_string(),
                // `id:` and `retry:` are unused.
                _ => {}
            }
        }
        events
    }
}

// ---------------------------------------------------------------------------
// Connection entry point
// ---------------------------------------------------------------------------

/// Result of the era probe.
enum ProbeOutcome {
    /// 2026-era: per-request metadata only.
    Modern(String),
    /// 2025-era streamable HTTP: initialize result + optional session id.
    LegacyStreamable {
        init: InitializeResult,
        session_id: Option<String>,
        protocol_version: String,
    },
    /// 2024-era HTTP+SSE.
    LegacySse,
}

/// Connect to an HTTP/SSE MCP server: probe the era, spawn the transport
/// tasks, run the handshake, and fetch the tool list.
pub(crate) async fn connect(name: String, config: &McpServerConfig) -> Result<McpClient> {
    let url = config
        .url
        .clone()
        .context("HTTP/SSE MCP server requires `url`")?;
    let timeout = request_timeout_for(config);

    let mut default_headers = reqwest::header::HeaderMap::new();
    for (k, v) in &config.headers {
        let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
            .with_context(|| format!("invalid MCP header name: {k}"))?;
        let value = reqwest::header::HeaderValue::from_str(v)
            .with_context(|| format!("invalid value for MCP header {k}"))?;
        default_headers.insert(name, value);
    }
    let client = reqwest::Client::builder()
        .default_headers(default_headers)
        .build()?;

    let (handle, writer_rx) = McpHandle::new_channel(&name, timeout);
    let pending = Arc::clone(&handle.pending);
    let task_handle = handle.clone();

    let outcome = match config.transport_kind() {
        Some(McpTransportKind::LegacySse) => ProbeOutcome::LegacySse,
        _ => probe_era(&client, &url, &name, timeout).await?,
    };

    let mut tasks = Vec::new();
    let mut delete = None;
    let mut pending_init: Option<InitializeResult> = None;

    match &outcome {
        ProbeOutcome::Modern(version) => {
            let meta = serde_json::json!({
                "io.modelcontextprotocol/protocolVersion": version,
                "io.modelcontextprotocol/clientInfo": {
                    "name": "jcode",
                    "version": jcode_build_meta::pkg_version(),
                },
                "io.modelcontextprotocol/clientCapabilities": {},
            });
            handle.set_request_meta(meta);
            tasks.push(spawn_streamable_writer(StreamableState {
                client: client.clone(),
                url: url.clone(),
                session_id: Arc::new(Mutex::new(None)),
                era: HttpEra::Modern,
                protocol_version: version.clone(),
                pending: Arc::clone(&pending),
                tools: Arc::clone(&task_handle.tools),
                handle: task_handle.clone(),
                request_timeout: timeout,
                rx: writer_rx,
            }));
        }
        ProbeOutcome::LegacyStreamable {
            init,
            session_id,
            protocol_version,
        } => {
            let session = Arc::new(Mutex::new(session_id.clone()));
            pending_init = Some(init.clone());
            if session_id.is_some() {
                delete = Some(DeleteContext {
                    client: client.clone(),
                    url: url.clone(),
                    session_id: Arc::clone(&session),
                });
            }
            tasks.push(spawn_streamable_writer(StreamableState {
                client: client.clone(),
                url: url.clone(),
                session_id: session,
                era: HttpEra::LegacyStreamable,
                protocol_version: protocol_version.clone(),
                pending: Arc::clone(&pending),
                tools: Arc::clone(&task_handle.tools),
                handle: task_handle.clone(),
                request_timeout: timeout,
                rx: writer_rx,
            }));
        }
        ProbeOutcome::LegacySse => {
            // Open the GET stream; first event gives the POST endpoint.
            let (endpoint_tx, endpoint_rx) = watch::channel::<Option<String>>(None);
            tasks.push(spawn_sse_reader(
                client.clone(),
                url.clone(),
                name.clone(),
                pending.clone(),
                task_handle.clone(),
                endpoint_tx,
            ));
            tasks.push(spawn_sse_writer(
                client.clone(),
                url.clone(),
                endpoint_rx,
                writer_rx,
                task_handle.clone(),
                timeout,
            ));
        }
    }

    // Periodic health check: refresh tools/list; a transport failure marks
    // the handle dead, which lets the next call reconnect.
    tasks.push(tokio::spawn(health_check_loop(task_handle.clone())));

    let mut client = McpClient::from_http(
        handle,
        HttpConnection {
            tasks,
            delete,
            era: match outcome {
                ProbeOutcome::Modern(_) => HttpEra::Modern,
                ProbeOutcome::LegacyStreamable { .. } => HttpEra::LegacyStreamable,
                ProbeOutcome::LegacySse => HttpEra::LegacySse,
            },
        },
    );

    // Finish the handshake.
    if let Some(init) = pending_init {
        client.apply_initialize_result(init);
        client.send_initialized().await?;
    } else if matches!(outcome, ProbeOutcome::LegacySse) {
        client.initialize().await?;
    }

    // Initial tool list.
    client.handle().refresh_tools().await?;
    Ok(client)
}

// ---------------------------------------------------------------------------
// Era probe
// ---------------------------------------------------------------------------

/// Classify a 4xx response from the initialize probe.
fn classify_modern_error(status: u16, body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    let code = v["error"]["code"].as_i64()?;
    match code {
        // Method not found / not implemented → modern server.
        -32601 | -32602 => Some(MODERN_PROTOCOL_VERSION.to_string()),
        // Unsupported protocol version: pick a supported one if listed.
        -32022 => {
            let supported = v["error"]["data"]["supported"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let version = supported
                .iter()
                .find(|v| v.as_str() == MODERN_PROTOCOL_VERSION)
                .cloned()
                .or_else(|| supported.first().cloned())
                .unwrap_or_else(|| MODERN_PROTOCOL_VERSION.to_string());
            Some(version)
        }
        _ => {
            let _ = status;
            None
        }
    }
}

async fn probe_era(
    client: &reqwest::Client,
    url: &str,
    name: &str,
    timeout: std::time::Duration,
) -> Result<ProbeOutcome> {
    // Step 1: POST initialize — the streamable-era handshake.
    let params = McpClient::initialize_params();
    let init_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": params,
    });
    let resp = match tokio::time::timeout(
        timeout,
        client
            .post(url)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .body(init_body.to_string())
            .send(),
    )
    .await
    {
        Ok(r) => r.with_context(|| format!("MCP [{name}]: initialize probe failed"))?,
        Err(_) => {
            return Err(anyhow::anyhow!("MCP [{name}]: initialize probe timed out"));
        }
    };
    let status = resp.status();

    if status.is_success() {
        let session_id = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        // Body may be JSON or SSE.
        let body = tokio::time::timeout(timeout, resp.text()).await??;
        let init = if body.trim_start().starts_with('{') {
            let r: JsonRpcResponse =
                serde_json::from_str(&body).context("initialize response is not JSON-RPC")?;
            let result = r.result.context("initialize response has no result")?;
            serde_json::from_value(result)?
        } else {
            let mut dec = SseDecoder::default();
            let events = dec.feed(body.as_bytes());
            let data = events
                .iter()
                .find(|e| e.event == "message" || !e.data.is_empty())
                .map(|e| e.data.clone())
                .context("initialize SSE response had no data event")?;
            let r: JsonRpcResponse = serde_json::from_str(&data)?;
            let result = r.result.context("initialize response has no result")?;
            serde_json::from_value(result)?
        };
        let init_result: InitializeResult = init;
        let protocol_version = init_result.protocol_version.clone();
        return Ok(ProbeOutcome::LegacyStreamable {
            init: init_result,
            session_id,
            protocol_version,
        });
    }

    if status.is_client_error() {
        let body = tokio::time::timeout(timeout, resp.text())
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default();
        if let Some(version) = classify_modern_error(status.as_u16(), &body) {
            return Ok(ProbeOutcome::Modern(version));
        }
        // Spec backwards-compatibility: 4xx on POST initialize means the
        // server may speak legacy HTTP+SSE. Probe with GET.
        return probe_sse(client, url, name, timeout).await;
    }

    Err(anyhow::anyhow!(
        "MCP [{name}]: initialize probe failed with HTTP {status}"
    ))
}

/// Probe for a legacy HTTP+SSE server: GET must return an SSE stream whose
/// first event is `endpoint`.
async fn probe_sse(
    client: &reqwest::Client,
    url: &str,
    name: &str,
    timeout: std::time::Duration,
) -> Result<ProbeOutcome> {
    let resp = tokio::time::timeout(
        timeout,
        client.get(url).header("accept", "text/event-stream").send(),
    )
    .await;
    match resp {
        Ok(Ok(r)) if r.status().is_success() => Ok(ProbeOutcome::LegacySse),
        Ok(Ok(r)) => Err(anyhow::anyhow!(
            "MCP [{name}]: SSE probe got HTTP {}",
            r.status()
        )),
        Ok(Err(e)) => Err(e).context(format!("MCP [{name}]: SSE probe failed")),
        Err(_) => Err(anyhow::anyhow!("MCP [{name}]: SSE probe timed out")),
    }
}

// ---------------------------------------------------------------------------
// Streamable HTTP transport (modern + legacy-streamable share one writer)
// ---------------------------------------------------------------------------

struct StreamableState {
    client: reqwest::Client,
    url: String,
    /// Legacy streamable only: session id captured from initialize response.
    session_id: Arc<Mutex<Option<String>>>,
    era: HttpEra,
    /// `MCP-Protocol-Version` header value (negotiated or modern).
    protocol_version: String,
    pending: PendingMap,
    tools: Arc<std::sync::RwLock<Vec<McpToolDef>>>,
    handle: McpHandle,
    request_timeout: std::time::Duration,
    rx: mpsc::Receiver<String>,
}

fn spawn_streamable_writer(mut state: StreamableState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(msg) = state.rx.recv().await {
            // One in-flight request per connection is enough for our use;
            // process serially to keep ordering simple.
            streamable_roundtrip(&mut state, msg).await;
        }
    })
}

/// POST one JSON-RPC message; route the response to the pending map.
/// A transport-level failure marks the connection dead.
async fn streamable_roundtrip(state: &mut StreamableState, msg: String) {
    let Ok(value) = serde_json::from_str::<Value>(&msg) else {
        return;
    };
    let id = value.get("id").and_then(|v| v.as_u64());
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let outcome = do_streamable_post(state, msg.clone(), &value, &method).await;
    match outcome {
        Ok(response_json) => {
            if let Some(_id) = id {
                if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&response_json) {
                    McpHandle::resolve_pending(&state.pending, resp).await;
                }
            }
            // Response had no matching id — could be a notification; ignore.
        }
        Err(RoundtripError::Transport(e)) => {
            // Network failure: dead connection → next call reconnects.
            state.handle.mark_dead();
            if let Some(id) = id {
                McpHandle::resolve_pending(
                    &state.pending,
                    JsonRpcResponse {
                        jsonrpc: "2.0".into(),
                        id: Some(id),
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32000,
                            message: format!("MCP transport error: {e}"),
                            data: None,
                        }),
                    },
                )
                .await;
            }
        }
        Err(RoundtripError::HttpStatus(status)) => {
            // 404 on legacy streamable = expired session → re-initialize and
            // retry once (spec §session-management).
            if status == 404 && state.era == HttpEra::LegacyStreamable {
                if let Ok(()) = legacy_reinitialize(state).await {
                    if let Ok(body) = do_streamable_post(state, msg, &value, &method).await {
                        if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&body) {
                            McpHandle::resolve_pending(&state.pending, resp).await;
                        }
                        return;
                    }
                }
                state.handle.mark_dead();
            }
            if let Some(id) = id {
                McpHandle::resolve_pending(
                    &state.pending,
                    JsonRpcResponse {
                        jsonrpc: "2.0".into(),
                        id: Some(id),
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32000,
                            message: format!("MCP HTTP error: status {status}"),
                            data: None,
                        }),
                    },
                )
                .await;
            }
        }
    }
}

enum RoundtripError {
    Transport(anyhow::Error),
    HttpStatus(u16),
}

/// Single POST. Returns the response body as a JSON string on 2xx.
/// `text/event-stream` bodies are decoded to the first `message` event.
async fn do_streamable_post(
    state: &StreamableState,
    body: String,
    value: &Value,
    method: &str,
) -> std::result::Result<String, RoundtripError> {
    let mut req = state
        .client
        .post(&state.url)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("mcp-protocol-version", &state.protocol_version)
        .header("mcp-method", method);

    if let Some(session) = state.session_id.lock().await.as_deref() {
        req = req.header("mcp-session-id", session);
    }
    // `Mcp-Name` for params.name / params.uri, `Mcp-Param-*` for declared
    // x-mcp-header schema fields.
    if let Some(n) = value
        .get("params")
        .and_then(|p| p.get("name").or_else(|| p.get("uri")))
        .and_then(Value::as_str)
    {
        req = req.header("mcp-name", n);
    }
    for (hdr, val) in mcp_param_headers(state, value) {
        req = req.header(hdr, val);
    }

    let resp = match tokio::time::timeout(state.request_timeout, req.body(body).send()).await {
        Ok(r) => r.map_err(|e| RoundtripError::Transport(e.into()))?,
        Err(_) => {
            return Err(RoundtripError::Transport(anyhow::anyhow!(
                "request timed out"
            )));
        }
    };

    let status = resp.status();
    if status.as_u16() == 404 && state.era == HttpEra::LegacyStreamable {
        return Err(RoundtripError::HttpStatus(404));
    }
    if !status.is_success() {
        return Err(RoundtripError::HttpStatus(status.as_u16()));
    }
    if status.as_u16() == 202 || status.as_u16() == 204 {
        return Ok(String::new());
    }

    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let text = tokio::time::timeout(state.request_timeout, resp.text())
        .await
        .map_err(|_| RoundtripError::Transport(anyhow::anyhow!("body read timed out")))?
        .map_err(|e| RoundtripError::Transport(e.into()))?;

    if ct.starts_with("text/event-stream") {
        let mut dec = SseDecoder::default();
        let events = dec.feed(text.as_bytes());
        let data = events
            .iter()
            .find(|e| !e.data.is_empty())
            .map(|e| e.data.clone())
            .unwrap_or_default();
        return Ok(data);
    }
    Ok(text)
}

/// Legacy streamable: re-run initialize on 404 to start a fresh session.
async fn legacy_reinitialize(state: &StreamableState) -> Result<()> {
    let params = McpClient::initialize_params();
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": params
    });
    let resp = tokio::time::timeout(
        state.request_timeout,
        state
            .client
            .post(&state.url)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .body(body.to_string())
            .send(),
    )
    .await??;
    if !resp.status().is_success() {
        anyhow::bail!("re-initialize got HTTP {}", resp.status());
    }
    let new_session = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    *state.session_id.lock().await = new_session;
    let _ = resp.text().await;
    // Re-notify initialized so the server is fully re-handshaken.
    let _ = state
        .client
        .post(&state.url)
        .header("content-type", "application/json")
        .body(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .send()
        .await;
    Ok(())
}

/// Extract `x-mcp-header` properties from the called tool's input schema
/// and render them as `Mcp-Param-*` headers.
fn mcp_param_headers(state: &StreamableState, value: &Value) -> Vec<(String, String)> {
    if value.get("method").and_then(Value::as_str) != Some("tools/call") {
        return vec![];
    }
    let tool_name = value["params"]["name"].as_str().unwrap_or_default();
    let args = &value["params"]["arguments"];
    let tools = state.tools.read().unwrap_or_else(|p| p.into_inner());
    let Some(tool) = tools.iter().find(|t| t.name == tool_name) else {
        return vec![];
    };
    let mut out = Vec::new();
    collect_x_mcp_headers(&tool.input_schema, args, &mut out);
    out
}

fn collect_x_mcp_headers(schema: &Value, args: &Value, out: &mut Vec<(String, String)>) {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    let Some(args_obj) = args.as_object() else {
        return;
    };
    for (key, prop) in props {
        if let Some(header) = prop.get("x-mcp-header").and_then(Value::as_str) {
            if let Some(arg) = args_obj.get(key) {
                if let Some(v) = header_value(arg) {
                    out.push((format!("mcp-param-{header}"), v));
                }
            }
        }
        // Nested objects recurse along properties chains.
        if let Some(nested_schema) = prop.get("properties") {
            if let Some(nested_args) = args_obj.get(key) {
                collect_x_mcp_headers(
                    &serde_json::json!({"properties": nested_schema}),
                    nested_args,
                    out,
                );
            }
        }
    }
}

fn header_value(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Legacy HTTP+SSE transport
// ---------------------------------------------------------------------------

/// GET the SSE stream; forward `endpoint` + `message` events.
fn spawn_sse_reader(
    client: reqwest::Client,
    url: String,
    name: String,
    pending: PendingMap,
    handle: McpHandle,
    endpoint_tx: watch::Sender<Option<String>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let resp = match client
            .get(&url)
            .header("accept", "text/event-stream")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                crate::logging::debug(&format!("MCP [{name}]: SSE connect failed: {e}"));
                handle.mark_dead();
                return;
            }
        };
        let mut stream = resp.bytes_stream();
        let mut dec = SseDecoder::default();
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            let Ok(bytes) = chunk else { break };
            for event in dec.feed(&bytes) {
                match event.event.as_str() {
                    "endpoint" => {
                        let _ = endpoint_tx.send(Some(event.data));
                    }
                    "message" | "" => {
                        if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&event.data) {
                            McpHandle::resolve_pending(&pending, resp).await;
                        }
                    }
                    _ => {}
                }
            }
        }
        handle.mark_dead();
    })
}

/// Wait for the endpoint, then POST every outgoing message to it.
fn spawn_sse_writer(
    client: reqwest::Client,
    base_url: String,
    mut endpoint_rx: watch::Receiver<Option<String>>,
    mut rx: mpsc::Receiver<String>,
    handle: McpHandle,
    request_timeout: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Wait for the endpoint event (arrives on the reader task).
        let endpoint = loop {
            if let Some(ep) = endpoint_rx.borrow().clone() {
                break resolve_endpoint(&base_url, &ep);
            }
            if endpoint_rx.changed().await.is_err() {
                return;
            }
        };
        while let Some(msg) = rx.recv().await {
            let req = client
                .post(&endpoint)
                .header("content-type", "application/json")
                .body(msg);
            match tokio::time::timeout(request_timeout, req.send()).await {
                Ok(Ok(r)) if r.status().is_success() || r.status().as_u16() == 202 => {}
                Ok(Ok(r)) => {
                    crate::logging::debug(&format!("MCP SSE POST got HTTP {}", r.status()));
                    handle.mark_dead();
                    return;
                }
                Ok(Err(_)) | Err(_) => {
                    handle.mark_dead();
                    return;
                }
            }
        }
    })
}

/// The endpoint event gives a URL (usually an absolute path on the same
/// origin); resolve against the base URL.
fn resolve_endpoint(base: &str, endpoint: &str) -> String {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        return endpoint.to_string();
    }
    if let Ok(u) = reqwest::Url::parse(base) {
        if endpoint.starts_with('/') {
            // Same-origin absolute path.
            let mut u = u;
            u.set_path("");
            u.set_query(None);
            return format!("{}{}", u.as_str().trim_end_matches('/'), endpoint);
        }
        if let Ok(u) = u.join(endpoint) {
            return u.to_string();
        }
    }
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        endpoint.trim_start_matches('/')
    )
}

// ---------------------------------------------------------------------------
// Health check
// ---------------------------------------------------------------------------

async fn health_check_loop(handle: McpHandle) {
    let mut interval = tokio::time::interval(HEALTH_CHECK_INTERVAL);
    interval.tick().await; // skip the immediate first tick
    loop {
        interval.tick().await;
        if !handle.is_alive() {
            return;
        }
        // refresh_tools sends a real tools/list; a transport failure inside
        // the writer marks the handle dead so the next tool call reconnects.
        // A transient RPC error leaves the connection alive — keep checking.
        if let Err(e) = handle.refresh_tools().await {
            crate::logging::debug(&format!("MCP health check failed: {e}"));
            if !handle.is_alive() {
                return;
            }
        }
    }
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod http_tests;
