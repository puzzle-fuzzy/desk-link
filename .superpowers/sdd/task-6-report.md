# Task 6 implementation report

Date: 2026-07-15

## RED evidence

The focused tests were written before the transport and relay production implementation.

```text
cargo test -p desklink-transport --test localhost
error: unresolved imports ... QuicClient, RelayJoin, TransportEvent, TransportError
exit code: 101

cargo test -p desklink-relay --test session
error: unresolved imports ... RelayConfig, RelayError, RelayServer, RelaySessionTable
exit code: 101
```

These failures were caused by the Task 6 public API and behavior not existing yet, rather than by test typos.

## GREEN evidence

Focused verification after implementation:

```text
cargo test -p desklink-transport --test localhost
3 passed, 0 failed

cargo test -p desklink-relay --test session
9 passed, 0 failed

cargo test --workspace
all workspace tests and doc-tests passed

cargo clippy --workspace --all-targets --all-features -- -D warnings
finished successfully

./scripts/verify.sh
finished successfully

cargo fmt --all -- --check
finished successfully

git diff --check
finished successfully
```

The focused coverage includes localhost channel separation and opaque forwarding, duplicate-controller rejection, session/authentication mismatch errors, expiry/sweep, precise detach, malformed and oversized join input, oversized reliable frames, malformed datagrams, and client-side 64KiB/1200-byte limits.

## Changed files

- `Cargo.toml` and `Cargo.lock`: Quinn/Tokio/rustls/rcgen/bytes workspace dependencies and resolved lockfile entries.
- `crates/desklink-transport/Cargo.toml`: transport dependencies.
- `crates/desklink-transport/src/lib.rs`: public join envelope, channel, event, error, and limit APIs.
- `crates/desklink-transport/src/quic.rs`: Quinn client with persistent reliable channel streams, marked datagrams, join handshake, keepalive, idle timeout, and malformed-input handling.
- `crates/desklink-transport/tests/localhost.rs`: localhost QUIC channel and boundary tests.
- `server/relay/Cargo.toml`: relay dependencies.
- `server/relay/src/lib.rs`: in-memory session table, authentication/role matching, expiry/sweep, QUIC accept loop, opaque reliable/datagram forwarding, and stable join rejection codes.
- `server/relay/src/main.rs`: Tokio relay entrypoint with configurable bind address and a local self-signed certificate.
- `server/relay/tests/session.rs`: session-table and localhost relay integration tests.

## Design decisions

- The join wire envelope is fixed-size and manually encoded: magic, version, role, 16-byte `SessionId`, and 32-byte authentication value. The relay decodes only this envelope for matching and redacts authentication in `Debug` output.
- Control, input, and video configuration each use their own persistent bidirectional QUIC stream. Each message is length-prefixed and checked against the 64KiB cap before allocation.
- Video and cursor use QUIC datagrams with a one-byte routing marker. The relay validates only the marker and total opaque payload boundary, then forwards the original bytes unchanged. Unknown or malformed datagrams close the source connection without payload deserialization.
- Quinn transport configuration uses a 5-second keepalive interval and 15-second maximum idle timeout on both client and relay server.
- `RelaySessionTable` stores one host and one controller per session, the authentication value, creation/expiry times, and connection IDs. Detach only removes the exact connection ID; sweeping removes expired records and closes their active participants.
- Rustls is pinned to the ring provider at the workspace level so Quinn and test certificates cannot select multiple process-level crypto providers.

## Known follow-ups

- `server/relay/src/main.rs` currently generates a self-signed certificate for the MVP/local entrypoint. A deployed relay should load a managed certificate and expose certificate pinning/trust configuration to clients.
- Session state is process-local; production multi-instance relay deployment would need a coordinated session registry or explicit single-instance routing.
- Reconnection policy, application-level heartbeat messages, and transport metrics remain responsibilities for the later session/FFI layers. QUIC keepalive and idle timeout are configured here as the Task 6 liveness boundary.
- The current relay intentionally has no payload inspection or payload logging, so operational observability should be added later using metadata only.

## Task 6 review follow-up

Date: 2026-07-15

### RED evidence

The review regressions were run before the fixes:

```text
cargo test -p desklink-transport --test localhost
5 tests: 3 passed, 2 failed
  - stalled_control_channel_does_not_block_input_channel timed out
  - client_rejects_invalid_timeout_overrides_before_connecting failed

cargo test -p desklink-relay --test session
failed to compile because sweep_expired, admission fields, and stable cap errors were absent
```

### GREEN evidence

After the review fixes:

```text
cargo test -p desklink-transport --test localhost
6 passed, 0 failed

cargo test -p desklink-relay --test session
13 passed, 0 failed

cargo test --workspace
all workspace tests and doc-tests passed

cargo fmt --all -- --check
passed

cargo clippy --workspace --all-targets --all-features -- -D warnings
passed

./scripts/verify.sh
passed

git diff --check
passed

The two flood regressions and the connection-cap regression each passed in three consecutive single-test repeat runs.
```

### Review-fix design decisions

- `ClientInner` now owns independent mutex-protected sender slots for control, input, and video configuration. Opening a stream or writing a blocked message holds only that channel's lock, so a stalled reliable peer cannot block another reliable channel. The stalled-control regression uses a one-byte peer receive window and proves input progress remains bounded.
- Relay expiry now returns exact expired connection IDs in `ExpiredSession` records. The relay state serializes attach, participant registration, detach, and sweep with one membership lock; sweep removes/closes only the expired connection IDs, so a same-session reattach cannot be closed by an old sweep.
- `RelayConfig` now has nonzero `max_connections` and `max_sessions` defaults of 1024. The accept loop atomically reserves bounded TLS admissions and refuses excess `quinn::Incoming` values; the session table independently enforces joined-connection and session caps under its mutex, returning stable `RelayError` variants and join rejection codes.
- Client and relay configuration reject zero keepalive/dead timeouts and require keepalive to be shorter than the dead timeout. The client also exposes `try_with_timeouts` for immediate validation while preserving the existing builder method and validates again before connecting.
- Relay authentication matching uses `subtle::ConstantTimeEq` for the 32-byte value. Client malformed datagrams and reliable peer streams now emit `Closed` signaling; malformed reliable streams are reset and protocol-invalid inputs close the connection. Normal finished reliable streams remain non-errors.
- No post-join payload is logged or deserialized by the relay. Reliable channel markers remain separate from datagram markers, with the existing 64KiB reliable and 1200-byte datagram payload caps.

### Review-fix changed files

- `Cargo.toml`, `Cargo.lock`, `server/relay/Cargo.toml`: direct `subtle` dependency for constant-time authentication comparison.
- `crates/desklink-transport/src/lib.rs`: timeout validation API and stable admission rejection codes.
- `crates/desklink-transport/src/quic.rs`: independent reliable sender state and malformed peer signaling/reset behavior.
- `crates/desklink-transport/tests/localhost.rs`: stalled-channel, timeout, malformed-peer, and existing boundary regressions.
- `server/relay/src/lib.rs`: exact expiry records, atomic membership sweep/reattach handling, admission caps, symmetric timeout validation, and constant-time auth matching.
- `server/relay/tests/session.rs`: exact expiry/reattach, cap, invalid-config, opaque forwarding, and malformed-input regressions.

### Known follow-ups

- The global admission cap is intentionally process-local, matching the existing process-local session table; multi-instance coordination remains outside Task 6.
- The relay still uses the existing local self-signed certificate entrypoint and does not add application-level rate limiting because active TLS/session admission is explicitly bounded here.

## Task 6 remediation

Date: 2026-07-15

### Remediation scope

- Inbound delivery now uses independent bounded queues for control, input, video configuration, video datagrams, cursor datagrams, and close notifications. Reliable stream readers can backpressure only their own channel queue; datagram readers use non-blocking `try_send` and drop the newest datagram when that queue is full. `next_event` drains the queues with a round-robin cursor so a continuously ready input queue cannot starve control or configuration events. The total queue capacities remain fixed and bounded.
- Relay admission still reserves `max_connections` with the existing atomic compare-and-exchange loop. An over-cap connection receives one bounded post-handshake application close using `RELAY_CONNECTION_LIMIT_CLOSE_CODE` (`0x444c_0001`); the client maps that exact close code to the dedicated `TransportError::ConnectionLimit`. The cap is not bypassed by the rejection path.
- Empty reliable streams and channel-only/truncated reliable streams are reset and reported through `TransportEvent::Closed`; normal EOF after complete messages remains a normal stream completion.

### RED evidence

The new regressions fail against the partial remediation before the final hardening:

```text
cargo test -p desklink-transport --test localhost
  input_flood_does_not_starve_control_delivery ... FAILED
  control was starved by the input flood

cargo test -p desklink-relay --test session relay_enforces_connection_and_session_admission_caps
  assertion failed: left Connection("connection lost") right ConnectionLimit
```

The empty and channel-only stream tests also add deterministic coverage for the previously untested EOF classifications.

### GREEN evidence

```text
cargo test -p desklink-transport --test localhost
10 passed, 0 failed

cargo test -p desklink-relay --test session
13 passed, 0 failed

cargo test --workspace
all workspace tests and doc-tests passed

cargo fmt --all -- --check
passed

cargo clippy --workspace --all-targets --all-features -- -D warnings
finished successfully

./scripts/verify.sh
finished successfully

git diff --check
passed
```

### Remediation files

- `crates/desklink-transport/src/lib.rs`
- `crates/desklink-transport/src/quic.rs`
- `crates/desklink-transport/tests/localhost.rs`
- `server/relay/src/lib.rs`
- `server/relay/tests/session.rs`

## Task 6 final follow-up

Date: 2026-07-15

The review found that separate producer queues were still exposed through one
receiver lock, so a consumer interested in input could not opt out of a video
or control flood. Each inbound lane now owns its own receiver mutex, and
`QuicClient` exposes `next_control`, `next_input`, `next_video_config`,
`next_video_datagram`, and `next_cursor_datagram`. The compatibility
`next_event` method uses the same bounded lane queues with a round-robin cursor;
dedicated consumers no longer contend with it.

The relay previously serialized application-close rejection through one slot
and called `Incoming::refuse()` for additional over-cap connections. Those
connections had no stable reason. Every over-cap incoming connection now gets a
post-handshake application close with `RELAY_CONNECTION_LIMIT_CLOSE_CODE`, and
the client maps both application-level and transport-level close forms of that
code to `TransportError::ConnectionLimit`. Join response EOF also consults the
connection close reason before falling back to a generic stream error.

Regression coverage:

```text
cargo test -p desklink-transport --test localhost
11 passed, 0 failed

cargo test -p desklink-relay --test session
14 passed, 0 failed

cargo test --workspace
all workspace tests and doc-tests passed

cargo clippy --workspace --all-targets --all-features -- -D warnings
passed

./scripts/verify.sh
passed
```
