use super::*;

static GPU_TEST_GATE: Mutex<()> = Mutex::new(());

#[test]
fn event_requires_s_ok_and_a_true_completion_flag() {
    assert!(!event_is_complete(HRESULT(1), 0).unwrap());
    assert!(!event_is_complete(HRESULT(1), 1).unwrap());
    assert!(!event_is_complete(HRESULT(0), 0).unwrap());
    assert!(event_is_complete(HRESULT(0), 1).unwrap());
    assert!(event_is_complete(windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL, 1).is_err());
}

#[test]
fn validates_dimensions_capacity_and_packed_nv12_rows() {
    assert!(surface_bytes(0, 4, DXGI_FORMAT_NV12).is_err());
    assert!(surface_bytes(3, 4, DXGI_FORMAT_NV12).is_err());
    assert_eq!(surface_bytes(4, 4, DXGI_FORMAT_NV12).unwrap(), 24);
    let padded: Vec<u8> = (0..48).collect();
    let mapped = D3D11_MAPPED_SUBRESOURCE {
        pData: padded.as_ptr().cast_mut().cast(),
        RowPitch: 8,
        DepthPitch: 48,
    };
    let packed = copy_mapped(&mapped, 4, 4, DXGI_FORMAT_NV12).unwrap();
    assert_eq!(
        packed,
        [
            0, 1, 2, 3, 8, 9, 10, 11, 16, 17, 18, 19, 24, 25, 26, 27, 32, 33, 34, 35, 40, 41, 42,
            43
        ]
    );
}

#[test]
fn mapped_bgra_preserves_all_rows_and_rejects_short_pitch() {
    let source: Vec<u8> = (0..48).collect();
    let mut mapped = D3D11_MAPPED_SUBRESOURCE {
        pData: source.as_ptr().cast_mut().cast(),
        RowPitch: 24,
        DepthPitch: 48,
    };
    assert_eq!(
        copy_mapped(&mapped, 4, 2, DXGI_FORMAT_B8G8R8A8_UNORM).unwrap(),
        [&source[..16], &source[24..40]].concat()
    );
    mapped.RowPitch = 15;
    assert!(copy_mapped(&mapped, 4, 2, DXGI_FORMAT_B8G8R8A8_UNORM).is_err());
}

fn hardware_device() -> ID3D11Device {
    let adapter = enumerate_adapters()
        .unwrap()
        .into_iter()
        .find(|adapter| !adapter.software)
        .expect("hardware adapter required");
    create_device(adapter.index).expect("video-capable feature-level-11 device required")
}

#[test]
#[ignore = "opt-in synthetic GPU adapter/video processor test; no desktop capture"]
fn owned_bgra_clone_blocks_reuse_and_survives_pool_drop() {
    let _gate = lock(&GPU_TEST_GATE);
    let device = hardware_device();
    let original = vec![37; 16 * 16 * 4];
    let source = create_synthetic_bgra(&device, 16, 16, &original).unwrap();
    let pool = GpuBgraPool::from_device(device.clone(), 16, 16, 1).unwrap();
    let frame = pool.copy_from(&source, 17).unwrap();
    let copy = frame.with_timestamp(18);
    assert!(matches!(pool.copy_from(&source, 19), Err(GpuError::Busy)));
    drop(source);
    drop(frame);
    assert_eq!(copy.timestamp_us(), 18);
    assert_eq!(copy.readback_bgra().unwrap(), original);
    let next_source = create_synthetic_bgra(&device, 16, 16, &vec![99; 16 * 16 * 4]).unwrap();
    assert!(matches!(
        pool.copy_from(&next_source, 19),
        Err(GpuError::Busy)
    ));
    drop(copy);
    let next = pool.copy_from(&next_source, 19).unwrap();
    drop(pool);
    assert_eq!(next.readback_bgra().unwrap(), vec![99; 16 * 16 * 4]);
}

#[test]
#[ignore = "opt-in synthetic GPU adapter/video processor test; no desktop capture"]
fn gpu_bt709_nv12_conversion_scaling_and_lease_ownership() {
    let _gate = lock(&GPU_TEST_GATE);
    let device = hardware_device();
    let mut pixels = vec![0; 64 * 32 * 4];
    for y in 0..32 {
        for x in 0..64 {
            let color = if x < 32 {
                [0, 0, 255, 255]
            } else {
                [255, 0, 0, 255]
            };
            pixels[(y * 64 + x) * 4..(y * 64 + x + 1) * 4].copy_from_slice(&color);
        }
    }
    let source = create_synthetic_bgra(&device, 64, 32, &pixels).unwrap();
    let bgra = GpuBgraPool::from_device(device.clone(), 64, 32, 1).unwrap();
    let frame = bgra.copy_from(&source, 31).unwrap();
    let nv12 = GpuNv12Pool::from_device(device.clone(), 64, 32, 32, 16, 1).unwrap();
    let before = gpu_metrics().cpu_readback_bytes;
    let lease = nv12.convert(&frame).unwrap();
    assert_eq!(
        gpu_metrics().cpu_readback_bytes,
        before,
        "GPU conversion must not Map"
    );
    assert_eq!(lease.timestamp_us(), 31);
    assert_eq!(lease.subresource_index(), 0);
    assert!(matches!(nv12.convert(&frame), Err(GpuError::Busy)));
    let bytes = readback_nv12(&lease).unwrap();
    assert_eq!(bytes.len(), 32 * 16 * 3 / 2);
    // Existing CPU BT.709 studio approximation: red=(63,102,240), blue=(32,241,118).
    for (x, expected) in [(8usize, [63u8, 102, 240]), (24usize, [32u8, 241, 118])] {
        let actual = [
            bytes[8 * 32 + x],
            bytes[32 * 16 + 4 * 32 + x],
            bytes[32 * 16 + 4 * 32 + x + 1],
        ];
        for (a, e) in actual.into_iter().zip(expected) {
            assert!(
                a.abs_diff(e) <= 2,
                "actual={actual:?} expected={expected:?}"
            );
        }
    }
    drop(nv12);
    assert_eq!(
        readback_nv12(&lease).unwrap(),
        bytes,
        "lease survives pool reconfiguration/drop"
    );
    drop(lease);
    let next = GpuNv12Pool::from_device(device, 64, 32, 32, 16, 1).unwrap();
    assert!(next.convert(&frame).is_ok());
}

#[test]
#[ignore = "opt-in synthetic GPU query/pool test; no desktop capture"]
fn released_lease_cannot_reuse_until_event_completes() {
    let _gate = lock(&GPU_TEST_GATE);
    let device = hardware_device();
    let source = create_synthetic_bgra(&device, 16, 16, &vec![0; 16 * 16 * 4]).unwrap();
    let pool = GpuBgraPool::from_device(device, 16, 16, 1).unwrap();
    let frame = pool.copy_from(&source, 1).unwrap();
    let query = pool.pool.device.query().unwrap();
    // A never-End'ed EVENT is invalid on this adapter. Drivers can also report
    // pending data. Neither an error nor pending data permits slot reuse.
    assert!(!matches!(pool.pool.device.completed(&query), Ok(true)));
    *lock(&frame.lease.0.fence) = Some(query.clone());
    drop(frame);
    assert!(matches!(
        pool.copy_from(&source, 2),
        Err(GpuError::Busy | GpuError::Native { .. })
    ));
    unsafe { pool.pool.device.context.End(&query) };
    pool.pool.device.wait(&query).unwrap();
    assert!(pool.copy_from(&source, 3).is_ok());
}

#[test]
#[ignore = "opt-in synthetic GPU global-budget test; no desktop capture"]
fn retired_generations_and_live_leases_share_one_global_slot_cap() {
    let _gate = lock(&GPU_TEST_GATE);
    let device = hardware_device();
    let source = create_synthetic_bgra(&device, 16, 16, &vec![0; 16 * 16 * 4]).unwrap();
    let mut frames = Vec::new();
    for _ in 0..MAX_GPU_RESIDENT_SLOTS {
        let pool = GpuBgraPool::from_device(device.clone(), 16, 16, 1).unwrap();
        frames.push(pool.copy_from(&source, 1).unwrap());
        // Drop generation while its frame remains owned; registry must count it.
    }
    let pool = GpuBgraPool::from_device(device, 16, 16, 1).unwrap();
    assert!(matches!(pool.copy_from(&source, 2), Err(GpuError::Busy)));
    frames.pop();
    assert!(pool.copy_from(&source, 3).is_ok());
}
