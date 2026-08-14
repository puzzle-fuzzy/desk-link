use std::{sync::Arc, time::Duration};

use desklink_crypto::{
    DeviceIdentity, NoiseResponder, SecureLane, SecureRole, SecureSession, SessionId,
};
use desklink_ffi::{ControllerEvent, ControllerRuntime};
use desklink_protocol::{
    AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, AudioCodec, AudioPacket, Codec, ControlMessage,
    CursorUpdate, DeviceCapabilities, DeviceRole, FrameFlags, H264Profile, InputEvent,
    NoiseHandshake, NoiseHandshakeStep, PROTOCOL_VERSION, Platform, TransferMessage,
    TransferResult, VideoConfig, decode_control, decode_input, decode_noise_handshake,
    decode_transfer, encode_audio_packet, encode_control, encode_cursor_update,
    encode_noise_handshake, encode_transfer, encode_video_config, encode_video_packet,
};
use desklink_relay::{RelayConfig, RelayServer};
use desklink_transport::{
    DirectVideoPathFallbackReason, QuicClient, QuicClientConfig, RelayJoin, VideoPathKind,
};
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
        false,
        true,
        true,
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

    // Exercise the same encrypted reliable lane used by the Windows UI for
    // clipboard and file transfers. The fake host validates the complete
    // offer/chunk/complete sequence and sends the expected acknowledgements
    // back through the relay.
    let transfer_id = [89; 16];
    runtime
        .send_transfer(TransferMessage::ClipboardSet {
            request_id: 41,
            text: "desklink transfer probe".to_owned(),
        })
        .await
        .unwrap();
    runtime
        .send_transfer(TransferMessage::FileOffer {
            transfer_id,
            request_id: None,
            name: "probe.txt".to_owned(),
            size: 4,
        })
        .await
        .unwrap();
    runtime
        .send_transfer(TransferMessage::FileChunk {
            transfer_id,
            offset: 0,
            bytes: vec![1, 2, 3, 4],
        })
        .await
        .unwrap();
    runtime
        .send_transfer(TransferMessage::FileComplete {
            transfer_id,
            content_hash: [90; 32],
        })
        .await
        .unwrap();

    let mut transfer_responses = Vec::new();
    while transfer_responses.len() < 3 {
        match tokio::time::timeout(Duration::from_secs(3), runtime.next_event())
            .await
            .unwrap()
            .unwrap()
        {
            ControllerEvent::Transfer(message) => transfer_responses.push(message),
            ControllerEvent::Closed { reason } => {
                panic!("transfer probe session closed unexpectedly: {reason}")
            }
            ControllerEvent::Control(_)
            | ControllerEvent::VideoConfig(_)
            | ControllerEvent::H264AccessUnit(_)
            | ControllerEvent::Cursor(_)
            | ControllerEvent::Audio(_) => {}
        }
    }
    assert_eq!(
        transfer_responses,
        vec![
            TransferMessage::ClipboardResult {
                request_id: 41,
                result: TransferResult::Completed,
            },
            TransferMessage::FileDecision {
                transfer_id,
                result: TransferResult::Completed,
                resume_offset: 0,
                resume_prefix_hash: None,
            },
            TransferMessage::FileResult {
                transfer_id,
                result: TransferResult::Completed,
            },
        ]
    );

    // The fake host intentionally repeats the first encrypted video packet.
    // A duplicate datagram must be discarded without terminating the secure
    // session or surfacing a replay error to the caller.
    assert!(
        tokio::time::timeout(Duration::from_millis(500), runtime.next_event())
            .await
            .is_err(),
        "duplicate video datagram must not terminate the controller"
    );

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
    assert_eq!(runtime.metrics().dropped_video_packets, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn controller_runtime_keeps_relay_video_after_direct_path_rejection() {
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
    let session_id = SessionId::from_bytes([77; 16]);
    let authentication = [78; 32];
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

    let host_identity = DeviceIdentity::from_secret_key([79; 16], &[80; 32]);
    let controller_identity = DeviceIdentity::from_secret_key([81; 16], &[82; 32]);
    let host_verify_key = host_identity.verify_key();
    let controller_verify_key = controller_identity.verify_key();
    let (continue_sender, continue_receiver) = oneshot::channel();
    let host_task = tokio::spawn(run_fake_host(
        host,
        host_identity,
        controller_verify_key,
        continue_receiver,
        true,
        false,
        false,
    ));

    let mut runtime = ControllerRuntime::connect_for_platform(
        controller,
        controller_identity,
        host_verify_key,
        Platform::Windows,
    )
    .await
    .unwrap();
    assert!(
        runtime.direct_candidate().is_some(),
        "Windows runtime should advertise a direct candidate"
    );
    let event = tokio::time::timeout(Duration::from_secs(3), runtime.next_event())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event, ControllerEvent::VideoConfig(_)));
    continue_sender.send(()).unwrap();
    for _ in 0..8 {
        if runtime.video_path_fallback_reason().is_some() {
            break;
        }
        match tokio::time::timeout(Duration::from_secs(3), runtime.next_event()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                panic!("controller runtime failed while draining relay events: {error}")
            }
            Err(_) => break,
        }
    }
    assert_eq!(runtime.video_path_kind(), VideoPathKind::Relay);
    assert_eq!(
        runtime.video_path_fallback_reason(),
        Some(DirectVideoPathFallbackReason::Rejected)
    );

    runtime
        .send_input(InputEvent::MouseWheel {
            delta_x: -30,
            delta_y: 60,
        })
        .await
        .unwrap();
    runtime.request_keyframe().await.unwrap();
    let (input, control) = tokio::time::timeout(Duration::from_secs(3), host_task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        input,
        InputEvent::MouseWheel {
            delta_x: -30,
            delta_y: 60,
        }
    );
    assert_eq!(control, ControlMessage::RequestKeyframe { stream_id: 9 });
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn controller_runtime_reconnects_with_a_fresh_ios_stream_boundary() {
    let relay = spawn_test_relay().await;
    let config = || {
        QuicClientConfig::with_client_config(
            relay.address,
            "localhost",
            relay.client_config.clone(),
        )
    };
    let host = QuicClient::connect(config()).await.unwrap();
    let session_id = SessionId::from_bytes([101; 16]);
    let authentication = [102; 32];
    host.join(RelayJoin::host_with_participant(
        session_id,
        authentication,
        [1; 16],
    ))
    .await
    .unwrap();

    let controller_secret = [104; 32];
    let controller_identity = DeviceIdentity::from_secret_key([105; 16], &controller_secret);
    let host_verify_key = DeviceIdentity::from_secret_key([103; 16], &[106; 32]).verify_key();
    let controller_verify_key = controller_identity.verify_key();
    let (first_ready_sender, first_ready_receiver) = oneshot::channel();
    let first_host_task = tokio::spawn(run_config_only_host(
        host,
        [106; 32],
        controller_verify_key,
        first_ready_receiver,
        9,
    ));

    let first_controller = QuicClient::connect(config()).await.unwrap();
    first_controller
        .join(RelayJoin::controller_with_participant(
            session_id,
            authentication,
            [105; 16],
        ))
        .await
        .unwrap();
    let mut first_runtime = ControllerRuntime::connect_for_platform(
        first_controller,
        controller_identity,
        host_verify_key,
        Platform::IOS,
    )
    .await
    .unwrap();
    assert!(first_runtime.direct_candidate().is_none());
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), first_runtime.next_event())
            .await
            .unwrap()
            .unwrap(),
        ControllerEvent::VideoConfig(VideoConfig { stream_id: 9, .. })
    ));
    first_ready_sender.send(()).unwrap();
    drop(first_runtime);
    tokio::time::timeout(Duration::from_secs(3), first_host_task)
        .await
        .unwrap()
        .unwrap();

    let second_session_id = SessionId::from_bytes([108; 16]);
    let second_host = QuicClient::connect(config()).await.unwrap();
    second_host
        .join(RelayJoin::host_with_participant(
            second_session_id,
            authentication,
            [9; 16],
        ))
        .await
        .unwrap();
    let (second_ready_sender, second_ready_receiver) = oneshot::channel();
    let second_host_task = tokio::spawn(run_config_only_host(
        second_host,
        [106; 32],
        controller_verify_key,
        second_ready_receiver,
        10,
    ));
    let second_controller = QuicClient::connect(config()).await.unwrap();
    second_controller
        .join(RelayJoin::controller_with_participant(
            second_session_id,
            authentication,
            [107; 16],
        ))
        .await
        .unwrap();
    let second_identity = DeviceIdentity::from_secret_key([107; 16], &controller_secret);
    let mut second_runtime = ControllerRuntime::connect_for_platform(
        second_controller,
        second_identity,
        host_verify_key,
        Platform::IOS,
    )
    .await
    .unwrap();
    assert!(second_runtime.direct_candidate().is_none());
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), second_runtime.next_event())
            .await
            .unwrap()
            .unwrap(),
        ControllerEvent::VideoConfig(VideoConfig { stream_id: 10, .. })
    ));
    second_ready_sender.send(()).unwrap();
    drop(second_runtime);

    tokio::time::timeout(Duration::from_secs(3), second_host_task)
        .await
        .unwrap()
        .unwrap();
}

async fn run_fake_host(
    host: QuicClient,
    identity: DeviceIdentity,
    expected_controller: ed25519_dalek::VerifyingKey,
    continue_receiver: oneshot::Receiver<()>,
    reject_direct_offer: bool,
    duplicate_video_packet: bool,
    probe_transfers: bool,
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

    if reject_direct_offer {
        let offer = open_control(&mut secure, host.next_control().await.unwrap());
        let candidate_id = match offer {
            ControlMessage::VideoPathCandidateOffer { candidate } => candidate.candidate_id(),
            message => panic!("expected direct video candidate offer, got {message:?}"),
        };
        send_control(
            &host,
            &mut secure,
            ControlMessage::VideoPathCandidateAnswer {
                candidate_id,
                accepted: false,
                candidate: None,
            },
        )
        .await;
    }

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
    let first_packet = packets.pop().expect("fake frame has packets");
    let first_plaintext = encode_video_packet(&first_packet).unwrap();
    let first_ciphertext = secure
        .seal(SecureLane::VideoDatagram, &first_plaintext)
        .unwrap();
    host.send_video_datagram(first_ciphertext.clone())
        .await
        .unwrap();

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

    for packet in packets {
        let plaintext = encode_video_packet(&packet).unwrap();
        host.send_video_datagram(secure.seal(SecureLane::VideoDatagram, &plaintext).unwrap())
            .await
            .unwrap();
    }
    if duplicate_video_packet {
        host.send_video_datagram(first_ciphertext).await.unwrap();
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

    let mut input = None;
    let mut control = None;
    let mut transfer_count = 0;
    while input.is_none() || control.is_none() || (probe_transfers && transfer_count < 4) {
        tokio::select! {
            input_result = host.next_input(), if input.is_none() => {
                let plaintext = secure.open(SecureLane::Input, &input_result.unwrap()).unwrap();
                input = Some(decode_input(&plaintext, now_micros()).unwrap().event);
            }
            control_result = host.next_control(), if control.is_none() => {
                control = Some(open_control(&mut secure, control_result.unwrap()));
            }
            transfer_result = host.next_transfer(), if probe_transfers && transfer_count < 4 => {
                let plaintext = secure.open(SecureLane::Transfer, &transfer_result.unwrap()).unwrap();
                let message = decode_transfer(&plaintext).unwrap();
                match message {
                    TransferMessage::ClipboardSet { request_id, text } => {
                        assert_eq!(request_id, 41);
                        assert_eq!(text, "desklink transfer probe");
                        send_transfer(
                            &host,
                            &mut secure,
                            TransferMessage::ClipboardResult {
                                request_id,
                                result: TransferResult::Completed,
                            },
                        ).await;
                    }
                    TransferMessage::FileOffer { transfer_id, name, size, .. } => {
                        assert_eq!(transfer_id, [89; 16]);
                        assert_eq!(name, "probe.txt");
                        assert_eq!(size, 4);
                        send_transfer(
                            &host,
                            &mut secure,
                            TransferMessage::FileDecision {
                                transfer_id,
                                result: TransferResult::Completed,
                                resume_offset: 0,
                                resume_prefix_hash: None,
                            },
                        ).await;
                    }
                    TransferMessage::FileChunk { transfer_id, offset, bytes } => {
                        assert_eq!(transfer_id, [89; 16]);
                        assert_eq!(offset, 0);
                        assert_eq!(bytes, vec![1, 2, 3, 4]);
                    }
                    TransferMessage::FileComplete { transfer_id, content_hash } => {
                        assert_eq!(transfer_id, [89; 16]);
                        assert_eq!(content_hash, [90; 32]);
                        send_transfer(
                            &host,
                            &mut secure,
                            TransferMessage::FileResult {
                                transfer_id,
                                result: TransferResult::Completed,
                            },
                        ).await;
                    }
                    message => panic!("unexpected transfer probe message: {message:?}"),
                }
                transfer_count += 1;
            }
        }
    }
    (input.unwrap(), control.unwrap())
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
    let first_control = open_control(&mut secure, host.next_control().await.unwrap());
    if first_control == (ControlMessage::RequestKeyframe { stream_id: 9 }) {
        assert_eq!(
            open_control(&mut secure, host.next_control().await.unwrap()),
            ControlMessage::SetAudioEnabled { enabled: false }
        );
    } else {
        assert_eq!(
            first_control,
            ControlMessage::SetAudioEnabled { enabled: false }
        );
        assert_eq!(
            open_control(&mut secure, host.next_control().await.unwrap()),
            ControlMessage::RequestKeyframe { stream_id: 9 }
        );
    }
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

async fn run_config_only_host(
    host: QuicClient,
    host_secret: [u8; 32],
    expected_controller: ed25519_dalek::VerifyingKey,
    ready: oneshot::Receiver<()>,
    stream_id: u64,
) {
    let mut secure = accept_reconnect_controller(&host, host_secret, expected_controller).await;
    send_reconnect_config(&host, &mut secure, stream_id).await;
    ready.await.unwrap();
}

async fn accept_reconnect_controller(
    host: &QuicClient,
    host_secret: [u8; 32],
    expected_controller: ed25519_dalek::VerifyingKey,
) -> SecureSession {
    let identity = DeviceIdentity::from_secret_key([103; 16], &host_secret);
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
    assert_eq!(finish.step, NoiseHandshakeStep::InitiatorFinish);
    responder.receive(&finish.payload).unwrap();
    let mut secure = responder
        .finish()
        .unwrap()
        .into_secure_session(SecureRole::Responder);

    assert!(matches!(
        open_control(&mut secure, host.next_control().await.unwrap()),
        ControlMessage::Hello { .. }
    ));
    assert!(matches!(
        open_control(&mut secure, host.next_control().await.unwrap()),
        ControlMessage::Capabilities(_)
    ));
    send_control(
        host,
        &mut secure,
        ControlMessage::Hello {
            platform: Platform::MacOS,
            role: DeviceRole::Host,
        },
    )
    .await;
    send_control(
        host,
        &mut secure,
        ControlMessage::Capabilities(DeviceCapabilities {
            platform: Platform::MacOS,
            role: DeviceRole::Host,
            codecs: vec![Codec::H264],
            h264_profiles: vec![H264Profile::Main],
            width: 1280,
            height: 720,
        }),
    )
    .await;
    secure
}

async fn send_reconnect_config(host: &QuicClient, secure: &mut SecureSession, stream_id: u64) {
    let config = VideoConfig {
        protocol_version: PROTOCOL_VERSION,
        stream_id,
        config_version: 1,
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

async fn send_transfer(host: &QuicClient, secure: &mut SecureSession, message: TransferMessage) {
    let plaintext = encode_transfer(&message).unwrap();
    host.send_transfer(secure.seal(SecureLane::Transfer, &plaintext).unwrap())
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
