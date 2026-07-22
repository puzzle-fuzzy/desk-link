use std::{future::Future, sync::Arc};

use crate::{DirectLanConnection, QuicClient, TransportError};

/// The selected video datagram route. Control, input, approval, clipboard,
/// and file lanes are deliberately not represented here and remain on the
/// authenticated relay connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoDatagramRoute {
    Relay,
    DirectLan { candidate_id: u64 },
}

/// Common zero-boxing interface for the video datagram data plane. The relay
/// implementation is active today; the DirectLan implementation attaches a
/// separately authenticated QUIC socket without changing frame packetizing or
/// keyframe recovery in the Windows runtime.
pub trait VideoDatagramBackend: Send + Sync {
    fn route(&self) -> VideoDatagramRoute;

    fn send<'a>(
        &'a self,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<(), TransportError>> + Send + 'a;
}

#[derive(Clone)]
pub struct RelayVideoPath {
    client: Arc<QuicClient>,
    peer_generation: u64,
}

impl RelayVideoPath {
    pub fn new(client: Arc<QuicClient>, peer_generation: u64) -> Self {
        Self {
            client,
            peer_generation,
        }
    }
}

impl VideoDatagramBackend for RelayVideoPath {
    fn route(&self) -> VideoDatagramRoute {
        VideoDatagramRoute::Relay
    }

    fn send<'a>(
        &'a self,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<(), TransportError>> + Send + 'a {
        self.client
            .send_video_datagram_for_generation(self.peer_generation, bytes)
    }
}

#[derive(Clone)]
pub struct DirectLanVideoPath {
    connection: std::sync::Arc<DirectLanConnection>,
}

impl DirectLanVideoPath {
    pub fn new(connection: std::sync::Arc<DirectLanConnection>) -> Self {
        Self { connection }
    }
}

impl VideoDatagramBackend for DirectLanVideoPath {
    fn route(&self) -> VideoDatagramRoute {
        VideoDatagramRoute::DirectLan {
            candidate_id: self.connection.candidate_id(),
        }
    }

    fn send<'a>(
        &'a self,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<(), TransportError>> + Send + 'a {
        std::future::ready(self.connection.send_datagram(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_backend_is_explicitly_classified() {
        assert_eq!(VideoDatagramRoute::Relay, VideoDatagramRoute::Relay);
    }
}
