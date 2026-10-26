//! Actual local QUIC/Noise producer ownership; no native capture or input injection.
use super::*;

async fn connected_owner(host: &HostRuntime) -> u64 {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match host.next_event().await.unwrap() {
                HostEvent::Connected { media_generation } => {
                    assert_ne!(media_generation, 0);
                    return media_generation;
                }
                HostEvent::Error(error) => panic!("host failed before owner grant: {error:?}"),
                HostEvent::State(HostState::Closed) => panic!("host closed before owner grant"),
                _ => {}
            }
        }
    })
    .await
    .expect("host did not grant a producer owner")
}

async fn start_owned(fixture: &HostTestFixture) -> (HostRuntime, RawController, u64) {
    let host = fixture.start_host_only().await;
    let mut controller = RawController::pair(fixture).await;
    assert!(matches!(
        fixture.next_event(&host).await,
        HostEvent::ApprovalRequested { .. }
    ));
    fixture.approve(&host).await;
    controller.negotiate().await;
    let owner = connected_owner(&host).await;
    (host, controller, owner)
}

async fn owned_config(
    host: &HostRuntime,
    controller: &mut RawController,
    owner: u64,
    stream: u64,
    version: u32,
    width: u16,
) -> VideoConfig {
    let command = raw_config_command(stream, version, width, 360);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match host.send_for_generation(owner, command.clone()) {
                Ok(()) => break,
                Err(HostError::CommandQueueFull) => tokio::task::yield_now().await,
                result => panic!("current producer was rejected: {result:?}"),
            }
        }
    })
    .await
    .expect("owned media command queue did not make progress");
    controller.receive_video_config().await
}

fn old_access_unit() -> HostCommand {
    HostCommand::SendVideoAccessUnit {
        stream_id: 9,
        frame_id: 1,
        config_version: 1,
        bytes: vec![0, 0, 0, 1, 0x65, 0x88, 0x84],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zero_or_unknown_producer_is_rejected_without_closing_the_current_session() {
    let fixture = HostTestFixture::new().await;
    let (host, mut controller, owner) = start_owned(&fixture).await;
    for stale in [0, owner ^ 1] {
        assert_ne!(stale, owner);
        assert_eq!(
            host.send_for_generation(stale, raw_config_command(9, 1, 640, 360)),
            Err(HostError::StaleMediaGeneration)
        );
        assert_eq!(
            host.send_for_generation(stale, old_access_unit()),
            Err(HostError::StaleMediaGeneration)
        );
    }
    let config = owned_config(&host, &mut controller, owner, 9, 1, 800).await;
    assert_eq!(
        config.width, 800,
        "a rejected stale config must never be published first"
    );
    let event = InputEvent::MouseMove { x: 23, y: 45 };
    controller
        .send_input_for_context(controller.input_context.unwrap(), event.clone())
        .await;
    expect_input(&host, event).await;
    assert_eq!(host.state(), HostState::Connected);
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delayed_old_producer_cannot_configure_or_kill_a_successor_noise_session() {
    let fixture = HostTestFixture::new().await;
    let (host, mut first, old_owner) = start_owned(&fixture).await;
    let old_config = owned_config(&host, &mut first, old_owner, 9, 1, 640).await;
    let retired_host_generation = first.host_generation;
    first.client.close(b"replace test producer ownership");
    drop(first);
    wait_for_state(&host, HostState::Connecting).await;
    fixture
        .wait_for_new_host_registration(retired_host_generation)
        .await;
    let mut successor = RawController::pair(&fixture).await;
    successor.negotiate().await;
    let owner = connected_owner(&host).await;
    assert_ne!(
        owner, old_owner,
        "new Noise must never inherit the old producer grant"
    );
    assert_eq!(
        host.send_for_generation(old_owner, raw_config_command(9, 1, 640, 360)),
        Err(HostError::StaleMediaGeneration)
    );
    assert_eq!(
        host.send_for_generation(old_owner, old_access_unit()),
        Err(HostError::StaleMediaGeneration)
    );
    let config = owned_config(&host, &mut successor, owner, 9, 1, 800).await;
    assert_eq!(config.width, 800);
    assert_ne!(config.input_epoch, old_config.input_epoch);
    let event = InputEvent::MouseWheel {
        delta_x: -8,
        delta_y: 13,
    };
    successor
        .send_input_for_context(successor.input_context.unwrap(), event.clone())
        .await;
    expect_input(&host, event).await;
    assert_eq!(host.state(), HostState::Connected);
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_handle_cannot_use_another_handles_producer_grant() {
    let first_fixture = HostTestFixture::new().await;
    let second_fixture = HostTestFixture::new().await;
    let (first_host, mut first_controller, first_owner) = start_owned(&first_fixture).await;
    let (second_host, mut second_controller, second_owner) = start_owned(&second_fixture).await;
    assert_ne!(first_owner, second_owner);
    assert_eq!(
        first_host.send_for_generation(second_owner, raw_config_command(9, 1, 640, 360)),
        Err(HostError::StaleMediaGeneration)
    );
    assert_eq!(
        second_host.send_for_generation(first_owner, raw_config_command(9, 1, 640, 360)),
        Err(HostError::StaleMediaGeneration)
    );
    owned_config(&first_host, &mut first_controller, first_owner, 9, 1, 640).await;
    owned_config(
        &second_host,
        &mut second_controller,
        second_owner,
        9,
        1,
        640,
    )
    .await;
    assert_eq!(first_host.state(), HostState::Connected);
    assert_eq!(second_host.state(), HostState::Connected);
    first_host.destroy();
    second_host.destroy();
}
