use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use desklink_crypto::SessionId;
use desklink_protocol::DeviceRole;
use desklink_transport::{
    ChannelKind, DEAD_TIMEOUT, JOIN_ENVELOPE_BYTES, JoinRejectCode, KEEPALIVE_INTERVAL,
    MAX_DATAGRAM_BYTES, MAX_RELIABLE_MESSAGE_BYTES, RelayJoin, decode_relay_join,
};
use thiserror::Error;

#[derive(Clone, Debug)]
pub struct RelayConfig {
    pub session_ttl: Duration,
    pub sweep_interval: Duration,
    pub keep_alive: Duration,
    pub dead_timeout: Duration,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            session_ttl: Duration::from_secs(600),
            sweep_interval: Duration::from_secs(1),
            keep_alive: KEEPALIVE_INTERVAL,
            dead_timeout: DEAD_TIMEOUT,
        }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RelayError {
    #[error("session is occupied")]
    SessionOccupied,
    #[error("session was not found")]
    SessionNotFound,
    #[error("authentication does not match the session")]
    AuthenticationMismatch,
    #[error("role does not match the session")]
    RoleMismatch,
    #[error("join envelope is malformed")]
    MalformedJoin,
    #[error("join envelope is too large")]
    JoinTooLarge,
    #[error("reliable message is too large")]
    ReliableMessageTooLarge,
    #[error("datagram is too large")]
    DatagramTooLarge,
    #[error("unknown channel")]
    UnknownChannel,
    #[error("peer is not connected")]
    PeerUnavailable,
    #[error("invalid relay configuration: {0}")]
    InvalidConfig(String),
    #[error("relay transport error: {0}")]
    Transport(String),
}

#[derive(Clone)]
pub struct RelaySessionTable {
    sessions: Arc<Mutex<HashMap<SessionId, SessionRecord>>>,
    config: RelayConfig,
}

struct SessionRecord {
    created_at: Instant,
    expires_at: Instant,
    authentication: Option<[u8; 32]>,
    host: Option<u64>,
    controller: Option<u64>,
}

impl RelaySessionTable {
    pub fn new(config: RelayConfig) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }

    pub fn attach_host(&self, session_id: SessionId, connection_id: u64) -> Result<(), RelayError> {
        self.attach_role(session_id, DeviceRole::Host, None, connection_id)
    }

    pub fn attach_controller(
        &self,
        session_id: SessionId,
        connection_id: u64,
    ) -> Result<(), RelayError> {
        self.attach_role(session_id, DeviceRole::Controller, None, connection_id)
    }

    pub fn attach_host_with_auth(
        &self,
        session_id: SessionId,
        authentication: [u8; 32],
        connection_id: u64,
    ) -> Result<(), RelayError> {
        self.attach_role(
            session_id,
            DeviceRole::Host,
            Some(authentication),
            connection_id,
        )
    }

    pub fn attach_controller_with_auth(
        &self,
        session_id: SessionId,
        authentication: [u8; 32],
        connection_id: u64,
    ) -> Result<(), RelayError> {
        self.attach_role(
            session_id,
            DeviceRole::Controller,
            Some(authentication),
            connection_id,
        )
    }

    pub fn attach_with_auth(
        &self,
        session_id: SessionId,
        role: DeviceRole,
        authentication: [u8; 32],
        connection_id: u64,
    ) -> Result<(), RelayError> {
        self.attach_role(session_id, role, Some(authentication), connection_id)
    }

    pub fn detach(&self, session_id: SessionId, connection_id: u64) -> bool {
        let now = Instant::now();
        let mut sessions = self.lock_sessions();
        if sessions
            .get(&session_id)
            .is_some_and(|session| session.expires_at <= now)
        {
            sessions.remove(&session_id);
            return false;
        }
        let Some(session) = sessions.get_mut(&session_id) else {
            return false;
        };
        let detached = if session.host == Some(connection_id) {
            session.host = None;
            true
        } else if session.controller == Some(connection_id) {
            session.controller = None;
            true
        } else {
            false
        };
        if detached && session.host.is_none() && session.controller.is_none() {
            sessions.remove(&session_id);
        }
        detached
    }

    pub fn sweep(&self, now: Instant) -> Vec<SessionId> {
        let mut sessions = self.lock_sessions();
        let expired = sessions
            .iter()
            .filter_map(|(session_id, session)| (session.expires_at <= now).then_some(*session_id))
            .collect::<Vec<_>>();
        for session_id in &expired {
            sessions.remove(session_id);
        }
        expired
    }

    pub fn has_connection(&self, session_id: SessionId, connection_id: u64) -> bool {
        let now = Instant::now();
        let sessions = self.lock_sessions();
        let Some(session) = sessions.get(&session_id) else {
            return false;
        };
        session.created_at <= now
            && session.expires_at > now
            && (session.host == Some(connection_id) || session.controller == Some(connection_id))
    }

    fn peer_connection(
        &self,
        session_id: SessionId,
        role: DeviceRole,
        connection_id: u64,
    ) -> Option<u64> {
        let now = Instant::now();
        let sessions = self.lock_sessions();
        let session = sessions.get(&session_id)?;
        if session.expires_at <= now {
            return None;
        }
        match role {
            DeviceRole::Host if session.host == Some(connection_id) => session.controller,
            DeviceRole::Controller if session.controller == Some(connection_id) => session.host,
            _ => None,
        }
    }

    fn attach_role(
        &self,
        session_id: SessionId,
        role: DeviceRole,
        authentication: Option<[u8; 32]>,
        connection_id: u64,
    ) -> Result<(), RelayError> {
        let now = Instant::now();
        let mut sessions = self.lock_sessions();
        sessions.retain(|_, session| session.expires_at > now);

        if let std::collections::hash_map::Entry::Vacant(entry) = sessions.entry(session_id) {
            if role == DeviceRole::Controller {
                return Err(RelayError::SessionNotFound);
            }
            entry.insert(SessionRecord {
                created_at: now,
                expires_at: now + self.config.session_ttl,
                authentication,
                host: Some(connection_id),
                controller: None,
            });
            return Ok(());
        }

        let Some(session) = sessions.get_mut(&session_id) else {
            return Err(RelayError::SessionNotFound);
        };
        apply_authentication(&mut session.authentication, authentication)?;
        match role {
            DeviceRole::Host => {
                if session.host.is_some() {
                    Err(RelayError::SessionOccupied)
                } else {
                    session.host = Some(connection_id);
                    Ok(())
                }
            }
            DeviceRole::Controller => {
                if session.host.is_none() {
                    return Err(RelayError::RoleMismatch);
                }
                if session.controller.is_some() {
                    Err(RelayError::SessionOccupied)
                } else {
                    session.controller = Some(connection_id);
                    Ok(())
                }
            }
        }
    }

    fn lock_sessions(&self) -> MutexGuard<'_, HashMap<SessionId, SessionRecord>> {
        match self.sessions.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

fn apply_authentication(
    expected: &mut Option<[u8; 32]>,
    actual: Option<[u8; 32]>,
) -> Result<(), RelayError> {
    match (*expected, actual) {
        (Some(expected), Some(actual)) if expected != actual => {
            Err(RelayError::AuthenticationMismatch)
        }
        (Some(_), None) => Err(RelayError::AuthenticationMismatch),
        (None, Some(actual)) => {
            *expected = Some(actual);
            Ok(())
        }
        _ => Ok(()),
    }
}

struct Participant {
    session_id: SessionId,
    connection: quinn::Connection,
}

struct RelayState {
    sessions: RelaySessionTable,
    participants: Mutex<HashMap<u64, Participant>>,
    next_connection_id: AtomicU64,
}

impl RelayState {
    fn next_connection_id(&self) -> u64 {
        let id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        if id == 0 {
            self.next_connection_id.fetch_add(1, Ordering::Relaxed)
        } else {
            id
        }
    }

    fn lock_participants(&self) -> MutexGuard<'_, HashMap<u64, Participant>> {
        match self.participants.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn peer(
        &self,
        session_id: SessionId,
        role: DeviceRole,
        connection_id: u64,
    ) -> Option<quinn::Connection> {
        let peer_id = self
            .sessions
            .peer_connection(session_id, role, connection_id)?;
        self.lock_participants()
            .get(&peer_id)
            .map(|participant| participant.connection.clone())
    }

    fn remove_connection(&self, session_id: SessionId, connection_id: u64) {
        self.sessions.detach(session_id, connection_id);
        self.lock_participants().remove(&connection_id);
    }

    fn remove_expired(&self, expired: &[SessionId]) {
        if expired.is_empty() {
            return;
        }
        let mut participants = self.lock_participants();
        participants.retain(|_, participant| {
            if expired.contains(&participant.session_id) {
                participant
                    .connection
                    .close(quinn::VarInt::from_u32(2), b"session expired");
                false
            } else {
                true
            }
        });
    }
}

pub struct RelayServer {
    endpoint: quinn::Endpoint,
    state: Arc<RelayState>,
    config: RelayConfig,
}

impl RelayServer {
    pub async fn bind(
        bind_addr: SocketAddr,
        mut server_config: quinn::ServerConfig,
        config: RelayConfig,
    ) -> Result<Self, RelayError> {
        if config.keep_alive.is_zero() || config.dead_timeout.is_zero() {
            return Err(RelayError::InvalidConfig(
                "keepalive and dead timeout must be nonzero".to_owned(),
            ));
        }
        let idle_timeout = quinn::IdleTimeout::try_from(config.dead_timeout)
            .map_err(|error| RelayError::InvalidConfig(error.to_string()))?;
        let mut transport = quinn::TransportConfig::default();
        transport
            .keep_alive_interval(Some(config.keep_alive))
            .max_idle_timeout(Some(idle_timeout));
        server_config.transport_config(Arc::new(transport));
        let endpoint = quinn::Endpoint::server(server_config, bind_addr)
            .map_err(|error| RelayError::Transport(error.to_string()))?;
        let state = Arc::new(RelayState {
            sessions: RelaySessionTable::new(config.clone()),
            participants: Mutex::new(HashMap::new()),
            next_connection_id: AtomicU64::new(1),
        });
        Ok(Self {
            endpoint,
            state,
            config,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, RelayError> {
        self.endpoint
            .local_addr()
            .map_err(|error| RelayError::Transport(error.to_string()))
    }

    pub fn session_table(&self) -> RelaySessionTable {
        self.state.sessions.clone()
    }

    pub fn close(&self) {
        self.endpoint
            .close(quinn::VarInt::from_u32(0), b"relay shutdown");
    }

    pub async fn run(&self) -> Result<(), RelayError> {
        let sweep_period = if self.config.sweep_interval.is_zero() {
            Duration::from_millis(1)
        } else {
            self.config.sweep_interval
        };
        let mut sweep = tokio::time::interval(sweep_period);
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                incoming = self.endpoint.accept() => {
                    let Some(incoming) = incoming else { return Ok(()); };
                    let state = self.state.clone();
                    let connection_id = state.next_connection_id();
                    tokio::spawn(async move {
                        if let Ok(connection) = incoming.await {
                            handle_connection(connection, state, connection_id).await;
                        }
                    });
                }
                _ = sweep.tick() => {
                    let expired = self.state.sessions.sweep(Instant::now());
                    self.state.remove_expired(&expired);
                }
            }
        }
    }
}

async fn handle_connection(
    connection: quinn::Connection,
    state: Arc<RelayState>,
    connection_id: u64,
) {
    let Ok((mut join_send, mut join_receive)) = connection.accept_bi().await else {
        return;
    };
    let join = match read_join(&mut join_receive).await {
        Ok(join) => join,
        Err(error) => {
            if join_send
                .write_all(&[join_error_code(&error)])
                .await
                .is_ok()
            {
                let _ = join_send.finish();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            return;
        }
    };
    let attach = state.sessions.attach_with_auth(
        join.session_id(),
        join.role(),
        *join.authentication(),
        connection_id,
    );
    if let Err(error) = attach {
        if join_send
            .write_all(&[relay_error_code(&error)])
            .await
            .is_ok()
        {
            let _ = join_send.finish();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        return;
    }
    let session_id = join.session_id();
    let role = join.role();
    state.lock_participants().insert(
        connection_id,
        Participant {
            session_id,
            connection: connection.clone(),
        },
    );
    if join_send.write_all(&[0]).await.is_err() || join_send.finish().is_err() {
        state.remove_connection(session_id, connection_id);
        return;
    }

    loop {
        tokio::select! {
            accepted = connection.accept_bi() => {
                let Ok((_send, receive)) = accepted else { break; };
                let state = state.clone();
                let source = connection.clone();
                tokio::spawn(async move {
                    forward_reliable(
                        state,
                        source,
                        connection_id,
                        session_id,
                        role,
                        receive,
                    )
                    .await;
                });
            }
            datagram = connection.read_datagram() => {
                let Ok(datagram) = datagram else { break; };
                if !forward_datagram(
                    &state,
                    &connection,
                    connection_id,
                    session_id,
                    role,
                    datagram,
                ) {
                    break;
                }
            }
            _ = connection.closed() => break,
        }
    }
    state.remove_connection(session_id, connection_id);
}

async fn forward_reliable(
    state: Arc<RelayState>,
    source: quinn::Connection,
    connection_id: u64,
    session_id: SessionId,
    role: DeviceRole,
    mut receive: quinn::RecvStream,
) {
    let mut channel = [0; 1];
    if receive.read_exact(&mut channel).await.is_err() {
        return;
    }
    let Ok(channel) = ChannelKind::try_from(channel[0]) else {
        close_connection(&source, b"unknown channel");
        return;
    };
    if !channel.is_reliable() {
        close_connection(&source, b"invalid reliable channel");
        return;
    }
    let Some(peer) = state.peer(session_id, role, connection_id) else {
        return;
    };
    let Ok((mut send, _receive)) = peer.open_bi().await else {
        return;
    };
    if send.write_all(&[channel as u8]).await.is_err() {
        return;
    }
    loop {
        let mut length = [0; 4];
        if receive.read_exact(&mut length).await.is_err() {
            let _ = send.finish();
            return;
        }
        let length = u32::from_be_bytes(length) as usize;
        if length > MAX_RELIABLE_MESSAGE_BYTES {
            close_connection(&source, b"reliable message too large");
            return;
        }
        let mut message = vec![0; length];
        if receive.read_exact(&mut message).await.is_err() {
            let _ = send.finish();
            return;
        }
        if send.write_all(&length_to_bytes(length)).await.is_err()
            || send.write_all(&message).await.is_err()
        {
            return;
        }
    }
}

fn forward_datagram(
    state: &RelayState,
    source: &quinn::Connection,
    connection_id: u64,
    session_id: SessionId,
    role: DeviceRole,
    datagram: bytes::Bytes,
) -> bool {
    if datagram.is_empty() || datagram.len() - 1 > MAX_DATAGRAM_BYTES {
        close_connection(source, b"datagram too large");
        return false;
    }
    let Ok(channel) = ChannelKind::try_from(datagram[0]) else {
        close_connection(source, b"unknown datagram channel");
        return false;
    };
    if !matches!(
        channel,
        ChannelKind::VideoDatagram | ChannelKind::CursorDatagram
    ) {
        close_connection(source, b"invalid datagram channel");
        return false;
    }
    if let Some(peer) = state.peer(session_id, role, connection_id) {
        let _ = peer.send_datagram(datagram);
    }
    true
}

fn close_connection(connection: &quinn::Connection, reason: &[u8]) {
    connection.close(quinn::VarInt::from_u32(3), reason);
}

fn length_to_bytes(length: usize) -> [u8; 4] {
    (length as u32).to_be_bytes()
}

async fn read_join(receive: &mut quinn::RecvStream) -> Result<RelayJoin, JoinRejectCode> {
    let mut length = [0; 4];
    receive
        .read_exact(&mut length)
        .await
        .map_err(|_| JoinRejectCode::Malformed)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > JOIN_ENVELOPE_BYTES {
        return Err(JoinRejectCode::TooLarge);
    }
    if length != JOIN_ENVELOPE_BYTES {
        return Err(JoinRejectCode::Malformed);
    }
    let mut bytes = vec![0; length];
    receive
        .read_exact(&mut bytes)
        .await
        .map_err(|_| JoinRejectCode::Malformed)?;
    decode_relay_join(&bytes).map_err(|_| JoinRejectCode::Malformed)
}

fn join_error_code(error: &JoinRejectCode) -> u8 {
    *error as u8
}

fn relay_error_code(error: &RelayError) -> u8 {
    JoinRejectCode::from(error) as u8
}

impl From<&RelayError> for JoinRejectCode {
    fn from(error: &RelayError) -> Self {
        match error {
            RelayError::SessionNotFound => Self::SessionNotFound,
            RelayError::SessionOccupied => Self::SessionOccupied,
            RelayError::AuthenticationMismatch => Self::AuthenticationMismatch,
            RelayError::RoleMismatch => Self::RoleMismatch,
            _ => Self::Internal,
        }
    }
}
