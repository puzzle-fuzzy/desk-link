# Exact diagnostic u64 contract

Diagnostic u64 values use canonical decimal JSON strings. Historical nonnegative
safe-integer JSON numbers remain supported. Stream IDs are nonzero; counters
and u64 durations may be zero. A string is exactly `0` or a nonzero ASCII digit
followed by digits, at most 20 characters and no greater than
`18446744073709551615`. There is no trimming, sign, leading zero, decimal point,
exponent, hexadecimal notation, separator or Unicode digit conversion.

These existing event/field pairs are the complete string-u64 scope:

| Event | u64 fields |
| --- | --- |
| `host_connecting`, `host_available`, `host_connected` | `stream_id` |
| `host_cursor_send` | `stream_id`, `send_attempts`, `send_errors` |
| `host_direct_listener_stopped` | `stream_id` |
| `controller_video_metrics` | `stream_id`, `received_video_packets`, `dropped_video_packets`, `completed_frames`, `delivered_video_frames`, `video_ipc_overflow_drops`, `video_ipc_keyframe_replacements`, `input_backpressure_count` |
| `controller_render_metrics` | `stream_id`, `received_frames`, `submitted_frames`, `displayed_frames`, `malformed_frames`, `coalesced_frame_drops`, `first_frame_ms`, `max_frame_gap_ms`, `profile_probe_ms` |

There are 18 unique u64 keys across seven events. This adds no event names, keys,
reason values or arbitrary numeric strings. The two HostMedia events retain
their exact key sets, host source, warning level, fixed reason and strict boolean
fields. Existing non-HostMedia source behavior is unchanged. Timestamps, schema,
u16/u32 fields and `Duration::as_millis` fields remain JSON numbers.

Unsafe historical JSON numbers are rejected: after `JSON.parse`, rounded digits
cannot be recovered. The producer must normalize exact Rust u64 numbers from
known historical event/field pairs before it serializes and signs the new batch.
Unknown fields or invalid strings do not gain acceptance through normalization.
The consumer's number-compatibility path can validate the parsed integer value,
not distinguish a historical safe number's `1`, `1.0` or `1e0` source spelling;
canonical string forms such as `"1.0"` and `"1e0"` are always invalid.

## Comparison, storage and reporting

`canonicalU64` checks length and syntax before bounded conversion. `parseU64`
creates BigInt only for local comparisons and arithmetic. The validated event
retains the original number or string; BigInt never enters payloads or JSON.
`send_errors <= send_attempts` is compared exactly, including adjacent values
above 2^53 and immediately below UINT64_MAX.

The store needs no schema migration. IDs and counters remain inside TEXT JSON;
the separate timestamp column is still a safe-integer timestamp. Signed body
bytes and hashes are not rewritten by the service. Existing atomic receipts,
deduplication, privacy checks and storage quotas remain in force.

Analysis uses canonical string stream keys. Historical `7` and new `"7"` share
one stream, while `"9007199254740992"` and `"9007199254740993"` remain distinct.
Counter maxima, sums and thresholds use exact local arithmetic. Report counters
stay numbers when lossless and become decimal strings when larger; an aggregate
across streams or attempts can exceed one u64 without truncation. Reports and
health JSON contain no BigInt. Packet-loss thresholds compare the original
integer ratio; only a rounded bounded percentage is converted to a Number.
Cursor send errors and listener shutdown still do not establish network loss,
datagram queue evictions or successful relay recovery.

## Proof boundaries

The canonical producer fixture is owned by the producer branch at
`apps/windows/tests/fixtures/diagnostic-u64-producer.json`. The service imports
the same pinned Git blob without generating, converting or re-signing it.
Its current and historical-normalized requests exercise the actual Rust log,
batch serialization, hash and signature path. Consumer integration uses those
raw bytes and signatures through loopback HTTP, production schema and SQLite.

The older cursor/listener fixtures remain unchanged as historical encoder
snapshots. Their unsafe numeric cases remain negative cases, while the new
complete-u64 decimal-string cases are positive acceptance cases. Synthetic
boundary tests supplement the canonical producer fixture; they do not replace
the source-bound producer evidence or establish real-device delivery.

## Pinned evidence and local validation

- Producer commit: `28c11ee05a08183f7636ede7946e93029c7bbb16`;
  parent `5069158584dfa1f729defe2fadb5b9933acdfcff`.
- Fixture Git blob: `58f386235e9dda4c5045b21af9a1a37a53cc39da`;
  72,875 bytes; SHA-256
  `8d7d3124436b52906c63905340d7fa7a968017de40a639db37efe18489bb19bf`.
- All eight `provenance.sources` blob IDs and SHA-256 digests match the producer
  commit. The wrapper records the base commit because embedding its own final
  commit would be self-referential; the commit above binds the final fixture.
- `u64_fields_by_event` comes from the real Rust production table. The fixture
  contains six current configured-ID/log/signed-request cases and six old-log
  cases normalized by Rust before fresh serialization, hashing and signing.
  Normalizing the historical cases changes only the 24 u64 field occurrences
  across each seven-event batch; other fields and event counts remain intact.

`src/u64-producer-contract.test.ts` sends the original `body_utf8`, hash and
Ed25519 signature through a real loopback listener into the production app and
file-backed store. All 12 batches return 202, store exact event values, keep
duplicate usage unchanged and survive close/reopen. The cases cover 2^53-1,
2^53, 2^53+1, UINT64_MAX IDs, all-u64-max counters/durations and zero counters
with nonzero stream 7. Unmodified old Rust-signed requests still accept safe
numbers and atomically reject unsafe numbers. Changing the body with a fresh
hash but the original signature returns 401 without storage.

The production fixture has null correlation IDs and host-owned batches
containing all seven event types. The integration test preserves those signed
privacy/source values and verifies uncorrelated analysis. A separate explicitly
synthetic analysis group over the stored rows proves that adjacent large stream
IDs stay distinct and that the three-stream render total is exactly
`18446744073709551621`. It does not manufacture a real product session.

On 2026-10-26, from `server/diagnostics`, Bun 1.3.14:

```powershell
npm exec --yes --package bun@1.3.14 -- bun test
npm exec --yes --package bun@1.3.14 -- bun run typecheck
```

Result: 129 tests passed, 0 failed, 6,017 assertions; typecheck passed.
Local logs: `E:\desk-link\.worktrees\dl2-targets\dl2-services-u64\diagnostics-test.log`
and `diagnostics-typecheck.log` in the same directory; `producer-provenance.json`
records the byte and eight-source fingerprint checks. Existing cursor/listener
fixture bytes remain unchanged. These results cover local contracts and storage;
GUI observer/export, hardware, real delivery and relay recovery remain separate
acceptance work for DL2-044/045.
