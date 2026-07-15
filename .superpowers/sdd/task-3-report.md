# Task 3 report: expose and test the macOS host C ABI

## Changes

- Added fixed-layout host C ABI declarations and synchronized the Rust and Swift-facing header APIs.
- Added `DesklinkHostHandle`, host configuration, host event/input/metrics records, host callback dispatch, and host lifecycle functions.
- Added signed pairing-invite creation/validation, relay client startup, approval/rejection, encrypted media submission, keyframe requests, `ReleaseAll`, stop, and destroy/join behavior.
- Added `DesklinkSavedHostMaterial` and `desklink_controller_copy_saved_host_material` for caller-owned Keychain staging.
- Added null-pointer, fixed-length invite, invalid invite, approval key, media payload, and destroy/ReleaseAll ABI coverage.

## Verification

- `cargo fmt -p desklink-ffi`: passed.
- `cargo clippy -p desklink-ffi --all-targets -- -D warnings`: passed.
- `cargo test -p desklink-ffi --test host_abi`: 3 passed, 0 failed.
- `cargo test -p desklink-ffi`: 23 passed, 0 failed; doctests passed.
- `git diff --check`: passed.

## Notes

- Host startup accepts only `quic://` relay URLs and validates the configured host identity against a signed invite.
- Callback data is owned by a dispatcher-local temporary and is valid only during the callback.
- The existing Task 2 worker received one lint-only `too_many_arguments` allowance, and its fixture test was simplified to satisfy denied-clippy verification; no protocol behavior changed.
- Full workspace formatting remains subject to the known linked-worktree vendor metadata issue; package formatting and denied-clippy checks pass.
