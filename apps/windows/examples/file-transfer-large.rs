//! Explicit heavy acceptance entry. This reads and encrypts every byte of a
//! real >4-GiB sparse source, writes a real destination, and rereads it in full.
//! It is an in-memory encrypted bridge, not public-network/two-device proof.
use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

pub use apps_windows::{transfer_checkpoint, transfer_task};
#[path = "../test-support/file-task-fixture.rs"]
mod fixture;
use fixture::{Fixture, Peers, start};
use transfer_checkpoint::{SourceIndex, source_index_bytes};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn check_capacity(directory: &std::path::Path, required: u64) -> Result<u64, String> {
    #[cfg(windows)]
    {
        use windows::{Win32::Storage::FileSystem::GetDiskFreeSpaceExW, core::HSTRING};
        let mut available = 0;
        unsafe {
            GetDiskFreeSpaceExW(
                &HSTRING::from(directory.as_os_str()),
                Some(&mut available),
                None,
                None,
            )
        }
        .map_err(|error| format!("free-space query failed: {error}"))?;
        if available < required {
            return Err(format!(
                "fixture requires {required} free bytes; available {available}"
            ));
        }
        Ok(available)
    }
    #[cfg(not(windows))]
    {
        let _ = (directory, required);
        Err("This entry validates the Windows implementation; run it on Windows.".into())
    }
}

fn make_sparse(file: &File) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::HANDLE,
            System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_SPARSE},
        };
        let mut returned = 0;
        unsafe {
            DeviceIoControl(
                HANDLE(file.as_raw_handle()),
                FSCTL_SET_SPARSE,
                None,
                0,
                None,
                0,
                Some(&mut returned),
                None,
            )
        }
        .map_err(|error| format!("sparse-file setup failed: {error}"))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if !arguments.iter().any(|argument| argument == "--run") {
        return Err("Explicit heavy run required: --run --fixture-root <existing absolute directory> [--keep]".into());
    }
    let base = arguments
        .windows(2)
        .find(|pair| pair[0] == "--fixture-root")
        .map(|pair| PathBuf::from(&pair[1]))
        .ok_or("--fixture-root is required")?;
    if !base.is_absolute() || !base.is_dir() {
        return Err("fixture root must be an existing absolute directory".into());
    }
    let base = fs::canonicalize(base)?;
    let size = (1_u64 << 32) + 123;
    let reserve = size
        + 3 * source_index_bytes(size).map_err(|error| format!("{error:?}"))?
        + 256 * 1024 * 1024;
    let available = check_capacity(&base, reserve)?;
    let mut fixture = Fixture::new_in(&base);
    if arguments.iter().any(|argument| argument == "--keep") {
        fixture.preserve();
    }
    let source = fixture.path("large-source.bin");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&source)?;
    make_sparse(&file)?;
    file.set_len(size)?;
    file.write_all(b"DeskLink DL2-030 real sparse source: all bytes are read and encrypted.\n")?;
    file.seek(SeekFrom::Start(size - 123))?;
    file.write_all(
        &(0..123)
            .map(|index| (index * 17 % 251) as u8)
            .collect::<Vec<_>>(),
    )?;
    file.sync_all()?;
    drop(file);
    let source_modified = fs::metadata(&source)?.modified()?;
    println!(
        "{}",
        serde_json::json!({"event":"start","size":size.to_string(),"fixtureRoot":fixture.0,"availableBytes":available.to_string(),"requiredBytes":reserve.to_string(),"transport":"Noise SecureSession, local in-memory bridge"})
    );
    let started = Instant::now();
    let peers = Peers::new(&fixture, true, Duration::ZERO).await;
    let task = [0x4f; 16];
    start(&peers.a, &source, task).await;
    let mut ticker = tokio::time::interval(Duration::from_secs(2));
    tokio::time::timeout(Duration::from_secs(3600), async {
        loop {
            let a = peers.a_progress.borrow().clone();
            let b = peers.b_progress.borrow().clone();
            for snapshot in [&a, &b].into_iter().flatten() {
                if matches!(snapshot.state, "failed" | "partial" | "cancelled") {
                    return Err(format!("actor failed: {snapshot:?}"));
                }
            }
            if a.as_ref().is_some_and(|snapshot| snapshot.state == "completed")
                && b.as_ref().is_some_and(|snapshot| snapshot.state == "completed") { return Ok(()); }
            ticker.tick().await;
            let stats = peers.stats.lock().unwrap();
            println!("{}", serde_json::json!({"event":"progress","elapsedSeconds":started.elapsed().as_secs(),"sourceState":a.as_ref().map(|snapshot|snapshot.state),"destinationState":b.as_ref().map(|snapshot|snapshot.state),"sourceMessage":a.as_ref().and_then(|snapshot|snapshot.message.as_ref()),"encryptedMessages":stats.encrypted_messages,"transferredBytes":stats.transferred_bytes.to_string(),"durableBlockAcks":stats.acknowledgements}));
        }
    }).await.map_err(|_| "heavy actor transfer timed out")??;
    let (
        encrypted_messages,
        transferred_bytes,
        acknowledgements,
        maximum_ack_offset,
        block_count,
        last_offset,
    ) = {
        let stats = peers.stats.lock().unwrap();
        (
            stats.encrypted_messages,
            stats.transferred_bytes,
            stats.acknowledgements,
            stats.maximum_ack_offset,
            stats.blocks.len(),
            stats.blocks.last().map(|entry| entry.1).unwrap_or(0),
        )
    };
    if transferred_bytes != size
        || maximum_ack_offset <= u64::from(u32::MAX)
        || acknowledgements != size.div_ceil(u64::from(transfer_checkpoint::FILE_BLOCK_BYTES))
    {
        return Err(
            "heavy fixture did not carry/acknowledge every real byte beyond the 4-GiB boundary"
                .into(),
        );
    }
    let source_cache = fixture.options(true, true);
    let source_index = source_cache
        .cache_root
        .join(source_cache.peer_key)
        .join(transfer_task::id_text(task))
        .join("1.index");
    let expected = SourceIndex::open(&source, &source_index)
        .map_err(|error| format!("source index: {error:?}"))?
        .content_root();
    let destination = fixture.received(false, task, "large-source.bin");
    if fs::metadata(&destination)?.len() != size
        || fs::metadata(&source)?.modified()? != source_modified
    {
        return Err("source identity or destination size changed unexpectedly".into());
    }
    peers.close().await;
    let mut last_log = Instant::now();
    let verified = SourceIndex::build(&destination, &fixture.path("independent-readback.index"), &AtomicBool::new(false), |bytes| {
        if last_log.elapsed() >= Duration::from_secs(2) {
            println!("{}", serde_json::json!({"event":"independentReadback","verifiedBytes":bytes.to_string()}));
            last_log = Instant::now();
        }
    }).map_err(|error| format!("independent full readback: {error:?}"))?;
    if verified.content_root() != expected {
        return Err("independent full destination readback root mismatch".into());
    }
    let summary = serde_json::json!({
        "result":"passed", "size":size.to_string(), "source":source, "destination":destination,
        "contentRootBlake2sBlocksV14":hex(&expected), "independentReadbackRoot":hex(&verified.content_root()),
        "encryptedMessages":encrypted_messages, "actualTransferredBytes":transferred_bytes.to_string(),
        "durableBlockAcks":acknowledgements, "maximumDurableOffset":maximum_ack_offset.to_string(), "blockCount":block_count, "lastBlockOffset":last_offset.to_string(),
        "elapsedSeconds":started.elapsed().as_secs_f64(), "fixtureRetained":arguments.iter().any(|argument|argument=="--keep"),
        "transport":"Noise SecureSession, local in-memory bridge; no public-network/two-device claim"
    });
    let summary_path = base.join(format!("dl2-030-large-summary-{}.json", std::process::id()));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&summary_path)?;
    serde_json::to_writer_pretty(&mut output, &summary)?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    println!("{}", summary);
    println!("summaryPath={}", summary_path.display());
    Ok(())
}
