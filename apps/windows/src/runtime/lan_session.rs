//! Host business transport and the Host-initiated protocol-14 route barrier.
use super::*;
use desklink_protocol::{
    DirectLanCandidate, LAN_DATA_ENVELOPE_VERSION, LAN_DATA_KNOWN_CHANNELS, LanDataCapabilities,
};
use desklink_transport::{LanSwitchAction, LanSwitchNegotiator, SessionLaneHealth};

pub(super) fn capabilities() -> LanDataCapabilities {
    LanDataCapabilities {
        envelope_version: LAN_DATA_ENVELOPE_VERSION,
        channels: LAN_DATA_KNOWN_CHANNELS,
        allow_relay_loss_continuation: true,
    }
}

pub(super) fn video_is_direct(data: &SessionDataPlane) -> bool {
    matches!(
        data.health().borrow().lanes[ChannelKind::VideoDatagram as usize - 1],
        SessionLaneHealth::Direct(_)
    )
}

/// One Host offers its own authenticated candidate. A probe cannot authorize
/// business traffic until the exact Offer/Ready/Commit has completed.
pub(super) struct HostLanSwitch {
    negotiator: Option<LanSwitchNegotiator>,
    candidate: Option<DirectLanCandidate>,
    pending: Option<DirectLanConnection>,
}

impl HostLanSwitch {
    pub(super) fn new(data: &SessionDataPlane, candidate: Option<DirectLanCandidate>) -> Self {
        Self {
            negotiator: data.agreement().map(LanSwitchNegotiator::new),
            candidate,
            pending: None,
        }
    }

    pub(super) async fn probed(
        &mut self,
        data: &SessionDataPlane,
        connection: DirectLanConnection,
    ) -> Result<(), HostRuntimeError> {
        let Some(negotiator) = self.negotiator.as_mut() else {
            return Err(SessionDataError::InvalidSwitch.into());
        };
        if self.pending.is_some() || data.health().borrow().direct_path.is_some() {
            connection.close(b"Host LAN path already selected");
            return Ok(());
        }
        let candidate = self
            .candidate
            .as_ref()
            .ok_or(SessionDataError::InvalidSwitch)?;
        if connection.candidate_id() != candidate.candidate_id()
            || connection.session_binding() != candidate.session_binding()
        {
            connection.close(b"Host LAN candidate mismatch");
            return Err(SessionDataError::InvalidSwitch.into());
        }
        let channels = data
            .agreement()
            .ok_or(SessionDataError::InvalidSwitch)?
            .channels();
        let offer = negotiator.offer(candidate, channels, now_unix_s())?;
        self.pending = Some(connection);
        send_control_data(data, &offer).await
    }

    pub(super) async fn receive(
        &mut self,
        data: &SessionDataPlane,
        message: &ControlMessage,
    ) -> Result<(), HostRuntimeError> {
        // Controller is never the route-switch initiator for this runtime.
        if !matches!(
            message,
            ControlMessage::LanDataSwitch {
                phase: desklink_protocol::LanDataSwitchPhase::Ready,
                ..
            }
        ) {
            return Err(SessionDataError::InvalidSwitch.into());
        }
        let candidate = self
            .candidate
            .as_ref()
            .ok_or(SessionDataError::InvalidSwitch)?;
        let negotiator = self
            .negotiator
            .as_mut()
            .ok_or(SessionDataError::InvalidSwitch)?;
        let LanSwitchAction::SendCommit(commit) =
            negotiator.receive(message, candidate, now_unix_s())?
        else {
            return Err(SessionDataError::InvalidSwitch.into());
        };
        send_control_data(data, &commit).await?;
        let authority = negotiator.commit_sent()?;
        let connection = self.pending.take().ok_or(SessionDataError::InvalidSwitch)?;
        data.attach_direct(authority, connection).await?;
        Ok(())
    }
}

impl Drop for HostLanSwitch {
    fn drop(&mut self) {
        if let Some(connection) = self.pending.take() {
            connection.close(b"Host LAN switch ended before commit");
        }
    }
}

pub(super) async fn send_control_data(
    data: &SessionDataPlane,
    message: &ControlMessage,
) -> Result<(), HostRuntimeError> {
    data.send(ChannelKind::Control, encode_control(message)?.into())
        .await?;
    Ok(())
}

pub(super) struct HostDataGuard(pub(super) Arc<SessionDataPlane>);
impl Drop for HostDataGuard {
    fn drop(&mut self) {
        self.0.close();
    }
}

pub(super) fn is_datagram_congestion(error: &SessionDataError) -> bool {
    matches!(
        error,
        SessionDataError::Transport(TransportError::Datagram(_))
            | SessionDataError::Lan(desklink_transport::LanDataError::Transport(
                TransportError::Datagram(_)
            ))
    )
}

/// Losing a non-control lane cannot revoke a healthy authenticated Input and
/// Control path. The finished service has already dropped/cancelled its file
/// workers; park it until the owning session ends instead of replaying work.
pub(super) async fn optional_lane<F>(
    data: Arc<SessionDataPlane>,
    service: F,
) -> Result<(), HostRuntimeError>
where
    F: std::future::Future<Output = Result<(), HostRuntimeError>>,
{
    let result = service.await;
    if matches!(&result, Err(HostRuntimeError::SessionData(
        SessionDataError::NoPath(channel) | SessionDataError::Interrupted(channel)
    )) if !matches!(channel, ChannelKind::Input | ChannelKind::Control))
    {
        let mut health = data.health();
        loop {
            if health.borrow().closed {
                return result;
            }
            if health.changed().await.is_err() {
                return result;
            }
        }
    }
    result
}

/// Confirmation retains the original explicit peer-disconnect requirement.
/// A WAN failure or replacement peer cannot acknowledge local revocation.
pub(super) async fn notify_local_control_ended_data(data: &SessionDataPlane) -> bool {
    let disconnected_before_notice = data.health().borrow().peer_disconnected;
    tokio::time::timeout(END_CONTROL_TIMEOUT, async {
        send_control_data(
            data,
            &ControlMessage::AccessDenied {
                reason: AccessDenialReason::HostUnavailable,
            },
        )
        .await?;
        let mut health = data.health();
        loop {
            let state = *health.borrow();
            if state.peer_replaced {
                return Ok(false);
            }
            if state.peer_disconnected {
                return Ok(!disconnected_before_notice);
            }
            if state.closed {
                return Ok(false);
            }
            if health.changed().await.is_err() {
                return Ok(false);
            }
        }
    })
    .await
    .is_ok_and(|result: Result<bool, HostRuntimeError>| result.unwrap_or(false))
}
