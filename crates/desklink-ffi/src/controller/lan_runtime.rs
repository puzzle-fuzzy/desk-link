//! Negotiated production LAN ownership. Bootstrap remains on authenticated
//! relay control; all subsequent selected business payloads are opened once
//! by SessionDataPlane, including packets received before VideoConfig.
use super::*;
use desklink_protocol::{LAN_DATA_ENVELOPE_VERSION, LAN_DATA_KNOWN_CHANNELS, LanDataCapabilities};
use desklink_transport::{LanSwitchAction, LanSwitchNegotiator, SessionDataHealth};

pub(super) enum ControllerPayload {
    Ciphertext(Vec<u8>),
    Plaintext(Vec<u8>),
}

impl ControllerPayload {
    pub(super) async fn open(
        self,
        secure: &Arc<Mutex<SecureSession>>,
        lane: SecureLane,
    ) -> Result<Vec<u8>, ControllerError> {
        match self {
            Self::Ciphertext(bytes) => super::open(secure, lane, &bytes).await,
            Self::Plaintext(bytes) => Ok(bytes),
        }
    }

    pub(super) async fn open_datagram(
        self,
        secure: &Arc<Mutex<SecureSession>>,
        lane: SecureLane,
    ) -> Result<Option<Vec<u8>>, ControllerError> {
        match self {
            Self::Ciphertext(bytes) => super::open_datagram(secure, lane, &bytes).await,
            Self::Plaintext(bytes) => Ok(Some(bytes)),
        }
    }
}

pub(super) enum ControllerInboundEvent {
    Data(ChannelKind, ControllerPayload),
    PeerDisconnected { channel: ChannelKind },
    Closed { reason: String },
}

impl From<TransportEvent> for ControllerInboundEvent {
    fn from(event: TransportEvent) -> Self {
        let (channel, bytes) = match event {
            TransportEvent::Control(bytes) => (ChannelKind::Control, bytes),
            TransportEvent::Input(bytes) => (ChannelKind::Input, bytes),
            TransportEvent::VideoConfig(bytes) => (ChannelKind::VideoConfig, bytes),
            TransportEvent::VideoDatagram(bytes) => (ChannelKind::VideoDatagram, bytes),
            TransportEvent::CursorDatagram(bytes) => (ChannelKind::CursorDatagram, bytes),
            TransportEvent::Transfer(bytes) => (ChannelKind::Transfer, bytes),
            TransportEvent::AudioDatagram(bytes) => (ChannelKind::AudioDatagram, bytes),
            TransportEvent::VideoReliable(bytes) => (ChannelKind::VideoReliable, bytes),
            TransportEvent::PeerDisconnected { channel } => {
                return Self::PeerDisconnected { channel };
            }
            TransportEvent::Closed { reason } => return Self::Closed { reason },
        };
        Self::Data(channel, ControllerPayload::Ciphertext(bytes))
    }
}

pub(super) fn controller_capabilities(platform: Platform) -> DeviceCapabilities {
    DeviceCapabilities {
        platform,
        role: DeviceRole::Controller,
        codecs: vec![Codec::H264],
        h264_profiles: supported_h264_profiles(platform),
        width: 1920,
        height: 1080,
        // Other platforms retain legacy mode until their actual native
        // transport lifecycle is validated, even though the ABI is shared.
        lan_data: (platform == Platform::Windows).then_some(LanDataCapabilities {
            envelope_version: LAN_DATA_ENVELOPE_VERSION,
            channels: LAN_DATA_KNOWN_CHANNELS,
            allow_relay_loss_continuation: true,
        }),
    }
}

type ProbeTask =
    tokio::task::JoinHandle<Result<(DirectLanConnection, DirectLanProbeResult), TransportError>>;

pub(super) struct ControllerLanRuntime {
    negotiator: LanSwitchNegotiator,
    pub(super) candidate: Option<DirectLanCandidate>,
    endpoint: Option<Arc<DirectLanEndpoint>>,
    probe: Option<ProbeTask>,
    connection: Option<DirectLanConnection>,
    pending_offer: Option<ControlMessage>,
    failed_candidate: Option<u64>,
    pub(super) quality: Option<VideoPathQuality>,
}

impl ControllerLanRuntime {
    pub(super) fn new(agreement: NegotiatedLanData) -> Self {
        Self {
            negotiator: LanSwitchNegotiator::new(agreement),
            candidate: None,
            endpoint: None,
            probe: None,
            connection: None,
            pending_offer: None,
            failed_candidate: None,
            quality: None,
        }
    }
}

impl Drop for ControllerLanRuntime {
    fn drop(&mut self) {
        if let Some(task) = self.probe.take() {
            task.abort();
        }
        if let Some(connection) = self.connection.take() {
            connection.close(b"controller LAN negotiation retired");
        }
    }
}

impl ControllerRuntime {
    pub fn session_data_health(&self) -> Option<SessionDataHealth> {
        self.data_plane
            .as_ref()
            .map(|plane| *plane.health().borrow())
    }

    pub(super) async fn next_inbound_event(
        &mut self,
        include_transfer: bool,
    ) -> Result<ControllerInboundEvent, ControllerError> {
        let Some(plane) = self.data_plane.clone() else {
            return self
                .next_transport_event(include_transfer)
                .await
                .map(Into::into);
        };
        loop {
            let has_probe = self.lan.as_ref().is_some_and(|lan| lan.probe.is_some());
            let probe_result = tokio::select! {
                result = async { self.lan.as_mut().unwrap().probe.as_mut().unwrap().await }, if has_probe => result,
                event = next_plaintext_event(&plane, include_transfer) => return event,
            };
            let lan = self.lan.as_mut().expect("negotiated plane owns LAN state");
            lan.probe.take();
            match probe_result {
                Ok(Ok((connection, probe))) => {
                    lan.quality = Some(VideoPathQuality {
                        kind: VideoPathKind::DirectLan,
                        rtt_ms: probe.rtt_ms,
                        loss_basis_points: u16::MAX,
                    });
                    lan.connection = Some(connection);
                    if let Some(offer) = lan.pending_offer.take() {
                        self.apply_lan_switch(&offer).await?;
                    }
                }
                _ => {
                    // A failed optional LAN probe leaves the negotiated relay
                    // format and all existing business lanes intact.
                    lan.pending_offer = None;
                    lan.endpoint = None;
                    lan.failed_candidate = lan
                        .candidate
                        .take()
                        .map(|candidate| candidate.candidate_id());
                }
            }
        }
    }

    pub(super) async fn handle_lan_control(
        &mut self,
        message: &ControlMessage,
    ) -> Result<bool, ControllerError> {
        match message {
            ControlMessage::VideoPathCandidateOffer { candidate } => {
                let plane = self
                    .data_plane
                    .as_ref()
                    .ok_or(SessionDataError::InvalidSwitch)?;
                let agreement = plane.agreement().ok_or(SessionDataError::InvalidSwitch)?;
                let lan = self.lan.as_mut().ok_or(SessionDataError::InvalidSwitch)?;
                if let Err(error) = candidate.validate(now_unix_s(), &agreement.session_binding()) {
                    if error == desklink_protocol::DirectLanCandidateError::InvalidExpiry {
                        // A delayed optional candidate must leave the already
                        // authenticated relay session usable. Never probe it
                        // or acknowledge a later switch for this attempt.
                        lan.failed_candidate = Some(candidate.candidate_id());
                        return Ok(true);
                    }
                    return Err(SessionDataError::InvalidSwitch.into());
                }
                if lan.probe.is_some()
                    || lan.connection.is_some()
                    || plane.health().borrow().direct_path.is_some()
                {
                    return Ok(true);
                }
                let bind = if candidate.address().is_ipv6() {
                    "[::]:0"
                } else {
                    "0.0.0.0:0"
                };
                let endpoint = match DirectLanEndpoint::bind(bind.parse().unwrap()) {
                    Ok(endpoint) => Arc::new(endpoint),
                    Err(_) => {
                        lan.failed_candidate = Some(candidate.candidate_id());
                        return Ok(true);
                    }
                };
                let remote = candidate.clone();
                let binding = agreement.session_binding();
                let secure = self.secure.clone();
                let probe_endpoint = endpoint.clone();
                lan.candidate = Some(candidate.clone());
                lan.failed_candidate = None;
                lan.endpoint = Some(endpoint);
                lan.probe = Some(tokio::spawn(async move {
                    tokio::time::timeout(
                        Duration::from_secs(4),
                        probe_endpoint.connect_shared(&remote, &binding, &secure, now_unix_s()),
                    )
                    .await
                    .map_err(|_| {
                        TransportError::Connection("authenticated LAN probe deadline".into())
                    })?
                    .map_err(|_| {
                        TransportError::Connection("authenticated LAN probe failed".into())
                    })
                }));
                Ok(true)
            }
            ControlMessage::LanDataSwitch {
                candidate_id,
                phase,
                ..
            } => {
                let lan = self.lan.as_mut().ok_or(SessionDataError::InvalidSwitch)?;
                if *phase == desklink_protocol::LanDataSwitchPhase::Offer
                    && lan.failed_candidate == Some(*candidate_id)
                {
                    // The optional attempt has retired. A delayed Offer must
                    // neither produce Ready without a connection nor end a
                    // healthy relay session after a probe deadline.
                    return Ok(true);
                }
                if lan.connection.is_none()
                    && lan.probe.is_some()
                    && *phase == desklink_protocol::LanDataSwitchPhase::Offer
                {
                    if lan.pending_offer.replace(message.clone()).is_some() {
                        return Err(SessionDataError::InvalidSwitch.into());
                    }
                } else {
                    self.apply_lan_switch(message).await?;
                }
                Ok(true)
            }
            ControlMessage::VideoPathCandidateAnswer { .. } => {
                // New mode has exactly one initiator: Controller probes the
                // Host candidate; only LanDataSwitch commits the data route.
                Err(SessionDataError::InvalidSwitch.into())
            }
            _ => Ok(false),
        }
    }

    async fn apply_lan_switch(&mut self, message: &ControlMessage) -> Result<(), ControllerError> {
        let lan = self.lan.as_mut().ok_or(SessionDataError::InvalidSwitch)?;
        let candidate = lan
            .candidate
            .as_ref()
            .ok_or(SessionDataError::InvalidSwitch)?;
        let action = lan.negotiator.receive(message, candidate, now_unix_s())?;
        match action {
            LanSwitchAction::SendReady(reply) => self.send_controller_control(reply).await?,
            LanSwitchAction::Attach(authority) => {
                let connection = lan
                    .connection
                    .take()
                    .ok_or(SessionDataError::InvalidSwitch)?;
                self.data_plane
                    .as_ref()
                    .ok_or(SessionDataError::InvalidSwitch)?
                    .attach_direct(authority, connection)
                    .await?;
            }
            // Host is the single route-switch proposer in this runtime.
            LanSwitchAction::SendCommit(_) => return Err(SessionDataError::InvalidSwitch.into()),
        }
        Ok(())
    }
}

async fn next_plaintext_event(
    plane: &SessionDataPlane,
    include_transfer: bool,
) -> Result<ControllerInboundEvent, ControllerError> {
    let (channel, result) = tokio::select! {
        result = plane.recv(ChannelKind::Control) => (ChannelKind::Control, result.map(|bytes| bytes.to_vec())),
        result = plane.recv(ChannelKind::Input) => (ChannelKind::Input, result.map(|bytes| bytes.to_vec())),
        result = recv_optional_lane(plane, ChannelKind::VideoConfig) => (ChannelKind::VideoConfig, result),
        result = recv_optional_lane(plane, ChannelKind::VideoReliable) => (ChannelKind::VideoReliable, result),
        result = recv_optional_lane(plane, ChannelKind::VideoDatagram) => (ChannelKind::VideoDatagram, result),
        result = recv_optional_lane(plane, ChannelKind::CursorDatagram) => (ChannelKind::CursorDatagram, result),
        result = recv_optional_lane(plane, ChannelKind::AudioDatagram) => (ChannelKind::AudioDatagram, result),
        result = recv_optional_lane(plane, ChannelKind::Transfer), if include_transfer => (ChannelKind::Transfer, result),
    };
    Ok(ControllerInboundEvent::Data(
        channel,
        ControllerPayload::Plaintext(result?),
    ))
}

async fn recv_optional_lane(
    plane: &SessionDataPlane,
    channel: ChannelKind,
) -> Result<Vec<u8>, SessionDataError> {
    let mut health = plane.health();
    loop {
        match plane.recv(channel).await {
            Err(SessionDataError::NoPath(_) | SessionDataError::Interrupted(_)) => {
                // A missing optional lane does not make healthy input/control
                // fail. Wait for another committed path or owning-session end.
                loop {
                    if health.borrow().closed {
                        return Err(SessionDataError::Closed);
                    }
                    health
                        .changed()
                        .await
                        .map_err(|_| SessionDataError::Closed)?;
                    let state = health.borrow();
                    if matches!(
                        state.lanes[channel as usize - 1],
                        SessionLaneHealth::Direct(_)
                    ) || (state.relay_connected
                        && matches!(state.lanes[channel as usize - 1], SessionLaneHealth::Relay))
                    {
                        break;
                    }
                }
            }
            result => return result.map(|bytes| bytes.to_vec()),
        }
    }
}

impl Drop for ControllerRuntime {
    fn drop(&mut self) {
        if let Some(plane) = &self.data_plane {
            plane.close();
        }
        if let Some(connection) = &self.direct_connection {
            connection.close(b"controller session retired");
        }
        self.client.close(b"controller session retired");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retired_optional_probe_ignores_late_offer_and_keeps_authenticated_relay_control() {
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut fixture =
                super::super::direct_silence_tests::connected_flow_control_fixture().await;
            let local = controller_capabilities(Platform::Windows);
            let mut remote = fixture.runtime.host_capabilities.clone();
            remote.lan_data = local.lan_data;
            // Exchange both real capability codecs over their authenticated
            // relay lane before creating the test's two business planes.
            fixture
                .host
                .send_control(
                    seal(
                        &fixture.host_secure,
                        SecureLane::Control,
                        &encode_control(&ControlMessage::Capabilities(remote.clone())).unwrap(),
                    )
                    .await
                    .unwrap(),
                )
                .await
                .unwrap();
            let wire = fixture.runtime.client.next_control().await.unwrap();
            assert_eq!(
                decode_control(
                    &open(&fixture.runtime.secure, SecureLane::Control, &wire)
                        .await
                        .unwrap()
                )
                .unwrap(),
                ControlMessage::Capabilities(remote.clone())
            );
            fixture
                .runtime
                .client
                .send_control(
                    seal(
                        &fixture.runtime.secure,
                        SecureLane::Control,
                        &encode_control(&ControlMessage::Capabilities(local.clone())).unwrap(),
                    )
                    .await
                    .unwrap(),
                )
                .await
                .unwrap();
            let wire = fixture.host.next_control().await.unwrap();
            assert_eq!(
                decode_control(
                    &open(&fixture.host_secure, SecureLane::Control, &wire)
                        .await
                        .unwrap()
                )
                .unwrap(),
                ControlMessage::Capabilities(local.clone())
            );
            let binding = fixture.runtime.secure.lock().await.video_path_binding();
            let agreement = NegotiatedLanData::from_authenticated_caps(
                &local,
                &remote,
                binding,
                fixture.runtime.client.peer_generation(),
            )
            .unwrap()
            .unwrap();
            let host_agreement = NegotiatedLanData::from_authenticated_caps(
                &remote,
                &local,
                binding,
                fixture.host.peer_generation(),
            )
            .unwrap()
            .unwrap();
            let host = SessionDataPlane::relay_only(
                host_agreement,
                fixture.host_secure.clone(),
                Arc::new(
                    QuicRelayBackend::new(fixture.host.clone(), fixture.host.peer_generation())
                        .unwrap(),
                ),
            )
            .await
            .unwrap();
            let plane = SessionDataPlane::relay_only(
                agreement,
                fixture.runtime.secure.clone(),
                Arc::new(
                    QuicRelayBackend::new(
                        fixture.runtime.client.clone(),
                        fixture.runtime.client.peer_generation(),
                    )
                    .unwrap(),
                ),
            )
            .await
            .unwrap();
            fixture.runtime.data_plane = Some(plane);
            fixture.runtime.lan = Some(ControllerLanRuntime::new(agreement));
            let now = now_unix_s();
            let expired = DirectLanCandidate::new(
                991,
                "127.0.0.1:9".parse().unwrap(),
                now - 1,
                binding,
                now - 2,
            )
            .unwrap();
            assert!(
                fixture
                    .runtime
                    .handle_lan_control(&ControlMessage::VideoPathCandidateOffer {
                        candidate: expired,
                    })
                    .await
                    .unwrap()
            );
            let late = ControlMessage::LanDataSwitch {
                candidate_id: 991,
                generation: 1,
                channels: agreement.channels(),
                phase: desklink_protocol::LanDataSwitchPhase::Offer,
            };
            assert!(fixture.runtime.handle_lan_control(&late).await.unwrap());
            assert!(
                tokio::time::timeout(Duration::from_millis(30), host.recv(ChannelKind::Control))
                    .await
                    .is_err(),
                "retired probe must not emit Ready without an authenticated connection"
            );
            let control = ControlMessage::SetAudioEnabled { enabled: true };
            fixture
                .runtime
                .send_controller_control(control.clone())
                .await
                .unwrap();
            assert_eq!(
                decode_control(&host.recv(ChannelKind::Control).await.unwrap()).unwrap(),
                control
            );
            assert!(!fixture.runtime.client.is_closed());
            host.close();
        })
        .await
        .expect("retired optional probe regression must remain bounded");
    }
}
