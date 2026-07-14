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
