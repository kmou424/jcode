# [11] MCP HTTP/SSE transports

Adds HTTP transports for MCP servers on top of the existing stdio transport,
with spec-defined era auto-detection, plus `!{cmd}` command substitution in
config strings.

## Config surface

```jsonc
// ~/.jcode/mcp.json — "servers" or "mcpServers"
{
  "mcpServers": {
    "deepwiki": {
      "type": "http",                          // http | streamable-http | remote | auto
      "url": "https://mcp.deepwiki.com/mcp",
      "headers": { "Authorization": "Bearer !{pass show mcp/deepwiki}" },
      "request_timeout_ms": 30000,
      "direct": true
    },
    "legacy": {
      "type": "sse",                           // deprecated 2024-11-05 HTTP+SSE
      "url": "https://old.example.com/sse"
    }
  }
}
```

Fields:

- `type` — `stdio` (default when `command` is set), `http`/`streamable-http`/
  `remote`/`auto` (HTTP family), `sse` (legacy HTTP+SSE, skips probing). A
  config with `url` and no `type`/`command` resolves to HTTP.
- `url` — endpoint for HTTP transports.
- `headers` — request headers; values support `${VAR}`/`${VAR:-default}` and
  `!{cmd}` expansion (see below).
- `request_timeout_ms` — per-request timeout; `timeout_secs` wins when both
  are set. Strictly snake_case — foreign camelCase spellings are ignored.
- `direct` — `true` (default) injects tools as direct `mcp__*` tools;
  `false` keeps them out of the direct surface but discoverable through
  `mcp_search` and callable on demand.

Deliberately not supported: `lifecycle` (pi-agent lazy/eager modes — jcode
connects eagerly), `toolResultRendering`, OAuth flows (headers suffice for
bearer-token auth), `directTools` (pi field; use `direct`).

## Era detection

Connect-time probe (`http::probe_era`):

1. POST `initialize` with `Accept: application/json, text/event-stream`.
   2xx → **legacy streamable** (2025-03-26..2025-11-25): capture
   `Mcp-Session-Id`, negotiate `protocolVersion` from the result, send
   `notifications/initialized`.
2. 4xx whose body is a modern-era JSON-RPC error (`-32601`, `-32602`, or
   `-32022` with `data.supported`) → **modern** (2026-07-28+): no handshake;
   every request carries `_meta.io.modelcontextprotocol/{protocolVersion,
   clientInfo, clientCapabilities}` plus `MCP-Protocol-Version`, `Mcp-Method`,
   `Mcp-Name`, and `Mcp-Param-*` headers for schema fields tagged
   `x-mcp-header`.
3. Any other 4xx → GET probe for **legacy HTTP+SSE** (2024-11-05): the SSE
   stream's first `endpoint` event names the POST endpoint; responses arrive
   as `message` events on the stream.

Explicit `type: sse` skips the probe and goes straight to the SSE reader.

## Session and failure handling

- Legacy streamable: `Mcp-Session-Id` replayed on every POST; HTTP 404 →
  re-initialize once and retry; `shutdown` best-effort DELETEs the session.
- Transport-level failures mark the handle dead → the next `call_tool`
  reconnects (same reconnect-if-dead model as [10]).
- A background task refreshes `tools/list` every 60s — doubles as the
  health check and keeps the tool catalog fresh.
- SSE response bodies (`text/event-stream` on POST) are decoded for the
  first `message` event, per spec.

## `!{cmd}` substitution

`expand_environment_string` now also expands `!{command}` in `command`,
`args`, `env`, `url`, and `headers` values: run via `sh -c` with a 10s
deadline, stdout trimmed; on any failure the literal `!{...}` is preserved.
Useful for CLI secret managers (`!{pass show x}`, `!{op read op://...}`,
`!{secret-tool lookup ...}`) so secrets stay out of mcp.json.

## Files

- `crates/jcode-base/src/mcp/http.rs` — probe, transports, SSE decoder,
  health check (new).
- `crates/jcode-base/src/mcp/http_tests.rs` — scripted mock HTTP server +
  era/transport tests (new).
- `crates/jcode-base/src/mcp/client.rs` — `McpClient.child` → `Option`,
  `http` field, `new_channel`/`resolve_pending`/`mark_dead`/
  `set_request_meta` handle plumbing, `request()` `_meta` injection,
  `apply_initialize_result`/`send_initialized` split.
- `crates/jcode-base/src/mcp/protocol.rs` — `McpTransportKind`,
  `transport_kind`/`is_supported`/`is_stdio`/`exposes_tools`, `url`/`headers`
  retained at load, `request_timeout_ms`/`direct` fields, `!{cmd}` expansion.
- `crates/jcode-base/src/mcp/manager.rs` — `all_tools` filters
  `direct: false`; `searchable_tools` keeps them.

## Upstream status

General-purpose feature; a cleaned-up version is a plausible upstream PR
(the `!{cmd}` syntax and `direct` field naming may need adjusting to
whatever upstream prefers).
