# Scan Performance Implementation Plan

## Objective

Reduce avoidable Wi-Fi scan latency and make the remaining macOS-controlled latency visible and predictable.

The current scan path can take up to 13 seconds. The application cannot guarantee a fixed physical scan duration because `CWInterface::scanForNetworksWithName:error:` is synchronous and CoreWLAN controls channel traversal. The implementation should instead ensure that:

- a scan never waits behind the current 10-second join-verification polling loop;
- simultaneous requests share one physical scan;
- reconnects can use a recent result immediately;
- daemon startup does not trigger a redundant scan;
- queue time and CoreWLAN time are measured separately;
- one client's scan result cannot accidentally satisfy another client's request.

## Current Architecture

The request path is:

```text
TUI or CLI
  -> Request over newline-delimited JSON
  -> daemon::serve_one
  -> LocalWifiHandle
  -> single CoreWLAN worker thread
  -> WifiInterface::scan()
  -> Event broadcast to all clients
```

Relevant code:

- `src/client.rs`: socket lifecycle, reconnect initialization, CLI one-shot requests.
- `src/daemon.rs`: client connections and global event fanout.
- `src/worker.rs`: serialized CoreWLAN operations and scan execution.
- `src/corewlan.rs`: synchronous CoreWLAN scan and result conversion.
- `src/event.rs`: wire events.
- `src/app.rs`: TUI scan state and latest result.
- `src/handler.rs`: manual scan keybinding.

Known latency multipliers:

1. `worker_loop` performs an unsolicited startup scan before a client necessarily subscribes.
2. `client::send_init` sends another scan on every connection and reconnect.
3. Every scan request becomes another physical CoreWLAN scan.
4. All worker operations share one FIFO queue.
5. `verify_join` sleeps on that worker for up to 10 seconds.
6. Results are broadcast without request correlation.
7. The daemon has no recent-result cache.

## Target Design

### Request envelopes

Introduce request correlation at the IPC boundary rather than adding IDs to every `Request` variant.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientRequest {
    pub id: u64,
    pub request: Request,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEvent {
    pub request_id: Option<u64>,
    pub event: Event,
}
```

`request_id: Some(id)` is used for direct responses. `None` is reserved for unsolicited state changes and notices. Increment `PROTOCOL_VERSION` because the wire shape changes.

The existing `RemoteWifiHandle::send(Request)` API can remain for TUI call sites. Internally it allocates an ID using the existing `next_id: AtomicU64` and sends a `ClientRequest`.

### Daemon scan coordinator

Add a daemon-owned `ScanCoordinator`. The daemon is the correct layer because it can see requests from all clients, operate asynchronously, and serve cached results without touching CoreWLAN.

Suggested state:

```rust
struct ScanCoordinator {
    in_flight: bool,
    waiters: Vec<ScanWaiter>,
    last_success: Option<CachedScan>,
}

struct ScanWaiter {
    client_id: u64,
    request_id: u64,
}

struct CachedScan {
    networks: Arc<Vec<ScannedNetwork>>,
    completed_at: Instant,
}
```

Use a daemon-internal client identifier allocated when `serve_one` completes its handshake. Do not serialize `Instant`; it is only coordinator state.

Initial cache policy:

- Cache lifetime: 5 seconds.
- A normal `Request::Scan` within the lifetime gets the cached result without a physical scan.
- A scan received while one is running joins `waiters`.
- The first uncached request starts one worker scan.
- A worker result is sent directly to all current waiters and stored in the cache.
- A worker error completes all waiters with an error and is not cached.
- Disconnected clients are removed from `waiters` when practical; failed sends are also safe to ignore.

Do not add a force-refresh request in the first implementation. Five-second expiry plus single-flight is sufficient to establish behavior without widening the user-facing API. Add force refresh later only if real usage demonstrates a need.

### Worker command separation

Stop sending socket `Request` values directly into the worker. Introduce an internal worker command so scan completions can be identified without exposing coordinator details to CoreWLAN code.

```rust
enum WorkerCommand {
    Request(Request),
    Scan { operation_id: u64 },
}

enum WorkerEvent {
    Event(Event),
    ScanFinished {
        operation_id: u64,
        result: Result<Vec<ScannedNetwork>, String>,
        timings: ScanTimings,
    },
}
```

The daemon translates non-scan `ClientRequest`s into `WorkerCommand::Request`. Scan requests go through `ScanCoordinator`, which creates a new `operation_id` only when it starts a physical scan.

The worker should no longer emit `Event::ScanStarted` or `Event::ScanResult` directly. It should return `WorkerEvent::ScanFinished`; the daemon creates correlated client events.

### Scan timing data

Measure with `std::time::Instant` at these boundaries:

- daemon receives the request;
- coordinator starts the worker operation;
- worker dequeues the operation;
- CoreWLAN call starts and returns;
- result sorting and validation finish;
- daemon receives `ScanFinished`;
- daemon sends the result.

Log at least:

```text
scan request_id=<id> cache=<hit|miss> coalesced=<bool>
queue_ms=<n> corewlan_ms=<n> postprocess_ms=<n> total_ms=<n>
networks=<n> blank_ssids=<n> waiters=<n> outcome=<ok|error>
```

Use durations with millisecond precision. The existing log timestamp can remain second precision because each timing is an explicit duration.

Do not initially add timings to the public `Event` payload. Logs are enough to establish the baseline without coupling the TUI to diagnostic data.

### Non-blocking join verification

The worker owns non-`Send` CoreWLAN handles, so the verification fix must preserve thread affinity. Replace the blocking `verify_join` loop with delayed worker commands rather than moving `WifiInterface` to Tokio.

Suggested internal commands:

```rust
enum WorkerCommand {
    // ...
    VerifyJoin {
        operation_id: u64,
        ssid: String,
        attempts_remaining: u8,
    },
}
```

When association or `networksetup` succeeds:

1. Return control to the worker loop immediately.
2. Schedule the first verification check through a timer outside the worker.
3. Each timer expiry enqueues one brief `VerifyJoin` command.
4. A successful state check emits the existing success event.
5. A failed check with attempts remaining schedules the next check after 500 ms.
6. The final failed check emits the existing unconfirmed/failure outcome.

Implementation options for the timer:

- Give `LocalWifiHandle` a cloneable command sender and spawn a small Tokio task from the daemon for each verification sequence.
- Alternatively, give the worker a dedicated scheduler thread that only sleeps and re-enqueues commands.

Prefer the Tokio scheduler because the daemon already owns a runtime and it avoids one thread per verification. The worker result should tell the daemon when another check is needed; the daemon sleeps asynchronously and re-enqueues it.

This phase must preserve the current semantic distinction between:

- `JoinWithPassword`: accepted but not confirmed produces a notice.
- `JoinSaved`: unconfirmed association produces `JoinSavedFailed` with `AssociationFailed`.

## Implementation Phases

### Phase 1: Baseline instrumentation

Files:

- `src/worker.rs`
- `src/corewlan.rs` only if conversion timing cannot be isolated in `worker.rs`
- `src/logging.rs` only if a helper is useful

Changes:

1. Add `Instant` timing around `iface.scan()` in `emit_scan`.
2. Separate CoreWLAN duration from sorting/redaction-check duration.
3. Log result count, blank SSID count, and outcome.
4. Add queue timing by wrapping internal worker messages with an enqueue timestamp.
5. Record at least 50 warm scans before behavior changes.

Deliverable:

- A baseline table containing median, p95, maximum queue time, CoreWLAN time, post-processing time, and total time.

Exit criteria:

- A 13-second sample can be classified as worker queue delay, CoreWLAN duration, or both.

### Phase 2: Remove duplicate startup scans

Files:

- `src/worker.rs`
- `src/client.rs`

Changes:

1. Remove `emit_scan(&iface, &events)` from worker initialization.
2. Keep `Request::Scan` in `client::send_init` so a subscribed TUI owns initial scan demand.
3. Keep startup state and preferred refreshes for now; they are not the primary latency problem.
4. Add a worker test or injectable fake proving startup does not invoke scan.

Exit criteria:

- Cold daemon plus one TUI connection starts exactly one physical scan.
- No scan result is emitted before a client can subscribe.

### Phase 3: Correlated IPC messages

Files:

- `src/ipc.rs`
- `src/client.rs`
- `src/daemon.rs`
- `src/main.rs`
- `src/event.rs` if envelopes are located there instead of `ipc.rs`

Changes:

1. Add `ClientRequest` and `ServerEvent` wire envelopes.
2. Increment `PROTOCOL_VERSION` from 2 to 3.
3. Change client outbound queue to `UnboundedSender<ClientRequest>`.
4. Allocate request IDs in `RemoteWifiHandle::send` with `fetch_add`.
5. Decode `ServerEvent` in the client and forward its inner `Event` to the TUI.
6. Make `cli_one_shot` retain its request ID and ignore terminal events for other IDs.
7. Allocate daemon client IDs after handshake.
8. Route direct command responses only to their requesting client. Preserve broadcast only where behavior is genuinely unsolicited.

Compatibility:

- No backward-compatible wire decoder is required. The existing build/version handshake already detects stale daemons and instructs users to reinstall.

Exit criteria:

- Two simultaneous CLI scans cannot complete from each other's events.
- Existing TUI call sites still use `wifi.send(Request::...)`.
- A stale protocol-v2 daemon is rejected cleanly.

### Phase 4: Single-flight and recent-result cache

Files:

- `src/daemon.rs`
- `src/worker.rs`
- Optionally a new `src/scan.rs` if coordinator logic plus tests would otherwise make `daemon.rs` difficult to read

Changes:

1. Add `ScanCoordinator`, `ScanWaiter`, and `CachedScan`.
2. Split worker commands/events from socket requests/events.
3. Route `Request::Scan` through the coordinator.
4. On a cache hit, send `ScanStarted` and `ScanResult` only to the requesting client without touching the worker.
5. On a cache miss, send `ScanStarted` to the first requester and start a worker operation.
6. For coalesced requests, immediately send `ScanStarted` to the new requester and add it to `waiters`.
7. On completion, send one correlated `ScanResult` to each waiter.
8. Clear in-flight state on both success and error.
9. Cache successful results only.
10. Move blank-SSID/redaction error generation to a shared helper so every waiter receives consistent diagnostics.

Notes:

- Sending `ScanStarted` on cache hits preserves the current TUI state transition and is immediately followed by the result.
- Keep the cache in memory only. Persisted Wi-Fi scan results would be misleading after daemon restart.
- Use `Arc<Vec<ScannedNetwork>>` internally if cloning large scan results appears in profiles. Serialization still requires visiting every item for each client.

Exit criteria:

- Ten scan requests within one second cause one `WifiInterface::scan()` call.
- A request within five seconds of a successful scan returns without another physical scan.
- A failed scan does not poison the cache.
- Every waiter leaves the scanning state after either success or failure.

### Phase 5: TUI admission behavior

Files:

- `src/handler.rs`
- `src/app.rs`
- TUI rendering file if result age is displayed

Changes:

1. Ignore `s` while `App::scanning` is true.
2. Ensure scan errors clear `App::scanning`; currently a worker error can leave it true because only `ScanResult` clears it.
3. Prefer adding a dedicated `ScanFailed(String)` event instead of inferring scan failure from the generic `Error` event.
4. Optionally store `last_scan_completed_at` locally and display result age. This is not required for the first speed release.

Exit criteria:

- Repeated `s` key presses do not enqueue work.
- The spinner always stops after scan failure.

### Phase 6: Make join verification non-blocking

Files:

- `src/worker.rs`
- `src/daemon.rs`
- Tests for worker command scheduling

Changes:

1. Replace `verify_join` with one-attempt state checks.
2. Add operation state carrying SSID, join kind, and remaining attempts.
3. Return a scheduling event to the daemon when another check is needed.
4. Use `tokio::time::sleep(Duration::from_millis(500))` outside the worker.
5. Re-enqueue the next check after the sleep.
6. Preserve current notices, failure reasons, password caching, and state refresh behavior.
7. Cancel pending verification when its originating client disconnects only if doing so does not change association behavior; otherwise allow it to finish but avoid sending to a dead client.

Exit criteria:

- No `thread::sleep` remains in the worker request loop.
- A scan requested during a failed 10-second verification begins within 250 ms of the request, excluding time CoreWLAN itself requires.
- Join success and failure behavior remains unchanged from the user's perspective.

### Phase 7: Profile-guided result conversion optimization

Files:

- `src/corewlan.rs`
- `src/app.rs`
- `src/main.rs`

Only perform this phase if Phase 1 shows post-processing is significant.

Candidates:

1. Measure the cost of the eight `supportsSecurity` calls per network.
2. Remove duplicate sorting from CLI or TUI paths where the worker already returns RSSI order.
3. Avoid cloning scan records when request routing can borrow or share them.
4. Consider deduplicating identical BSS entries only after defining desired behavior for multiple APs sharing an SSID.

Do not optimize these paths based only on intuition; physical scan and queue time are expected to dominate.

## Testing Strategy

### Unit tests

Add a fake scan backend or test the coordinator independently from CoreWLAN.

Required cases:

1. Fresh cache returns the cached value and starts no worker operation.
2. Expired cache starts a new operation.
3. Multiple in-flight requests produce one worker operation and multiple waiters.
4. Successful completion updates the cache and completes all waiters.
5. Failed completion clears in-flight state and leaves the old cache unchanged or expired.
6. A disconnected waiter does not prevent other waiters from completing.
7. Scan failure clears TUI scanning state.
8. CLI ignores an event carrying a different request ID.
9. Worker initialization does not perform a scan.
10. Join verification retries are scheduled without blocking worker command handling.
11. Join verification emits the correct final result for password and saved-network joins.

Prefer a narrow trait around scan execution rather than abstracting the entire `WifiInterface` unless tests require it:

```rust
trait ScanBackend {
    fn scan(&self) -> anyhow::Result<Vec<ScannedNetwork>>;
}
```

If introducing a trait creates more production complexity than the tests justify, test `ScanCoordinator` as a pure state machine and keep one macOS integration test for the worker boundary.

### Integration tests

Use a temporary socket via `MACWIFI_SOCKET_PATH` and a fake worker where possible.

Required cases:

1. Two clients issue scans concurrently and receive results with their own IDs.
2. Ten requests result in one fake physical scan.
3. Cached scan response is delivered without a fake worker command.
4. Protocol version mismatch remains readable and actionable.
5. Client disconnect during a scan does not affect remaining clients.

### Manual macOS benchmarks

Run at least 50 samples for each core scenario and report median, p95, maximum, and failure count.

Scenarios:

1. Warm daemon, isolated scan.
2. Ten requests in one second.
3. TUI reconnect within cache lifetime.
4. Cold daemon with Location permission already granted.
5. Scan while a join verification is pending.
6. One TUI plus five CLI clients.
7. Associated and disconnected interface states.
8. Wi-Fi immediately after power-on and after sleep/wake.

Performance targets:

- IPC plus application post-processing: under 50 ms at p95.
- Fresh cache response: under 100 ms at p95.
- Concurrent scan requests: one physical scan.
- Worker queue delay for scan during join verification: under 250 ms at p95.
- Cold daemon plus one client: one physical scan.
- No indefinite scanning indicator after errors.

Do not set a hard target for CoreWLAN's physical scan duration until the baseline separates it from application queue time.

## Rollout and Verification

Implement and verify each phase separately. After every phase:

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```

Then reinstall/restart the bundled daemon before manual benchmarks so the client and daemon use the same protocol and build ID.

Recommended commit boundaries:

1. Add scan latency instrumentation.
2. Remove duplicate daemon-start scan.
3. Add correlated IPC envelopes.
4. Add daemon scan single-flight and cache.
5. Harden TUI scan state and repeated-key behavior.
6. Refactor join verification scheduling.
7. Apply only profile-proven conversion optimizations.

## Risks and Mitigations

### CoreWLAN cannot be cancelled

A Tokio timeout cannot stop an in-progress synchronous CoreWLAN call. Treat client timeout as request abandonment, not physical cancellation. Do not restart the worker automatically without first proving CoreWLAN handle recreation is safe.

### Cache freshness

A cache that is too long can show departed networks. Start at five seconds, keep it memory-only, and measure user behavior before changing it.

### Event-routing regression

Moving from broadcasts to correlated responses may reveal call sites that relied on global events. Classify each event explicitly as direct or broadcast and cover simultaneous clients in integration tests.

### Join behavior regression

The current blocking verification contains important password and failure semantics. Preserve those semantics in explicit operation state and test both `JoinSaved` and `JoinWithPassword` before removing the old loop.

### First-run Location prompt

Location authorization can block daemon readiness independently of scanning. Keep authorization wait time out of scan metrics and report it separately in logs. Improving first-run authorization UX is outside this plan unless measurements show it is being misreported as scan latency.

## Definition of Done

The scan performance work is complete when:

- instrumentation distinguishes queue, CoreWLAN, post-processing, and total latency;
- daemon startup and client initialization produce one scan, not two;
- concurrent scans are coalesced;
- results up to five seconds old are served from daemon memory;
- scan replies are request-correlated;
- join verification no longer sleeps on the CoreWLAN worker;
- scan errors always clear TUI scanning state;
- all automated checks pass;
- manual benchmarks meet the application-controlled targets above;
- any remaining long scan is demonstrably inside CoreWLAN rather than avoidable application queueing.
