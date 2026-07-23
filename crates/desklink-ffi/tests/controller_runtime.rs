use std::{sync::Arc, time::Duration};

use desklink_crypto::{
    DeviceIdentity, NoiseResponder, SecureLane, SecureRole, SecureSession, SessionId,
};
use desklink_ffi::{ControllerEvent, ControllerRuntime};
use desklink_protocol::{
    AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, AudioCodec, AudioPacket, Codec, ControlMessage,
    CursorUpdate, DeviceCapabilities, DeviceRole, FrameFlags, H264Profile, InputEvent,
    NoiseHandshake, NoiseHandshakeStep, PROTOCOL_VERSION, Platform, VideoConfig, decode_control,
    decode_input, decode_noise_handshake, encode_audio_packet, encode_control,
    encode_cursor_update, encode_noise_handshake, encode_video_config, encode_video_packet,
};
use desklink_relay::{RelayConfig, RelayServer};
use desklink_transport::{QuicClient, QuicClientConfig, RelayJoin};
use desklink_video::{EncodedFrame, packetize_frame};
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
async fn controller_runtime_authenticates_decrypts_reassembles_and_sends_encrypted_actions() {
    let relay = spawn_test_relay().await;
    let config = || {
        QuicClientConfig::with_client_config(
            relay.address,
            "localhost",
            relay.client_config.clone(),
        )
    };
    let host = QuicClient::connect(config()).await.unwrap();
    let controller = QuicClient::connect(config()).await.unwrap();
    let session_id = SessionId::from_bytes([71; 16]);
    let authentication = [72; 32];
    host.join(RelayJoin::host_with_participant(
        session_id,
        authentication,
        [1; 16],
    ))
    .await
    .unwrap();
    controller
        .join(RelayJoin::controller_with_participant(
            session_id,
            authentication,
            [2; 16],
        ))
        .await
        .unwrap();

    let host_identity = DeviceIdentity::from_secret_key([73; 16], &[74; 32]);
    let controller_identity = DeviceIdentity::from_secret_key([75; 16], &[76; 32]);
    let host_verify_key = host_identity.verify_key();
    let controller_verify_key = controller_identity.verify_key();
    let (continue_sender, continue_receiver) = oneshot::channel();
    let host_task = tokio::spawn(run_fake_host(
        host,
        host_identity,
        controller_verify_key,
        continue_receiver,
    ));

    let mut runtime = ControllerRuntime::connect(controller, controller_identity, host_verify_key)
        .await
        .unwrap();
    let config = match tokio::time::timeout(Duration::from_secs(3), runtime.next_event())
        .await
        .unwrap()
        .unwrap()
    {
        ControllerEvent::VideoConfig(config) => config,
        event => panic!("expected video config, got {event:?}"),
    };
    assert_eq!(config.stream_id, 9);
    continue_sender.send(()).unwrap();

    let mut received_frame = None;
    let mut received_cursor = None;
    let mut received_audio = None;
    while received_frame.is_none() || received_cursor.is_none() || received_audio.is_none() {
        match tokio::time::timeout(Duration::from_secs(3), runtime.next_event())
            .await
            .unwrap()
            .unwrap()
        {
            ControllerEvent::H264AccessUnit(frame) => received_frame = Some(frame),
            ControllerEvent::Cursor(cursor) => received_cursor = Some(cursor),
            ControllerEvent::Audio(audio) => received_audio = Some(audio),
            ControllerEvent::Control(_)
            | ControllerEvent::VideoConfig(_)
            | ControllerEvent::Transfer(_)
            | ControllerEvent::Closed { .. } => {}
        }
    }
    let frame = received_frame.unwrap();
    assert_eq!(frame.stream_id, 9);
    assert_eq!(frame.frame_id, 11);
    assert_eq!(frame.data, vec![0x5a; 2_500]);
    assert_eq!(received_cursor.unwrap().stream_id, 9);
    let audio = received_audio.unwrap();
    assert_eq!(audio.stream_id, 9);
    assert_eq!(audio.sequence, 1);
    assert_eq!(audio.payload, vec![0x2a; 960]);

    runtime
        .send_input(InputEvent::MouseWheel {
            delta_x: -120,
            delta_y: 240,
        })
        .await
        .unwrap();
    runtime.request_keyframe().await.unwrap();
    let (input, keyframe) = tokio::time::timeout(Duration::from_secs(3), host_task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        input,
        InputEvent::MouseWheel {
            delta_x: -120,
            delta_y: 240,
        }
    );
    assert_eq!(keyframe, ControlMessage::RequestKeyframe { stream_id: 9 });
    assert_eq!(runtime.metrics().completed_frames, 1);
    assert_eq!(runtime.metrics().dropped_video_packets, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn controller_runtime_requests_a_keyframe_after_a_reference_gap() {
    let relay = spawn_test_relay().await;
    let config = || {
        QuicClientConfig::with_client_config(
            relay.address,
            "localhost",
            relay.client_config.clone(),
        )
    };
    let host = QuicClient::connect(config()).await.unwrap();
    let controller = QuicClient::connect(config()).await.unwrap();
    let session_id = SessionId::from_bytes([81; 16]);
    let authentication = [82; 32];
    host.join(RelayJoin::host_with_participant(
        session_id,
        authentication,
        [1; 16],
    ))
    .await
    .unwrap();
    controller
        .join(RelayJoin::controller_with_participant(
            session_id,
            authentication,
            [2; 16],
        ))
        .await
        .unwrap();

    let host_identity = DeviceIdentity::from_secret_key([83; 16], &[84; 32]);
    let controller_identity = DeviceIdentity::from_secret_key([85; 16], &[86; 32]);
    let host_verify_key = host_identity.verify_key();
    let controller_verify_key = controller_identity.verify_key();
    let (release_host, keep_host_alive) = oneshot::channel();
    let host_task = tokio::spawn(run_reference_gap_host(
        host,
        host_identity,
        controller_verify_key,
        keep_host_alive,
    ));

    let mut runtime = ControllerRuntime::connect(controller, controller_identity, host_verify_key)
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), runtime.next_event())
            .await
            .unwrap()
            .unwrap(),
        ControllerEvent::VideoConfig(VideoConfig { stream_id: 9, .. })
    ));
    runtime.set_audio_enabled(false).await.unwrap();

    let first = tokio::time::timeout(Duration::from_secs(3), runtime.next_event())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        first,
        ControllerEvent::H264AccessUnit(EncodedFrame { frame_id: 10, .. })
    ));
    runtime.set_audio_enabled(true).await.unwrap();

    let recovered = tokio::time::timeout(Duration::from_secs(3), runtime.next_event())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        recovered,
        ControllerEvent::H264AccessUnit(EncodedFrame { frame_id: 13, .. })
    ));
    let next = tokio::time::timeout(Duration::from_secs(3), runtime.next_event())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        next,
        ControllerEvent::H264AccessUnit(EncodedFrame { frame_id: 14, .. })
    ));
    release_host.send(()).unwrap();

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), host_task)
            .await
            .unwrap()
            .unwrap(),
        ControlMessage::RequestKeyframe { stream_id: 9 }
    );
    assert_eq!(runtime.metrics().completed_frames, 3);
}

async fn run_fake_host(
    host: QuicClient,
    identity: DeviceIdentity,
    expected_controller: ed25519_dalek::VerifyingKey,
    continue_receiver: oneshot::Receiver<()>,
) -> (InputEvent, ControlMessage) {
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

    let first = open_control(&mut secure, host.next_control().await.unwrap());
    let second = open_control(&mut secure, host.next_control().await.unwrap());
    assert!(matches!(
        first,
        ControlMessage::Hello {
            role: DeviceRole::Controller,
            ..
        }
    ));
    assert!(matches!(
        second,
        ControlMessage::Capabilities(DeviceCapabilities {
            role: DeviceRole::Controller,
            ..
        })
    ));

    send_control(
        &host,
        &mut secure,
        ControlMessage::Hello {
            platform: Platform::Windows,
            role: DeviceRole::Host,
        },
    )
    .await;
    send_control(
        &host,
        &mut secure,
        ControlMessage::Capabilities(DeviceCapabilities {
            platform: Platform::Windows,
            role: DeviceRole::Host,
            codecs: vec![Codec::H264],
            h264_profiles: vec![H264Profile::Main],
            width: 1280,
            height: 720,
        }),
    )
    .await;

    let config = VideoConfig {
        protocol_version: PROTOCOL_VERSION,
        stream_id: 9,
        config_version: 3,
        codec: Codec::H264,
        width: 1280,
        height: 720,
        sequence_header: vec![
            0, 0, 0, 1, 0x67, 0x64, 0, 0x1f, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80,
        ],
    };
    let config_bytes = encode_video_config(&config).unwrap();
    host.send_video_config(secure.seal(SecureLane::VideoConfig, &config_bytes).unwrap())
        .await
        .unwrap();
    continue_receiver.await.unwrap();

    let frame = EncodedFrame {
        stream_id: 9,
        frame_id: 11,
        config_version: 3,
        capture_timestamp_us: 123,
        width: 1280,
        height: 720,
        flags: FrameFlags(FrameFlags::KEYFRAME.0 | FrameFlags::CONFIG.0),
        data: vec![0x5a; 2_500],
    };
    let mut packets = packetize_frame(&frame).unwrap();
    packets.reverse();
    for packet in packets {
        let plaintext = encode_video_packet(&packet).unwrap();
        host.send_video_datagram(secure.seal(SecureLane::VideoDatagram, &plaintext).unwrap())
            .await
            .unwrap();
    }
    let cursor = encode_cursor_update(&CursorUpdate {
        protocol_version: PROTOCOL_VERSION,
        stream_id: 9,
        sequence: 1,
        timestamp_us: 123,
        x_millionths: 500_000,
        y_millionths: 500_000,
        visible: true,
        shape_id: 1,
    })
    .unwrap();
    host.send_cursor_datagram(secure.seal(SecureLane::CursorDatagram, &cursor).unwrap())
        .await
        .unwrap();
    let audio = encode_audio_packet(&AudioPacket {
        protocol_version: PROTOCOL_VERSION,
        stream_id: 9,
        sequence: 1,
        capture_timestamp_us: 124,
        codec: AudioCodec::PcmS16Le,
        sample_rate: AUDIO_SAMPLE_RATE,
        channels: AUDIO_CHANNELS,
        payload: vec![0x2a; 960],
    })
    .unwrap();
    host.send_audio_datagram(secure.seal(SecureLane::AudioDatagram, &audio).unwrap())
        .await
        .unwrap();

    let (input, control) = tokio::join!(host.next_input(), host.next_control());
    let input = secure.open(SecureLane::Input, &input.unwrap()).unwrap();
    let input = decode_input(&input, now_micros()).unwrap().event;
    let control = open_control(&mut secure, control.unwrap());
    (input, control)
}

async fn run_reference_gap_host(
    host: QuicClient,
    identity: DeviceIdentity,
    expected_controller: ed25519_dalek::VerifyingKey,
    keep_host_alive: oneshot::Receiver<()>,
) -> ControlMessage {
    let first = decode_noise_handshake(&host.next_control().await.unwrap()).unwrap();
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
    responder.receive(&finish.payload).unwrap();
    let mut secure = responder
        .finish()
        .unwrap()
        .into_secure_session(SecureRole::Responder);

    let first = open_control(&mut secure, host.next_control().await.unwrap());
    let second = open_control(&mut secure, host.next_control().await.unwrap());
    assert!(matches!(first, ControlMessage::Hello { .. }));
    assert!(matches!(second, ControlMessage::Capabilities(_)));
    send_control(
        &host,
        &mut secure,
        ControlMessage::Hello {
            platform: Platform::Windows,
            role: DeviceRole::Host,
        },
    )
    .await;
    send_control(
        &host,
        &mut secure,
        ControlMessage::Capabilities(DeviceCapabilities {
            platform: Platform::Windows,
            role: DeviceRole::Host,
            codecs: vec![Codec::H264],
            h264_profiles: vec![H264Profile::Main],
            width: 1280,
            height: 720,
        }),
    )
    .await;

    let config = VideoConfig {
        protocol_version: PROTOCOL_VERSION,
        stream_id: 9,
        config_version: 3,
        codec: Codec::H264,
        width: 1280,
        height: 720,
        sequence_header: vec![
            0, 0, 0, 1, 0x67, 0x64, 0, 0x1f, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80,
        ],
    };
    let config_bytes = encode_video_config(&config).unwrap();
    host.send_video_config(secure.seal(SecureLane::VideoConfig, &config_bytes).unwrap())
        .await
        .unwrap();
    assert_eq!(
        open_control(&mut secure, host.next_control().await.unwrap()),
        ControlMessage::SetAudioEnabled { enabled: false }
    );
    send_test_video_frame(&host, &mut secure, 10, true).await;
    assert_eq!(
        open_control(&mut secure, host.next_control().await.unwrap()),
        ControlMessage::SetAudioEnabled { enabled: true }
    );
    send_test_video_frame(&host, &mut secure, 12, false).await;

    let request = open_control(&mut secure, host.next_control().await.unwrap());
    send_test_video_frame(&host, &mut secure, 13, true).await;
    send_test_video_frame(&host, &mut secure, 14, false).await;
    let _ = keep_host_alive.await;
    request
}

async fn send_test_video_frame(
    host: &QuicClient,
    secure: &mut SecureSession,
    frame_id: u64,
    keyframe: bool,
) {
    let frame = EncodedFrame {
        stream_id: 9,
        frame_id,
        config_version: 3,
        capture_timestamp_us: frame_id,
        width: 1280,
        height: 720,
        flags: if keyframe {
            FrameFlags(FrameFlags::KEYFRAME.0)
        } else {
            FrameFlags(0)
        },
        data: vec![frame_id as u8; 2_500],
    };
    for packet in packetize_frame(&frame).unwrap() {
        let plaintext = encode_video_packet(&packet).unwrap();
        host.send_video_datagram(secure.seal(SecureLane::VideoDatagram, &plaintext).unwrap())
            .await
            .unwrap();
    }
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

fn now_micros() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}
