use std::net::{IpAddr, SocketAddr, UdpSocket};

use desklink_protocol::DirectLanCandidate;

/// Discovers the local address selected by the operating system for a route.
/// UDP connect does not send application data; it only asks the OS to choose
/// the interface that would reach the supplied peer/relay address.
pub fn discover_local_private_address(route: SocketAddr) -> Option<IpAddr> {
    let bind = match route {
        SocketAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
        SocketAddr::V6(_) => SocketAddr::from(([0; 8], 0)),
    };
    let socket = UdpSocket::bind(bind).ok()?;
    socket.connect(route).ok()?;
    let address = socket.local_addr().ok()?.ip();
    (!address.is_unspecified()).then_some(address)
}

/// Builds a short-lived candidate from the route-selected local interface.
/// Candidate validation remains in the protocol crate, so public addresses,
/// zero ports, invalid IDs, and stale/session-unbound values fail closed.
pub fn make_local_candidate(
    candidate_id: u64,
    route: SocketAddr,
    port: u16,
    session_binding: [u8; 16],
    now_unix_s: u64,
) -> Option<DirectLanCandidate> {
    let address = discover_local_private_address(route)?;
    DirectLanCandidate::new(
        candidate_id,
        SocketAddr::new(address, port),
        now_unix_s.saturating_add(u64::from(desklink_protocol::MAX_DIRECT_LAN_CANDIDATE_TTL_S)),
        session_binding,
        now_unix_s,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_selection_uses_loopback_without_sending_payload() {
        assert_eq!(
            discover_local_private_address(SocketAddr::from(([127, 0, 0, 1], 9))),
            Some(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
        );
    }

    #[test]
    fn candidate_builder_applies_protocol_bounds_and_session_binding() {
        let candidate = make_local_candidate(
            42,
            SocketAddr::from(([127, 0, 0, 1], 9)),
            45_100,
            [7; 16],
            100,
        )
        .expect("loopback candidate");
        assert_eq!(candidate.candidate_id(), 42);
        assert_eq!(
            candidate.address(),
            SocketAddr::from(([127, 0, 0, 1], 45_100))
        );
        assert_eq!(candidate.session_binding(), &[7; 16]);
        assert_eq!(candidate.expires_at_unix_s(), 110);
    }

    #[test]
    fn invalid_candidate_id_fails_closed() {
        assert!(
            make_local_candidate(
                0,
                SocketAddr::from(([127, 0, 0, 1], 9)),
                45_100,
                [7; 16],
                100,
            )
            .is_none()
        );
    }
}
