//! Explicit heavy acceptance entry. This reads and encrypts every byte of a
//! real >4-GiB sparse source, writes a real destination, and rereads it in full.
//! --restart recreates both disk actors after a durable partial transfer. This
//! is an in-process restart with an encrypted in-memory bridge, not an OS
//! process restart or public-network/two-device proof.
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
use fixture::{BridgeStats, Fixture, Peers, start};
use transfer_checkpoint::{SourceIndex, source_index_bytes};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Default)]
struct Counters {
    encrypted_messages: u64,
    transferred_bytes: u64,
    acknowledgements: u64,
    maximum_ack_offset: u64,
    block_count: usize,
    last_offset: u64,
}

fn counters(stats: &BridgeStats) -> Counters {
    Counters {
        encrypted_messages: stats.encrypted_messages,
        transferred_bytes: stats.transferred_bytes,
        acknowledgements: stats.acknowledgements,
        maximum_ack_offset: stats.maximum_ack_offset,
        block_count: stats.blocks.len(),
        last_offset: stats.blocks.last().map_or(0, |entry| entry.1),
    }
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
        return Err("Explicit heavy run required: --run --fixture-root <existing absolute directory> [--restart] [--keep]".into());
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
    let restart = arguments.iter().any(|argument| argument == "--restart");
    // Carry actual blocks whose START offset exceeds u32::MAX, as well as a
    // final short block. The historical +123 run only crossed within a block.
    let size = (1_u64 << 32) + 2 * u64::from(transfer_checkpoint::FILE_BLOCK_BYTES) + 123;
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
        serde_json::json!({"event":"start","size":size.to_string(),"restartRequested":restart,"fixtureRoot":fixture.0,"availableBytes":available.to_string(),"requiredBytes":reserve.to_string(),"transport":"Noise SecureSession, local in-memory bridge"})
    );
    let started = Instant::now();
    let mut peers = Peers::new(
        &fixture,
        true,
        if restart {
            Duration::from_millis(20)
        } else {
            Duration::ZERO
        },
    )
    .await;
    let task = [0x4f; 16];
    let mut before_restart = Counters::default();
    let mut actors_stopped_before_recreation = false;
    start(&peers.a, &source, task).await;
    if restart {
        let mut last_log = Instant::now();
        tokio::time::timeout(Duration::from_secs(3600), async {
            loop {
                for progress in [&peers.a_progress, &peers.b_progress] {
                    if let Some(snapshot) = progress.borrow().as_ref()
                        && matches!(snapshot.state, "failed" | "partial" | "cancelled" | "completed")
                    {
                        return Err(format!("actor stopped before restart boundary: {snapshot:?}"));
                    }
                }
                {
                    let stats = peers.stats.lock().unwrap();
                    if stats.acknowledgements >= 2 { return Ok(()); }
                    if last_log.elapsed() >= Duration::from_secs(2) {
                        println!("{}", serde_json::json!({"event":"beforeRestartProgress","elapsedSeconds":started.elapsed().as_secs(),"actualTransferredBytes":stats.transferred_bytes.to_string(),"confirmedOffset":stats.maximum_ack_offset.to_string()}));
                        last_log = Instant::now();
                    }
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }).await.map_err(|_| "waiting for durable partial transfer timed out")??;
        let first_stats = peers.stats.clone();
        let mut source_stopped = peers.a_progress.clone();
        let mut receiver_stopped = peers.b_progress.clone();
        peers.close().await;
        // A closed progress producer proves the old worker ended and released
        // its handles; merely dropping UI/transport handles is insufficient.
        tokio::time::timeout(Duration::from_secs(15), async {
            while source_stopped.changed().await.is_ok() {}
            while receiver_stopped.changed().await.is_ok() {}
        })
        .await
        .map_err(|_| "old actors did not stop before recreation")?;
        actors_stopped_before_recreation = true;
        before_restart = counters(&first_stats.lock().unwrap());
        if before_restart.maximum_ack_offset < 2 * u64::from(transfer_checkpoint::FILE_BLOCK_BYTES)
            || before_restart.maximum_ack_offset >= size
            || fixture.received(false, task, "large-source.bin").exists()
        {
            return Err("restart did not occur at a durable unpublished partial file".into());
        }
        println!(
            "{}",
            serde_json::json!({"event":"actorsStoppedForRestart","confirmedOffset":before_restart.maximum_ack_offset.to_string(),"actualTransferredBytes":before_restart.transferred_bytes.to_string(),"oldActorProgressProducersClosed":actors_stopped_before_recreation})
        );
        peers = Peers::new(&fixture, true, Duration::ZERO).await;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match peers.a.retry(task, None) {
                    Ok(()) => return Ok(()),
                    Err(desklink_protocol::TransferResult::Busy) => {
                        tokio::time::sleep(Duration::from_millis(5)).await
                    }
                    Err(error) => return Err(format!("restart retry failed: {error:?}")),
                }
            }
        })
        .await
        .map_err(|_| "recreated sender did not accept retry")??;
    }
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
            println!("{}", serde_json::json!({"event":"progress","elapsedSeconds":started.elapsed().as_secs(),"sourceState":a.as_ref().map(|snapshot|snapshot.state),"destinationState":b.as_ref().map(|snapshot|snapshot.state),"sourceMessage":a.as_ref().and_then(|snapshot|snapshot.message.as_ref()),"encryptedMessages":before_restart.encrypted_messages+stats.encrypted_messages,"transferredBytes":(before_restart.transferred_bytes+stats.transferred_bytes).to_string(),"durableBlockAcks":before_restart.acknowledgements+stats.acknowledgements}));
        }
    }).await.map_err(|_| "heavy actor transfer timed out")??;
    let (
        after_restart,
        persisted_resume_offset,
        first_resumed_block_offset,
        confirmed_prefix_resent,
    ) = {
        let stats = peers.stats.lock().unwrap();
        let resumed = if restart {
            stats
                .resumes
                .iter()
                .find(|(id, _)| *id == task)
                .map(|entry| entry.1)
                .ok_or("no durable resume checkpoint from recreated receiver")?
        } else {
            0
        };
        let first = if restart {
            stats
                .blocks
                .iter()
                .find(|(id, _)| *id == task)
                .map(|entry| entry.1)
        } else {
            None
        };
        let prefix_resent = restart
            && stats
                .blocks
                .iter()
                .any(|(id, offset)| *id == task && *offset < resumed);
        (counters(&stats), resumed, first, prefix_resent)
    };
    let encrypted_messages = before_restart.encrypted_messages + after_restart.encrypted_messages;
    let transferred_bytes = before_restart.transferred_bytes + after_restart.transferred_bytes;
    let acknowledgements = before_restart.acknowledgements + after_restart.acknowledgements;
    let maximum_ack_offset = before_restart
        .maximum_ack_offset
        .max(after_restart.maximum_ack_offset);
    let block_count = before_restart.block_count + after_restart.block_count;
    let last_offset = after_restart.last_offset;
    let logical_blocks = size.div_ceil(u64::from(transfer_checkpoint::FILE_BLOCK_BYTES));
    if persisted_resume_offset > size {
        return Err("receiver resume offset exceeds the real source size".into());
    }
    let retransmitted_unconfirmed_bytes = transferred_bytes
        .checked_sub(size)
        .ok_or("encrypted byte count is below logical file size")?;
    if after_restart.transferred_bytes != size - persisted_resume_offset
        || maximum_ack_offset != size
        || maximum_ack_offset <= u64::from(u32::MAX)
        || last_offset <= u64::from(u32::MAX)
        || after_restart.acknowledgements
            != logical_blocks
                - persisted_resume_offset.div_ceil(u64::from(transfer_checkpoint::FILE_BLOCK_BYTES))
        || retransmitted_unconfirmed_bytes > u64::from(transfer_checkpoint::FILE_BLOCK_BYTES)
    {
        return Err(
            "heavy fixture did not carry/acknowledge every real byte beyond the 4-GiB boundary"
                .into(),
        );
    }
    if restart
        && (persisted_resume_offset < before_restart.maximum_ack_offset
            || persisted_resume_offset == 0
            || persisted_resume_offset > before_restart.transferred_bytes
            || first_resumed_block_offset != Some(persisted_resume_offset)
            || confirmed_prefix_resent
            || retransmitted_unconfirmed_bytes
                != before_restart.transferred_bytes - persisted_resume_offset)
    {
        return Err(
            "recreated actors did not resume the persisted confirmed prefix exactly".into(),
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
        "logicalCompletedBytes":size.to_string(), "logicalBlockCount":logical_blocks,
        "restartPerformed":restart, "actorsStoppedBeforeRecreation":actors_stopped_before_recreation,
        "restartBoundary":if restart { "Both file actors stopped and recreated in the same OS process; no OS-process restart claim" } else { "No restart requested" },
        "confirmedOffsetBeforeRestart":before_restart.maximum_ack_offset.to_string(),
        "persistedResumeOffset":persisted_resume_offset.to_string(),
        "firstResumedBlockOffset":first_resumed_block_offset.map(|offset|offset.to_string()),
        "confirmedPrefixResent":confirmed_prefix_resent,
        "actualTransferredBytesBeforeRestart":before_restart.transferred_bytes.to_string(),
        "actualTransferredBytesAfterRestart":after_restart.transferred_bytes.to_string(),
        "confirmedBlockAcksBeforeRestart":before_restart.acknowledgements,
        "blockAcksAfterRestart":after_restart.acknowledgements,
        "unconfirmedBytesBeforeRestart":before_restart.transferred_bytes.saturating_sub(before_restart.maximum_ack_offset).to_string(),
        "retransmittedUnconfirmedBytes":retransmitted_unconfirmed_bytes.to_string(),
        "unconfirmedRetransmissionBoundBytes":transfer_checkpoint::FILE_BLOCK_BYTES,
        "unconfirmedRetransmissionBoundVerified":retransmitted_unconfirmed_bytes<=u64::from(transfer_checkpoint::FILE_BLOCK_BYTES),
        "payloadByteCountScope":"Sum of real Block payload bytes sealed/opened across both bridge generations; excludes transport/ciphertext overhead",
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
