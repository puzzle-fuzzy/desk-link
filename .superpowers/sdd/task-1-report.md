# Task 1 Report: Repair the macOS build baseline and isolate VideoToolbox flags

## RED

Command:

```sh
cd apps/macos && swift test --arch arm64
```

Output: failed as required while compiling `H264Decoder.swift`:

```text
error: type 'VTDecodeFrameFlags.ArrayLiteralElement' (aka 'VTDecodeFrameFlags') has no case 'enableAsynchronousDecompression', but it does have a case named '_EnableAsynchronousDecompression'
```

## GREEN

The required Rust artifact was built first because the Swift package links `desklink_ffi`:

```sh
cargo build --manifest-path Cargo.toml --release -p desklink-ffi --target aarch64-apple-darwin
```

Output: `Finished release profile [optimized] target(s)` (exit 0).

The scoped decoder tests pass:

```sh
cd apps/macos && swift test --arch arm64 --filter H264DecoderTests
```

Output: `Executed 2 tests, with 0 failures` (exit 0).

The full requested command was rerun after the Rust artifact build. It compiled and linked, and the two new decoder tests passed, but the existing suite had one unrelated failure:

```text
cd apps/macos && swift test --arch arm64
```

Output: `Executed 13 tests, with 1 failure`; `VideoGeometryTests.testAspectFitLetterboxesWideVideoInsideSquareSurface` expected `(0.0, 218.75, 1000.0, 562.5)` but received `(-5.684341886080802e-14, 218.75, 1000.0000000000001, 562.5)`.

Rust FFI verification also passed:

```sh
cargo test -p desklink-ffi
```

Output: 10 tests passed, 0 failed.

## Files changed

- `apps/macos/Sources/DeskLinkApp/Video/H264Decoder.swift`: added internal `decodeFlags`, switched the decode call site to `Self.decodeFlags`, and added the Swift 6.3.3-required `nonisolated(unsafe)` annotation for the VideoToolbox session used by `deinit`.
- `apps/macos/Tests/DeskLinkAppTests/H264DecoderTests.swift`: added the two required `@MainActor` tests.
- `apps/macos/Package.swift`: unchanged; no unrelated linker frameworks were added.
- Windows code: unchanged.

## Commit

- `66498616f39a31eec8001b49aa1a47d1af9bb88b` — `fix(macos): adapt VideoToolbox decoder to current SDK`

## Self-review

- Confirmed the public caller-facing methods `configure(sequenceHeader:width:height:version:)`, `receive(accessUnit:frameID:version:)`, and `reset()` remain unchanged.
- Confirmed only the decoder source and required decoder test were committed.
- `git diff --check` passed.

## Concerns

- The full Swift suite remains non-green because of the pre-existing exact `CGRect` floating-point assertion in `VideoGeometryTests`; it was outside Task 1 and was not modified.
- `nonisolated(unsafe)` preserves the existing deinitializer behavior under Swift 6.3.3 but intentionally bypasses compiler isolation checking for that stored VideoToolbox session; later macOS host work should revisit ownership/isolation if the lifecycle changes.
- Rust-built objects emit deployment-target warnings because the local SDK reports macOS 26.5 while the Swift package targets macOS 13; no deployment target change was made in this task.

## Fix

### Files changed

- `apps/macos/Tests/DeskLinkAppTests/H264DecoderTests.swift`: configure the decoder through VideoToolbox with a complete H.264 Annex B SPS/PPS fixture before reset, then assert the populated configuration is cleared.
- `apps/macos/Tests/DeskLinkAppTests/VideoGeometryTests.swift`: compare rectangle origin and size components with `1e-12` accuracy to tolerate platform floating-point rounding.

### Verification

```text
cd apps/macos && swift test --arch arm64 --filter H264DecoderTests
Executed 2 tests, with 0 failures (0 unexpected)

cd apps/macos && swift test --arch arm64
Executed 13 tests, with 0 failures (0 unexpected)

cargo test -p desklink-ffi
3 unit tests, 6 ABI tests, 1 controller runtime test, and 0 doc tests passed; 0 failed
```

### Self-review

- Confirmed the reset test now exercises `configure`, verifies `configVersion == 1`, and retains the non-weakened post-reset state assertions.
- Confirmed only the two reviewer-requested test files and this report changed after commit `6649861`.
- `git diff --check` passed.
