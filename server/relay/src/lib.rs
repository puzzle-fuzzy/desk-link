use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use desklink_crypto::SessionId;
use desklink_protocol::DeviceRole;
use desklink_transport::{
    ChannelKind, DEAD_TIMEOUT, DIRECTORY_ACCESS_CODE_BYTES, DIRECTORY_LOOKUP_ENVELOPE_BYTES,
    DIRECTORY_LOOKUP_FOUND, DIRECTORY_LOOKUP_NOT_FOUND, DIRECTORY_LOOKUP_RATE_LIMITED,
    JoinRejectCode, KEEPALIVE_INTERVAL, MAX_DATAGRAM_BYTES, MAX_JOIN_ENVELOPE_BYTES,
    MAX_RELIABLE_MESSAGE_BYTES, RELAY_CONNECTION_LIMIT_CLOSE_CODE, RelayDirectoryLookup, RelayJoin,
    decode_directory_lookup, decode_relay_join,
};
use subtle::ConstantTimeEq;
use thiserror::Error;

#[derive(Clone, Debug)]
pub struct RelayConfig {
    pub session_ttl: Duration,
    pub sweep_interval: Duration,
    pub keep_alive: Duration,
    pub dead_timeout: Duration,
    pub max_connections: usize,
    pub max_sessions: usize,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            session_ttl: Duration::from_secs(86_400),
            sweep_interval: Duration::from_secs(1),
            keep_alive: KEEPALIVE_INTERVAL,
            dead_timeout: DEAD_TIMEOUT,
            max_connections: 1024,
            max_sessions: 1024,
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
    #[error("connection admission limit reached")]
    ConnectionLimitReached,
    #[error("session admission limit reached")]
    SessionLimitReached,
    #[error("device directory ID is already registered by another host")]
    DirectoryConflict,
}

const DIRECTORY_LOOKUP_WINDOW: Duration = Duration::from_secs(60);
const DIRECTORY_LOOKUP_FAILURE_LIMIT: u8 = 5;

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
    host_participant_id: Option<[u8; 16]>,
    controller_participant_id: Option<[u8; 16]>,
}

impl RelaySessionTable {
    pub fn new(config: RelayConfig) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }

    pub fn attach_host(&self, session_id: SessionId, connection_id: u64) -> Result<(), RelayError> {
        self.attach_role(session_id, DeviceRole::Host, None, None, connection_id)
            .map(|_| ())
    }

    pub fn attach_controller(
        &self,
        session_id: SessionId,
        connection_id: u64,
    ) -> Result<(), RelayError> {
        self.attach_role(
            session_id,
            DeviceRole::Controller,
            None,
            None,
            connection_id,
        )
        .map(|_| ())
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
            None,
            connection_id,
        )
        .map(|_| ())
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
            None,
            connection_id,
        )
        .map(|_| ())
    }

    pub fn attach_with_auth(
        &self,
        session_id: SessionId,
        role: DeviceRole,
        authentication: [u8; 32],
        connection_id: u64,
    ) -> Result<(), RelayError> {
        self.attach_role(session_id, role, Some(authentication), None, connection_id)
            .map(|_| ())
    }

    fn attach_with_auth_and_participant(
        &self,
        session_id: SessionId,
        role: DeviceRole,
        authentication: [u8; 32],
        participant_id: Option<[u8; 16]>,
        connection_id: u64,
    ) -> Result<Option<u64>, RelayError> {
        self.attach_role(
            session_id,
            role,
            Some(authentication),
            participant_id,
            connection_id,
        )
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
            session.host_participant_id = None;
            true
        } else if session.controller == Some(connection_id) {
            session.controller = None;
            session.controller_participant_id = None;
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
        self.sweep_expired(now)
            .into_iter()
            .map(|expired| expired.session_id)
            .collect()
    }

    pub fn sweep_expired(&self, now: Instant) -> Vec<ExpiredSession> {
        let mut sessions = self.lock_sessions();
        let expired = sessions
            .iter()
            .filter_map(|(session_id, session)| {
                (session.expires_at <= now).then_some(ExpiredSession {
                    session_id: *session_id,
                    host: session.host,
                    controller: session.controller,
                })
            })
            .collect::<Vec<_>>();
        for expired_session in &expired {
            sessions.remove(&expired_session.session_id);
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
        participant_id: Option<[u8; 16]>,
        connection_id: u64,
    ) -> Result<Option<u64>, RelayError> {
        let now = Instant::now();
        let mut sessions = self.lock_sessions();
        sessions.retain(|_, session| session.expires_at > now);

        if !sessions.contains_key(&session_id) {
            if role == DeviceRole::Controller {
                return Err(RelayError::SessionNotFound);
            }
            if sessions.len() >= self.config.max_sessions {
                return Err(RelayError::SessionLimitReached);
            }
            if active_connection_count(&sessions) >= self.config.max_connections {
                return Err(RelayError::ConnectionLimitReached);
            }
            sessions.insert(
                session_id,
                SessionRecord {
                    created_at: now,
                    expires_at: now + self.config.session_ttl,
                    authentication,
                    host: Some(connection_id),
                    controller: None,
                    host_participant_id: participant_id,
                    controller_participant_id: None,
                },
            );
            return Ok(None);
        }

        let current_connections = active_connection_count(&sessions);
        let Some(session) = sessions.get_mut(&session_id) else {
            return Err(RelayError::SessionNotFound);
        };
        apply_authentication(&mut session.authentication, authentication)?;
        match role {
            DeviceRole::Host => {
                if let Some(current) = session.host {
                    if participant_id.is_some() && participant_id == session.host_participant_id {
                        session.host = Some(connection_id);
                        session.host_participant_id = participant_id;
                        Ok(Some(current))
                    } else {
                        Err(RelayError::SessionOccupied)
                    }
                } else {
                    if current_connections >= self.config.max_connections {
                        return Err(RelayError::ConnectionLimitReached);
                    }
                    session.host = Some(connection_id);
                    session.host_participant_id = participant_id;
                    Ok(None)
                }
            }
            DeviceRole::Controller => {
                if session.host.is_none() {
                    return Err(RelayError::RoleMismatch);
                }
                if let Some(current) = session.controller {
                    if participant_id.is_some()
                        && participant_id == session.controller_participant_id
                    {
                        session.controller = Some(connection_id);
                        session.controller_participant_id = participant_id;
                        Ok(Some(current))
                    } else {
                        Err(RelayError::SessionOccupied)
                    }
                } else {
                    if current_connections >= self.config.max_connections {
                        return Err(RelayError::ConnectionLimitReached);
                    }
                    session.controller = Some(connection_id);
                    session.controller_participant_id = participant_id;
                    Ok(None)
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpiredSession {
    session_id: SessionId,
    host: Option<u64>,
    controller: Option<u64>,
}

impl ExpiredSession {
    pub fn session_id(self) -> SessionId {
        self.session_id
    }

    pub fn host_connection_id(self) -> Option<u64> {
        self.host
    }

    pub fn controller_connection_id(self) -> Option<u64> {
        self.controller
    }
}

fn active_connection_count(sessions: &HashMap<SessionId, SessionRecord>) -> usize {
    sessions
        .values()
        .map(|session| {
            usize::from(session.host.is_some()) + usize::from(session.controller.is_some())
        })
        .sum()
}

fn apply_authentication(
    expected: &mut Option<[u8; 32]>,
    actual: Option<[u8; 32]>,
) -> Result<(), RelayError> {
    match (expected.as_ref(), actual) {
        (Some(expected), Some(actual)) if expected.ct_eq(&actual).unwrap_u8() == 0 => {
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
    connection: quinn::Connection,
}

struct RelayState {
    sessions: RelaySessionTable,
    membership: Mutex<()>,
    participants: Mutex<HashMap<u64, Participant>>,
    directory: Mutex<HashMap<u64, DirectoryRecord>>,
    lookup_attempts: Mutex<HashMap<(IpAddr, u64), LookupAttempt>>,
    next_connection_id: AtomicU64,
    active_connections: std::sync::atomic::AtomicUsize,
}

struct DirectoryRecord {
    participant_id: [u8; 16],
    connection_id: u64,
    access_code: [u8; DIRECTORY_ACCESS_CODE_BYTES],
    invitation: Vec<u8>,
    expires_at: Option<Instant>,
}

impl Drop for DirectoryRecord {
    fn drop(&mut self) {
        self.access_code.fill(0);
        self.invitation.fill(0);
    }
}

struct LookupAttempt {
    window_started: Instant,
    failures: u8,
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

    fn lock_membership(&self) -> MutexGuard<'_, ()> {
        match self.membership.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn try_reserve_connection(&self) -> bool {
        let maximum = self.sessions.config.max_connections;
        let mut current = self.active_connections.load(Ordering::Acquire);
        loop {
            if current >= maximum {
                return false;
            }
            match self.active_connections.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    fn release_connection(&self) {
        self.active_connections.fetch_sub(1, Ordering::AcqRel);
    }

    fn attach_participant(
        &self,
        join: &RelayJoin,
        connection_id: u64,
        connection: quinn::Connection,
    ) -> Result<Option<quinn::Connection>, RelayError> {
        let _membership = self.lock_membership();
        if let Some(registration) = join.directory_registration() {
            self.validate_directory_slot(registration.device_id(), join.participant_id().copied())?;
        }
        let replaced = self.sessions.attach_with_auth_and_participant(
            join.session_id(),
            join.role(),
            *join.authentication(),
            join.participant_id().copied(),
            connection_id,
        )?;
        if let Some(registration) = join.directory_registration() {
            self.publish_directory(
                registration.device_id(),
                join.participant_id()
                    .copied()
                    .ok_or(RelayError::MalformedJoin)?,
                connection_id,
                *registration.access_code(),
                registration.invitation().to_vec(),
                registration.ttl_s(),
            );
        }
        let mut participants = self.lock_participants();
        let replaced = replaced.and_then(|connection_id| {
            participants
                .remove(&connection_id)
                .map(|participant| participant.connection)
        });
        participants.insert(connection_id, Participant { connection });
        Ok(replaced)
    }

    fn validate_directory_slot(
        &self,
        device_id: u64,
        participant_id: Option<[u8; 16]>,
    ) -> Result<(), RelayError> {
        let Some(participant_id) = participant_id else {
            return Err(RelayError::MalformedJoin);
        };
        let now = Instant::now();
        let mut directory = match self.directory.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        directory.retain(|_, record| record.expires_at.is_none_or(|expires_at| expires_at > now));
        if directory
            .get(&device_id)
            .is_some_and(|record| record.participant_id != participant_id)
        {
            Err(RelayError::DirectoryConflict)
        } else {
            Ok(())
        }
    }

    fn publish_directory(
        &self,
        device_id: u64,
        participant_id: [u8; 16],
        connection_id: u64,
        access_code: [u8; DIRECTORY_ACCESS_CODE_BYTES],
        invitation: Vec<u8>,
        ttl_s: u16,
    ) {
        let mut directory = match self.directory.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        directory.insert(
            device_id,
            DirectoryRecord {
                participant_id,
                connection_id,
                access_code,
                invitation,
                expires_at: (ttl_s != 0)
                    .then(|| Instant::now() + Duration::from_secs(u64::from(ttl_s))),
            },
        );
    }

    fn lookup_directory(
        &self,
        source: IpAddr,
        lookup: &RelayDirectoryLookup,
    ) -> Result<Vec<u8>, u8> {
        let now = Instant::now();
        let key = (source, lookup.device_id());
        {
            let mut attempts = match self.lookup_attempts.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            attempts.retain(|_, attempt| {
                now.duration_since(attempt.window_started) < DIRECTORY_LOOKUP_WINDOW
            });
            if attempts
                .get(&key)
                .is_some_and(|attempt| attempt.failures >= DIRECTORY_LOOKUP_FAILURE_LIMIT)
            {
                return Err(DIRECTORY_LOOKUP_RATE_LIMITED);
            }
        }

        let invitation = {
            let mut directory = match self.directory.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            directory
                .retain(|_, record| record.expires_at.is_none_or(|expires_at| expires_at > now));
            directory.get(&lookup.device_id()).and_then(|record| {
                bool::from(record.access_code.ct_eq(lookup.access_code()))
                    .then(|| record.invitation.clone())
            })
        };
        if let Some(invitation) = invitation {
            if let Ok(mut attempts) = self.lookup_attempts.lock() {
                attempts.remove(&key);
            }
            return Ok(invitation);
        }

        let mut attempts = match self.lookup_attempts.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let attempt = attempts.entry(key).or_insert(LookupAttempt {
            window_started: now,
            failures: 0,
        });
        if now.duration_since(attempt.window_started) >= DIRECTORY_LOOKUP_WINDOW {
            attempt.window_started = now;
            attempt.failures = 0;
        }
        attempt.failures = attempt.failures.saturating_add(1);
        Err(DIRECTORY_LOOKUP_NOT_FOUND)
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
        let _membership = self.lock_membership();
        self.sessions.detach(session_id, connection_id);
        self.lock_participants().remove(&connection_id);
        let mut directory = match self.directory.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        directory.retain(|_, record| record.connection_id != connection_id);
    }

    fn sweep_expired(&self, now: Instant) {
        let _membership = self.lock_membership();
        let expired = self.sessions.sweep_expired(now);
        let mut participants = self.lock_participants();
        for expired_session in expired {
            for connection_id in [expired_session.host, expired_session.controller]
                .into_iter()
                .flatten()
            {
                if let Some(participant) = participants.remove(&connection_id) {
                    participant
                        .connection
                        .close(quinn::VarInt::from_u32(2), b"session expired");
                }
            }
        }
        let mut directory = match self.directory.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        directory.retain(|_, record| record.expires_at.is_none_or(|expires_at| expires_at > now));
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
        if config.keep_alive >= config.dead_timeout {
            return Err(RelayError::InvalidConfig(
                "keepalive must be shorter than dead timeout".to_owned(),
            ));
        }
        if config.max_connections == 0 || config.max_sessions == 0 {
            return Err(RelayError::InvalidConfig(
                "admission limits must be nonzero".to_owned(),
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
            membership: Mutex::new(()),
            participants: Mutex::new(HashMap::new()),
            directory: Mutex::new(HashMap::new()),
            lookup_attempts: Mutex::new(HashMap::new()),
            next_connection_id: AtomicU64::new(1),
            active_connections: std::sync::atomic::AtomicUsize::new(0),
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
                    if !state.try_reserve_connection() {
                        tokio::spawn(async move {
                            if let Ok(connection) = incoming.await {
                                connection.close(
                                    quinn::VarInt::from_u32(RELAY_CONNECTION_LIMIT_CLOSE_CODE),
                                    b"connection admission limit reached",
                                );
                                let _ = connection.closed().await;
                            }
                        });
                        continue;
                    }
                    let connection_id = state.next_connection_id();
                    tokio::spawn(async move {
                        if let Ok(connection) = incoming.await {
                            handle_connection(connection, state.clone(), connection_id).await;
                        }
                        state.release_connection();
                    });
                }
                _ = sweep.tick() => {
                    self.state.sweep_expired(Instant::now());
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
    let request = match read_request(&mut join_receive).await {
        Ok(request) => request,
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
    let join = match request {
        RelayRequest::Join(join) => join,
        RelayRequest::DirectoryLookup(lookup) => {
            let response = state.lookup_directory(connection.remote_address().ip(), &lookup);
            match response {
                Ok(mut invitation) => {
                    let length = invitation.len() as u16;
                    let sent = join_send.write_all(&[DIRECTORY_LOOKUP_FOUND]).await.is_ok()
                        && join_send.write_all(&length.to_be_bytes()).await.is_ok()
                        && join_send.write_all(&invitation).await.is_ok();
                    invitation.fill(0);
                    if sent {
                        let _ = join_send.finish();
                    }
                }
                Err(status) => {
                    if join_send.write_all(&[status]).await.is_ok() {
                        let _ = join_send.finish();
                    }
                }
            }
            // Keep the QUIC connection alive briefly so the peer can consume the completed
            // response stream before this handler releases its final connection handle.
            tokio::time::sleep(Duration::from_millis(20)).await;
            return;
        }
    };
    let replaced = match state.attach_participant(&join, connection_id, connection.clone()) {
        Ok(replaced) => replaced,
        Err(error) => {
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
    };
    if let Some(previous) = replaced {
        previous.close(
            quinn::VarInt::from_u32(4),
            b"participant resumed on a new connection",
        );
    }
    let session_id = join.session_id();
    let role = join.role();
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

enum RelayRequest {
    Join(RelayJoin),
    DirectoryLookup(RelayDirectoryLookup),
}

async fn read_request(receive: &mut quinn::RecvStream) -> Result<RelayRequest, JoinRejectCode> {
    let mut length = [0; 4];
    receive
        .read_exact(&mut length)
        .await
        .map_err(|_| JoinRejectCode::Malformed)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_JOIN_ENVELOPE_BYTES {
        return Err(JoinRejectCode::TooLarge);
    }
    let mut bytes = vec![0; length];
    receive
        .read_exact(&mut bytes)
        .await
        .map_err(|_| JoinRejectCode::Malformed)?;
    if length == DIRECTORY_LOOKUP_ENVELOPE_BYTES
        && let Ok(lookup) = decode_directory_lookup(&bytes)
    {
        return Ok(RelayRequest::DirectoryLookup(lookup));
    }
    decode_relay_join(&bytes)
        .map(RelayRequest::Join)
        .map_err(|_| JoinRejectCode::Malformed)
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
            RelayError::ConnectionLimitReached => Self::ConnectionLimit,
            RelayError::SessionLimitReached => Self::SessionLimit,
            RelayError::DirectoryConflict => Self::DirectoryConflict,
            _ => Self::Internal,
        }
    }
}
