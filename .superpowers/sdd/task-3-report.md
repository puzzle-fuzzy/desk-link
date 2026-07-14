# Task 3 report: latest-frame-first video queues and frame assembly

## RED

Added `crates/desklink-video/tests/ordering.rs` with the brief's queue, expiry, and stale-presentation cases. The mandated command initially failed because the `ordering` test target and requested video exports did not exist. After adding the tests, it failed to compile with unresolved `LatestFrameQueue`, `FrameAssembler`, `EncodedFrame`, `AssembleResult`, and `desklink_protocol` imports.

## GREEN

Implemented:

- bounded `LatestFrameQueue<T>` with non-zero capacity assertion, oldest eviction, newest-first draining, and length reporting;
- `FrameAssembler` with validated packets, duplicate and metadata mismatch drops, bounded incomplete-frame eviction, explicit expiry, complete frame assembly, and lexicographic stream/frame stale rejection;
- public `EncodedFrame`, `DropReason`, and `AssembleResult` interfaces;
- `desklink-protocol` as the video crate's dependency;
- focused edge-case tests for duplicate chunks, metadata mismatch, capacity eviction, and cross-stream presentation ordering.

Verification:

- `cargo test -p desklink-video --test ordering`: 6 passed, 0 failed.
- `cargo test --workspace`: all unit, integration, and doc tests passed; 14 protocol tests and 6 video ordering tests passed.
- `git diff --check`: passed.

## Concerns

- `LatestFrameQueue::new` and `FrameAssembler::new` reject zero capacity by panic, matching the brief's “reject capacity zero” requirement.
- `FrameAssembler::push` accepts an owned `VideoPacket`, so malformed packets can only be represented by manually constructed values; those are revalidated and reported as `DropReason::Malformed`.
- The generated root `Cargo.lock` is untracked and was not included in the video-only commit.

## Task 3 review fixes

Fix evidence:

- `desklink-protocol` now exports documented `MAX_VIDEO_CHUNKS = 4096` and rejects zero, out-of-range, and over-bound `chunk_count` values during header/packet validation. This bounds a frame to about 4.9 MiB at 1200-byte datagrams and supports the 1920x1080 H.264 MVP. `desklink-video` uses the validated protocol bound when completing frames.
- `FrameAssembler::begin_stream(stream_id)` clears partial and presentation state. Packets and presentations from non-active streams are rejected; a numerically smaller stream rollover is covered by `smaller_stream_rollover_clears_state_and_rejects_delayed_old_packets`.
- `FrameAssembler::push` calls `expire(now)` before packet acceptance. `push_expires_overdue_partials_before_accepting_new_packet` proves an overdue partial cannot complete with a later packet.
- Removed unreachable public `DropReason::Evicted` and `DropReason::Expired` variants. Eviction and expiry remain observable through the existing `push`/`expire` behavior without claiming a drop outcome that those APIs do not return.

Files changed:

- `crates/desklink-protocol/src/lib.rs`
- `crates/desklink-protocol/src/codec.rs`
- `crates/desklink-protocol/tests/round_trip.rs`
- `crates/desklink-video/src/packet.rs`
- `crates/desklink-video/src/queue.rs`
- `crates/desklink-video/tests/ordering.rs`

Commits: implementation `1f2794044fdcb683b61b83abe11507fce5e36a32`; report-inclusive amendment `a00d7e0f762ef2cc075ec4119d353ba58cd3a761` (this final report edit is amended once more below).

Verification evidence:

- TDD red: focused tests failed before implementation because `begin_stream` and `MAX_VIDEO_CHUNKS` were absent.
- `cargo test -p desklink-video --test ordering`: 8 passed, 0 failed.
- `cargo test --workspace`: all tests and doc-tests passed; protocol 14 passed and video ordering 8 passed.
- `./scripts/verify.sh`: passed, including clippy with warnings denied and the workspace test suite.
- Self-review: reviewed the final diff against all four Critical/Important findings; no changes outside protocol/video crates and this report were staged. The pre-existing untracked root `Cargo.lock` remains untouched.

Remaining concerns:

- `begin_stream` returns `false` when asked to begin the already-active stream; callers should treat rollover as an explicit transition to a different stream ID.
- Expiry and capacity eviction are counted by `expire`/capacity behavior but are not individually returned as `AssembleResult` values; this is intentional after removing misleading public variants and keeps the current interface focused.

## Remaining review issue: retired stream IDs

Change:

- Added `FrameAssembler::retired_streams: BTreeSet<u64>`.
- `begin_stream` now rejects the active ID and every retired ID; when switching, it retires the prior active ID before clearing partial/presentation state.
- First stream initialization remains allowed because no ID is retired initially.
- Added `retired_stream_id_cannot_be_reactivated_after_rollover`, covering 10 → 2 → 10 and rejection of a delayed packet from the original stream 10 while stream 2 remains current.

Exact verification evidence:

- `cargo test -p desklink-video --test ordering retired_stream_id_cannot_be_reactivated_after_rollover`: failed before the fix at `assert!(!assembler.begin_stream(10))`; passed after the fix.
- `cargo test -p desklink-video --test ordering`: `9 passed, 0 failed`.
- `cargo test --workspace`: completed successfully; video ordering `9 passed, 0 failed`, protocol round-trip `14 passed, 0 failed`, all other unit/doc test targets passed.
- `./scripts/verify.sh`: completed successfully; clippy/check and workspace test stages passed.
- `git diff --check`: passed.

Self-review:

- Reviewed the final diff against the requested lifecycle: first initialization succeeds, active ID is rejected, rollover retires the old ID, retired IDs cannot be reused, delayed old packets are rejected, and the current stream continues accepting packets.
- Only `crates/desklink-video/src/packet.rs`, `crates/desklink-video/tests/ordering.rs`, and this report are changed for this fix. The pre-existing untracked root `Cargo.lock` remains untouched and is not included.

Concerns:

- Retired stream IDs are retained for the assembler lifetime as required; an assembler that processes an unbounded number of stream IDs will grow the set accordingly.
- `begin_stream` continues to return `false` for the active ID and now also returns `false` for retired IDs; callers must treat stream rollover as one-way.
