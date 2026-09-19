//! Tests for the MCP HTTP transports against a scripted in-process HTTP
//! server. Each test drives the public `McpClient::connect` path so the
//! whole era-detection → handshake → tools/list pipeline is exercised.

use super::*;
use crate::mcp::client::McpClient;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex as TokioMutex;

// ---------------------------------------------------------------------------
// Mock HTTP/1.1 server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&name))
            .map(|(_, v)| v.as_str())
    }
}

/// What a mock handler returns for one request.
enum MockResponse {
    /// status code, content-type, body (connection closes after writing)
    Full(u16, &'static str, String),
    /// Open an SSE stream: events pushed through the receiver are written
    /// verbatim (each item should be a full `event:`/`data:`/blank-line
    /// block). The stream lives until the receiver closes or the peer
    /// disconnects.
    SseStream(mpsc::Receiver<String>),
}

type Handler = Arc<dyn Fn(RecordedRequest, &MockCtx) -> MockResponse + Send + Sync>;

/// Shared context passed to handlers so POSTs can push SSE events.
#[derive(Clone)]
struct MockCtx {
    /// std::sync::Mutex: handlers are sync fns called inside async tasks,
    /// so a tokio Mutex would panic on blocking_lock.
    sse_tx: Arc<std::sync::Mutex<Option<mpsc::Sender<String>>>>,
}

struct MockHttp {
    url: String,
    hits: Arc<TokioMutex<Vec<RecordedRequest>>>,
    ctx: MockCtx,
}

impl MockHttp {
    /// Spawn a server where `handler` answers every request.
    async fn start(handler: Handler) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(TokioMutex::new(Vec::new()));
        let hits2 = Arc::clone(&hits);
        let ctx = MockCtx {
            sse_tx: Arc::new(std::sync::Mutex::new(None)),
        };
        let ctx_accept = ctx.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let hits = Arc::clone(&hits2);
                let ctx = ctx_accept.clone();
                let handler = Arc::clone(&handler);
                tokio::spawn(async move {
                    let req = read_request(&mut sock).await;
                    hits.lock().await.push(req.clone());
                    match handler(req, &ctx) {
                        MockResponse::Full(status, ct, body) => {
                            let reason = match status {
                                200 => "OK",
                                202 => "Accepted",
                                204 => "No Content",
                                400 => "Bad Request",
                                404 => "Not Found",
                                405 => "Method Not Allowed",
                                _ => "Error",
                            };
                            let resp = format!(
                                "HTTP/1.1 {status} {reason}\r\ncontent-type: {ct}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = sock.write_all(resp.as_bytes()).await;
                        }
                        MockResponse::SseStream(mut rx) => {
                            let resp = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n";
                            if sock.write_all(resp.as_bytes()).await.is_err() {
                                return;
                            }
                            while let Some(event) = rx.recv().await {
                                if sock.write_all(event.as_bytes()).await.is_err() {
                                    break;
                                }
                                let _ = sock.flush().await;
                            }
                        }
                    }
                });
            }
        });
        Self {
            url: format!("http://{addr}/mcp"),
            hits,
            ctx,
        }
    }

    async fn requests(&self) -> Vec<RecordedRequest> {
        self.hits.lock().await.clone()
    }
}

/// Read exactly one HTTP request: request line, headers, Content-Length body.
async fn read_request(sock: &mut tokio::net::TcpStream) -> RecordedRequest {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos;
        }
        let n = sock.read(&mut chunk).await.unwrap_or(0);
        assert!(n > 0, "connection closed before headers complete");
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_string();
    let path = request_line.next().unwrap_or_default().to_string();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_string();
            let v = v.trim().to_string();
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.parse().unwrap_or(0);
            }
            headers.push((k, v));
        }
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        let n = sock.read(&mut chunk).await.unwrap_or(0);
        assert!(n > 0, "connection closed before body complete");
        body.extend_from_slice(&chunk[..n]);
    }
    RecordedRequest {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body[..content_length]).to_string(),
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// Shared test helpers
// ---------------------------------------------------------------------------

fn http_config(url: &str) -> McpServerConfig {
    serde_json::from_value(json!({ "type": "http", "url": url })).unwrap()
}

fn sse_config(url: &str) -> McpServerConfig {
    serde_json::from_value(json!({ "type": "sse", "url": url })).unwrap()
}

fn init_result() -> Value {
    json!({
        "protocolVersion": "2025-06-18",
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "mock", "version": "1.0" }
    })
}

fn tools_result() -> Value {
    json!({
        "tools": [{
            "name": "echo",
            "description": "echoes",
            "inputSchema": { "type": "object", "properties": {} }
        }]
    })
}

fn jsonrpc_result(id: u64, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// Respond to a JSON-RPC request body with the appropriate canned result.
fn canned_rpc(body: &str) -> Value {
    let req: Value = serde_json::from_str(body).unwrap();
    match req["method"].as_str().unwrap_or("") {
        "initialize" => jsonrpc_result(req["id"].as_u64().unwrap_or(0), init_result()),
        "tools/list" => jsonrpc_result(req["id"].as_u64().unwrap_or(0), tools_result()),
        "tools/call" => jsonrpc_result(
            req["id"].as_u64().unwrap_or(0),
            json!({"content": [{"type": "text", "text": "ok"}]}),
        ),
        _ => jsonrpc_result(req["id"].as_u64().unwrap_or(0), json!({})),
    }
}

// ---------------------------------------------------------------------------
// Legacy streamable HTTP (2025-era)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamable_http_json_responses() {
    let server = MockHttp::start(Arc::new(|req, _| {
        assert_eq!(req.method, "POST");
        // Must accept both content types per spec.
        let accept = req.header("accept").unwrap_or_default();
        assert!(accept.contains("application/json") && accept.contains("text/event-stream"));
        MockResponse::Full(200, "application/json", canned_rpc(&req.body).to_string())
    }))
    .await;

    let client = McpClient::connect("t".into(), &http_config(&server.url))
        .await
        .expect("connect should succeed");
    let tools = client.tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");

    // tools/call works through the same POST roundtrip
    let result = client
        .handle()
        .call_tool("echo", json!({}))
        .await
        .expect("call_tool");
    assert!(matches!(
        &result.content[0],
        ContentBlock::Text { text } if text == "ok"
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamable_http_sse_response_body() {
    // Server answers every POST with an SSE body (allowed by the spec).
    let server = MockHttp::start(Arc::new(|req, _| {
        let body = canned_rpc(&req.body).to_string();
        MockResponse::Full(
            200,
            "text/event-stream",
            format!("event: message\ndata: {body}\n\n"),
        )
    }))
    .await;

    let client = McpClient::connect("t".into(), &http_config(&server.url))
        .await
        .expect("connect should succeed");
    assert_eq!(client.tools()[0].name, "echo");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamable_http_session_id_replayed() {
    let server = MockHttp::start(Arc::new(|req, _| {
        let is_init = req.body.contains("\"initialize\"");
        let _ = is_init;
        MockResponse::Full(200, "application/json", canned_rpc(&req.body).to_string())
    }))
    .await;
    // Wrap: respond to initialize with a session header — simplest is a
    // second mock that varies by request; do it inline with a stateful
    // handler instead.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(TokioMutex::new(Vec::new()));
    let hits2 = Arc::clone(&hits);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let hits = Arc::clone(&hits2);
            tokio::spawn(async move {
                let req = read_request(&mut sock).await;
                hits.lock().await.push(req.clone());
                let extra = if req.body.contains("\"initialize\"") {
                    "mcp-session-id: sess-42\r\n"
                } else {
                    ""
                };
                let body = canned_rpc(&req.body).to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    let url = format!("http://{addr}/mcp");
    let _ = server; // silence unused

    let client = McpClient::connect("t".into(), &http_config(&url))
        .await
        .expect("connect should succeed");
    let _ = client.handle().call_tool("echo", json!({})).await;

    let reqs = hits.lock().await;
    // Every POST after initialize must carry the session header.
    let posts: Vec<_> = reqs.iter().filter(|r| r.method == "POST").collect();
    assert!(posts.len() >= 2);
    assert!(
        posts[0].body.contains("\"initialize\""),
        "first POST is the initialize probe"
    );
    for r in &posts[1..] {
        assert_eq!(
            r.header("mcp-session-id"),
            Some("sess-42"),
            "request must replay the session id: {r:?}"
        );
    }
    // Negotiated protocol version header on every request after initialize
    // (the initialize POST itself does not carry it — the version isn't
    // negotiated yet).
    for r in &posts[1..] {
        assert_eq!(r.header("mcp-protocol-version"), Some("2025-06-18"));
    }
}

// ---------------------------------------------------------------------------
// Legacy HTTP+SSE (2024-11-05)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_sse_transport() {
    let server = MockHttp::start(Arc::new(|req, ctx| {
        match req.method.as_str() {
            "GET" => {
                let (tx, rx) = mpsc::channel(16);
                // First event tells the client where to POST.
                tx.try_send("event: endpoint\ndata: /mcp?sessionId=abc\n\n".to_string())
                    .unwrap();
                *ctx.sse_tx.lock().unwrap() = Some(tx);
                MockResponse::SseStream(rx)
            }
            "POST" => {
                // Message endpoint: push the JSON-RPC response on the stream.
                let tx = ctx.sse_tx.lock().unwrap().clone().unwrap();
                let body = canned_rpc(&req.body).to_string();
                tx.try_send(format!("event: message\ndata: {body}\n\n"))
                    .unwrap();
                MockResponse::Full(202, "text/plain", "Accepted".into())
            }
            _ => MockResponse::Full(405, "text/plain", String::new()),
        }
    }))
    .await;

    let client = McpClient::connect("t".into(), &sse_config(&server.url))
        .await
        .expect("connect should succeed");
    assert_eq!(client.tools()[0].name, "echo");
    let result = client.handle().call_tool("echo", json!({})).await.unwrap();
    assert!(matches!(
        &result.content[0],
        ContentBlock::Text { text } if text == "ok"
    ));

    // Requests went to the endpoint path (with the session query).
    let reqs = server.requests().await;
    let posts: Vec<_> = reqs.iter().filter(|r| r.method == "POST").collect();
    assert!(
        posts
            .iter()
            .all(|r| r.path.starts_with("/mcp?sessionId=abc"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_type_falls_back_to_sse_on_405() {
    // `type: "http"` server that only speaks legacy SSE: POST initialize
    // gets a plain 405 (not a modern-era error body) → probe GET SSE.
    let server = MockHttp::start(Arc::new(|req, ctx| match req.method.as_str() {
        "GET" => {
            let (tx, rx) = mpsc::channel(16);
            tx.try_send("event: endpoint\ndata: /mcp\n\n".to_string())
                .unwrap();
            *ctx.sse_tx.lock().unwrap() = Some(tx);
            MockResponse::SseStream(rx)
        }
        "POST" if !req.body.contains("sessionId") && req.path == "/mcp" => {
            if ctx.sse_tx.lock().unwrap().is_some() {
                // The SSE endpoint is also /mcp here; once the stream exists,
                // POSTs are messages.
                let tx = ctx.sse_tx.lock().unwrap().clone().unwrap();
                let body = canned_rpc(&req.body).to_string();
                tx.try_send(format!("event: message\ndata: {body}\n\n"))
                    .unwrap();
                MockResponse::Full(202, "text/plain", "Accepted".into())
            } else {
                MockResponse::Full(405, "text/plain", String::new())
            }
        }
        _ => MockResponse::Full(405, "text/plain", String::new()),
    }))
    .await;

    let client = McpClient::connect("t".into(), &http_config(&server.url))
        .await
        .expect("connect should fall back to SSE");
    assert_eq!(client.tools()[0].name, "echo");
}

// ---------------------------------------------------------------------------
// Modern era (2026-07-28)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn modern_era_detected_and_meta_injected() {
    let server = MockHttp::start(Arc::new(|req, _| {
        let body: Value = serde_json::from_str(&req.body).unwrap();
        if body["method"] == "initialize" {
            // Modern servers reject the legacy handshake.
            let err = json!({
                "jsonrpc": "2.0", "id": body["id"],
                "error": {"code": -32601, "message": "Method not found"}
            });
            return MockResponse::Full(400, "application/json", err.to_string());
        }
        // Everything else is normal JSON-RPC, no session headers required.
        MockResponse::Full(200, "application/json", canned_rpc(&req.body).to_string())
    }))
    .await;

    let client = McpClient::connect("t".into(), &http_config(&server.url))
        .await
        .expect("connect should succeed in modern era");
    assert_eq!(client.tools()[0].name, "echo");
    let _ = client.handle().call_tool("echo", json!({})).await.unwrap();

    let reqs = server.requests().await;
    // Exactly one failed initialize probe, then no more.
    let inits: Vec<_> = reqs
        .iter()
        .filter(|r| r.body.contains("\"initialize\""))
        .collect();
    assert_eq!(inits.len(), 1);
    // Modern requests carry _meta protocolVersion + Mcp-Method header.
    for r in reqs.iter().filter(|r| !r.body.contains("\"initialize\"")) {
        let body: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(
            body["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
            MODERN_PROTOCOL_VERSION,
            "request must carry _meta protocolVersion: {}",
            r.body
        );
        assert!(
            r.header("mcp-method").is_some(),
            "request must carry Mcp-Method header"
        );
    }
}

// ---------------------------------------------------------------------------
// SSE decoder unit tests
// ---------------------------------------------------------------------------

#[test]
fn sse_decoder_basic_events() {
    let mut dec = SseDecoder::default();
    let events = dec.feed(b"event: message\ndata: {\"a\":1}\n\nevent: ping\ndata: x\n\n");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event, "message");
    assert_eq!(events[0].data, "{\"a\":1}");
    assert_eq!(events[1].event, "ping");
}

#[test]
fn sse_decoder_multiline_and_comments() {
    let mut dec = SseDecoder::default();
    let events = dec.feed(b": comment\ndata: line1\ndata: line2\n\n");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "line1\nline2");
}

#[test]
fn sse_decoder_split_chunks() {
    let mut dec = SseDecoder::default();
    assert!(dec.feed(b"data: par").is_empty());
    assert!(dec.feed(b"tial\nda").is_empty());
    let events = dec.feed(b"ta: done\n\n");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "partial\ndone");
}
