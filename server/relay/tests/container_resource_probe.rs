//! Explicit local Docker/cgroup fixture, never a production relay probe.
//! The image revision must match a caller-supplied committed source SHA.
//! Measurements include the short-lived observer process in the same cgroup;
//! its process RSS is not the long-running relay process's RSS.

use std::{
    net::SocketAddr,
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use desklink_crypto::SessionId;
use desklink_transport::{
    ChannelKind, MAX_RELIABLE_MESSAGE_BYTES, QuicClient, QuicClientConfig, RelayJoin,
};
use quinn::Endpoint;
use rustls::pki_types::CertificateDer;

const OPERATION_TIMEOUT: Duration = Duration::from_secs(8);
const CONTAINER_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
const OBSERVER: &str = "/usr/local/bin/desklink-relay";

fn docker(arguments: &[&str]) -> String {
    let result = Command::new("docker")
        .args(arguments)
        .output()
        .expect("run local Docker CLI");
    assert!(
        result.status.success(),
        "Docker operation failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout)
        .expect("Docker stdout must be UTF-8")
        .trim()
        .to_owned()
}

fn docker_logs(container_id: &str) -> String {
    let result = Command::new("docker")
        .args(["logs", container_id])
        .output()
        .expect("read owned container logs");
    assert!(result.status.success(), "read task container logs");
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    )
}

#[derive(Clone, Copy, Debug)]
struct Observation {
    observer_resident_bytes: u64,
    current_bytes: u64,
    peak_bytes: u64,
    limit_bytes: u64,
    oom_kills: u64,
}

fn numeric_field(json: &str, key: &str) -> Option<u64> {
    // The observer's schema is a flat object of fixed numeric/null fields.
    // This test-only reader rejects missing fields and nonnumeric values.
    assert!(json.starts_with('{') && json.ends_with('}'));
    for member in json[1..json.len() - 1].split(',') {
        let (name, value) = member
            .split_once(':')
            .expect("flat observation JSON member");
        if name.trim().trim_matches('"') == key {
            let value = value.trim();
            return if value == "null" {
                None
            } else {
                Some(value.parse().expect("numeric observation field"))
            };
        }
    }
    panic!("missing observation field {key}");
}

struct DockerProbe {
    name: String,
    container_id: Option<String>,
    directory: PathBuf,
    revision: String,
    image_id: String,
    address: SocketAddr,
    client_config: quinn::ClientConfig,
}

impl Drop for DockerProbe {
    fn drop(&mut self) {
        // Only the immutable ID returned by this task's successful create.
        // A failed run/name collision must never delete a pre-existing
        // container, image, network, volume or directory.
        if let Some(container_id) = &self.container_id {
            match Command::new("docker")
                .args(["rm", "--force", container_id])
                .output()
            {
                Ok(result) if result.status.success() => {}
                result => eprintln!(
                    "task container cleanup failed for {} ({container_id}): {result:?}",
                    self.name
                ),
            }
        }
        let _ = std::fs::remove_file(self.directory.join("cert.pem"));
        let _ = std::fs::remove_file(self.directory.join("key.pem"));
        let _ = std::fs::remove_dir(&self.directory);
    }
}

impl DockerProbe {
    fn new() -> Self {
        let image = std::env::var("DESKLINK_RESOURCE_PROBE_IMAGE")
            .expect("set the exact local source image");
        let revision = std::env::var("DESKLINK_RESOURCE_PROBE_REVISION")
            .expect("set the committed image source SHA");
        assert!(revision.len() == 40 && revision.bytes().all(|b| b.is_ascii_hexdigit()));
        let image_id = docker(&["image", "inspect", "--format", "{{.Id}}", &image]);
        assert!(image_id.starts_with("sha256:"));
        let label = docker(&[
            "image",
            "inspect",
            "--format",
            "{{index .Config.Labels \"org.opencontainers.image.revision\"}}",
            &image_id,
        ]);
        assert_eq!(label, revision, "refuse a stale or unproven relay image");
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("desklink-dl052-probe-{}-{nonce}", std::process::id());
        let directory = std::env::temp_dir().join(&name);
        std::fs::create_dir(&directory).expect("create unique temporary TLS directory");
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(certificate.cert.der().to_vec()))
            .unwrap();
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
        let mut client_config = quinn::ClientConfig::new(Arc::new(crypto));
        let mut transport = quinn::TransportConfig::default();
        transport
            .stream_receive_window(65_549_u32.into())
            .receive_window(131_072_u32.into())
            .send_window(131_072);
        client_config.transport_config(Arc::new(transport));
        let mut probe = Self {
            name,
            container_id: None,
            directory,
            revision,
            image_id,
            address: "127.0.0.1:0".parse().unwrap(),
            client_config,
        };
        std::fs::write(probe.directory.join("cert.pem"), certificate.cert.pem()).unwrap();
        std::fs::write(
            probe.directory.join("key.pem"),
            certificate.signing_key.serialize_pem(),
        )
        .unwrap();
        let mount = format!(
            "type=bind,source={},target=/run/probe-tls,readonly",
            probe.directory.display()
        );
        let container_id = docker(&[
            "run",
            "--detach",
            "--pull=never",
            "--name",
            &probe.name,
            "--publish",
            "127.0.0.1::4433/udp",
            "--read-only",
            "--memory=256m",
            "--memory-swap=256m",
            "--cpus=0.75",
            "--pids-limit=64",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges:true",
            "--no-healthcheck",
            "--log-driver=json-file",
            "--log-opt=max-size=1m",
            "--log-opt=max-file=1",
            "--mount",
            &mount,
            "--env",
            "DESKLINK_RELAY_ADDR=0.0.0.0:4433",
            "--env",
            "DESKLINK_RELAY_CERT_PEM=/run/probe-tls/cert.pem",
            "--env",
            "DESKLINK_RELAY_KEY_PEM=/run/probe-tls/key.pem",
            "--env",
            "DESKLINK_RELAY_MAX_CONNECTIONS=64",
            "--env",
            "DESKLINK_RELAY_MAX_SESSIONS=64",
            "--env",
            "DESKLINK_RELAY_BUFFER_BUDGET_MIB=160",
            &probe.image_id,
        ]);
        assert!(container_id.len() == 64 && container_id.bytes().all(|b| b.is_ascii_hexdigit()));
        probe.container_id = Some(container_id);
        let published = docker(&["port", probe.id(), "4433/udp"]);
        probe.address = published.parse().expect("one loopback UDP mapping");
        assert!(probe.address.ip().is_loopback() && probe.address.port() != 0);
        probe
    }

    fn config(&self) -> QuicClientConfig {
        QuicClientConfig::with_client_config(
            self.address,
            "localhost".to_owned(),
            self.client_config.clone(),
        )
    }

    fn id(&self) -> &str {
        self.container_id
            .as_deref()
            .expect("owned container was created")
    }

    fn observe(&self, phase: &str) -> Observation {
        let output = docker(&["exec", self.id(), OBSERVER, "--resource-observation"]);
        println!("DL052_OBSERVATION phase={phase} {output}");
        assert_eq!(numeric_field(&output, "schema"), Some(1));
        Observation {
            observer_resident_bytes: numeric_field(&output, "process_resident_bytes")
                .expect("observer RSS is available"),
            current_bytes: numeric_field(&output, "cgroup_current_bytes")
                .expect("cgroup v2 current is available"),
            peak_bytes: numeric_field(&output, "cgroup_peak_bytes")
                .expect("cgroup v2 peak is available"),
            limit_bytes: numeric_field(&output, "cgroup_limit_bytes")
                .expect("finite cgroup memory limit"),
            oom_kills: numeric_field(&output, "cgroup_oom_kills")
                .expect("cgroup OOM events are available"),
        }
    }

    async fn raw(&self, join: Option<RelayJoin>) -> RawPeer {
        let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(self.client_config.clone());
        let connection = tokio::time::timeout(
            OPERATION_TIMEOUT,
            endpoint.connect(self.address, "localhost").unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        if let Some(join) = join {
            tokio::time::timeout(OPERATION_TIMEOUT, async {
                let (mut send, mut receive) = connection.open_bi().await.unwrap();
                let envelope = join.encode();
                send.write_all(&(envelope.len() as u32).to_be_bytes())
                    .await
                    .unwrap();
                send.write_all(&envelope).await.unwrap();
                send.finish().unwrap();
                let mut acknowledgment = [0; 1];
                receive.read_exact(&mut acknowledgment).await.unwrap();
                assert_eq!(acknowledgment, [0]);
            })
            .await
            .unwrap();
        }
        RawPeer {
            endpoint,
            connection,
        }
    }

    async fn verify_prejoin_datagram_rejection(&self) {
        for partial_join in [false, true] {
            for payload in [Vec::new(), vec![0xa5]] {
                let peer = self.raw(None).await;
                let _incomplete_join = if partial_join {
                    let (mut send, receive) = peer.connection.open_bi().await.unwrap();
                    send.write_all(&[0]).await.unwrap();
                    Some((send, receive))
                } else {
                    None
                };
                peer.connection.send_datagram(payload.into()).unwrap();
                let reason = tokio::time::timeout(Duration::from_secs(1), peer.connection.closed())
                    .await.expect("candidate must reject even an empty pre-join datagram before its eight-second join deadline");
                assert!(matches!(
                    reason,
                    quinn::ConnectionError::ApplicationClosed(_)
                ));
                peer.endpoint
                    .close(0_u32.into(), b"pre-join datagram phase complete");
                tokio::time::timeout(OPERATION_TIMEOUT, peer.endpoint.wait_idle())
                    .await
                    .unwrap();
            }
        }
        println!(
            "DL052_PREJOIN_DATAGRAM_REJECTION cases=4 no_join=true partial_join=true empty=true nonempty=true application_close_deadline_ms=1000"
        );
    }
}

struct RawPeer {
    endpoint: Endpoint,
    connection: quinn::Connection,
}

impl Drop for RawPeer {
    fn drop(&mut self) {
        self.connection
            .close(0_u32.into(), b"container fixture cleanup");
        self.endpoint
            .close(0_u32.into(), b"container fixture cleanup");
    }
}

fn join(id: u8, host: bool) -> RelayJoin {
    let session = SessionId::from_bytes([id; 16]);
    if host {
        RelayJoin::host_with_participant(session, [id; 32], [1; 16])
    } else {
        RelayJoin::controller_with_participant(session, [id; 32], [2; 16])
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit local Docker probe requires a source-revision-labelled image and cgroup v2"]
async fn local_container_under_declared_connection_and_stream_pressure() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let probe = DockerProbe::new();
    probe.verify_prejoin_datagram_rejection().await;
    let healthy_host = tokio::time::timeout(OPERATION_TIMEOUT, QuicClient::connect(probe.config()))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(OPERATION_TIMEOUT, healthy_host.join(join(80, true)))
        .await
        .unwrap()
        .unwrap();
    let healthy_controller =
        tokio::time::timeout(OPERATION_TIMEOUT, QuicClient::connect(probe.config()))
            .await
            .unwrap()
            .unwrap();
    tokio::time::timeout(OPERATION_TIMEOUT, healthy_controller.join(join(80, false)))
        .await
        .unwrap()
        .unwrap();
    let baseline = probe.observe("baseline");
    assert_eq!(baseline.limit_bytes, CONTAINER_MEMORY_BYTES);
    assert_eq!(baseline.oom_kills, 0);

    // First verify the pre-join credit limits independently. Retire these
    // connections before sustained pressure so a correct join deadline does
    // not silently reduce the live connection count during the measurement.
    let mut peers = Vec::new();
    for _ in 0..2 {
        let peer = probe.raw(None).await;
        let mut retained_streams = Vec::new();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), peer.connection.open_uni())
                .await
                .is_err()
        );
        for _ in 0..17 {
            let (mut send, receive) = peer.connection.open_bi().await.unwrap();
            send.write_all(&[0]).await.unwrap();
            retained_streams.push((send, receive));
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), peer.connection.open_bi())
                .await
                .is_err()
        );
        peer.connection
            .close(0_u32.into(), b"credit phase complete");
        peer.endpoint.close(0_u32.into(), b"credit phase complete");
        tokio::time::timeout(OPERATION_TIMEOUT, peer.endpoint.wait_idle())
            .await
            .unwrap();
        drop(retained_streams);
    }
    // 2 healthy + 2 joined idle Hosts + 30 stopped-reader pairs = 64.
    peers.push(probe.raw(Some(join(81, true))).await);
    peers.push(probe.raw(Some(join(82, true))).await);
    let completed_frames = Arc::new(AtomicUsize::new(0));
    let started_writers = Arc::new(AtomicUsize::new(0));
    let writers_with_a_complete_frame = Arc::new(AtomicUsize::new(0));
    let mut source_progress = Vec::new();
    let mut floods = tokio::task::JoinSet::new();
    for id in 100..130 {
        let host = probe.raw(Some(join(id, true))).await;
        let controller = probe.raw(Some(join(id, false))).await;
        let source_frames = Arc::new(AtomicUsize::new(0));
        source_progress.push(source_frames.clone());
        let mut prepared = Vec::new();
        for _ in 0..16 {
            // Reserve every stream's small header before any body can fill
            // this source's bounded connection send window. Otherwise the
            // first body can correctly backpressure later header writers.
            let (mut send, receive) = controller.connection.open_bi().await.unwrap();
            send.write_all(&[ChannelKind::Transfer as u8])
                .await
                .unwrap();
            send.write_all(&0_u64.to_be_bytes()).await.unwrap();
            send.write_all(&(MAX_RELIABLE_MESSAGE_BYTES as u32).to_be_bytes())
                .await
                .unwrap();
            prepared.push((send, receive));
        }
        for (mut send, _receive) in prepared {
            let completed = completed_frames.clone();
            let started = started_writers.clone();
            let first_frame = writers_with_a_complete_frame.clone();
            let source_completed = source_frames.clone();
            floods.spawn(async move {
                started.fetch_add(1, Ordering::Release);
                let payload = vec![0xa5; MAX_RELIABLE_MESSAGE_BYTES];
                for frame_index in 0..8 {
                    if frame_index != 0 {
                        send.write_all(&(payload.len() as u32).to_be_bytes())
                            .await
                            .unwrap();
                    }
                    send.write_all(&payload).await.unwrap();
                    completed.fetch_add(1, Ordering::Release);
                    source_completed.fetch_add(1, Ordering::Release);
                    if frame_index == 0 {
                        first_frame.fetch_add(1, Ordering::Release);
                    }
                }
                send.finish().unwrap();
            });
        }
        peers.extend([host, controller]);
    }
    assert_eq!(peers.len() + 2, 64);
    tokio::time::timeout(OPERATION_TIMEOUT, async {
        while completed_frames.load(Ordering::Acquire) < 32
            || started_writers.load(Ordering::Acquire) != 480
            || source_progress
                .iter()
                .any(|progress| progress.load(Ordering::Acquire) == 0)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("pressure must include real complete reliable frames");

    for _ in 0..5 {
        tokio::time::timeout(OPERATION_TIMEOUT, async {
            healthy_controller
                .send_input(b"container probe input".to_vec())
                .await
                .unwrap();
            assert_eq!(
                healthy_host.next_input().await.unwrap(),
                b"container probe input"
            );
        })
        .await
        .expect("healthy session must remain serviceable under local pressure");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let mut finished_writers = 0;
    while let Some(result) = floods.try_join_next() {
        result.expect("pressure writer must not fail before fixture cleanup");
        finished_writers += 1;
    }
    assert!(
        finished_writers < 480,
        "stopped readers must backpressure bounded writers"
    );
    assert!(completed_frames.load(Ordering::Acquire) < 480 * 8);
    assert!(
        peers
            .iter()
            .all(|peer| peer.connection.close_reason().is_none())
    );
    let pressure = probe.observe("pressure");
    assert_eq!(pressure.limit_bytes, CONTAINER_MEMORY_BYTES);
    assert_eq!(pressure.oom_kills, 0);
    assert!(pressure.current_bytes <= CONTAINER_MEMORY_BYTES);
    assert!(pressure.peak_bytes <= CONTAINER_MEMORY_BYTES);
    assert_eq!(
        docker(&[
            "inspect",
            "--format",
            "{{.State.Running}} {{.State.OOMKilled}}",
            probe.id()
        ]),
        "true false"
    );

    floods.abort_all();
    while floods.join_next().await.is_some() {}
    drop(peers);
    healthy_host.close(b"container fixture cleanup");
    healthy_controller.close(b"container fixture cleanup");
    tokio::time::sleep(Duration::from_millis(250)).await;
    let after = probe.observe("after_cleanup");
    assert_eq!(after.oom_kills, 0);
    println!(
        "DL052_CONTAINER_PROBE source={} image={} workload_connections=64 scheduled_writers=480 started_writers={} writers_with_complete_frame={} sources_with_complete_frame=30 pending_writers={} completed_frames={} baseline_cgroup_bytes={} pressure_cgroup_bytes={} after_cgroup_bytes={} peak_cgroup_bytes={} limit_cgroup_bytes={} observer_rss_bytes={} oom_kills={}",
        probe.revision,
        probe.image_id,
        started_writers.load(Ordering::Acquire),
        writers_with_a_complete_frame.load(Ordering::Acquire),
        480 - finished_writers,
        completed_frames.load(Ordering::Acquire),
        baseline.current_bytes,
        pressure.current_bytes,
        after.current_bytes,
        after.peak_bytes,
        after.limit_bytes,
        after.observer_resident_bytes,
        after.oom_kills,
    );

    // This must be the same candidate/cgroup just measured, with a live
    // joined peer that can distinguish application close from process death.
    // Older images lacking either pre-join rejection or Unix signal handling
    // cannot satisfy this fixture's earlier gate and final assertions.
    let shutdown_peer = probe.raw(Some(join(90, true))).await;
    let stop_output = docker(&["stop", "--time", "10", probe.id()]);
    // Docker may include a --time deprecation notice on stdout. The CLI
    // operation must succeed and identify our exact owned container last.
    assert_eq!(stop_output.lines().last(), Some(probe.id()));
    println!("DL057_DOCKER_STOP_OUTPUT {stop_output}");
    let reason = tokio::time::timeout(OPERATION_TIMEOUT, shutdown_peer.connection.closed())
        .await
        .expect("joined peer must observe relay shutdown application close");
    let quinn::ConnectionError::ApplicationClosed(closed) = reason else {
        panic!("SIGTERM must produce an application close, got {reason:?}");
    };
    assert_eq!(closed.reason.as_ref(), b"relay shutdown");
    assert_eq!(
        docker(&[
            "inspect",
            "--format",
            "{{.State.ExitCode}} {{.State.OOMKilled}} {{.State.Running}}",
            probe.id()
        ]),
        "0 false false"
    );
    let logs = docker_logs(probe.id());
    let requested = logs
        .lines()
        .find(|line| *line == "relay_stop_requested signal=sigterm")
        .expect("real container stop must reach the Unix SIGTERM handler");
    let summary = logs
        .lines()
        .find(|line| line.starts_with("relay_shutdown outcome=graceful "))
        .expect("same candidate must log a successful graceful shutdown");
    for expected in [
        "accepted_connections=0",
        "attached_participants=0",
        "media_workers=0",
        "reliable_workers=0",
        "rejection_tasks=0",
        "forced_tasks=0",
        "endpoint_idle_confirmed=true",
    ] {
        assert!(
            summary.split_whitespace().any(|field| field == expected),
            "missing shutdown evidence {expected}: {summary}"
        );
    }
    println!(
        "DL057_DOCKER_SIGTERM source={} image={} exit_code=0 oom_killed=false peer_application_close=relay_shutdown\n{requested}\n{summary}",
        probe.revision, probe.image_id
    );
}
