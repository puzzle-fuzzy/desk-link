//! Real QUIC/Noise input authorization retained through application delivery.
use super::*;

async fn connected_grant(host: &HostRuntime) -> u64 {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match host.next_event().await.unwrap() {
                HostEvent::Connected { media_generation } => {
                    assert_ne!(media_generation, 0);
                    return media_generation;
                }
                HostEvent::Error(error) => panic!("host failed before Connected: {error:?}"),
                HostEvent::State(HostState::Closed) => panic!("host closed before Connected"),
                _ => {}
            }
        }
    })
    .await
    .unwrap()
}

async fn start_input_session(fixture: &HostTestFixture) -> (HostRuntime, RawController, u64) {
    let host = fixture.start_host_only().await;
    let mut controller = RawController::pair(fixture).await;
    assert!(matches!(
        fixture.next_event(&host).await,
        HostEvent::ApprovalRequested { .. }
    ));
    fixture.approve(&host).await;
    controller.negotiate().await;
    let grant = connected_grant(&host).await;
    (host, controller, grant)
}

async fn captured_input(host: &HostRuntime) -> HostEvent {
    let event = tokio::time::timeout(Duration::from_secs(3), host.next_event())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(event, HostEvent::Input { .. }),
        "unexpected input delivery: {event:?}"
    );
    event
}

fn envelope(context: InputContext, sequence: u64, event: InputEvent) -> InputEnvelope {
    InputEnvelope {
        sequence,
        timestamp_us: now_micros(),
        stream_id: context.stream_id,
        input_epoch: context.input_epoch,
        event,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn captured_input_keeps_original_context_and_retired_plaintext_cannot_poison_successor() {
    let fixture = HostTestFixture::new().await;
    let (host, mut controller, grant) = start_input_session(&fixture).await;
    publish_raw_config(&host, &mut controller, 9, 1, 640, 360).await;
    let old_context = controller.input_context.unwrap();
    let original = envelope(
        old_context,
        17,
        InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        },
    );
    controller.send_input_envelope(original.clone()).await;
    let delayed = captured_input(&host).await;
    assert_eq!(
        delayed,
        HostEvent::Input {
            media_generation: grant,
            input: original.clone()
        }
    );

    publish_raw_config(&host, &mut controller, 9, 2, 800, 600).await;
    expect_release_all(&host).await;
    let current = controller.input_context.unwrap();
    assert_ne!(current, old_context);
    controller
        .send_input_envelope(InputEnvelope {
            sequence: u64::MAX,
            timestamp_us: u64::MAX,
            stream_id: old_context.stream_id,
            input_epoch: old_context.input_epoch,
            event: InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            },
        })
        .await;
    let accepted = envelope(
        current,
        18,
        InputEvent::MouseWheel {
            delta_x: 7,
            delta_y: -9,
        },
    );
    controller.send_input_envelope(accepted.clone()).await;
    assert_eq!(
        captured_input(&host).await,
        HostEvent::Input {
            media_generation: grant,
            input: accepted
        }
    );
    // A consumer retaining A until after a target replacement still owns A's
    // complete authorization, never a reconstructed current-target label.
    assert_eq!(
        delayed,
        HostEvent::Input {
            media_generation: grant,
            input: original
        }
    );
    assert_eq!(host.state(), HostState::Connected);
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_noise_input_carries_its_own_grant_and_rejects_retired_context_plaintext() {
    let fixture = HostTestFixture::new().await;
    let (host, mut first, old_grant) = start_input_session(&fixture).await;
    publish_raw_config(&host, &mut first, 9, 1, 640, 360).await;
    let old_context = first.input_context.unwrap();
    let original = envelope(old_context, 1, InputEvent::MouseMove { x: 20, y: 30 });
    first.send_input_envelope(original.clone()).await;
    let delayed = captured_input(&host).await;
    let retired_relay_generation = first.host_generation;
    first.client.close(b"replace input authorization owner");
    drop(first);
    wait_for_state(&host, HostState::Connecting).await;
    fixture
        .wait_for_new_host_registration(retired_relay_generation)
        .await;
    let mut successor = RawController::pair(&fixture).await;
    successor.negotiate().await;
    let new_grant = connected_grant(&host).await;
    assert_ne!(new_grant, old_grant);
    publish_raw_config(&host, &mut successor, 9, 1, 640, 360).await;
    let current = successor.input_context.unwrap();
    assert_ne!(current, old_context);
    // Encrypt old application plaintext in the fresh Noise session: it must
    // fail the target gate without spending the new session's sequence space.
    successor
        .send_input_envelope(InputEnvelope {
            sequence: u64::MAX,
            timestamp_us: u64::MAX,
            ..original.clone()
        })
        .await;
    let accepted = envelope(current, 1, InputEvent::MouseMove { x: 40, y: 50 });
    successor.send_input_envelope(accepted.clone()).await;
    assert_eq!(
        captured_input(&host).await,
        HostEvent::Input {
            media_generation: new_grant,
            input: accepted
        }
    );
    assert_eq!(
        delayed,
        HostEvent::Input {
            media_generation: old_grant,
            input: original
        }
    );
    assert_eq!(host.state(), HostState::Connected);
    host.destroy();
}
