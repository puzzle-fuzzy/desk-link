# Windows Host media diagnostics

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

`DiagnosticEvent::HostMedia(event)` formats the product log event. The product
owner must bind the observer to the actual log/export sink (and, separately, any
server schema). Without an observer the worker uses stderr as a fallback; this
does not establish user-visible export or cloud delivery.

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
