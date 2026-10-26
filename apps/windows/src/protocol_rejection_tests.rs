use super::*;
use desklink_crypto::SessionId;
use desklink_ffi::ControllerRuntime;
use desklink_relay::{RelayConfig, RelayServer};
use desklink_transport::JoinRejectCode;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

struct TestRelay {
    config: QuicClientConfig,
    server: Arc<RelayServer>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestRelay {
    fn drop(&mut self) {
        self.server.close();
        self.task.abort();
    }
}

async fn relay() -> TestRelay {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let der = certificate.cert.der().to_vec();
    let server_tls = quinn::ServerConfig::with_single_cert(
        vec![CertificateDer::from(der.clone())],
        PrivateKeyDer::Pkcs8(certificate.key_pair.serialize_der().into()),
    )
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(der)).unwrap();
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let client_tls = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
    let server = Arc::new(
        RelayServer::bind(
            "127.0.0.1:0".parse().unwrap(),
            server_tls,
            RelayConfig::default(),
        )
        .await
        .unwrap(),
    );
    let config = QuicClientConfig::with_client_config(
        server.local_addr().unwrap(),
        "localhost",
        quinn::ClientConfig::new(Arc::new(client_tls)),
    );
    let worker = server.clone();
    let task = tokio::spawn(async move {
        worker.run().await.unwrap();
    });
    TestRelay {
        config,
        server,
        task,
    }
}

async fn joined_host_and_controller(relay: &TestRelay, session: u8) -> (QuicClient, QuicClient) {
    let host = QuicClient::connect(relay.config.clone()).await.unwrap();
    host.join(RelayJoin::host_with_participant(
        SessionId::from_bytes([session; 16]),
        [31; 32],
        [32; 16],
    ))
    .await
    .unwrap();
    let controller = joined_controller(relay, session, 33).await;
    (host, controller)
}

async fn joined_controller(relay: &TestRelay, session: u8, participant: u8) -> QuicClient {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let controller = QuicClient::connect(relay.config.clone()).await.unwrap();
            match controller
                .join(RelayJoin::controller_with_participant(
                    SessionId::from_bytes([session; 16]),
                    [31; 32],
                    [participant; 16],
                ))
                .await
            {
                Ok(()) => return controller,
                Err(TransportError::JoinRejected(JoinRejectCode::SessionOccupied)) => {
                    controller.close(b"test waits for old controller departure");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("join failed: {error:?}"),
            }
        }
    })
    .await
    .unwrap()
}

/// The common wire layout is decoded with the current codec, then the exact
/// protocol-12/13 version guard is applied. This is an explicitly synthetic legacy
/// peer, not execution of an installed old Windows application.
fn legacy_version_guard(bytes: &[u8], expected_version: u16) -> Result<(), ProtocolError> {
    let response = decode_noise_handshake(bytes)?;
    if response.protocol_version != expected_version {
        return Err(ProtocolError::UnsupportedVersion(response.protocol_version));
    }
    Ok(())
}

struct ExpectedController(VerifyingKey);

impl ControllerAuthorizer for ExpectedController {
    fn pinned_verify_key(&self) -> Option<VerifyingKey> {
        Some(self.0)
    }
    fn authorize(&self, identity: PeerIdentity) -> Result<ControllerAuthorization, String> {
        Ok(if identity.verify_key() == self.0 {
            ControllerAuthorization::Authorized
        } else {
            ControllerAuthorization::Unknown
        })
    }
}

/// Real relay + production host admission, followed by actual current Noise and
/// capability negotiation. No capture, audio, clipboard, or input injection runs.
#[tokio::test]
async fn old_noise_gets_a_version_reply_and_same_host_accepts_a_current_authenticated_peer() {
    for rejected_version in [12, 13] {
        reject_old_noise_and_admit_current_peer(rejected_version).await;
    }
}

async fn reject_old_noise_and_admit_current_peer(rejected_version: u8) {
    let relay = relay().await;
    let (host, legacy) = joined_host_and_controller(&relay, 71).await;
    let mut old_hello = encode_noise_handshake(&NoiseHandshake {
        protocol_version: PROTOCOL_VERSION,
        step: NoiseHandshakeStep::InitiatorHello,
        payload: vec![1],
    })
    .unwrap();
    assert_eq!(old_hello[0], PROTOCOL_VERSION as u8);
    old_hello[0] = rejected_version;
    legacy.send_control(old_hello).await.unwrap();
    let reject_and_rearm = async {
        let result = next_initiator_hello(&host).await;
        // The production supervisor rearms immediately after a peer error;
        // flushing its reliable lanes must not erase the version response.
        host.reset_reliable_channels().await;
        result
    };
    let (host_result, response) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(2), reject_and_rearm),
        tokio::time::timeout(Duration::from_secs(2), legacy.next_control()),
    );
    let error = host_result.unwrap().unwrap_err();
    assert!(matches!(
        &error,
        HostRuntimeError::Protocol(ProtocolError::UnsupportedVersion(version))
            if *version == u16::from(rejected_version)
    ));
    assert!(host_error_rearms_on_connected_relay(&error));
    let response = response.unwrap().unwrap();
    let reply = decode_noise_handshake(&response).unwrap();
    assert_eq!(reply.protocol_version, PROTOCOL_VERSION);
    assert_eq!(reply.step, NoiseHandshakeStep::ResponderHello);
    assert_eq!(reply.payload, [0]);
    assert_eq!(
        legacy_version_guard(&response, u16::from(rejected_version)),
        Err(ProtocolError::UnsupportedVersion(PROTOCOL_VERSION))
    );
    // The synthetic legacy peer stops on this version error. The Host has
    // already rearmed, retaining its original relay connection.
    legacy.close(b"legacy controller stops after protocol mismatch");
    drop(legacy);

    let host_identity = DeviceIdentity::generate(&mut OsRng);
    let host_key = host_identity.verify_key();
    let controller_identity = DeviceIdentity::generate(&mut OsRng);
    let controller_key = controller_identity.verify_key();
    let peer = joined_controller(&relay, 71, 34).await;
    let host_admission = async {
        let (secure, generation) =
            perform_noise_handshake(&host, host_identity, &ExpectedController(controller_key))
                .await
                .unwrap();
        let secure = Arc::new(AsyncMutex::new(secure));
        receive_controller_capabilities(
            &host,
            &secure,
            generation,
            Arc::new(AtomicBool::new(false)),
            1,
        )
        .await
        .unwrap();
        send_host_capabilities(
            &host,
            &secure,
            generation,
            16,
            16,
            H264Profile::Main,
            &[RemoteDisplay {
                id: 1,
                width: 16,
                height: 16,
                primary: true,
            }],
            1,
        )
        .await
        .unwrap();
    };
    let ((), controller) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(
            host_admission,
            ControllerRuntime::connect(peer, controller_identity, host_key)
        )
    })
    .await
    .unwrap();
    let controller = controller.expect("same Host must admit a current authenticated peer");
    drop(controller);
    host.close(b"test complete");
}

#[tokio::test]
async fn malformed_and_stale_current_handshakes_keep_the_existing_admission_behavior() {
    let relay = relay().await;
    let (host, controller) = joined_host_and_controller(&relay, 72).await;
    controller.send_control(vec![0xff, 0xff]).await.unwrap();
    controller
        .send_control(
            encode_noise_handshake(&NoiseHandshake {
                protocol_version: PROTOCOL_VERSION,
                step: NoiseHandshakeStep::ResponderHello,
                payload: vec![1],
            })
            .unwrap(),
        )
        .await
        .unwrap();
    let admission = next_initiator_hello(&host);
    tokio::pin!(admission);
    tokio::select! {
        result = &mut admission => panic!("malformed/stale messages must not be classified as a version rejection: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(30)) => {}
    }
    controller
        .send_control(
            encode_noise_handshake(&NoiseHandshake {
                protocol_version: PROTOCOL_VERSION,
                step: NoiseHandshakeStep::InitiatorHello,
                payload: vec![2],
            })
            .unwrap(),
        )
        .await
        .unwrap();
    let (_, hello) = tokio::time::timeout(Duration::from_secs(1), admission)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hello.step, NoiseHandshakeStep::InitiatorHello);
    assert_eq!(hello.payload, [2]);
    // No rejection response was generated by the malformed/stale inputs.
    assert!(
        tokio::time::timeout(Duration::from_millis(30), controller.next_control())
            .await
            .is_err()
    );
    controller.close(b"test complete");
    host.close(b"test complete");
}
