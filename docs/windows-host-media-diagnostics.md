# Windows Host media diagnostics

The current full-width JSON format is defined in
[`windows-diagnostic-u64-contract.md`](windows-diagnostic-u64-contract.md).
All 18 typed diagnostic u64 fields now use canonical decimal strings; native
observer types remain u64. The numeric fixtures and validation counts below
describe the frozen `5069158` GUI-wiring slice. Its unsafe-number rejection was
a contract defect for legal Rust u64 values, corrected by the new producer and
old-log normalization slice; full-width strings are positive cases.

`HostSupervisor::with_media_diagnostics_observer` and
`HostRuntime::with_media_diagnostics_observer` accept a
`HostMediaDiagnosticsObserver` (`Arc<dyn Fn(HostMediaDiagnostic) + Send + Sync>`).
The supervisor forwards the observer to each authenticated runtime attempt.

Initialization creates one process-wide worker and a bounded queue of 32 events.
Media/input tasks only call `try_send`; disk/HTTP or slow product callbacks run on
that worker. A full/unavailable queue cannot block media. Callback panics are
caught. `rejected_host_media_diagnostics()` reports queue rejections, or `None`
when the dispatcher is unavailable. It measures diagnostic backpressure only.
There is no queue allocation per peer and no unbounded retry queue.

`CursorSend` contains only stream_id, send_attempts, send_errors, disabled, and
the fixed `CapabilityOrSize` reason. First `TransportError::Datagram` disables
the optional cursor lane for that peer and emits one event; the cursor future
then parks until its owning peer cancels it. Other transport errors propagate.

The pinned Quinn send_datagram API evicts queued old datagrams and returns Ok
under buffer pressure. It has no QueueFull error. Its unsupported/disabled/size
errors map to the optional Datagram category, whereas ConnectionLost maps to a
connection error. Generation and application size checks remain independent.
These counters neither prove delivery nor count network loss or queue eviction.
Raw error strings, endpoint addresses, cursor pixels and input content are absent.

`DirectListenerStopped` contains stream_id, retired_active and the fixed
`ListenerClosed` reason. It is emitted once when the direct acceptor reaches
terminal None. Ordinary handshake/probe errors continue accepting. Shutdown
cancels pending probes, retires the selected direct connection outside its lock,
requests an IDR and keeps the control/input session running through relay.
Disabling and every installation share the path mutex, so a late successful
probe cannot repopulate a disabled slot. A missing direct session parks the
accept future instead of repeatedly returning None.

`DiagnosticEvent::HostMedia(event)` formats the product log event. The Windows
desktop registers a log observer in both normal and pairing Host preparation.
The DL2-044/045 GUI slices record both `CursorSend` and
`DirectListenerStopped` into the same `DiagnosticLog` instance as that worker's
lifecycle events. The existing report export reads its sanitized
history and includes the exact event fields. The callback retains the original
stream and writes history only; it never changes current GUI lifecycle state.
Runtime dispatch keeps its filesystem work on the existing bounded worker.
Without an observer, other consumers still use the background stderr fallback.

The desktop uploader remains unchanged and retains these events in ordinary
batches. Its current service contract is a separate source history:
`b1118ad465703493255123e23b6f522662c3d603` for cursor events and
`bf5d7eb27f3f41be59ac317b63d7e93d10f38066` for direct-listener events. That
history is not merged into this GUI branch. Both regression fixtures are copied
byte-for-byte from the committed service producer fixtures and compare the actual
GUI callback/log representation, ignoring only the generated timestamp.
The service's u64-overflow negative fixture remains valid local history; this
does not claim that unsafe JavaScript integers are accepted by the service.
The listener record preserves `retired_active` as an observed retirement state;
it does not assert that relay media subsequently recovered. Actual Tauri
interaction, combined service ingestion/storage and real devices remain separate
acceptance work.

Local validation:

```powershell
python scripts/run-windows-cargo.py test -p desklink-windows --lib --locked cursor
python scripts/run-windows-cargo.py test -p desklink-windows --lib --locked media_diagnostics
python scripts/run-windows-cargo.py test -p desklink-transport --test localhost --locked
python scripts/run-windows-cargo.py clippy -p desklink-windows -p desklink-transport --lib --tests --locked -- -D warnings
```

The transport cases use real loopback QUIC, including unsupported datagrams,
the peer's advertised size limit, connection/preflight errors and tiny send
buffers. They do not create a remote-control session or inject native input.

Desktop GUI source validation uses
`CARGO_TARGET_DIR=E:/desk-link/.worktrees/dl2-targets/windows-diagnostics`:

```powershell
# 044: selected library targets, not a full-workspace test run
python scripts/run-windows-cargo.py test -p desklink-windows -p desklink-windows-ui --lib --locked --no-fail-fast
# 045: changed GUI library, including all three log/report/fixture regressions
python scripts/run-windows-cargo.py test -p desklink-windows-ui --lib --locked
# Both phases: full strict linting for these packages' library and test targets
python scripts/run-windows-cargo.py clippy -p desklink-windows -p desklink-windows-ui --lib --tests --locked -- -D warnings
```

044 passed Windows **218 / 2 ignored** and GUI **137 / 1 ignored**. 045 passed
GUI **138 / 1 ignored**, including both cursor tests again; Windows/shared
source hashes remained identical to the 044 run. These library counts differ
from the frozen joint's Windows **283 / 17 ignored**, which also included
integration targets. Both strict Clippy runs passed without `allow` or
`--no-deps`. The ignored GUI test requires a specifically built signed update
asset; Windows exclusions cover interactive capture and an opt-in readback
benchmark. Frontend JS and the uploader were unchanged. The inherited full-FFI
Apple Host failures were not repaired or rerun by these GUI-only checks.

The three GUI tests directly invoke the production callback, use real temporary
logs and `sync_data`, then feed the existing history merger/report builder.
They do not open Tauri, start an authenticated peer, exercise actual Downloads
export or submit HTTP. Producer/dispatcher and real transport behavior retain
their separate source tests; these callback tests do not claim a new physical
end-to-end remote-control result.

The copied cursor/listener fixture SHA256 values are respectively
`8f3f077e9356144c63d302614851d0bb3e7dc8a2ad3b0dfe8c07146def2333c8` and
`b370b5175e8807dd4f2b13faf3457214ba81209f60b644f4803d9b332a4145ae`.
The raw logs, source-before/source-after manifests, commit identities and
digests are indexed by
[the GUI handoff](E:/desk-link/.worktrees/dl2-targets/windows-diagnostics/validation/handoff-final.json).
The earlier `044-warmup-*` logs are not final evidence.
