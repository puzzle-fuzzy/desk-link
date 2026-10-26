//! Real binary entry points. Unix signal delivery runs only on Unix; Windows
//! CLI checks do not substitute for SIGTERM or an interactive Ctrl+C test.

use std::{
    io::{BufRead, BufReader, Read},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::Duration,
};

struct CapturedChild {
    child: Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
}

fn collect_output(
    reader: impl Read + Send + 'static,
    output: Arc<Mutex<String>>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            let mut output = output.lock().unwrap();
            // Continue draining the pipe after the diagnostic cap so an
            // unexpected logger burst cannot block the child on a full pipe.
            if output.len() + line.len() < 128 * 1024 {
                output.push_str(&line);
                output.push('\n');
            }
        }
    })
}

impl CapturedChild {
    fn spawn(arguments: &[&str], address: &str) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_desklink-relay"));
        command
            .args(arguments)
            .env("DESKLINK_RELAY_ADDR", address)
            .env("DESKLINK_RELAY_MAX_CONNECTIONS", "64")
            .env("DESKLINK_RELAY_MAX_SESSIONS", "64")
            .env("DESKLINK_RELAY_BUFFER_BUDGET_MIB", "160")
            .env("DESKLINK_RELAY_SESSION_TTL_S", "86400")
            .env("DESKLINK_RELAY_JOIN_TIMEOUT_S", "8")
            .env("DESKLINK_RELAY_PARTIAL_FRAME_TIMEOUT_S", "30")
            .env_remove("DESKLINK_RELAY_CERT_PEM")
            .env_remove("DESKLINK_RELAY_KEY_PEM")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = command.spawn().unwrap();
        let stdout = Arc::new(Mutex::new(String::new()));
        let stderr = Arc::new(Mutex::new(String::new()));
        let readers = vec![
            collect_output(child.stdout.take().unwrap(), stdout.clone()),
            collect_output(child.stderr.take().unwrap(), stderr.clone()),
        ];
        Self {
            child,
            stdout,
            stderr,
            readers,
        }
    }

    async fn wait(&mut self, budget: Duration) -> ExitStatus {
        tokio::time::timeout(budget, async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "child exceeded its exit budget; stderr: {}",
                self.stderr.lock().unwrap()
            )
        })
    }

    fn finish_readers(&mut self) {
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
    }
}

impl Drop for CapturedChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            // This is exclusively the child spawned by this fixture.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

#[tokio::test]
async fn cli_checks_finish_without_binding_or_entering_the_signal_loop() {
    let occupied = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = occupied.local_addr().unwrap().to_string();
    for argument in ["--check-config", "--resource-observation"] {
        let mut process = CapturedChild::spawn(&[argument], &address);
        let status = process.wait(Duration::from_secs(5)).await;
        process.finish_readers();
        let stderr = process.stderr.lock().unwrap();
        assert!(status.success(), "CLI failed: {stderr}");
        assert!(!stderr.contains("listening on"));
        assert!(!stderr.contains("relay_shutdown"));
        if argument == "--check-config" {
            assert!(stderr.contains("configuration is valid"));
        } else {
            assert!(
                process
                    .stdout
                    .lock()
                    .unwrap()
                    .contains("process_resident_bytes")
            );
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn sigterm_closes_a_normal_quic_peer_and_exits_the_real_binary() {
    use desklink_crypto::SessionId;
    use desklink_transport::{QuicClientConfig, RelayJoin};

    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut process = CapturedChild::spawn(&[], "127.0.0.1:0");
    let address: std::net::SocketAddr = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(address) = process
                .stderr
                .lock()
                .unwrap()
                .lines()
                .find_map(|line| line.strip_prefix("DeskLink relay listening on "))
                .and_then(|address| address.parse().ok())
            {
                return address;
            }
            assert!(process.child.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let config = QuicClientConfig::new_lan(address, "localhost").unwrap();
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(config.client_config);
    let connection = tokio::time::timeout(
        Duration::from_secs(5),
        endpoint.connect(address, "localhost").unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    let (mut send, mut receive) = connection.open_bi().await.unwrap();
    let join = RelayJoin::host_with_participant(SessionId::from_bytes([57; 16]), [57; 32], [1; 16]);
    let envelope = join.encode();
    send.write_all(&(envelope.len() as u32).to_be_bytes())
        .await
        .unwrap();
    send.write_all(&envelope).await.unwrap();
    send.finish().unwrap();
    let mut acknowledgment = [0; 1];
    tokio::time::timeout(
        Duration::from_secs(5),
        receive.read_exact(&mut acknowledgment),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(acknowledgment, [0]);

    unsafe extern "C" {
        fn kill(pid: std::ffi::c_int, signal: std::ffi::c_int) -> std::ffi::c_int;
    }
    let child_pid = i32::try_from(process.child.id()).unwrap();
    assert!(child_pid > 0);
    // SAFETY: target is this fixture's live child PID, never a process group;
    // SIGTERM's platform value comes from Tokio rather than a guessed integer.
    assert_eq!(
        unsafe {
            kill(
                child_pid,
                tokio::signal::unix::SignalKind::terminate().as_raw_value(),
            )
        },
        0
    );
    let reason = tokio::time::timeout(Duration::from_secs(3), connection.closed())
        .await
        .expect("SIGTERM did not promptly notify the peer");
    let quinn::ConnectionError::ApplicationClosed(close) = reason else {
        panic!("expected CONNECTION_CLOSE rather than idle timeout: {reason:?}");
    };
    assert_eq!(close.reason.as_ref(), b"relay shutdown");
    let status = process.wait(Duration::from_secs(10)).await;
    process.finish_readers();
    let stderr = process.stderr.lock().unwrap();
    assert!(status.success(), "signal shutdown failed: {stderr}");
    assert!(stderr.contains("relay_stop_requested signal=sigterm"));
    assert!(stderr.contains("relay_shutdown outcome=graceful accepted_connections=0 attached_participants=0 media_workers=0 reliable_workers=0 rejection_tasks=0"));
}
