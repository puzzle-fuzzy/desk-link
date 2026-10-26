//! One owner for business-lane encryption, ingress and route selection after
//! authenticated capability negotiation. Bootstrap Noise/capability traffic
//! remains outside this module; a session never changes its wire mode later.

use std::{
    sync::{Arc, Mutex as StdMutex, MutexGuard, atomic::AtomicU16},
    time::{SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use desklink_crypto::{
    CryptoError, MAX_ENCRYPTED_MESSAGE_BYTES, MAX_PACKET_PLAINTEXT_BYTES, SecureSession,
};
use desklink_protocol::{
    ControlMessage, DeviceCapabilities, DirectLanCandidate, LanDataCapabilities, LanDataSwitchPhase,
};
use thiserror::Error;
use tokio::{
    sync::{Mutex, mpsc, oneshot, watch},
    task::JoinHandle,
};

use crate::lan_data::{
    DirectStreamAuthority, Ingress, lane, read_datagrams_for_channels, read_streams_with_fences,
    seal_fence, seal_payload, send_datagram, send_fence_query, send_reliable,
};
use crate::{
    ChannelKind, DirectLanConnection, LanDataConfig, LanDataError, LanDataRoute, LanRelayBackend,
    MAX_DATAGRAM_BYTES, QuicClient, TransportError,
};

const LANES: usize = 8;
const INTERACTIVE_CHANNELS: u16 = 0x03;
const RECEIPT_OVERHEAD: usize = 20;
const CIPHER_OVERHEAD: usize = MAX_ENCRYPTED_MESSAGE_BYTES - MAX_PACKET_PLAINTEXT_BYTES;

pub(crate) struct TaskCounter {
    active: watch::Sender<usize>,
}
pub(crate) struct TaskLease {
    counter: Arc<TaskCounter>,
}
impl TaskCounter {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            active: watch::channel(0).0,
        })
    }
    pub(crate) fn lease(self: &Arc<Self>) -> TaskLease {
        self.active.send_modify(|active| *active += 1);
        TaskLease {
            counter: self.clone(),
        }
    }
    fn spawn(
        self: &Arc<Self>,
        future: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> JoinHandle<()> {
        let lease = self.lease();
        tokio::spawn(async move {
            let _lease = lease;
            future.await;
        })
    }
    async fn wait_empty(&self) {
        let mut active = self.active.subscribe();
        loop {
            if *active.borrow_and_update() == 0 {
                return;
            }
            if active.changed().await.is_err() {
                return;
            }
        }
    }
}
impl Drop for TaskLease {
    fn drop(&mut self) {
        self.counter.active.send_modify(|active| *active -= 1);
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SessionDataError {
    #[error("business transport is closed")]
    Closed,
    #[error("authenticated LAN capability negotiation is invalid")]
    InvalidNegotiation,
    #[error("LAN switch is stale, out of order or bound to another session")]
    InvalidSwitch,
    #[error("a healthy authenticated LAN path is already installed")]
    AlreadyAttached,
    #[error("no authenticated route is available for {0:?}")]
    NoPath(ChannelKind),
    #[error("reliable delivery was interrupted for {0:?}")]
    Interrupted(ChannelKind),
    #[error("business transport operation timed out")]
    Timeout,
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Lan(#[from] LanDataError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NegotiatedLanData {
    capabilities: LanDataCapabilities,
    binding: [u8; 16],
    relay_generation: u64,
}

impl NegotiatedLanData {
    /// Both values must be the real capabilities exchanged inside this Noise
    /// session. An absent/nonoverlapping capability selects explicit legacy.
    pub fn from_authenticated_caps(
        local: &DeviceCapabilities,
        peer: &DeviceCapabilities,
        session_binding: [u8; 16],
        relay_peer_generation: u64,
    ) -> Result<Option<Self>, SessionDataError> {
        local
            .validate()
            .map_err(|_| SessionDataError::InvalidNegotiation)?;
        peer.validate()
            .map_err(|_| SessionDataError::InvalidNegotiation)?;
        if local.role == peer.role || session_binding == [0; 16] || relay_peer_generation == 0 {
            return Err(SessionDataError::InvalidNegotiation);
        }
        Ok(local.negotiated_lan_data(peer).map(|capabilities| Self {
            capabilities,
            binding: session_binding,
            relay_generation: relay_peer_generation,
        }))
    }

    pub const fn channels(self) -> u16 {
        self.capabilities.channels
    }
    pub const fn session_binding(self) -> [u8; 16] {
        self.binding
    }
    pub const fn relay_peer_generation(self) -> u64 {
        self.relay_generation
    }
    pub const fn allow_relay_loss_continuation(self) -> bool {
        self.capabilities.allow_relay_loss_continuation
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectPathId(pub u64);

#[derive(Debug)]
pub struct LanAttachAuthority {
    agreement: NegotiatedLanData,
    candidate: DirectLanCandidate,
    generation: u64,
    channels: u16,
}

impl LanAttachAuthority {
    pub const fn generation(&self) -> u64 {
        self.generation
    }
    pub const fn candidate_id(&self) -> u64 {
        self.candidate.candidate_id()
    }
    pub const fn channels(&self) -> u16 {
        self.channels
    }
}

#[derive(Debug)]
pub enum LanSwitchAction {
    SendReady(ControlMessage),
    SendCommit(ControlMessage),
    Attach(LanAttachAuthority),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SwitchStage {
    Offered,
    ReadySent,
    CommitPending,
}

struct PendingSwitch {
    candidate: DirectLanCandidate,
    generation: u64,
    channels: u16,
    stage: SwitchStage,
}

pub struct LanSwitchNegotiator {
    agreement: NegotiatedLanData,
    last_generation: u64,
    pending: Option<PendingSwitch>,
}

impl LanSwitchNegotiator {
    pub fn new(agreement: NegotiatedLanData) -> Self {
        Self {
            agreement,
            last_generation: 0,
            pending: None,
        }
    }

    pub fn offer(
        &mut self,
        candidate: &DirectLanCandidate,
        channels: u16,
        now_unix_s: u64,
    ) -> Result<ControlMessage, SessionDataError> {
        let generation = self
            .last_generation
            .checked_add(1)
            .ok_or(SessionDataError::InvalidSwitch)?;
        self.validate(candidate, generation, channels, now_unix_s)?;
        if self.pending.is_some() {
            return Err(SessionDataError::InvalidSwitch);
        }
        self.pending = Some(PendingSwitch {
            candidate: candidate.clone(),
            generation,
            channels,
            stage: SwitchStage::Offered,
        });
        Ok(switch(
            candidate.candidate_id(),
            generation,
            channels,
            LanDataSwitchPhase::Offer,
        ))
    }

    pub fn receive(
        &mut self,
        message: &ControlMessage,
        candidate: &DirectLanCandidate,
        now_unix_s: u64,
    ) -> Result<LanSwitchAction, SessionDataError> {
        let ControlMessage::LanDataSwitch {
            candidate_id,
            generation,
            channels,
            phase,
        } = *message
        else {
            return Err(SessionDataError::InvalidSwitch);
        };
        if candidate.candidate_id() != candidate_id {
            return Err(SessionDataError::InvalidSwitch);
        }
        self.validate(candidate, generation, channels, now_unix_s)?;
        match phase {
            LanDataSwitchPhase::Offer => {
                if self.pending.is_some() {
                    return Err(SessionDataError::InvalidSwitch);
                }
                self.pending = Some(PendingSwitch {
                    candidate: candidate.clone(),
                    generation,
                    channels,
                    stage: SwitchStage::ReadySent,
                });
                Ok(LanSwitchAction::SendReady(switch(
                    candidate_id,
                    generation,
                    channels,
                    LanDataSwitchPhase::Ready,
                )))
            }
            LanDataSwitchPhase::Ready | LanDataSwitchPhase::Commit => {
                let pending = self
                    .pending
                    .as_mut()
                    .ok_or(SessionDataError::InvalidSwitch)?;
                if pending.candidate != *candidate
                    || pending.generation != generation
                    || pending.channels != channels
                {
                    return Err(SessionDataError::InvalidSwitch);
                }
                if phase == LanDataSwitchPhase::Ready && pending.stage == SwitchStage::Offered {
                    pending.stage = SwitchStage::CommitPending;
                    Ok(LanSwitchAction::SendCommit(switch(
                        candidate_id,
                        generation,
                        channels,
                        LanDataSwitchPhase::Commit,
                    )))
                } else if phase == LanDataSwitchPhase::Commit
                    && pending.stage == SwitchStage::ReadySent
                {
                    Ok(LanSwitchAction::Attach(self.finish_pending()?))
                } else {
                    Err(SessionDataError::InvalidSwitch)
                }
            }
        }
    }

    /// Call only after the authenticated Commit has completed its reliable
    /// send. A failed/cancelled send cannot mint attachment authority.
    pub fn commit_sent(&mut self) -> Result<LanAttachAuthority, SessionDataError> {
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.stage != SwitchStage::CommitPending)
        {
            return Err(SessionDataError::InvalidSwitch);
        }
        self.finish_pending()
    }

    fn finish_pending(&mut self) -> Result<LanAttachAuthority, SessionDataError> {
        let pending = self.pending.take().ok_or(SessionDataError::InvalidSwitch)?;
        self.last_generation = pending.generation;
        Ok(LanAttachAuthority {
            agreement: self.agreement,
            candidate: pending.candidate,
            generation: pending.generation,
            channels: pending.channels,
        })
    }

    fn validate(
        &self,
        candidate: &DirectLanCandidate,
        generation: u64,
        channels: u16,
        now: u64,
    ) -> Result<(), SessionDataError> {
        candidate
            .validate(now, &self.agreement.binding)
            .map_err(|_| SessionDataError::InvalidSwitch)?;
        if generation == 0
            || generation <= self.last_generation
            || channels == 0
            || channels & !self.agreement.channels() != 0
        {
            return Err(SessionDataError::InvalidSwitch);
        }
        Ok(())
    }
}

fn switch(
    candidate_id: u64,
    generation: u64,
    channels: u16,
    phase: LanDataSwitchPhase,
) -> ControlMessage {
    ControlMessage::LanDataSwitch {
        candidate_id,
        generation,
        channels,
        phase,
    }
}

pub struct QuicRelayBackend {
    client: Arc<QuicClient>,
    generation: u64,
    writers: [Option<mpsc::Sender<RelayWrite>>; LANES],
    tasks: StdMutex<Vec<JoinHandle<()>>>,
    stopped: watch::Sender<bool>,
    task_counter: Arc<TaskCounter>,
}

struct RelayWrite {
    ciphertext: Bytes,
    result: oneshot::Sender<Result<(), TransportError>>,
}

impl QuicRelayBackend {
    pub fn new(client: Arc<QuicClient>, peer_generation: u64) -> Result<Self, SessionDataError> {
        if peer_generation == 0 || client.peer_generation() != peer_generation {
            return Err(TransportError::PeerReplaced.into());
        }
        let (stopped, _) = watch::channel(false);
        let task_counter = TaskCounter::new();
        let mut tasks = Vec::new();
        let writers = std::array::from_fn(|index| {
            let channel = ChannelKind::try_from(index as u8 + 1).expect("known business lane");
            if !channel.is_reliable() {
                return None;
            }
            // A caller owns only its reply wait. In particular, timing out a
            // fence or file send must not cancel a partially written relay
            // frame (QuicClient correctly closes a connection on that case).
            let (sender, mut receiver) = mpsc::channel::<RelayWrite>(1);
            let client = client.clone();
            tasks.push(task_counter.spawn(async move {
                while let Some(request) = receiver.recv().await {
                    if request.result.is_closed() {
                        continue;
                    }
                    let result = client
                        .send_channel_for_generation(channel, peer_generation, request.ciphertext)
                        .await;
                    let _ = request.result.send(result);
                }
            }));
            Some(sender)
        });
        Ok(Self {
            client,
            generation: peer_generation,
            writers,
            tasks: StdMutex::new(tasks),
            stopped,
            task_counter,
        })
    }
    pub const fn peer_generation(&self) -> u64 {
        self.generation
    }

    fn close(&self) {
        self.stopped.send_replace(true);
        for task in lock(&self.tasks).iter() {
            task.abort();
        }
    }

    async fn close_and_wait(&self) {
        self.close();
        self.task_counter.wait_empty().await;
        let handles: Vec<_> = lock(&self.tasks).drain(..).collect();
        for handle in handles {
            let _ = handle.await;
        }
    }
}

impl LanRelayBackend for QuicRelayBackend {
    async fn send(&self, channel: ChannelKind, ciphertext: Bytes) -> Result<(), TransportError> {
        let mut stopped = self.stopped.subscribe();
        if *stopped.borrow() {
            return Err(TransportError::Closed);
        }
        let Some(writer) = &self.writers[index(channel)] else {
            return self
                .client
                .send_channel_for_generation(channel, self.generation, ciphertext)
                .await;
        };
        let (result, received) = oneshot::channel();
        tokio::select! {
            biased;
            _ = stopped.changed() => Err(TransportError::Closed),
            sent = writer.send(RelayWrite { ciphertext, result }) => {
                sent.map_err(|_| TransportError::Closed)?;
                received.await.map_err(|_| TransportError::Closed)?
            }
        }
    }
}

impl Drop for QuicRelayBackend {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionLaneHealth {
    Relay,
    Direct(DirectPathId),
    Unavailable,
    Interrupted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionDataHealth {
    pub relay_connected: bool,
    pub peer_disconnected: bool,
    pub peer_replaced: bool,
    pub direct_path: Option<DirectPathId>,
    pub lanes: [SessionLaneHealth; LANES],
    pub can_continue_without_relay: bool,
    pub closed: bool,
}

#[derive(Clone)]
struct Attached {
    id: DirectPathId,
    connection: quinn::Connection,
}

struct State {
    relay_connected: bool,
    peer_disconnected: bool,
    peer_replaced: bool,
    closed: bool,
    direct: Option<Attached>,
    last_generation: u64,
    routes: [SessionLaneHealth; LANES],
}

struct SharedState {
    state: StdMutex<State>,
    health: watch::Sender<SessionDataHealth>,
    continuation: bool,
    stopped: watch::Sender<bool>,
}

impl SharedState {
    fn update(&self, update: impl FnOnce(&mut State)) {
        let mut state = lock(&self.state);
        update(&mut state);
        if state.closed {
            self.stopped.send_replace(true);
            if let Some(direct) = state.direct.take() {
                direct
                    .connection
                    .close(0_u32.into(), b"business session closed");
            }
        }
        let lanes = std::array::from_fn(|index| match state.routes[index] {
            SessionLaneHealth::Relay if !state.relay_connected => SessionLaneHealth::Unavailable,
            route => route,
        });
        self.health.send_replace(SessionDataHealth {
            relay_connected: state.relay_connected,
            peer_disconnected: state.peer_disconnected,
            peer_replaced: state.peer_replaced,
            direct_path: state.direct.as_ref().map(|direct| direct.id),
            lanes,
            can_continue_without_relay: self.continuation
                && !state.closed
                && lanes
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| INTERACTIVE_CHANNELS & (1 << index) != 0)
                    .all(|(_, route)| matches!(route, SessionLaneHealth::Direct(_))),
            closed: state.closed,
        });
    }
}

pub struct SessionDataPlane {
    agreement: Option<NegotiatedLanData>,
    ingress: Arc<Ingress>,
    receivers: [Mutex<mpsc::Receiver<Bytes>>; LANES],
    outbound: [Arc<Mutex<()>>; LANES],
    relay: Arc<QuicRelayBackend>,
    shared: Arc<SharedState>,
    stopped: watch::Sender<bool>,
    relay_tasks: StdMutex<Vec<JoinHandle<()>>>,
    direct_tasks: StdMutex<Vec<JoinHandle<()>>>,
    registration_closed: StdMutex<bool>,
    task_counter: Arc<TaskCounter>,
    config: LanDataConfig,
}

impl SessionDataPlane {
    pub fn legacy(secure: Arc<Mutex<SecureSession>>, relay: Arc<QuicRelayBackend>) -> Arc<Self> {
        Self::create(None, secure, relay, LanDataConfig::default())
    }

    pub async fn relay_only(
        agreement: NegotiatedLanData,
        secure: Arc<Mutex<SecureSession>>,
        relay: Arc<QuicRelayBackend>,
    ) -> Result<Arc<Self>, SessionDataError> {
        Self::relay_only_with_config(agreement, secure, relay, LanDataConfig::default()).await
    }

    pub async fn relay_only_with_config(
        agreement: NegotiatedLanData,
        secure: Arc<Mutex<SecureSession>>,
        relay: Arc<QuicRelayBackend>,
        config: LanDataConfig,
    ) -> Result<Arc<Self>, SessionDataError> {
        if secure.lock().await.video_path_binding() != agreement.binding
            || relay.generation != agreement.relay_generation
            || config.direct_timeout.is_zero()
            || config.operation_timeout < config.direct_timeout
        {
            return Err(SessionDataError::InvalidNegotiation);
        }
        Ok(Self::create(Some(agreement), secure, relay, config))
    }

    fn create(
        agreement: Option<NegotiatedLanData>,
        secure: Arc<Mutex<SecureSession>>,
        relay: Arc<QuicRelayBackend>,
        config: LanDataConfig,
    ) -> Arc<Self> {
        let mask = agreement.map_or(0, |agreement| agreement.channels());
        let (ingress, receivers) = Ingress::new_channels(secure, mask);
        let initial = SessionDataHealth {
            relay_connected: !relay.client.is_closed(),
            peer_disconnected: false,
            peer_replaced: false,
            direct_path: None,
            lanes: [SessionLaneHealth::Relay; LANES],
            can_continue_without_relay: false,
            closed: false,
        };
        let (health, _) = watch::channel(initial);
        let (stopped, _) = watch::channel(false);
        let shared = Arc::new(SharedState {
            state: StdMutex::new(State {
                relay_connected: initial.relay_connected,
                peer_disconnected: false,
                peer_replaced: false,
                closed: false,
                direct: None,
                last_generation: 0,
                routes: [SessionLaneHealth::Relay; LANES],
            }),
            health,
            continuation: agreement
                .is_some_and(|agreement| agreement.allow_relay_loss_continuation()),
            stopped: stopped.clone(),
        });
        let plane = Arc::new(Self {
            agreement,
            ingress,
            receivers,
            outbound: std::array::from_fn(|_| Arc::new(Mutex::new(()))),
            relay,
            shared,
            stopped,
            relay_tasks: StdMutex::new(Vec::new()),
            direct_tasks: StdMutex::new(Vec::new()),
            registration_closed: StdMutex::new(false),
            task_counter: TaskCounter::new(),
            config,
        });
        plane.start_relay_readers();
        plane
    }

    fn start_relay_readers(&self) {
        let mut tasks = lock(&self.relay_tasks);
        for number in 1..=8 {
            let channel = ChannelKind::try_from(number).expect("known business channel");
            let relay = self.relay.clone();
            let ingress = self.ingress.clone();
            let shared = self.shared.clone();
            let mut stopped = self.stopped.subscribe();
            tasks.push(self.task_counter.spawn(async move {
                if *stopped.borrow() { return; }
                loop {
                    let received = tokio::select! {
                        biased;
                        _ = stopped.changed() => return,
                        received = relay.client.next_channel_for_generation(channel, relay.generation) => received,
                    };
                    let ciphertext = match received {
                        Ok(ciphertext) => ciphertext,
                        Err(error) => {
                            shared.update(|state| { state.relay_connected = false; state.peer_disconnected |= matches!(error, TransportError::PeerDisconnected); state.peer_replaced |= matches!(error, TransportError::PeerReplaced); if state.peer_replaced { state.closed = true; } });
                            // A durable host can keep the old authenticated
                            // LAN while its peer loses only relay. Keep the
                            // generation-aware control observer alive: it
                            // preserves a replacement's first Noise payload
                            // and reports PeerReplaced to retire this plane.
                            if channel == ChannelKind::Control
                                && error == TransportError::PeerDisconnected
                                && !relay.client.is_closed()
                            {
                                continue;
                            }
                            return;
                        }
                    };
                    let admitted = tokio::select! {
                        biased;
                        _ = stopped.changed() => return,
                        admitted = ingress.admit(channel, ciphertext) => admitted,
                    };
                    if let Err(error) = admitted {
                        if matches!(error, LanDataError::Crypto(CryptoError::ReplayRejected)) { continue; }
                        shared.update(|state| state.routes[index(channel)] = SessionLaneHealth::Interrupted);
                        return;
                    }
                }
            }));
        }
        let relay = self.relay.clone();
        let shared = self.shared.clone();
        let mut stopped = self.stopped.subscribe();
        tasks.push(self.task_counter.spawn(async move {
                if *stopped.borrow() { return; }
            tokio::select! { _ = stopped.changed() => {}, _ = relay.client.closed() => shared.update(|state| state.relay_connected = false) }
        }));
    }

    pub fn health(&self) -> watch::Receiver<SessionDataHealth> {
        self.shared.health.subscribe()
    }
    pub const fn agreement(&self) -> Option<NegotiatedLanData> {
        self.agreement
    }

    pub async fn send(
        &self,
        channel: ChannelKind,
        plaintext: Bytes,
    ) -> Result<LanDataRoute, SessionDataError> {
        let wrapped = self
            .agreement
            .is_some_and(|agreement| agreement.channels() & bit(channel) != 0);
        let maximum = if channel.is_reliable() {
            MAX_PACKET_PLAINTEXT_BYTES - if wrapped { RECEIPT_OVERHEAD } else { 0 }
        } else {
            MAX_DATAGRAM_BYTES - CIPHER_OVERHEAD - if wrapped { 6 } else { 0 }
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
            return Err(SessionDataError::Closed);
        }
        let operation = async {
            let _order = self.outbound[index(channel)].lock().await;
            self.check_available(channel)?;
            let mut pending = PendingBusinessSend {
                shared: &self.shared,
                channel,
                attempted: false,
                completed: false,
            };
            let (ciphertext, receipt) = if wrapped {
                seal_payload(&mut *self.ingress.secure.lock().await, channel, &plaintext)?
            } else {
                (
                    self.ingress
                        .secure
                        .lock()
                        .await
                        .seal(lane(channel), &plaintext)?
                        .into(),
                    None,
                )
            };
            let direct = {
                let state = lock(&self.shared.state);
                match state.routes[index(channel)] {
                    SessionLaneHealth::Direct(id) => state
                        .direct
                        .as_ref()
                        .filter(|direct| direct.id == id)
                        .cloned(),
                    _ => None,
                }
            };
            if let Some(direct) = direct {
                pending.attempted = true;
                let sent = if channel.is_reliable() {
                    tokio::time::timeout(
                        self.config.direct_timeout,
                        send_reliable(
                            &direct.connection,
                            channel,
                            &ciphertext,
                            &receipt.expect("wrapped reliable receipt"),
                        ),
                    )
                    .await
                    .map_err(|_| {
                        LanDataError::Transport(TransportError::Stream(
                            "LAN receipt timed out".to_owned(),
                        ))
                    })
                    .and_then(|result| result)
                } else {
                    if wrapped {
                        send_datagram(&direct.connection, channel, &ciphertext)
                    } else {
                        direct
                            .connection
                            .send_datagram(ciphertext.clone())
                            .map_err(|error| {
                                LanDataError::Transport(TransportError::Datagram(error.to_string()))
                            })
                    }
                };
                if sent.is_ok() {
                    pending.completed = true;
                    return Ok(LanDataRoute::DirectLan);
                }
                self.shared.update(|state| { if matches!(state.routes[index(channel)], SessionLaneHealth::Direct(id) if id == direct.id) { state.routes[index(channel)] = SessionLaneHealth::Relay; } });
            }
            if !lock(&self.shared.state).relay_connected {
                return Err(SessionDataError::NoPath(channel));
            }
            pending.attempted = true;
            self.relay.send(channel, ciphertext).await?;
            pending.completed = true;
            Ok(LanDataRoute::Relay)
        };
        tokio::select! {
            biased;
            _ = stopped.changed() => Err(SessionDataError::Closed),
            result = tokio::time::timeout(self.config.operation_timeout, operation) => match result {
                Ok(result) => result,
                Err(_) if channel.is_reliable() => {
                    self.shared.update(|state| state.routes[index(channel)] = SessionLaneHealth::Interrupted);
                    Err(SessionDataError::Interrupted(channel))
                }
                Err(_) => Err(SessionDataError::Timeout),
            }
        }
    }

    pub async fn recv(&self, channel: ChannelKind) -> Result<Bytes, SessionDataError> {
        let mut stopped = self.stopped.subscribe();
        let mut health = self.health();
        loop {
            self.check_available(channel)?;
            tokio::select! {
                biased;
                _ = stopped.changed() => return Err(SessionDataError::Closed),
                _ = health.changed() => {},
                payload = async { self.receivers[index(channel)].lock().await.recv().await } => return payload.ok_or(SessionDataError::Closed),
            }
        }
    }

    fn check_available(&self, channel: ChannelKind) -> Result<(), SessionDataError> {
        let state = lock(&self.shared.state);
        if state.closed || *self.stopped.borrow() {
            return Err(SessionDataError::Closed);
        }
        if !state.relay_connected
            && !self
                .agreement
                .is_some_and(|agreement| agreement.allow_relay_loss_continuation())
        {
            return Err(SessionDataError::NoPath(channel));
        }
        match state.routes[index(channel)] {
            SessionLaneHealth::Interrupted => Err(SessionDataError::Interrupted(channel)),
            SessionLaneHealth::Direct(_) => Ok(()),
            SessionLaneHealth::Relay if state.relay_connected => Ok(()),
            _ => Err(SessionDataError::NoPath(channel)),
        }
    }

    /// Installs authenticated readers immediately. Each reliable outbound lane
    /// remains on relay until its own relay-admission fence is acknowledged.
    /// A slow file lane therefore cannot delay input/control promotion.
    pub async fn attach_direct(
        &self,
        authority: LanAttachAuthority,
        authenticated: DirectLanConnection,
    ) -> Result<DirectPathId, SessionDataError> {
        let mut incoming = PendingIncoming {
            authenticated,
            shared: self.shared.clone(),
            installed: false,
        };
        let agreement = self.agreement.ok_or(SessionDataError::InvalidSwitch)?;
        if authority.agreement != agreement
            || authority.candidate.candidate_id() != incoming.authenticated.candidate_id()
            || incoming.authenticated.session_binding() != &agreement.binding
        {
            return Err(SessionDataError::InvalidSwitch);
        }
        authority
            .candidate
            .validate(now_unix_s(), &agreement.binding)
            .map_err(|_| SessionDataError::InvalidSwitch)?;
        let id = DirectPathId(authority.generation);
        let connection = incoming.authenticated.authenticated_quic_connection();
        let registration = lock(&self.registration_closed);
        if *registration {
            return Err(SessionDataError::Closed);
        }
        self.install_connection(id, connection.clone(), authority.channels, false)?;
        let allowed = Arc::new(AtomicU16::new(authority.channels & !reliable_channels()));
        let mut tasks = lock(&self.direct_tasks);
        tasks.push(self.task_counter.spawn(read_streams_with_fences(
            connection.clone(),
            self.ingress.clone(),
            self.stopped.subscribe(),
            self.config,
            allowed,
            DirectStreamAuthority {
                channels: authority.channels,
                generation: authority.generation,
            },
            Some(self.task_counter.clone()),
        )));
        tasks.push(self.task_counter.spawn(read_datagrams_for_channels(
            connection.clone(),
            self.ingress.clone(),
            self.stopped.subscribe(),
            authority.channels,
        )));
        for number in 1..=8 {
            let channel = ChannelKind::try_from(number).expect("known channel");
            if !channel.is_reliable() || authority.channels & bit(channel) == 0 {
                continue;
            }
            let outbound = self.outbound[index(channel)].clone();
            let relay = self.relay.clone();
            let ingress = self.ingress.clone();
            let shared = self.shared.clone();
            let connection = connection.clone();
            let config = self.config;
            let mut stopped = self.stopped.subscribe();
            tasks.push(self.task_counter.spawn(async move {
                if *stopped.borrow() { return; }
                let fence = async {
                    let _order = outbound.lock().await;
                    {
                        let state = lock(&shared.state);
                        if state.closed || !state.relay_connected || state.routes[index(channel)] == SessionLaneHealth::Interrupted || !state.direct.as_ref().is_some_and(|direct| direct.id == id) { return Err(SessionDataError::NoPath(channel)); }
                    }
                    let (ciphertext, receipt) = seal_fence(&mut *ingress.secure.lock().await, channel, id.0)?;
                    relay.send(channel, ciphertext.clone()).await?;
                    send_fence_query(&connection, channel, &ciphertext, &receipt).await?;
                    shared.update(|state| {
                        if state.direct.as_ref().is_some_and(|direct| direct.id == id)
                            && state.routes[index(channel)] != SessionLaneHealth::Interrupted
                        { state.routes[index(channel)] = SessionLaneHealth::Direct(id); }
                    });
                    Ok::<(), SessionDataError>(())
                };
                tokio::select! { biased; _ = stopped.changed() => {}, _ = tokio::time::timeout(config.operation_timeout, fence) => {} }
            }));
        }
        self.start_direct_close_observer(&mut tasks, connection, id);
        incoming.installed = true;
        self.shared.update(|_| {});
        Ok(id)
    }

    /// The explicitly legacy mode keeps the existing video-only bare cipher
    /// path. No reliable/input/audio lane is enabled by this entry point.
    pub async fn attach_legacy_video(
        &self,
        authenticated: DirectLanConnection,
    ) -> Result<DirectPathId, SessionDataError> {
        let mut incoming = PendingIncoming {
            authenticated,
            shared: self.shared.clone(),
            installed: false,
        };
        let mut stopped = self.stopped.subscribe();
        if *stopped.borrow() {
            return Err(SessionDataError::Closed);
        }
        let binding = tokio::select! {
            biased;
            _ = stopped.changed() => return Err(SessionDataError::Closed),
            secure = self.ingress.secure.lock() => secure.video_path_binding(),
        };
        if self.agreement.is_some() || binding != *incoming.authenticated.session_binding() {
            return Err(SessionDataError::InvalidSwitch);
        }
        let id = DirectPathId(
            lock(&self.shared.state)
                .last_generation
                .checked_add(1)
                .ok_or(SessionDataError::InvalidSwitch)?,
        );
        let connection = incoming.authenticated.authenticated_quic_connection();
        let registration = lock(&self.registration_closed);
        if *registration {
            return Err(SessionDataError::Closed);
        }
        self.install_connection(
            id,
            connection.clone(),
            bit(ChannelKind::VideoDatagram),
            true,
        )?;
        let mut tasks = lock(&self.direct_tasks);
        let reader = connection.clone();
        let ingress = self.ingress.clone();
        let mut stopped = self.stopped.subscribe();
        tasks.push(self.task_counter.spawn(async move {
                if *stopped.borrow() { return; }
            loop {
                let packet = tokio::select! { biased; _ = stopped.changed() => return, packet = reader.read_datagram() => packet };
                let Ok(ciphertext) = packet else { return };
                let _ = tokio::select! { biased; _ = stopped.changed() => return, admitted = ingress.admit(ChannelKind::VideoDatagram, ciphertext) => admitted };
            }
        }));
        self.start_direct_close_observer(&mut tasks, connection, id);
        incoming.installed = true;
        self.shared.update(|_| {});
        Ok(id)
    }

    fn install_connection(
        &self,
        id: DirectPathId,
        connection: quinn::Connection,
        channels: u16,
        legacy: bool,
    ) -> Result<(), SessionDataError> {
        let mut state = lock(&self.shared.state);
        if state.closed || *self.stopped.borrow() || id.0 <= state.last_generation {
            return Err(SessionDataError::InvalidSwitch);
        }
        if state
            .direct
            .as_ref()
            .is_some_and(|direct| direct.connection.close_reason().is_none())
        {
            return Err(SessionDataError::AlreadyAttached);
        }
        if let Some(old) = state.direct.take() {
            old.connection.close(0_u32.into(), b"LAN path replaced");
        }
        for task in lock(&self.direct_tasks).drain(..) {
            task.abort();
        }
        state.last_generation = id.0;
        state.direct = Some(Attached { id, connection });
        for number in 1..=8 {
            let channel = ChannelKind::try_from(number).expect("known channel");
            if state.routes[index(channel)] == SessionLaneHealth::Interrupted {
                continue;
            }
            state.routes[index(channel)] =
                if channels & bit(channel) != 0 && (!channel.is_reliable() || legacy) {
                    SessionLaneHealth::Direct(id)
                } else {
                    SessionLaneHealth::Relay
                };
        }
        Ok(())
    }

    fn start_direct_close_observer(
        &self,
        tasks: &mut Vec<JoinHandle<()>>,
        connection: quinn::Connection,
        id: DirectPathId,
    ) {
        let shared = self.shared.clone();
        let mut stopped = self.stopped.subscribe();
        tasks.push(self.task_counter.spawn(async move {
                if *stopped.borrow() { return; }
            tokio::select! { _ = stopped.changed() => {}, _ = connection.closed() => {
                shared.update(|state| {
                    if state.direct.as_ref().is_some_and(|direct| direct.id == id) {
                        state.direct = None;
                        for route in &mut state.routes { if *route == SessionLaneHealth::Direct(id) { *route = SessionLaneHealth::Relay; } }
                    }
                });
            } }
        }));
    }

    pub fn close(&self) {
        let mut registration = lock(&self.registration_closed);
        *registration = true;
        self.stopped.send_replace(true);
        self.relay.close();
        self.shared.update(|state| {
            state.closed = true;
            if let Some(direct) = state.direct.take() {
                direct
                    .connection
                    .close(0_u32.into(), b"business session closed");
            }
        });
        for task in lock(&self.relay_tasks)
            .iter()
            .chain(lock(&self.direct_tasks).iter())
        {
            task.abort();
        }
    }

    pub fn active_tasks(&self) -> usize {
        *self.task_counter.active.borrow() + *self.relay.task_counter.active.borrow()
    }

    /// Waits for owned workers and accepted LAN stream children. An idle
    /// durable relay remains open; aborting an already partial reliable write
    /// at teardown retains QuicClient's connection-close framing safeguard.
    /// Cancel-safe observers may retry.
    pub async fn close_and_wait(&self) {
        self.close();
        self.relay.close_and_wait().await;
        self.task_counter.wait_empty().await;
        let handles: Vec<_> = lock(&self.relay_tasks)
            .drain(..)
            .chain(lock(&self.direct_tasks).drain(..))
            .collect();
        for handle in handles {
            let _ = handle.await;
        }
    }
}

impl Drop for SessionDataPlane {
    fn drop(&mut self) {
        self.close();
    }
}

struct PendingBusinessSend<'a> {
    shared: &'a SharedState,
    channel: ChannelKind,
    attempted: bool,
    completed: bool,
}

struct PendingIncoming {
    authenticated: DirectLanConnection,
    shared: Arc<SharedState>,
    installed: bool,
}

impl Drop for PendingIncoming {
    fn drop(&mut self) {
        if !self.installed {
            let incoming_id = self
                .authenticated
                .authenticated_quic_connection()
                .stable_id();
            let aliases_active = lock(&self.shared.state)
                .direct
                .as_ref()
                .is_some_and(|direct| direct.connection.stable_id() == incoming_id);
            if !aliases_active {
                self.authenticated.close(b"LAN attachment rejected");
            }
        }
    }
}

fn reliable_channels() -> u16 {
    (1..=8)
        .map(|number| ChannelKind::try_from(number).expect("known channel"))
        .filter(|channel| channel.is_reliable())
        .fold(0, |mask, channel| mask | bit(channel))
}
impl Drop for PendingBusinessSend<'_> {
    fn drop(&mut self) {
        if self.channel.is_reliable() && self.attempted && !self.completed {
            self.shared
                .update(|state| state.routes[index(self.channel)] = SessionLaneHealth::Interrupted);
        }
    }
}

fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn index(channel: ChannelKind) -> usize {
    channel as usize - 1
}
fn bit(channel: ChannelKind) -> u16 {
    1 << index(channel)
}
fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}
