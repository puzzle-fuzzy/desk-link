# Windows owned GPU media path

The second wave starts from frozen `65c70fd` on `codex/dl2-windows-gpu`.
Protocol 13 and its input epoch/configuration ownership remain unchanged.
The first GPU slice adds owned surfaces and capture APIs. The DL2-016 slice
connects production runtime selection and Media Foundation surface input.

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

## D3D11 surface encoding

The runtime selects CPU or GPU capture using the encoder's actual input policy.
GPU initialization binds the source device's adapter LUID and configures only
an activated D3D11-aware hardware MFT. Async unlock precedes the DXGI device
manager message, and the manager is installed before media-type negotiation.
`MF_SA_D3D11_AWARE` is read, never overwritten. NV12 textures are wrapped in
`MFCreateDXGISurfaceBuffer`; the tracked sample owns the pool lease.

The existing async input credits, 16 pending frames, 8 MiB output budget and
progress watchdog remain. A consumed output does not return the input lease.
An external sample COM reference can keep a slot busy even after encoder flush,
shutdown, drop and release of its startup guard. Only the final sample reference
permits the allocator callback to return the lease.

Every actual backend/configuration change reserves a checked new version before
publication. GPU errors keep the source owned BGRA image for a same-frame CPU
readback, software IDR retry and sticky GPU disable for this peer. Version
exhaustion exits with its typed error. Ordinary CPU input after GPU changes the
real backend/configuration without latching a device failure, allowing later
GPU input to bind again. Empty old NV12 pool owners are dropped during switches;
outstanding sample leases continue to own their surfaces independently.

`gpu_status.surface_backend_active` describes the current backend. Adapter/MFT
and manager negotiation fields retain historical attempt information and cannot
alone identify an active GPU path. Normal GPU submissions report
`gpu_surface_input=true`, CPU NV12 writes=0 and MF input copy bytes=0. BGRA GPU
CopyResource and VP output writes still occur: this is not a zero-copy claim.

Native opt-in tests verified NVIDIA720p eight surface inputs/eight outputs,
forced IDRs, color attributes, CPU/GPU transitions, delayed output identity,
same-frame software fallback, version exhaustion and final-release ownership.
A separate software H.264 decoder validates 128 actual encoded frames across
Main/High, two phases and 512²→640×384 reconfiguration. It checks timestamp,
SPS/profile/dimensions and tile colors with compression tolerance; swapped
axes/UV and stale-phase counterexamples fail the comparator.

The R2 regression obtains input tokens from real encoded pipeline VideoConfig
bytes. GPU→CPU→GPU and codec-only changes preserve the epoch. Same-size origin
changes renew it; old encoded output and old input releases cannot acquire the
new context. Injection uses a recording backend, never native SendInput.

```powershell
python scripts/run-windows-cargo.py test -p desklink-windows --lib --locked encoder::gpu_tests:: -- --ignored --test-threads=1
python scripts/run-windows-cargo.py test -p desklink-windows --lib --locked gpu::bitstream_validation -- --include-ignored --test-threads=1
python scripts/run-windows-cargo.py test -p desklink-windows --lib --locked gpu_media_backend_changes -- --ignored --test-threads=1
```

Intel/AMD/hybrid adapters, physical device/driver loss, actual duplication,
two-machine latency and sustained workload measurements remain separate gates.
The registry reclaims completed, unowned retired generations on allocation and
through `trim_retired_gpu_surfaces`. Pool drop clears the generation's own
references before a safe trim. A software capture policy releases its old GPU
pool/cache; the worker also trims after capture and encoder have returned and
dropped. These are generation/shutdown paths, not per-frame or allocator-callback
work. A still-owned lease or incomplete/invalid fence on a live device remains
accounted and retained. A late callback can leave bounded retired resources until
the next trim/allocation; cleanup never promises to interrupt driver calls or
force a sample's release.

`gpu_resident_stats` reports registry slots and logical image bytes. These are
not driver VRAM measurements. The native cleanup regression checks both live
leases and incomplete events survive trim, then confirms completed retired slots
are reclaimed after the final ownership/completion boundary.

## Official API basis

- [MF_SA_D3D11_AWARE](https://learn.microsoft.com/en-us/windows/win32/medfound/mf-sa-d3d11-aware): read-only capability before manager negotiation.
- [DXGI surface buffers](https://learn.microsoft.com/en-us/windows/win32/api/mfapi/nf-mfapi-mfcreatedxgisurfacebuffer): wrap a D3D11 texture/subresource.
- [Tracked sample allocation](https://learn.microsoft.com/en-us/windows/win32/api/mfidl/nf-mfidl-imftrackedsample-setallocator): callback after other sample references are released.
- [VideoProcessorBlt](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11videocontext-videoprocessorblt): GPU output and query limits below feature level 11.
- [Rate conversion capabilities](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ns-d3d11-d3d11_video_processor_rate_conversion_caps): history for optimal processing.
