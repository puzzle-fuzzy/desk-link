# Diagnostic u64 JSON contract

The native observer and diagnostic event types retain Rust `u64`. At the log
and signed JSON boundary, the fields below are canonical decimal **strings**.
The same convention is already used for full-width native input identities;
this diagnostic change does not change input IPC, protocol wire, configuration
acceptance or file transfer.

| Event | Exact u64 fields |
| --- | --- |
| `host_connecting`, `host_available`, `host_connected` | `stream_id` |
| `host_cursor_send` | `stream_id`, `send_attempts`, `send_errors` |
| `host_direct_listener_stopped` | `stream_id` |
| `controller_video_metrics` | `stream_id` when present, `received_video_packets`, `dropped_video_packets`, `completed_frames`, `delivered_video_frames`, `video_ipc_overflow_drops`, `video_ipc_keyframe_replacements`, `input_backpressure_count` |
| `controller_render_metrics` | `stream_id`, `received_frames`, `submitted_frames`, `displayed_frames`, `malformed_frames`, `coalesced_frame_drops`, `first_frame_ms` when present, `max_frame_gap_ms` when present, `profile_probe_ms` when present |

These are 18 distinct keys, matched by the exact event and field pair. No other
keys are added or converted. Schema, timestamp, u16/u32 measurements and
`Duration::as_millis()` fields retain their existing numeric types and bounds.
The fixed event names, source rules, levels, reasons and privacy restrictions
remain in force. `send_errors` describes local send errors, not delivery or
packet loss; listener retirement does not prove subsequent relay recovery.

A string has 1–20 ASCII decimal digits, no leading zero except `"0"`, and value
at most `18446744073709551615`. Counters may be zero; a stream ID must be
nonzero. Signs, whitespace, Unicode digits, fractions, exponent/hex notation,
21-digit strings and `18446744073709551616` are invalid. Optional fields remain
absent when the native value is absent.

The previous numeric producer could log a **legal** stream ID that the service
could not accept. `HostConnectionSettings::from_text` accepts the full nonzero
u64 range. Before changing the producer, a synthetic accepted configuration was
recorded with the actual `DiagnosticLog`, then passed through the actual batch
serialization, Blake2s ID and Ed25519 signature path. The original Rust body and
signature were passed unchanged to production `verifyBatch` from service commit
`bf5d7eb27f3f41be59ac317b63d7e93d10f38066`; no JS reserialization or re-signing
was used. All six signatures verified independently. Safe and zero-counter
controls passed, while `2^53`, `2^53+1`, `u64::MAX` stream IDs and a safe stream
with maximum u64 counters failed with 400. The body retained
`9007199254740993`, while JS parsed it as `9007199254740992`. These failures are
the defect, rather than evidence that those Rust values are invalid.

Old raw log lines are parsed by Rust before crossing the JS boundary.
`serde_json` retains a native integer through `u64::MAX` exactly. The uploader
selects its existing latest-100-event budget and normalizes only the listed
event/field pairs before generating a **new** body, hash and signature.
Canonical strings are preserved. Known malformed values, including floating
`1.0`, negative integers, invalid strings and zero stream IDs, fail the selected
batch with `Encoding`; no event is dropped to hide the failure. A failed event
is not partially rewritten. Unknown events/fields remain unchanged for the
service's existing validation, rather than being admitted by a global key rule.
An invalid line outside the selected batch does not block the latest 100.

The service may explicitly support old safe-integer numeric history. It must
not recover an unsafe numeric value after JS parsing by rounding or clamping.
An old signed numeric body is therefore still a rejection case, while a full
u64 string from either new logs or the Rust upgrade path is a positive case.

The producer owns the single new fixture at
`apps/windows/tests/fixtures/diagnostic-u64-producer.json`. Its schema-2 wrapper
contains the field table generated from the actual Rust matcher, six current
configuration/log/actual-signed-request cases, six old-log/normalized/resigned
cases, and source blob/SHA256/compiler provenance. The raw old requests are
retained alongside their new requests. Packaging adds provenance only: it does
not alter a request body, hash or signature. The parent commit and source hashes
avoid a self-referencing commit ID; the fixture's final Git blob and producer
commit are pinned in the handoff. Consumers copy that blob without rebuilding
its format. The original pre-fix log/body fixture is retained unchanged at
`apps/windows/tests/fixtures/diagnostic-u64-legacy-producer.json`.

The fixture uses synthetic keys and accepted configurations. Its seven event
variants share a test host log to cover the encoder; it is not a captured remote
session. Stream 1/counter 1 and rejected stream 0 have additional actual-path
Rust assertions. The fixture's `zero_counters` case has stream **7**, not zero.
Current and upgraded history both retain every event/key. New batch hashes and
signatures differ from the old bodies and are independently verified in Rust.

The GUI report reader accepts canonical strings and exact original Rust integer
history. It compares stream IDs as u64, keeps adjacent large streams distinct,
and does not use time matching when an explicit identity is malformed or zero.
No UI input or current lifecycle state is changed.

Local validation uses Rust 1.97.1 and
`CARGO_TARGET_DIR=E:/desk-link/.worktrees/dl2-targets/diagnostic-u64`:

```powershell
python scripts/run-windows-cargo.py test -p desklink-windows -p desklink-windows-ui --lib --locked --no-fail-fast
python scripts/run-windows-cargo.py clippy -p desklink-windows -p desklink-windows-ui --lib --tests --locked -- -D warnings
```

The selected libraries pass Windows **226 / 2 ignored** and GUI **141 / 1
ignored**; strict Clippy passes. These are not full-workspace counts. They cover
strict bounds, atomic event normalization, actual configuration/log/batch/hash/
signature upgrade, budget behavior, report visibility and adjacent stream
identity. The three ignored cases require an interactive desktop, an explicit
readback benchmark, or a specifically built signed update asset.

The frozen `5069158` GUI slice and its v1 numeric fixtures remain historical
evidence. Services implements the matching consumer/store/analysis slice in its
own branch; that history is not merged here. Full candidate integration,
actual GUI/Downloads export, live HTTPS ingestion and real devices remain open.
The inherited two Apple FFI Host failures are outside this selected-target run.
Raw before/fix logs, original signed bodies, field/fixture hashes and exact
source/commit identities are indexed by
[the precision handoff](E:/desk-link/.worktrees/dl2-targets/diagnostic-u64/validation/handoff-final.json).
