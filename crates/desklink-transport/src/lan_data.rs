//! Opt-in, authenticated LAN data plane. See `docs/lan-data-plane.md` for
//! negotiation and runtime integration requirements. Existing video traffic
//! must not share a connection with this envelope until both peers opt in.

use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use desklink_crypto::{
    CryptoError, MAX_ENCRYPTED_MESSAGE_BYTES, MAX_PACKET_PLAINTEXT_BYTES, SecureLane, SecureSession,
};
use rand_core::{OsRng, RngCore};
use thiserror::Error;
use tokio::{
    sync::{Mutex, mpsc, watch},
    task::{JoinHandle, JoinSet},
};

use crate::{ChannelKind, DirectLanConnection, MAX_DATAGRAM_BYTES, TransportError};

const MAGIC: &[u8; 4] = b"DLD1";
const VERSION: u8 = 1;
const HEADER_BYTES: usize = 6;
const RELIABLE_PAYLOAD_MAGIC: &[u8; 4] = b"DLB1";
const FENCE_MAGIC: &[u8; 4] = b"DLF1";
const FENCE_QUERY: u8 = 0x80;
const FENCE_PAYLOAD_BYTES: usize = 4 + 8 + RECEIPT_BYTES;
const RECEIPT_BYTES: usize = 16;
const RELIABLE_PAYLOAD_OVERHEAD: usize = 4 + RECEIPT_BYTES;
// Business ChannelKind values 1..=8 only. Crypto-only lanes such as
// DirectProbe are not transport channels and do not add queue slots here.
const CHANNEL_COUNT: usize = 8;
const MAX_INBOUND_STREAMS: usize = 16;
const PACKET_OVERHEAD: usize = MAX_ENCRYPTED_MESSAGE_BYTES - MAX_PACKET_PLAINTEXT_BYTES;
const DIRECT: u8 = 0;
const RELAY: u8 = 1;
const INTERRUPTED: u8 = 2;

#[derive(Clone, Copy, Debug)]
pub struct LanDataConfig {
    /// How long to wait for an authenticated reliable LAN admission receipt.
    pub direct_timeout: Duration,
    /// Includes lane serialization, LAN attempt, relay fallback and queue waits.
    pub operation_timeout: Duration,
}

impl Default for LanDataConfig {
    fn default() -> Self {
        Self {
            direct_timeout: Duration::from_millis(250),
            operation_timeout: Duration::from_secs(2),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LanDataRoute {
    DirectLan,
    Relay,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum LanDataError {
    #[error("LAN data plane is closed or this reliable lane was interrupted")]
    Closed,
    #[error("LAN data operation timed out")]
    Timeout,
    #[error("LAN data envelope is malformed")]
    Malformed,
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error(transparent)]
    Transport(#[from] TransportError),
}

/// Sends the *same ciphertext* on the existing authenticated relay lane.
/// A backend must preserve reliable-lane order and must never reseal a retry.
/// Its peer feeds the ciphertext to `accept_relay_ciphertext`, not directly
/// to the old runtime decryptor. Relay availability is independent of LAN.
pub trait LanRelayBackend: Send + Sync {
    fn send<'a>(
        &'a self,
        channel: ChannelKind,
        ciphertext: Bytes,
    ) -> impl Future<Output = Result<(), TransportError>> + Send + 'a;
}

pub(crate) struct Ingress {
    pub(crate) secure: Arc<Mutex<SecureSession>>,
    wrapped_channels: u16,
    senders: [mpsc::Sender<Bytes>; CHANNEL_COUNT],
    // A reliable retry is acknowledged only if these exact authenticated
    // bytes were already admitted. ReplayRejected alone is insufficient:
    // it also describes unseen packets older than the crypto replay window.
    last_reliable: [Mutex<Option<AdmittedReliable>>; CHANNEL_COUNT],
    fences: [Mutex<Option<AdmittedFence>>; CHANNEL_COUNT],
    fence_changed: [watch::Sender<u64>; CHANNEL_COUNT],
}

struct AdmittedReliable {
    ciphertext: Bytes,
    receipt: [u8; RECEIPT_BYTES],
}

struct AdmittedFence {
    ciphertext: Bytes,
    receipt: [u8; RECEIPT_BYTES],
    generation: u64,
}

impl Ingress {
    pub(crate) fn new_channels(
        secure: Arc<Mutex<SecureSession>>,
        wrapped_channels: u16,
    ) -> (Arc<Self>, [Mutex<mpsc::Receiver<Bytes>>; CHANNEL_COUNT]) {
        let channels: [_; CHANNEL_COUNT] =
            std::array::from_fn(|i| mpsc::channel(queue_capacity(i)));
        let mut senders = Vec::with_capacity(CHANNEL_COUNT);
        let mut receivers = Vec::with_capacity(CHANNEL_COUNT);
        for (sender, receiver) in channels {
            senders.push(sender);
            receivers.push(Mutex::new(receiver));
        }
        (
            Arc::new(Self {
                secure,
                wrapped_channels,
                senders: senders.try_into().expect("eight channel senders"),
                last_reliable: std::array::from_fn(|_| Mutex::new(None)),
                fences: std::array::from_fn(|_| Mutex::new(None)),
                fence_changed: std::array::from_fn(|_| watch::channel(0).0),
            }),
            receivers.try_into().expect("eight channel receivers"),
        )
    }

    pub(crate) async fn admit(
        &self,
        channel: ChannelKind,
        ciphertext: Bytes,
    ) -> Result<Option<[u8; RECEIPT_BYTES]>, LanDataError> {
        self.admit_from(channel, ciphertext, true).await
    }

    async fn admit_from(
        &self,
        channel: ChannelKind,
        ciphertext: Bytes,
        from_relay: bool,
    ) -> Result<Option<[u8; RECEIPT_BYTES]>, LanDataError> {
        let index = index(channel);
        if channel.is_reliable() {
            let mut last = self.last_reliable[index].lock().await;
            if let Some(admitted) = last.as_ref()
                && admitted.ciphertext == ciphertext
            {
                return Ok(Some(admitted.receipt));
            }
            // Reserve before opening: cancellation or a full consumer queue
            // cannot burn a crypto sequence for a message never delivered.
            let permit = self.senders[index]
                .reserve()
                .await
                .map_err(|_| LanDataError::Closed)?;
            let plaintext = self.secure.lock().await.open(lane(channel), &ciphertext)?;
            if self.wrapped_channels & (1 << index) == 0 {
                permit.send(Bytes::from(plaintext));
                return Ok(None);
            }
            if plaintext.len() == FENCE_PAYLOAD_BYTES && &plaintext[..4] == FENCE_MAGIC {
                if !from_relay {
                    return Err(LanDataError::Malformed);
                }
                let generation = u64::from_be_bytes(plaintext[4..12].try_into().unwrap());
                if generation == 0 {
                    return Err(LanDataError::Malformed);
                }
                let receipt = plaintext[12..].try_into().unwrap();
                *self.fences[index].lock().await = Some(AdmittedFence {
                    ciphertext,
                    receipt,
                    generation,
                });
                self.fence_changed[index]
                    .send_modify(|revision| *revision = revision.wrapping_add(1));
                return Ok(Some(receipt));
            }
            if plaintext.len() < RELIABLE_PAYLOAD_OVERHEAD
                || &plaintext[..4] != RELIABLE_PAYLOAD_MAGIC
            {
                return Err(LanDataError::Malformed);
            }
            let receipt = plaintext[4..RELIABLE_PAYLOAD_OVERHEAD].try_into().unwrap();
            permit.send(Bytes::from(plaintext).slice(RELIABLE_PAYLOAD_OVERHEAD..));
            *last = Some(AdmittedReliable {
                ciphertext,
                receipt,
            });
            return Ok(Some(receipt));
        } else if let Ok(permit) = self.senders[index].try_reserve() {
            let plaintext = self.secure.lock().await.open(lane(channel), &ciphertext)?;
            permit.send(Bytes::from(plaintext));
        }
        Ok(None)
    }

    async fn fence_receipt(
        &self,
        channel: ChannelKind,
        ciphertext: &Bytes,
        expected_generation: u64,
    ) -> Result<[u8; RECEIPT_BYTES], LanDataError> {
        let mut changed = self.fence_changed[index(channel)].subscribe();
        loop {
            if let Some(admitted) = self.fences[index(channel)].lock().await.as_ref()
                && admitted.ciphertext == *ciphertext
                && admitted.generation == expected_generation
            {
                return Ok(admitted.receipt);
            }
            changed.changed().await.map_err(|_| LanDataError::Closed)?;
        }
    }
}

/// Owns the readers for one already authenticated LAN connection and one
/// Noise session. It is the exclusive payload sealer/opener for these lanes.
/// Each reliable lane is serialized through its receipt or relay fallback;
/// unrelated lanes have separate locks, bounded queues and QUIC priorities.
pub struct LanDataPlane {
    connection: quinn::Connection,
    ingress: Arc<Ingress>,
    receivers: [Mutex<mpsc::Receiver<Bytes>>; CHANNEL_COUNT],
    outbound: [Mutex<()>; CHANNEL_COUNT],
    routes: [AtomicU8; CHANNEL_COUNT],
    stopped: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
    config: LanDataConfig,
}

impl LanDataPlane {
    /// This capability is only returned by the existing authenticated probe.
    /// Runtime capability negotiation must happen before calling this method.
    pub fn new(
        authenticated: DirectLanConnection,
        secure: Arc<Mutex<SecureSession>>,
        config: LanDataConfig,
    ) -> Result<Self, LanDataError> {
        if config.direct_timeout.is_zero() || config.operation_timeout < config.direct_timeout {
            return Err(
                TransportError::InvalidConfig("invalid LAN data deadlines".to_owned()).into(),
            );
        }
        let connection = authenticated.authenticated_quic_connection();
        let (ingress, receivers) = Ingress::new_channels(secure, 0xff);
        let (stopped, _) = watch::channel(false);
        let tasks = vec![
            tokio::spawn(read_streams(
                connection.clone(),
                ingress.clone(),
                stopped.subscribe(),
                config,
            )),
            tokio::spawn(read_datagrams(
                connection.clone(),
                ingress.clone(),
                stopped.subscribe(),
            )),
        ];
        Ok(Self {
            connection,
            ingress,
            receivers,
            outbound: std::array::from_fn(|_| Mutex::new(())),
            routes: std::array::from_fn(|_| AtomicU8::new(DIRECT)),
            stopped,
            tasks,
            config,
        })
    }

    pub fn route(&self, channel: ChannelKind) -> Option<LanDataRoute> {
        if *self.stopped.borrow() {
            return None;
        }
        match self.routes[index(channel)].load(Ordering::Acquire) {
            DIRECT => Some(LanDataRoute::DirectLan),
            RELAY => Some(LanDataRoute::Relay),
            _ => None,
        }
    }

    /// Failover is monotonic for a lane's lifetime. Reactivating LAN requires
    /// a fresh negotiated plane, preventing an old relay backlog being
    /// overtaken by newer direct input. Other lanes remain independently live.
    pub fn use_relay(&self, channel: ChannelKind) {
        let _ = self.routes[index(channel)].compare_exchange(
            DIRECT,
            RELAY,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub async fn send(
        &self,
        channel: ChannelKind,
        plaintext: &[u8],
        relay: &impl LanRelayBackend,
    ) -> Result<LanDataRoute, LanDataError> {
        let maximum = if channel.is_reliable() {
            MAX_PACKET_PLAINTEXT_BYTES - RELIABLE_PAYLOAD_OVERHEAD
        } else {
            MAX_DATAGRAM_BYTES - HEADER_BYTES - PACKET_OVERHEAD
        };
        if plaintext.len() > maximum {
            return Err(TransportError::MessageTooLarge {
                actual: plaintext.len(),
                maximum,
            }
            .into());
        }
        let mut stopped = self.stopped.subscribe();
        if *stopped.borrow() {
            return Err(LanDataError::Closed);
        }
        let operation = async {
            let _order = self.outbound[index(channel)].lock().await;
            let route = &self.routes[index(channel)];
            if route.load(Ordering::Acquire) == INTERRUPTED {
                return Err(LanDataError::Closed);
            }
            let mut pending = PendingSend {
                route,
                reliable: channel.is_reliable(),
                completed: false,
            };
            let (ciphertext, receipt) =
                seal_payload(&mut *self.ingress.secure.lock().await, channel, plaintext)?;
            if route.load(Ordering::Acquire) == DIRECT {
                let direct = if channel.is_reliable() {
                    tokio::time::timeout(
                        self.config.direct_timeout,
                        send_reliable(
                            &self.connection,
                            channel,
                            &ciphertext,
                            &receipt.expect("reliable receipt"),
                        ),
                    )
                    .await
                    .map_err(|_| LanDataError::Timeout)
                    .and_then(|result| result)
                } else {
                    send_datagram(&self.connection, channel, &ciphertext)
                };
                if direct.is_ok() {
                    pending.completed = true;
                    return Ok(LanDataRoute::DirectLan);
                }
                self.use_relay(channel);
            }
            relay.send(channel, ciphertext).await?;
            pending.completed = true;
            Ok(LanDataRoute::Relay)
        };
        tokio::select! {
            biased;
            _ = stopped.changed() => Err(LanDataError::Closed),
            result = tokio::time::timeout(self.config.operation_timeout, operation) => {
                result.map_err(|_| LanDataError::Timeout)?
            }
        }
    }

    /// Bridge an existing relay reader into this plane. Pass exact ciphertext
    /// and await admission before reading the next message on that lane.
    pub async fn accept_relay_ciphertext(
        &self,
        channel: ChannelKind,
        ciphertext: Bytes,
    ) -> Result<(), LanDataError> {
        let maximum = if channel.is_reliable() {
            MAX_ENCRYPTED_MESSAGE_BYTES
        } else {
            MAX_DATAGRAM_BYTES - HEADER_BYTES
        };
        if ciphertext.len() > maximum {
            return Err(LanDataError::Malformed);
        }
        let mut stopped = self.stopped.subscribe();
        if *stopped.borrow() {
            return Err(LanDataError::Closed);
        }
        tokio::select! {
            biased;
            _ = stopped.changed() => Err(LanDataError::Closed),
            result = self.ingress.admit(channel, ciphertext) => result.map(|_| ()),
        }
    }

    pub async fn recv(&self, channel: ChannelKind) -> Result<Bytes, LanDataError> {
        let mut stopped = self.stopped.subscribe();
        if *stopped.borrow() {
            return Err(LanDataError::Closed);
        }
        tokio::select! {
            biased;
            _ = stopped.changed() => Err(LanDataError::Closed),
            payload = async { self.receivers[index(channel)].lock().await.recv().await } => {
                payload.ok_or(LanDataError::Closed)
            }
        }
    }

    pub fn close(&self) {
        self.stopped.send_replace(true);
        self.connection
            .close(0_u32.into(), b"LAN data plane closed");
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Drop for LanDataPlane {
    fn drop(&mut self) {
        self.close();
    }
}

struct PendingSend<'a> {
    route: &'a AtomicU8,
    reliable: bool,
    completed: bool,
}

impl Drop for PendingSend<'_> {
    fn drop(&mut self) {
        if self.reliable && !self.completed {
            // A cancelled keyDown may already have reached the peer. Do not
            // replay it or send later input through an ambiguous lane; the
            // runtime must release input and rebuild the session explicitly.
            self.route.store(INTERRUPTED, Ordering::Release);
        }
    }
}

pub(crate) async fn send_reliable(
    connection: &quinn::Connection,
    channel: ChannelKind,
    ciphertext: &[u8],
    expected_receipt: &[u8; RECEIPT_BYTES],
) -> Result<(), LanDataError> {
    send_reliable_kind(connection, channel, ciphertext, expected_receipt, false).await
}

pub(crate) async fn send_fence_query(
    connection: &quinn::Connection,
    channel: ChannelKind,
    ciphertext: &[u8],
    expected_receipt: &[u8; RECEIPT_BYTES],
) -> Result<(), LanDataError> {
    send_reliable_kind(connection, channel, ciphertext, expected_receipt, true).await
}

async fn send_reliable_kind(
    connection: &quinn::Connection,
    channel: ChannelKind,
    ciphertext: &[u8],
    expected_receipt: &[u8; RECEIPT_BYTES],
    fence_query: bool,
) -> Result<(), LanDataError> {
    let (mut send, mut receive) = connection.open_bi().await.map_err(stream_error)?;
    send.set_priority(priority(channel)).map_err(stream_error)?;
    let mut header = [0_u8; HEADER_BYTES + 4];
    header[..4].copy_from_slice(MAGIC);
    header[4] = VERSION;
    header[5] = channel as u8 | if fence_query { FENCE_QUERY } else { 0 };
    header[HEADER_BYTES..].copy_from_slice(&(ciphertext.len() as u32).to_be_bytes());
    send.write_all(&header).await.map_err(stream_error)?;
    send.write_all(ciphertext).await.map_err(stream_error)?;
    send.finish().map_err(stream_error)?;
    let mut receipt = [0_u8; RECEIPT_BYTES];
    receive
        .read_exact(&mut receipt)
        .await
        .map_err(stream_error)?;
    if receipt != *expected_receipt {
        return Err(LanDataError::Malformed);
    }
    Ok(())
}

pub(crate) fn send_datagram(
    connection: &quinn::Connection,
    channel: ChannelKind,
    ciphertext: &[u8],
) -> Result<(), LanDataError> {
    let mut envelope = Vec::with_capacity(HEADER_BYTES + ciphertext.len());
    envelope.extend_from_slice(MAGIC);
    envelope.extend_from_slice(&[VERSION, channel as u8]);
    envelope.extend_from_slice(ciphertext);
    connection
        .send_datagram(envelope.into())
        .map_err(stream_error)
}

pub(crate) async fn read_streams(
    connection: quinn::Connection,
    ingress: Arc<Ingress>,
    stopped: watch::Receiver<bool>,
    config: LanDataConfig,
) {
    read_streams_with_fences(
        connection,
        ingress,
        stopped,
        config,
        Arc::new(std::sync::atomic::AtomicU16::new(0xff)),
        DirectStreamAuthority {
            channels: 0xff,
            generation: 1,
        },
        None,
    )
    .await
}

#[derive(Clone, Copy)]
pub(crate) struct DirectStreamAuthority {
    pub(crate) channels: u16,
    pub(crate) generation: u64,
}

pub(crate) async fn read_streams_with_fences(
    connection: quinn::Connection,
    ingress: Arc<Ingress>,
    mut stopped: watch::Receiver<bool>,
    config: LanDataConfig,
    allowed: Arc<std::sync::atomic::AtomicU16>,
    authority: DirectStreamAuthority,
    task_counter: Option<Arc<crate::business_transport::TaskCounter>>,
) {
    let mut readers = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = stopped.changed() => break,
            _ = readers.join_next(), if !readers.is_empty() => {},
            accepted = connection.accept_bi() => {
                let Ok((mut send, mut receive)) = accepted else { break };
                if readers.len() >= MAX_INBOUND_STREAMS {
                    let _ = receive.stop(1_u32.into());
                    let _ = send.reset(1_u32.into());
                    continue;
                }
                let ingress = ingress.clone();
                let allowed = allowed.clone();
                let lease = task_counter.as_ref().map(|counter| counter.lease());
                readers.spawn(async move {
                    let _lease = lease;
                    let _ = tokio::time::timeout(config.operation_timeout, async {
                        let mut header = [0; HEADER_BYTES + 4];
                        receive.read_exact(&mut header).await.map_err(stream_error)?;
                        let query = header[5] & FENCE_QUERY != 0;
                        header[5] &= !FENCE_QUERY;
                        let channel = decode_header(&header)?;
                        if !channel.is_reliable() || authority.channels & (1 << index(channel)) == 0 || ingress.wrapped_channels & (1 << index(channel)) == 0 { return Err(LanDataError::Malformed); }
                        let length = u32::from_be_bytes(header[HEADER_BYTES..].try_into().unwrap()) as usize;
                        if !(PACKET_OVERHEAD..=MAX_ENCRYPTED_MESSAGE_BYTES).contains(&length) {
                            return Err(LanDataError::Malformed);
                        }
                        let mut ciphertext = vec![0; length];
                        receive.read_exact(&mut ciphertext).await.map_err(stream_error)?;
                        let ciphertext: Bytes = ciphertext.into();
                        let receipt = if query {
                            // Exact relay-admitted ciphertext only: a query
                            // never opens/burns a new sequence ahead of backlog.
                            let receipt = ingress.fence_receipt(channel, &ciphertext, authority.generation).await?;
                            allowed.fetch_or(1 << index(channel), std::sync::atomic::Ordering::AcqRel);
                            receipt
                        } else {
                            if allowed.load(std::sync::atomic::Ordering::Acquire) & (1 << index(channel)) == 0 { return Err(LanDataError::Malformed); }
                            ingress.admit_from(channel, ciphertext, false).await?.ok_or(LanDataError::Malformed)?
                        };
                        send.write_all(&receipt).await.map_err(stream_error)?;
                        send.finish().map_err(stream_error)
                    }).await;
                });
            }
        }
    }
}

pub(crate) async fn read_datagrams(
    connection: quinn::Connection,
    ingress: Arc<Ingress>,
    stopped: watch::Receiver<bool>,
) {
    read_datagrams_for_channels(connection, ingress, stopped, 0xff).await;
}

pub(crate) async fn read_datagrams_for_channels(
    connection: quinn::Connection,
    ingress: Arc<Ingress>,
    mut stopped: watch::Receiver<bool>,
    channels: u16,
) {
    loop {
        tokio::select! {
            biased;
            _ = stopped.changed() => break,
            received = connection.read_datagram() => {
                let Ok(bytes) = received else { break };
                if bytes.len() > MAX_DATAGRAM_BYTES { continue; }
                let Ok(channel) = decode_header(&bytes) else { continue };
                if channel.is_reliable() || channels & (1 << index(channel)) == 0 || ingress.wrapped_channels & (1 << index(channel)) == 0 { continue; }
                let _ = ingress.admit(channel, bytes.slice(HEADER_BYTES..)).await;
            }
        }
    }
}

fn decode_header(bytes: &[u8]) -> Result<ChannelKind, LanDataError> {
    if bytes.len() < HEADER_BYTES || &bytes[..4] != MAGIC || bytes[4] != VERSION {
        return Err(LanDataError::Malformed);
    }
    ChannelKind::try_from(bytes[5]).map_err(|_| LanDataError::Malformed)
}

fn stream_error(error: impl std::fmt::Display) -> LanDataError {
    TransportError::Stream(error.to_string()).into()
}

pub(crate) fn seal_payload(
    secure: &mut SecureSession,
    channel: ChannelKind,
    plaintext: &[u8],
) -> Result<(Bytes, Option<[u8; RECEIPT_BYTES]>), LanDataError> {
    if channel.is_reliable() {
        // The receipt is an unguessable proof that the peer opened this exact
        // packet. A QUIC intermediary can relay the Noise probe but cannot
        // forge admission by echoing visible ciphertext or an old receipt.
        // Receipts never advance the reverse business-lane replay window.
        let mut receipt = [0_u8; RECEIPT_BYTES];
        OsRng.fill_bytes(&mut receipt);
        let mut envelope = Vec::with_capacity(RELIABLE_PAYLOAD_OVERHEAD + plaintext.len());
        envelope.extend_from_slice(RELIABLE_PAYLOAD_MAGIC);
        envelope.extend_from_slice(&receipt);
        envelope.extend_from_slice(plaintext);
        Ok((secure.seal(lane(channel), &envelope)?.into(), Some(receipt)))
    } else {
        Ok((secure.seal(lane(channel), plaintext)?.into(), None))
    }
}

pub(crate) fn seal_fence(
    secure: &mut SecureSession,
    channel: ChannelKind,
    generation: u64,
) -> Result<(Bytes, [u8; RECEIPT_BYTES]), LanDataError> {
    if generation == 0 {
        return Err(LanDataError::Malformed);
    }
    let mut receipt = [0; RECEIPT_BYTES];
    OsRng.fill_bytes(&mut receipt);
    let mut plaintext = Vec::with_capacity(FENCE_PAYLOAD_BYTES);
    plaintext.extend_from_slice(FENCE_MAGIC);
    plaintext.extend_from_slice(&generation.to_be_bytes());
    plaintext.extend_from_slice(&receipt);
    Ok((secure.seal(lane(channel), &plaintext)?.into(), receipt))
}

fn index(channel: ChannelKind) -> usize {
    channel as usize - 1
}

fn queue_capacity(index: usize) -> usize {
    match index {
        0 => 16, // control
        1 => 32, // input
        2 => 4,  // configuration
        3 => 8,  // video datagrams
        4 => 2,  // cursor
        5 => 4,  // file transfer
        6 => 8,  // audio
        _ => 8,  // reliable video
    }
}

fn priority(channel: ChannelKind) -> i32 {
    match channel {
        ChannelKind::Input => 30,
        ChannelKind::Control => 20,
        ChannelKind::VideoConfig => 10,
        ChannelKind::Transfer => -10,
        _ => 0,
    }
}

pub(crate) fn lane(channel: ChannelKind) -> SecureLane {
    match channel {
        ChannelKind::Control => SecureLane::Control,
        ChannelKind::Input => SecureLane::Input,
        ChannelKind::VideoConfig => SecureLane::VideoConfig,
        ChannelKind::VideoDatagram => SecureLane::VideoDatagram,
        ChannelKind::CursorDatagram => SecureLane::CursorDatagram,
        ChannelKind::Transfer => SecureLane::Transfer,
        ChannelKind::AudioDatagram => SecureLane::AudioDatagram,
        ChannelKind::VideoReliable => SecureLane::VideoReliable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DirectLanEndpoint;
    use crate::rng_compat::ChaCha20Rng;
    use desklink_crypto::{DeviceIdentity, NoiseInitiator, NoiseResponder, SecureRole};
    use desklink_protocol::{
        Codec, DirectLanCandidate, InputContext, InputEnvelope, InputEvent, PROTOCOL_VERSION,
        VideoConfig, decode_session_input, decode_video_config, encode_input, encode_video_config,
    };
    use rand_core::SeedableRng;
    use std::sync::atomic::AtomicUsize;

    struct CaptureRelay(Mutex<Vec<(ChannelKind, Bytes)>>);

    impl CaptureRelay {
        fn new() -> Self {
            Self(Mutex::new(Vec::new()))
        }
    }

    impl LanRelayBackend for CaptureRelay {
        async fn send(
            &self,
            channel: ChannelKind,
            ciphertext: Bytes,
        ) -> Result<(), TransportError> {
            self.0.lock().await.push((channel, ciphertext));
            Ok(())
        }
    }

    struct OfflineRelay(AtomicUsize);

    impl LanRelayBackend for OfflineRelay {
        async fn send(&self, _: ChannelKind, _: Bytes) -> Result<(), TransportError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Err(TransportError::Closed)
        }
    }

    struct Fixture {
        _initiator: DirectLanEndpoint,
        _responder: Arc<DirectLanEndpoint>,
        a: Arc<LanDataPlane>,
        b: Arc<LanDataPlane>,
    }

    impl Fixture {
        async fn new() -> Self {
            Self::with_direct_timeout(Duration::from_millis(100)).await
        }

        async fn with_direct_timeout(direct_timeout: Duration) -> Self {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let initiator = DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let responder =
                Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
            let (mut a_secure, mut b_secure) = secure_pair(1);
            let binding = a_secure.video_path_binding();
            let candidate =
                DirectLanCandidate::new(17, responder.local_addr().unwrap(), 110, binding, 100)
                    .unwrap();
            let peer = responder.clone();
            let accepting = tokio::spawn(async move {
                let connection = peer.accept().await.unwrap().unwrap();
                let (authenticated, _) = peer
                    .accept_probe_connection(connection, 17, &binding, &mut b_secure, 100)
                    .await
                    .unwrap();
                (authenticated, b_secure)
            });
            let (a_connection, _) = initiator
                .connect(&candidate, &binding, &mut a_secure, 100)
                .await
                .unwrap();
            let (b_connection, b_secure) = accepting.await.unwrap();
            let config = LanDataConfig {
                direct_timeout,
                operation_timeout: Duration::from_secs(2),
            };
            let a = Arc::new(
                LanDataPlane::new(a_connection, Arc::new(Mutex::new(a_secure)), config).unwrap(),
            );
            let b = Arc::new(
                LanDataPlane::new(b_connection, Arc::new(Mutex::new(b_secure)), config).unwrap(),
            );
            Self {
                _initiator: initiator,
                _responder: responder,
                a,
                b,
            }
        }

        async fn publish_input_context(&self, offered: InputContext) -> InputContext {
            offered.validate().unwrap();
            // The fixture Host explicitly issues the context in a real
            // VideoConfig. The Controller derives its input context from the
            // received config rather than inventing a default epoch.
            let config = VideoConfig {
                protocol_version: PROTOCOL_VERSION,
                stream_id: offered.stream_id,
                input_epoch: offered.input_epoch,
                config_version: 1,
                codec: Codec::H264,
                width: 1920,
                height: 1080,
                sequence_header: vec![0, 0, 0, 1, 0x67, 0x4d, 0, 0x1f],
            };
            let wire = encode_video_config(&config).unwrap();
            let relay = OfflineRelay(AtomicUsize::new(0));
            assert_eq!(
                self.b
                    .send(ChannelKind::VideoConfig, &wire, &relay)
                    .await
                    .unwrap(),
                LanDataRoute::DirectLan,
            );
            let received = receive(&self.a, ChannelKind::VideoConfig).await;
            assert_eq!(received.as_ref(), wire);
            let decoded = decode_video_config(&received).unwrap();
            assert_eq!(decoded, config);
            let captured = InputContext {
                stream_id: decoded.stream_id,
                input_epoch: decoded.input_epoch,
            };
            assert_eq!(captured, offered);
            assert_eq!(relay.0.load(Ordering::Relaxed), 0);
            captured
        }
    }

    fn secure_pair(seed: u8) -> (SecureSession, SecureSession) {
        let a = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([seed; 32]));
        let b = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([seed + 1; 32]));
        let a_key = a.verify_key();
        let b_key = b.verify_key();
        let (mut initiator, first) = NoiseInitiator::start(a, b_key).unwrap();
        let (mut responder, second) = NoiseResponder::accept(&first, b, a_key).unwrap();
        let third = initiator.receive(&second).unwrap();
        responder.receive(&third).unwrap();
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

    async fn receive(plane: &LanDataPlane, channel: ChannelKind) -> Bytes {
        tokio::time::timeout(Duration::from_secs(1), plane.recv(channel))
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn every_lane_works_bidirectionally_without_a_relay() {
        let f = Fixture::new().await;
        let relay = OfflineRelay(AtomicUsize::new(0));
        for number in 1..=8 {
            let channel = ChannelKind::try_from(number).unwrap();
            assert_eq!(
                f.a.send(channel, &[number, 42], &relay).await.unwrap(),
                LanDataRoute::DirectLan
            );
            assert_eq!(receive(&f.b, channel).await.as_ref(), &[number, 42]);
            assert_eq!(
                f.b.send(channel, &[number, 24], &relay).await.unwrap(),
                LanDataRoute::DirectLan
            );
            assert_eq!(receive(&f.a, channel).await.as_ref(), &[number, 24]);
        }
        assert_eq!(relay.0.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn slow_transfer_falls_back_alone_and_preserves_input_audio_and_exactly_once_delivery() {
        let f = Fixture::new().await;
        let relay = CaptureRelay::new();
        for n in 0..4 {
            f.a.send(ChannelKind::Transfer, &[n], &relay).await.unwrap();
        }
        // The receiver's file queue is full; this fifth message cannot obtain
        // a receipt, but the connection can still service unrelated lanes.
        assert_eq!(
            f.a.send(ChannelKind::Transfer, &[4], &relay).await.unwrap(),
            LanDataRoute::Relay
        );
        assert_eq!(f.a.route(ChannelKind::Transfer), Some(LanDataRoute::Relay));
        assert_eq!(f.a.route(ChannelKind::Input), Some(LanDataRoute::DirectLan));
        assert_eq!(
            f.a.send(ChannelKind::Input, b"key down", &relay)
                .await
                .unwrap(),
            LanDataRoute::DirectLan
        );
        assert_eq!(
            receive(&f.b, ChannelKind::Input).await.as_ref(),
            b"key down"
        );
        f.a.send(ChannelKind::AudioDatagram, b"audio", &relay)
            .await
            .unwrap();
        assert_eq!(
            receive(&f.b, ChannelKind::AudioDatagram).await.as_ref(),
            b"audio"
        );
        for n in 0..4 {
            assert_eq!(receive(&f.b, ChannelKind::Transfer).await.as_ref(), &[n]);
        }
        let (channel, retry) = relay.0.lock().await.pop().unwrap();
        f.b.accept_relay_ciphertext(channel, retry).await.unwrap();
        assert_eq!(receive(&f.b, channel).await.as_ref(), &[4]);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), f.b.recv(channel))
                .await
                .is_err()
        );
        assert!(relay.0.lock().await.is_empty());
    }

    #[tokio::test]
    async fn exact_reliable_ciphertext_is_deduplicated_across_local_quic_and_relay() {
        let f = Fixture::new().await;
        let (ciphertext, receipt) = seal_payload(
            &mut *f.a.ingress.secure.lock().await,
            ChannelKind::Input,
            b"one key press",
        )
        .unwrap();
        send_reliable(
            &f.a.connection,
            ChannelKind::Input,
            &ciphertext,
            &receipt.unwrap(),
        )
        .await
        .unwrap();
        // Re-enact a lost LAN receipt: retry exactly the packet on relay.
        f.b.accept_relay_ciphertext(ChannelKind::Input, ciphertext.clone())
            .await
            .unwrap();
        assert_eq!(
            receive(&f.b, ChannelKind::Input).await.as_ref(),
            b"one key press"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), f.b.recv(ChannelKind::Input))
                .await
                .is_err()
        );
        let mut forged = ciphertext.to_vec();
        *forged.last_mut().unwrap() ^= 1;
        assert!(
            f.b.accept_relay_ciphertext(ChannelKind::Input, forged.into())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn foreign_session_and_wrong_lane_do_not_consume_a_valid_input_packet() {
        let f = Fixture::new().await;
        let (ciphertext, _) = seal_payload(
            &mut *f.a.ingress.secure.lock().await,
            ChannelKind::Input,
            b"valid input",
        )
        .unwrap();
        assert!(matches!(
            f.b.accept_relay_ciphertext(ChannelKind::Control, ciphertext.clone())
                .await,
            Err(LanDataError::Crypto(CryptoError::AuthenticationFailed))
        ));
        let (mut foreign, _) = secure_pair(9);
        let other = foreign.seal(SecureLane::Input, b"foreign input").unwrap();
        assert!(matches!(
            f.b.accept_relay_ciphertext(ChannelKind::Input, other.into())
                .await,
            Err(LanDataError::Crypto(CryptoError::AuthenticationFailed))
        ));
        f.b.accept_relay_ciphertext(ChannelKind::Input, ciphertext)
            .await
            .unwrap();
        assert_eq!(
            receive(&f.b, ChannelKind::Input).await.as_ref(),
            b"valid input"
        );
    }

    #[tokio::test]
    async fn unseen_packet_outside_replay_window_is_not_acknowledged_as_a_delivered_retry() {
        let f = Fixture::new().await;
        let (unseen, _) = seal_payload(
            &mut *f.a.ingress.secure.lock().await,
            ChannelKind::Input,
            b"never delivered",
        )
        .unwrap();
        for _ in 0..129 {
            let (newer, _) = seal_payload(
                &mut *f.a.ingress.secure.lock().await,
                ChannelKind::Input,
                b"newer",
            )
            .unwrap();
            f.b.accept_relay_ciphertext(ChannelKind::Input, newer)
                .await
                .unwrap();
            receive(&f.b, ChannelKind::Input).await;
        }
        assert!(matches!(
            f.b.accept_relay_ciphertext(ChannelKind::Input, unseen)
                .await,
            Err(LanDataError::Crypto(CryptoError::ReplayRejected))
        ));
    }

    #[tokio::test]
    async fn closing_wakes_full_queues_and_serialized_senders_without_replaying_cancelled_input() {
        let f = Fixture::new().await;
        let relay = Arc::new(CaptureRelay::new());
        for n in 0..32 {
            f.a.send(ChannelKind::Input, &[n], relay.as_ref())
                .await
                .unwrap();
        }
        let sending = f.a.clone();
        let sending_relay = relay.clone();
        let pending = tokio::spawn(async move {
            sending
                .send(ChannelKind::Input, b"cancel me", sending_relay.as_ref())
                .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        f.a.close();
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(500), pending)
                .await
                .unwrap()
                .unwrap(),
            Err(LanDataError::Closed)
        ));
        assert!(relay.0.lock().await.is_empty());
        assert!(matches!(
            f.a.recv(ChannelKind::Control).await,
            Err(LanDataError::Closed)
        ));
        assert!(matches!(
            f.a.send(ChannelKind::Control, b"late", relay.as_ref())
                .await,
            Err(LanDataError::Closed)
        ));
        assert_eq!(f.a.route(ChannelKind::Input), None);
    }

    #[tokio::test]
    async fn reliable_operation_timeout_poisoning_prevents_later_input_overtaking_ambiguous_send() {
        struct Blackhole;
        impl LanRelayBackend for Blackhole {
            async fn send(&self, _: ChannelKind, _: Bytes) -> Result<(), TransportError> {
                std::future::pending().await
            }
        }
        let f = Fixture::new().await;
        f.a.use_relay(ChannelKind::Input);
        assert!(matches!(
            f.a.send(ChannelKind::Input, b"uncertain", &Blackhole).await,
            Err(LanDataError::Timeout)
        ));
        assert_eq!(f.a.route(ChannelKind::Input), None);
        assert!(matches!(
            f.a.send(ChannelKind::Input, b"must not overtake", &Blackhole)
                .await,
            Err(LanDataError::Closed)
        ));
        let relay = OfflineRelay(AtomicUsize::new(0));
        assert_eq!(
            f.a.send(ChannelKind::Control, b"still independent", &relay)
                .await
                .unwrap(),
            LanDataRoute::DirectLan
        );
        assert_eq!(
            receive(&f.b, ChannelKind::Control).await.as_ref(),
            b"still independent"
        );
    }

    #[tokio::test]
    async fn closed_direct_connection_retries_same_packet_on_relay_and_media_has_a_hard_bound() {
        let f = Fixture::new().await;
        let relay = CaptureRelay::new();
        let too_large = vec![0; MAX_DATAGRAM_BYTES - HEADER_BYTES - PACKET_OVERHEAD + 1];
        assert!(matches!(
            f.a.send(ChannelKind::AudioDatagram, &too_large, &relay)
                .await,
            Err(LanDataError::Transport(
                TransportError::MessageTooLarge { .. }
            ))
        ));
        assert_eq!(
            f.a.route(ChannelKind::AudioDatagram),
            Some(LanDataRoute::DirectLan)
        );
        f.b.close();
        tokio::time::timeout(Duration::from_secs(1), f.a.connection.closed())
            .await
            .unwrap();
        assert_eq!(
            f.a.send(ChannelKind::Input, b"fallback", &relay)
                .await
                .unwrap(),
            LanDataRoute::Relay
        );
        let (channel, ciphertext) = relay.0.lock().await.pop().unwrap();
        assert_eq!(channel, ChannelKind::Input);
        let opened =
            f.b.ingress
                .secure
                .lock()
                .await
                .open(SecureLane::Input, &ciphertext)
                .unwrap();
        assert_eq!(&opened[RELIABLE_PAYLOAD_OVERHEAD..], b"fallback");
    }

    #[tokio::test]
    async fn bare_forged_and_previous_receipts_cannot_hide_dropped_input() {
        for kind in 0..3 {
            let f = Fixture::new().await;
            let relay = CaptureRelay::new();
            f.a.send(ChannelKind::Input, b"earlier", &relay)
                .await
                .unwrap();
            receive(&f.b, ChannelKind::Input).await;
            let earlier = f.b.ingress.last_reliable[index(ChannelKind::Input)]
                .lock()
                .await
                .as_ref()
                .unwrap()
                .receipt;
            f.b.tasks[0].abort();
            // Stand in for a QUIC intermediary after it has forwarded a
            // valid Noise probe. It sees ciphertext, but cannot decrypt the
            // new receipt token and intentionally drops the business input.
            let peer = f.b.connection.clone();
            let malicious = tokio::spawn(async move {
                let (mut response, mut request) = peer.accept_bi().await.unwrap();
                let mut header = [0; HEADER_BYTES + 4];
                request.read_exact(&mut header).await.unwrap();
                let length =
                    u32::from_be_bytes(header[HEADER_BYTES..].try_into().unwrap()) as usize;
                let mut observed = vec![0; length];
                request.read_exact(&mut observed).await.unwrap();
                let forged = match kind {
                    0 => vec![0],
                    1 => vec![0; RECEIPT_BYTES],
                    _ => earlier.to_vec(),
                };
                response.write_all(&forged).await.unwrap();
                response.finish().unwrap();
                observed
            });
            assert_eq!(
                f.a.send(ChannelKind::Input, b"must reach relay", &relay)
                    .await
                    .unwrap(),
                LanDataRoute::Relay
            );
            let observed = malicious.await.unwrap();
            let (channel, retry) = relay.0.lock().await.pop().unwrap();
            assert_eq!(retry.as_ref(), observed);
            f.b.accept_relay_ciphertext(channel, retry).await.unwrap();
            assert_eq!(receive(&f.b, channel).await.as_ref(), b"must reach relay");
        }
    }

    #[tokio::test]
    async fn receipts_do_not_advance_reverse_business_replay_window() {
        let f = Fixture::with_direct_timeout(Duration::from_secs(1)).await;
        let relay = Arc::new(CaptureRelay::new());
        for n in 0..32 {
            f.b.send(ChannelKind::Input, &[n], relay.as_ref())
                .await
                .unwrap();
        }
        let sender = f.b.clone();
        let sender_relay = relay.clone();
        let delayed = tokio::spawn(async move {
            sender
                .send(
                    ChannelKind::Input,
                    b"older reverse payload",
                    sender_relay.as_ref(),
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        for _ in 0..130 {
            f.a.send(ChannelKind::Input, b"forward input", relay.as_ref())
                .await
                .unwrap();
            assert_eq!(
                receive(&f.b, ChannelKind::Input).await.as_ref(),
                b"forward input"
            );
        }
        for n in 0..32 {
            assert_eq!(receive(&f.a, ChannelKind::Input).await.as_ref(), &[n]);
        }
        assert_eq!(delayed.await.unwrap().unwrap(), LanDataRoute::DirectLan);
        assert_eq!(
            receive(&f.a, ChannelKind::Input).await.as_ref(),
            b"older reverse payload"
        );
        assert!(relay.0.lock().await.is_empty());
    }

    #[tokio::test]
    async fn explicit_close_reclaims_readers_even_while_shared_crypto_is_locked() {
        let f = Fixture::new().await;
        let crypto_guard = f.b.ingress.secure.lock().await;
        f.a.send(
            ChannelKind::AudioDatagram,
            b"wait for crypto",
            &CaptureRelay::new(),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        f.b.close();
        tokio::time::timeout(Duration::from_millis(500), async {
            while f.b.tasks.iter().any(|task| !task.is_finished()) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("close must reclaim readers without waiting for the crypto lock");
        drop(crypto_guard);
    }

    #[tokio::test]
    async fn an_early_fence_query_never_opens_new_crypto_or_acknowledges_unseen_relay_data() {
        let f = Fixture::new().await;
        let (older, _) = seal_payload(
            &mut *f.a.ingress.secure.lock().await,
            ChannelKind::Input,
            b"older relay input",
        )
        .unwrap();
        for _ in 0..128 {
            let _ = seal_payload(
                &mut *f.a.ingress.secure.lock().await,
                ChannelKind::Input,
                b"queued ahead of fence",
            )
            .unwrap();
        }
        let (fence, receipt) =
            seal_fence(&mut *f.a.ingress.secure.lock().await, ChannelKind::Input, 1).unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(30),
                send_fence_query(&f.a.connection, ChannelKind::Input, &fence, &receipt)
            )
            .await
            .is_err()
        );
        // If the query had opened sequence 130, this unseen sequence one would
        // now be outside the 128-packet replay window and could not be read.
        f.b.accept_relay_ciphertext(ChannelKind::Input, older)
            .await
            .unwrap();
        assert_eq!(
            receive(&f.b, ChannelKind::Input).await.as_ref(),
            b"older relay input"
        );
        f.b.accept_relay_ciphertext(ChannelKind::Input, fence.clone())
            .await
            .unwrap();
        send_fence_query(&f.a.connection, ChannelKind::Input, &fence, &receipt)
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), f.b.recv(ChannelKind::Input))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_committed_subset_cannot_query_or_deliver_an_unselected_reliable_lane() {
        let f = Fixture::new().await;
        f.b.tasks[0].abort();
        while !f.b.tasks[0].is_finished() {
            tokio::task::yield_now().await;
        }
        let (stopped, receiver) = watch::channel(false);
        let reader = tokio::spawn(read_streams_with_fences(
            f.b.connection.clone(),
            f.b.ingress.clone(),
            receiver,
            f.b.config,
            Arc::new(std::sync::atomic::AtomicU16::new(0)),
            DirectStreamAuthority {
                channels: 0x03,
                generation: 1,
            },
            None,
        ));
        let (fence, token) = seal_fence(
            &mut *f.a.ingress.secure.lock().await,
            ChannelKind::Transfer,
            1,
        )
        .unwrap();
        // Transfer is negotiated in the capability intersection, so relay
        // admission is valid. This connection only committed Input/Control.
        f.b.accept_relay_ciphertext(ChannelKind::Transfer, fence.clone())
            .await
            .unwrap();
        let queried = tokio::time::timeout(
            Duration::from_millis(500),
            send_fence_query(&f.a.connection, ChannelKind::Transfer, &fence, &token),
        )
        .await
        .unwrap();
        assert!(
            queried.is_err(),
            "a real cached fence must not authorize a lane outside the committed subset"
        );
        let (payload, receipt) = seal_payload(
            &mut *f.a.ingress.secure.lock().await,
            ChannelKind::Transfer,
            b"relay-only transfer",
        )
        .unwrap();
        let sent = tokio::time::timeout(
            Duration::from_millis(500),
            send_reliable(
                &f.a.connection,
                ChannelKind::Transfer,
                &payload,
                &receipt.unwrap(),
            ),
        )
        .await
        .unwrap();
        assert!(sent.is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(30), f.b.recv(ChannelKind::Transfer))
                .await
                .is_err()
        );
        // Header rejection also must not open/burn this ciphertext's sequence.
        f.b.accept_relay_ciphertext(ChannelKind::Transfer, payload)
            .await
            .unwrap();
        assert_eq!(
            receive(&f.b, ChannelKind::Transfer).await.as_ref(),
            b"relay-only transfer"
        );
        stopped.send_replace(true);
        reader.await.unwrap();
    }

    #[tokio::test]
    async fn a_new_switch_generation_cannot_reuse_a_previously_admitted_fence() {
        let f = Fixture::new().await;
        let (old_fence, old_token) =
            seal_fence(&mut *f.a.ingress.secure.lock().await, ChannelKind::Input, 1).unwrap();
        f.b.accept_relay_ciphertext(ChannelKind::Input, old_fence.clone())
            .await
            .unwrap();
        f.b.tasks[0].abort();
        while !f.b.tasks[0].is_finished() {
            tokio::task::yield_now().await;
        }
        let (stopped, receiver) = watch::channel(false);
        let reader = tokio::spawn(read_streams_with_fences(
            f.b.connection.clone(),
            f.b.ingress.clone(),
            receiver,
            f.b.config,
            Arc::new(std::sync::atomic::AtomicU16::new(0)),
            DirectStreamAuthority {
                channels: 0x03,
                generation: 2,
            },
            None,
        ));
        assert!(
            tokio::time::timeout(
                Duration::from_millis(30),
                send_fence_query(&f.a.connection, ChannelKind::Input, &old_fence, &old_token)
            )
            .await
            .is_err()
        );
        let (before_fence, token) = seal_payload(
            &mut *f.a.ingress.secure.lock().await,
            ChannelKind::Input,
            b"still on relay",
        )
        .unwrap();
        assert!(
            send_reliable(
                &f.a.connection,
                ChannelKind::Input,
                &before_fence,
                &token.unwrap()
            )
            .await
            .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), f.b.recv(ChannelKind::Input))
                .await
                .is_err()
        );
        f.b.accept_relay_ciphertext(ChannelKind::Input, before_fence)
            .await
            .unwrap();
        assert_eq!(
            receive(&f.b, ChannelKind::Input).await.as_ref(),
            b"still on relay"
        );
        let (new_fence, new_token) =
            seal_fence(&mut *f.a.ingress.secure.lock().await, ChannelKind::Input, 2).unwrap();
        f.b.accept_relay_ciphertext(ChannelKind::Input, new_fence.clone())
            .await
            .unwrap();
        send_fence_query(&f.a.connection, ChannelKind::Input, &new_fence, &new_token)
            .await
            .unwrap();
        let (after_fence, token) = seal_payload(
            &mut *f.a.ingress.secure.lock().await,
            ChannelKind::Input,
            b"generation two direct",
        )
        .unwrap();
        send_reliable(
            &f.a.connection,
            ChannelKind::Input,
            &after_fence,
            &token.unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            receive(&f.b, ChannelKind::Input).await.as_ref(),
            b"generation two direct"
        );
        stopped.send_replace(true);
        reader.await.unwrap();
    }

    #[tokio::test]
    async fn typed_input_preserves_config_issued_context_u64_boundaries_and_leading_zero_over_lan()
    {
        let f = Fixture::new().await;
        let relay = OfflineRelay(AtomicUsize::new(0));
        for offered in [
            InputContext {
                stream_id: 1,
                input_epoch: 1,
            },
            InputContext {
                stream_id: u64::MAX,
                input_epoch: u64::MAX,
            },
            InputContext {
                stream_id: u64::MAX,
                input_epoch: 1,
            },
            InputContext {
                stream_id: 1,
                input_epoch: u64::MAX,
            },
        ] {
            let captured = f.publish_input_context(offered).await;
            let input = InputEnvelope {
                sequence: u64::MAX,
                timestamp_us: u64::MAX,
                stream_id: captured.stream_id,
                input_epoch: captured.input_epoch,
                event: InputEvent::MouseMove { x: 0, y: 1_000_000 },
            };
            let wire = encode_input(&input).unwrap();
            assert_eq!(wire.first(), Some(&0));
            assert_eq!(
                f.a.send(ChannelKind::Input, &wire, &relay).await.unwrap(),
                LanDataRoute::DirectLan,
            );
            let received = receive(&f.b, ChannelKind::Input).await;
            assert_eq!(received.as_ref(), wire);
            assert_eq!(received.first(), Some(&0));
            // This is a transport/codec assertion, without wall-clock,
            // active-epoch or OS input-injection decisions.
            let decoded = decode_session_input(&received).unwrap();
            assert_eq!(decoded, input);
            assert_eq!(decoded.context(), captured);
        }
        assert_eq!(relay.0.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn typed_input_keeps_config_context_and_identical_ciphertext_on_relay_fallback() {
        let f = Fixture::new().await;
        let captured = f
            .publish_input_context(InputContext {
                stream_id: u64::MAX,
                input_epoch: u64::MAX,
            })
            .await;
        let input = InputEnvelope {
            sequence: u64::MAX,
            timestamp_us: u64::MAX,
            stream_id: captured.stream_id,
            input_epoch: captured.input_epoch,
            event: InputEvent::MouseWheel {
                delta_x: -1200,
                delta_y: 1200,
            },
        };
        let wire = encode_input(&input).unwrap();
        assert_eq!(wire.first(), Some(&0));

        f.b.tasks[0].abort();
        tokio::time::timeout(Duration::from_millis(500), async {
            while !f.b.tasks[0].is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let peer = f.b.connection.clone();
        let direct_attempt = tokio::spawn(async move {
            let (mut response, mut request) = peer.accept_bi().await.unwrap();
            let mut header = [0; HEADER_BYTES + 4];
            request.read_exact(&mut header).await.unwrap();
            assert_eq!(decode_header(&header).unwrap(), ChannelKind::Input);
            let length = u32::from_be_bytes(header[HEADER_BYTES..].try_into().unwrap()) as usize;
            let mut observed = vec![0; length];
            request.read_exact(&mut observed).await.unwrap();
            // Simulate a LAN endpoint that received the encrypted packet but
            // cannot prove admission. The sender must retry those same bytes.
            response.finish().unwrap();
            observed
        });
        let relay = CaptureRelay::new();
        assert_eq!(
            f.a.send(ChannelKind::Input, &wire, &relay).await.unwrap(),
            LanDataRoute::Relay,
        );
        let lan_ciphertext = direct_attempt.await.unwrap();
        let (channel, retry) = relay.0.lock().await.pop().unwrap();
        assert_eq!(channel, ChannelKind::Input);
        assert_eq!(retry.as_ref(), lan_ciphertext);
        f.b.accept_relay_ciphertext(channel, retry.clone())
            .await
            .unwrap();
        let received = receive(&f.b, channel).await;
        assert_eq!(received.as_ref(), wire);
        assert_eq!(received.first(), Some(&0));
        let decoded = decode_session_input(&received).unwrap();
        assert_eq!(decoded, input);
        assert_eq!(decoded.context(), captured);
        f.b.accept_relay_ciphertext(channel, retry).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), f.b.recv(channel))
                .await
                .is_err()
        );
        assert!(relay.0.lock().await.is_empty());
    }
}
