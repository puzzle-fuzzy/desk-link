# Windows owned GPU media path

The second wave starts from frozen `65c70fd` on `codex/dl2-windows-gpu`.
Protocol 13 and its input epoch/configuration ownership remain unchanged.
The first GPU slice adds owned surfaces and capture APIs; production runtime
selection and Media Foundation surface input are a subsequent DL2-016 slice.

## Surface ownership

`GpuBgraPool::copy_from` copies an acquired BGRA texture into an owned default
texture. It delivers a frame only after its D3D11 EVENT completes. Capture then
releases the duplication frame. Retaining a desktop texture COM reference alone
does not replace this ownership boundary.

BGRA frames share an immutable lease through Arc. NV12 leases remain exclusive
and must stay owned until Media Foundation releases the last tracked sample
reference. Encoded output timestamps do not establish that release.

The process registry retains retired generations and pending fences. It limits
resources to 64 slots and 512 MiB of logical image bytes; driver tiling, padding,
views, queries and MFT allocations are outside that byte measurement. A timeout
quarantines the slot. Released leases still require completed fences before
reuse. Invalid queries cannot authorize reuse. D3D11 GetData's S_FALSE must be
checked explicitly; its generated Result wrapper alone does not mean complete.

The video processor scales progressive RGB full-range input to NV12 BT.709
limited range, matching the existing CPU conversion coefficients. It disables
automatic enhancement. Rate-profile history counts describe optimal temporal
processing, so profile selection does not reject progressive input merely for
advertising history. Actual progressive blits and synthetic color validation
establish the supported path on this adapter.

The GPU module requires video/BGRA device support and feature level 11. Below
11, EVENT/TIMESTAMP queries may not cover VideoProcessorBlt. Unsupported GPU
operations retain the CPU path. Capture tries video-capable device creation
first and preserves a BGRA-only duplication device fallback.

## Capture compatibility

`DesktopCapturer::next_frame` continues returning CPU `CapturedFrame` values.
The new `next_media_frame` chooses an owned GPU image or reads back the same
acquired source before ReleaseFrame. A new CPU image invalidates the GPU cache.
Only an immediately preceding DXGI Idle result permits `snapshot_media_frame`
to reuse the last owned image with a fresh submission timestamp. New capturers,
topology replacement and backend recovery discard old pool/cache state.

## Evidence boundaries

Known counters distinguish BGRA GPU copy bytes, NV12 GPU output bytes, explicit
CPU readback bytes, texture creation, query creation and video view creation.
Stable texture counts do not imply zero allocation: the current conversion
still creates queries and views. Driver/COM heap allocations are unknown.
CPU submission/wait wall time is not a GPU timestamp measurement or end-to-end
latency. No source desktop image is saved by these synthetic fixtures.

```powershell
$env:CARGO_TARGET_DIR='E:\desk-link\.worktrees\dl2-targets\windows-gpu'
python scripts/run-windows-cargo.py test -p desklink-windows --lib --locked gpu::tests -- --include-ignored --test-threads=1
python scripts/run-windows-cargo.py clippy -p desklink-windows --lib --tests --locked -- -D warnings
```

The first slice was tested in a source snapshot containing the frozen base plus
its exact four source overrides, excluding concurrent encoder work. Seven GPU
tests passed on RTX 4070 Ti SUPER; four explicitly opted in to synthetic native
GPU execution. This proves owned-copy, conversion and pool behavior, and does
not prove MFT surface encoding, interactive DXGI capture, multi-adapter recovery
or performance gains.

## Official API basis

- [MF_SA_D3D11_AWARE](https://learn.microsoft.com/en-us/windows/win32/medfound/mf-sa-d3d11-aware): read-only capability before manager negotiation.
- [DXGI surface buffers](https://learn.microsoft.com/en-us/windows/win32/api/mfapi/nf-mfapi-mfcreatedxgisurfacebuffer): wrap a D3D11 texture/subresource.
- [Tracked sample allocation](https://learn.microsoft.com/en-us/windows/win32/api/mfidl/nf-mfidl-imftrackedsample-setallocator): callback after other sample references are released.
- [VideoProcessorBlt](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11videocontext-videoprocessorblt): GPU output and query limits below feature level 11.
- [Rate conversion capabilities](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ns-d3d11-d3d11_video_processor_rate_conversion_caps): history for optimal processing.
