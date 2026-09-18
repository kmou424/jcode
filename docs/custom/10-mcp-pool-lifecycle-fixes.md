# [10] MCP shared-pool connect cancellation + lifecycle fixes

## What it does

Fixes a set of latent bugs in the MCP lifecycle (`crates/jcode-base/src/mcp/`),
all present in upstream v0.85.0. The headline bug: **a cancelled leader connect
stranded the server forever** — every subsequent connect parked on a `Notify`
that could never fire, until daemon restart. This is the likely cause of the
observed "had to kill the jcode server" incident.

## Bugs fixed

1. **Stranded connect slot on leader cancellation** (`pool.rs`).
   `ensure_connected`'s `Leader` branch ran `McpClient::connect` inline; if that
   await was cancelled (request drop, session close, abort), `finish_connect`
   never ran and the `connecting` map entry was orphaned. Fix: the connect now
   runs in a detached `tokio::spawn`; the leader awaits the shared resolution
   like any other waiter, so caller cancellation can no longer strand the slot.

2. **`Notify::notify_waiters` misses late-registered waiters** (`pool.rs`).
   A waiter that cloned the `Arc<Notify>` but hadn't begun awaiting when the
   wake fired slept forever. Fix: the resolution signal is now a
   `Shared<oneshot::Receiver<()>>` (`ConnectWait`) — resolved futures stay
   resolved for late subscribers, and cancelling one waiter never consumes the
   signal.

3. **Disconnect racing an in-flight connect resurrected the server**
   (`pool.rs`). `disconnect_server`/`disconnect_all` now remove the connecting
   slot, and `finish_connect` admits its result only when its slot generation
   (`connect_seq`) is still current; a superseded attempt's freshly-connected
   client is dropped (killing the child) instead of being inserted.

4. **Pending-request map leak** (`client.rs`). `McpHandle::request` inserted a
   `pending` entry but only removed it on success; send failure, channel close,
   and timeout all leaked the entry (and a late reply would land in a dead
   slot). All failure paths now remove the entry.

5. **Dead server handles never reconnected** (`manager.rs`, `client.rs`).
   `call_tool`'s fast paths returned the cached handle/client unconditionally,
   so a dead child produced broken-pipe errors forever. New `is_alive()`
   (writer-channel liveness, `&self`) on `McpHandle`/`McpClient`; dead entries
   are evicted and reconnected through the normal connect path.

## TDD evidence

- `cancelled_leader_connect_does_not_strand_followup_connects` — aborts the
  leader mid-connect to `sleep 30`; before the fix the follow-up connect hung
  (15 s timeout tripped), after the fix it resolves.
- `notify_waiters_misses_late_registered_waiters` — pins the `Notify` primitive
  hazard the fix avoids.
- `begin_connect_deduplicates_concurrent_attempts` — updated to assert both
  waiters resolve on the leader's send.
- All 60 `mcp::` tests pass; workspace `cargo check` clean.

## Affected files

- `crates/jcode-base/src/mcp/pool.rs` — ConnectSlot/ConnectWait scheme,
  detached connect task, seq-gated admission, disconnect clears slots.
- `crates/jcode-base/src/mcp/client.rs` — pending-map cleanup, `is_alive`.
- `crates/jcode-base/src/mcp/manager.rs` — dead-handle eviction + reconnect.

## Upstream status

**Upstream-fix candidate.** All bugs exist verbatim in upstream v0.85.0
(`git log -S` provenance: code predates the fork). Worth a PR; the fix is
self-contained in jcode-base.
