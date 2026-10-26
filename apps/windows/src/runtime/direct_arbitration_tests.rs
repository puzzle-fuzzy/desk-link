use std::{sync::Arc, time::Duration};

use desklink_crypto::{
    DeviceIdentity, NoiseInitiator, NoiseResponder, SecureLane, SecureRole, SecureSession,
};
use desklink_protocol::DirectLanCandidate;
use desklink_transport::{DirectLanConnection, DirectLanEndpoint};
use rand_core::OsRng;
use tokio::sync::{Mutex as AsyncMutex, Semaphore, oneshot};

use super::{direct_path_slot, install_direct_connection};

fn connected_secure_sessions() -> (SecureSession, SecureSession) {
    let controller_identity = DeviceIdentity::generate(&mut OsRng);
    let host_identity = DeviceIdentity::generate(&mut OsRng);
    let controller_key = controller_identity.verify_key();
    let host_key = host_identity.verify_key();
    let (mut initiator, hello) = NoiseInitiator::start(controller_identity, host_key).unwrap();
    let (mut responder, response) =
        NoiseResponder::accept(&hello, host_identity, controller_key).unwrap();
    let finish = initiator.receive(&response).unwrap();
    responder.receive(&finish).unwrap();
    (
        initiator
            .finish()
            .unwrap()
            .into_secure_session(SecureRole::Initiator),
        responder
            .finish()
            .unwrap()
            .into_secure_session(SecureRole::Responder),
    )
}

async fn connect_path(
    initiator: &Arc<DirectLanEndpoint>,
    responder: &Arc<DirectLanEndpoint>,
    initiator_secure: &Arc<AsyncMutex<SecureSession>>,
    responder_secure: &Arc<AsyncMutex<SecureSession>>,
    candidate_id: u64,
    binding: [u8; 16],
) -> (Arc<DirectLanConnection>, Arc<DirectLanConnection>) {
    let candidate = DirectLanCandidate::new(
        candidate_id,
        responder.local_addr().unwrap(),
        110,
        binding,
        100,
    )
    .unwrap();
    let accepting = tokio::spawn({
        let endpoint = responder.clone();
        let secure = responder_secure.clone();
        async move {
            let connection = endpoint.accept().await.unwrap().unwrap();
            endpoint
                .accept_probe_connection_shared(connection, candidate_id, &binding, &secure, 100)
                .await
                .unwrap()
                .0
        }
    });
    let (initiated, _) = tokio::time::timeout(
        Duration::from_secs(3),
        initiator.connect_shared(&candidate, &binding, initiator_secure, 100),
    )
    .await
    .unwrap()
    .unwrap();
    let accepted = tokio::time::timeout(Duration::from_secs(3), accepting)
        .await
        .unwrap()
        .unwrap();
    (Arc::new(initiated), Arc::new(accepted))
}

struct AuthenticatedPaths {
    // Endpoint ownership must survive both probes and every datagram assertion.
    _controller_endpoint: Arc<DirectLanEndpoint>,
    _host_endpoint: Arc<DirectLanEndpoint>,
    controller_secure: Arc<AsyncMutex<SecureSession>>,
    host_secure: Arc<AsyncMutex<SecureSession>>,
    inbound: Arc<DirectLanConnection>,
    inbound_peer: Arc<DirectLanConnection>,
    outbound: Arc<DirectLanConnection>,
    outbound_peer: Arc<DirectLanConnection>,
}

impl AuthenticatedPaths {
    async fn new() -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let controller_endpoint =
            Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
        let host_endpoint =
            Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
        let (controller_secure, host_secure) = connected_secure_sessions();
        let controller_secure = Arc::new(AsyncMutex::new(controller_secure));
        let host_secure = Arc::new(AsyncMutex::new(host_secure));
        let binding = host_secure.lock().await.video_path_binding();
        assert_eq!(controller_secure.lock().await.video_path_binding(), binding);
        // First the controller initiates an authenticated inbound host path.
        let (inbound_peer, inbound) = connect_path(
            &controller_endpoint,
            &host_endpoint,
            &controller_secure,
            &host_secure,
            41,
            binding,
        )
        .await;
        // The host's competing outbound probe uses the SAME Noise session pair,
        // including its real direct-probe sequence/replay state.
        let (outbound, outbound_peer) = connect_path(
            &host_endpoint,
            &controller_endpoint,
            &host_secure,
            &controller_secure,
            42,
            binding,
        )
        .await;
        Self {
            _controller_endpoint: controller_endpoint,
            _host_endpoint: host_endpoint,
            controller_secure,
            host_secure,
            inbound,
            inbound_peer,
            outbound,
            outbound_peer,
        }
    }

    async fn exchange_authenticated_video(
        &self,
        host: &DirectLanConnection,
        controller: &DirectLanConnection,
    ) {
        let outbound = self
            .host_secure
            .lock()
            .await
            .seal(SecureLane::VideoDatagram, b"active host video")
            .unwrap();
        host.send_datagram(outbound).unwrap();
        let packet = tokio::time::timeout(Duration::from_secs(1), controller.recv_datagram())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            self.controller_secure
                .lock()
                .await
                .open(SecureLane::VideoDatagram, &packet)
                .unwrap(),
            b"active host video"
        );
        let reverse = self
            .controller_secure
            .lock()
            .await
            .seal(SecureLane::VideoDatagram, b"active reverse video")
            .unwrap();
        controller.send_datagram(reverse).unwrap();
        let packet = tokio::time::timeout(Duration::from_secs(1), host.recv_datagram())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            self.host_secure
                .lock()
                .await
                .open(SecureLane::VideoDatagram, &packet)
                .unwrap(),
            b"active reverse video"
        );
        assert!(!host.is_closed());
        assert!(!controller.is_closed());
    }
}

#[tokio::test]
async fn real_late_outbound_probe_cannot_close_or_replace_the_active_inbound_path() {
    let paths = AuthenticatedPaths::new().await;
    let slot = Arc::new(AsyncMutex::new(None));
    assert!(install_direct_connection(&slot, paths.inbound.clone()).await);
    // An alias must be recognized before rejection/close handling.
    assert!(install_direct_connection(&slot, paths.inbound.clone()).await);
    assert!(!paths.inbound.is_closed());
    assert!(!install_direct_connection(&slot, paths.outbound.clone()).await);
    assert!(paths.outbound.is_closed());
    tokio::time::timeout(Duration::from_secs(1), paths.outbound_peer.closed())
        .await
        .unwrap();
    assert!(Arc::ptr_eq(
        slot.lock().await.as_ref().unwrap(),
        &paths.inbound
    ));
    paths
        .exchange_authenticated_video(&paths.inbound, &paths.inbound_peer)
        .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), paths.inbound.closed())
            .await
            .is_err()
    );
    paths.inbound.close(b"arbitration fixture complete");
    paths.inbound_peer.close(b"arbitration fixture complete");
}

#[tokio::test]
async fn real_closed_active_path_can_be_replaced_without_stale_egress_retiring_its_successor() {
    let paths = AuthenticatedPaths::new().await;
    let slot = Arc::new(AsyncMutex::new(None));
    assert!(install_direct_connection(&slot, paths.inbound.clone()).await);
    paths.inbound.close(b"simulate old path failure");
    tokio::time::timeout(Duration::from_secs(1), paths.inbound_peer.closed())
        .await
        .unwrap();
    assert!(install_direct_connection(&slot, paths.outbound.clone()).await);
    {
        let mut active = slot.lock().await;
        assert!(direct_path_slot::retire_if_current(&mut active, &paths.inbound).is_none());
        assert!(Arc::ptr_eq(active.as_ref().unwrap(), &paths.outbound));
    }
    paths
        .exchange_authenticated_video(&paths.outbound, &paths.outbound_peer)
        .await;
    paths.outbound.close(b"replacement fixture complete");
    paths.outbound_peer.close(b"replacement fixture complete");
}

#[tokio::test]
async fn dropping_pending_probe_join_set_releases_guard_permit_and_shared_crypto_lock() {
    struct DropSignal(Option<oneshot::Sender<()>>);
    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    let _ = rustls::crypto::ring::default_provider().install_default();
    let controller = Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
    let host = Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
    let (controller_secure, host_secure) = connected_secure_sessions();
    let controller_secure = Arc::new(AsyncMutex::new(controller_secure));
    let host_secure = Arc::new(AsyncMutex::new(host_secure));
    let binding = host_secure.lock().await.video_path_binding();
    let candidate =
        DirectLanCandidate::new(43, host.local_addr().unwrap(), 110, binding, 100).unwrap();
    let permits = Arc::new(Semaphore::new(1));
    let permit = permits.clone().try_acquire_owned().unwrap();
    let (entered, wait_entered) = oneshot::channel();
    let (dropped, wait_dropped) = oneshot::channel();
    let mut probes = tokio::task::JoinSet::new();
    probes.spawn({
        let endpoint = controller.clone();
        let secure = controller_secure.clone();
        async move {
            let _drop_signal = DropSignal(Some(dropped));
            // Reverse declaration/drop order makes the drop signal imply that
            // the owned semaphore permit has already returned to its owner.
            let _permit = permit;
            entered.send(()).unwrap();
            endpoint
                .connect_shared(&candidate, &binding, &secure, 100)
                .await
        }
    });
    wait_entered.await.unwrap();
    let peer = tokio::time::timeout(Duration::from_secs(1), host.accept())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let (_response, mut request) = tokio::time::timeout(Duration::from_secs(1), peer.accept_bi())
        .await
        .unwrap()
        .unwrap();
    let mut length = [0_u8; 4];
    request.read_exact(&mut length).await.unwrap();
    let length = u32::from_be_bytes(length) as usize;
    assert!((1..=512).contains(&length));
    let mut encrypted_request = vec![0_u8; length];
    request.read_exact(&mut encrypted_request).await.unwrap();
    assert!(
        !host_secure
            .lock()
            .await
            .open(SecureLane::DirectProbe, &encrypted_request)
            .unwrap()
            .is_empty()
    );
    // A real authenticated request has arrived, but this peer never sends its
    // response. The production connect future is now pending on network I/O.
    assert_eq!(permits.available_permits(), 0);
    assert!(controller_secure.try_lock().is_ok());
    assert!(!probes.is_empty());
    drop(probes);
    tokio::time::timeout(Duration::from_millis(250), wait_dropped)
        .await
        .expect("pending probe guard was not cancelled promptly")
        .unwrap();
    assert_eq!(permits.available_permits(), 1);
    assert!(controller_secure.try_lock().is_ok());
    assert!(host_secure.try_lock().is_ok());
    // JoinSet cancellation proves task/resource cleanup, not a peer-observed
    // QUIC close deadline. Explicitly close the local fixture connection.
    peer.close(0_u32.into(), b"cancelled probe fixture complete");
}
