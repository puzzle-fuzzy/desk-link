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
