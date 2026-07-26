use std::{ffi::CString, ptr::null_mut, sync::Arc, time::Duration};

use desklink_crypto::{DeviceIdentity, NoiseResponder, SecureLane, SecureRole, SecureSession};
use desklink_ffi::{
    ControllerRuntime, DesklinkConfig, DesklinkPlatform, DesklinkResult,
    desklink_create_for_platform, desklink_destroy,
};
use desklink_protocol::{
    Codec, ControlMessage, DeviceCapabilities, DeviceRole, H264Profile, NoiseHandshake,
    NoiseHandshakeStep, PROTOCOL_VERSION, Platform, decode_control, decode_noise_handshake,
    encode_control, encode_noise_handshake,
};
use desklink_relay::{RelayConfig, RelayServer};
use desklink_transport::{QuicClient, QuicClientConfig, RelayJoin};
use quinn::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::oneshot;

struct TestRelay {
    address: std::net::SocketAddr,
    client_config: quinn::ClientConfig,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ios_controller_declares_ios_platform_to_the_host() {
    let relay = spawn_test_relay().await;
    let client_config = || {
        QuicClientConfig::with_client_config(
            relay.address,
            "localhost",
            relay.client_config.clone(),
        )
    };
    let host = QuicClient::connect(client_config()).await.unwrap();
    let session_id = desklink_crypto::SessionId::from_bytes([71; 16]);
    let authentication = [72; 32];
    host.join(RelayJoin::host_with_participant(
        session_id,
        authentication,
        [1; 16],
    ))
    .await
    .unwrap();

    let host_identity = DeviceIdentity::from_secret_key([73; 16], &[74; 32]);
    let host_verify_key = *host_identity.verify_key().as_bytes();
    let controller_identity = DeviceIdentity::from_secret_key([75; 16], &[76; 32]);
    let controller_verify_key = controller_identity.verify_key().to_owned();
    let (verified_sender, verified_receiver) = oneshot::channel();
    let (release_host_sender, release_host_receiver) = oneshot::channel();
    let host_task = tokio::spawn(run_fake_host(
        host,
        host_identity,
        controller_verify_key,
        verified_sender,
        release_host_receiver,
    ));

    let relay_url = CString::new(format!("quic://{}", relay.address)).unwrap();
    let mut handle = null_mut();
    let config = DesklinkConfig {
        relay_url: relay_url.as_ptr(),
        log_level: 1,
    };
    assert_eq!(
        unsafe {
            desklink_create_for_platform(
                &config,
                DesklinkPlatform::IOS as u32,
                None,
                null_mut(),
                &mut handle,
            )
        },
        DesklinkResult::Ok
    );

    let controller = QuicClient::connect(client_config()).await.unwrap();
    controller
        .join(RelayJoin::controller_with_participant(
            session_id,
            authentication,
            [75; 16],
        ))
        .await
        .unwrap();
    let runtime = ControllerRuntime::connect_for_platform(
        controller,
        controller_identity,
        ed25519_dalek::VerifyingKey::from_bytes(&host_verify_key).unwrap(),
        Platform::IOS,
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), verified_receiver)
        .await
        .unwrap()
        .unwrap();
    release_host_sender.send(()).unwrap();
    drop(runtime);

    unsafe { desklink_destroy(handle) };
    tokio::time::timeout(Duration::from_secs(3), host_task)
        .await
        .unwrap()
        .unwrap();
}

async fn run_fake_host(
    host: QuicClient,
    identity: DeviceIdentity,
    expected_controller: ed25519_dalek::VerifyingKey,
    verified_sender: oneshot::Sender<()>,
    keep_alive_receiver: oneshot::Receiver<()>,
) {
    let first = decode_noise_handshake(&host.next_control().await.unwrap()).unwrap();
    assert_eq!(first.step, NoiseHandshakeStep::InitiatorHello);
    let (mut responder, response) =
        NoiseResponder::accept(&first.payload, identity, expected_controller).unwrap();
    host.send_control(
        encode_noise_handshake(&NoiseHandshake {
            protocol_version: PROTOCOL_VERSION,
            step: NoiseHandshakeStep::ResponderHello,
            payload: response,
        })
        .unwrap(),
    )
    .await
    .unwrap();
    let finish = decode_noise_handshake(&host.next_control().await.unwrap()).unwrap();
    assert_eq!(finish.step, NoiseHandshakeStep::InitiatorFinish);
    responder.receive(&finish.payload).unwrap();
    let mut secure = responder
        .finish()
        .unwrap()
        .into_secure_session(SecureRole::Responder);

    assert_eq!(
        open_control(&mut secure, host.next_control().await.unwrap()),
        ControlMessage::Hello {
            platform: Platform::IOS,
            role: DeviceRole::Controller,
        }
    );
    assert_eq!(
        open_control(&mut secure, host.next_control().await.unwrap()),
        ControlMessage::Capabilities(DeviceCapabilities {
            platform: Platform::IOS,
            role: DeviceRole::Controller,
            codecs: vec![Codec::H264],
            h264_profiles: vec![H264Profile::Main],
            width: 1920,
            height: 1080,
        })
    );

    send_control(
        &host,
        &mut secure,
        ControlMessage::Hello {
            platform: Platform::MacOS,
            role: DeviceRole::Host,
        },
    )
    .await;
    send_control(
        &host,
        &mut secure,
        ControlMessage::Capabilities(DeviceCapabilities {
            platform: Platform::MacOS,
            role: DeviceRole::Host,
            codecs: vec![Codec::H264],
            h264_profiles: vec![H264Profile::Main],
            width: 1920,
            height: 1080,
        }),
    )
    .await;
    verified_sender.send(()).unwrap();
    keep_alive_receiver.await.unwrap();
}

fn open_control(secure: &mut SecureSession, ciphertext: Vec<u8>) -> ControlMessage {
    let plaintext = secure.open(SecureLane::Control, &ciphertext).unwrap();
    decode_control(&plaintext).unwrap()
}

async fn send_control(host: &QuicClient, secure: &mut SecureSession, message: ControlMessage) {
    let plaintext = encode_control(&message).unwrap();
    host.send_control(secure.seal(SecureLane::Control, &plaintext).unwrap())
        .await
        .unwrap();
}

async fn spawn_test_relay() -> TestRelay {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let certificate_der = certificate.cert.der().to_vec();
    let key_der = certificate.key_pair.serialize_der();
    let server_config = ServerConfig::with_single_cert(
        vec![CertificateDer::from(certificate_der.clone())],
        PrivateKeyDer::Pkcs8(key_der.into()),
    )
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(certificate_der)).unwrap();
    let client_tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let client_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(client_tls).unwrap();
    let client_config = quinn::ClientConfig::new(Arc::new(client_crypto));
    let relay = Arc::new(
        RelayServer::bind(
            "127.0.0.1:0".parse().unwrap(),
            server_config,
            RelayConfig::default(),
        )
        .await
        .unwrap(),
    );
    let address = relay.local_addr().unwrap();
    let task_relay = relay.clone();
    let task = tokio::spawn(async move {
        let _ = task_relay.run().await;
    });
    TestRelay {
        address,
        client_config,
        task,
    }
}
