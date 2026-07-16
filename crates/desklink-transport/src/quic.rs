use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use bytes::Bytes;
use tokio::sync::{Mutex, mpsc};

use crate::{
    ChannelKind, DIRECTORY_LOOKUP_FOUND, DIRECTORY_LOOKUP_MALFORMED, DIRECTORY_LOOKUP_NOT_FOUND,
    DIRECTORY_LOOKUP_RATE_LIMITED, JoinRejectCode, MAX_DATAGRAM_BYTES,
    MAX_DIRECTORY_INVITATION_BYTES, MAX_RELIABLE_MESSAGE_BYTES, QuicClientConfig,
    RELAY_CONNECTION_LIMIT_CLOSE_CODE, RelayDirectoryLookup, RelayJoin, TransportError,
    TransportEvent,
};

const INBOUND_RELIABLE_QUEUE_CAPACITY: usize = 128;
const INBOUND_DATAGRAM_QUEUE_CAPACITY: usize = 128;
const INBOUND_CLOSED_QUEUE_CAPACITY: usize = 8;
const INBOUND_LANE_COUNT: usize = 6;

pub struct QuicClient {
    _endpoint: quinn::Endpoint,
    inner: Arc<ClientInner>,
}

struct ClientInner {
    connection: quinn::Connection,
    joined: AtomicBool,
    join_lock: Mutex<()>,
    control: Mutex<Option<quinn::SendStream>>,
    input: Mutex<Option<quinn::SendStream>>,
    video_config: Mutex<Option<quinn::SendStream>>,
    events: InboundReceivers,
}

#[derive(Clone)]
struct InboundSenders {
    control: mpsc::Sender<TransportEvent>,
    input: mpsc::Sender<TransportEvent>,
    video_config: mpsc::Sender<TransportEvent>,
    video_datagram: mpsc::Sender<TransportEvent>,
    cursor_datagram: mpsc::Sender<TransportEvent>,
    closed: mpsc::Sender<TransportEvent>,
}

struct InboundReceivers {
    control: Mutex<mpsc::Receiver<TransportEvent>>,
    input: Mutex<mpsc::Receiver<TransportEvent>>,
    video_config: Mutex<mpsc::Receiver<TransportEvent>>,
    video_datagram: Mutex<mpsc::Receiver<TransportEvent>>,
    cursor_datagram: Mutex<mpsc::Receiver<TransportEvent>>,
    closed: Mutex<mpsc::Receiver<TransportEvent>>,
    control_open: AtomicBool,
    input_open: AtomicBool,
    video_config_open: AtomicBool,
    video_datagram_open: AtomicBool,
    cursor_datagram_open: AtomicBool,
    closed_open: AtomicBool,
    next_lane: AtomicUsize,
}

enum LanePoll {
    Event(TransportEvent),
    Empty,
    Closed,
    Locked,
}

impl InboundReceivers {
    fn new(
        control: mpsc::Receiver<TransportEvent>,
        input: mpsc::Receiver<TransportEvent>,
        video_config: mpsc::Receiver<TransportEvent>,
        video_datagram: mpsc::Receiver<TransportEvent>,
        cursor_datagram: mpsc::Receiver<TransportEvent>,
        closed: mpsc::Receiver<TransportEvent>,
    ) -> Self {
        Self {
            control: Mutex::new(control),
            input: Mutex::new(input),
            video_config: Mutex::new(video_config),
            video_datagram: Mutex::new(video_datagram),
            cursor_datagram: Mutex::new(cursor_datagram),
            closed: Mutex::new(closed),
            control_open: AtomicBool::new(true),
            input_open: AtomicBool::new(true),
            video_config_open: AtomicBool::new(true),
            video_datagram_open: AtomicBool::new(true),
            cursor_datagram_open: AtomicBool::new(true),
            closed_open: AtomicBool::new(true),
            next_lane: AtomicUsize::new(0),
        }
    }

    fn has_open_lane(&self) -> bool {
        self.control_open.load(Ordering::Acquire)
            || self.input_open.load(Ordering::Acquire)
            || self.video_config_open.load(Ordering::Acquire)
            || self.video_datagram_open.load(Ordering::Acquire)
            || self.cursor_datagram_open.load(Ordering::Acquire)
            || self.closed_open.load(Ordering::Acquire)
    }

    fn lane_open(&self, lane: usize) -> bool {
        match lane {
            0 => self.control_open.load(Ordering::Acquire),
            1 => self.input_open.load(Ordering::Acquire),
            2 => self.video_config_open.load(Ordering::Acquire),
            3 => self.video_datagram_open.load(Ordering::Acquire),
            4 => self.cursor_datagram_open.load(Ordering::Acquire),
            5 => self.closed_open.load(Ordering::Acquire),
            _ => unreachable!("invalid inbound lane index"),
        }
    }

    fn receiver(&self, lane: usize) -> &Mutex<mpsc::Receiver<TransportEvent>> {
        match lane {
            0 => &self.control,
            1 => &self.input,
            2 => &self.video_config,
            3 => &self.video_datagram,
            4 => &self.cursor_datagram,
            5 => &self.closed,
            _ => unreachable!("invalid inbound lane index"),
        }
    }

    fn try_recv_lane(&self, lane: usize) -> LanePoll {
        let Ok(mut receiver) = self.receiver(lane).try_lock() else {
            return LanePoll::Locked;
        };
        match receiver.try_recv() {
            Ok(event) => LanePoll::Event(event),
            Err(mpsc::error::TryRecvError::Empty) => LanePoll::Empty,
            Err(mpsc::error::TryRecvError::Disconnected) => LanePoll::Closed,
        }
    }

    fn close_lane(&self, lane: usize) {
        match lane {
            0 => self.control_open.store(false, Ordering::Release),
            1 => self.input_open.store(false, Ordering::Release),
            2 => self.video_config_open.store(false, Ordering::Release),
            3 => self.video_datagram_open.store(false, Ordering::Release),
            4 => self.cursor_datagram_open.store(false, Ordering::Release),
            5 => self.closed_open.store(false, Ordering::Release),
            _ => unreachable!("invalid inbound lane index"),
        }
    }

    fn set_next_lane(&self, lane: usize) {
        self.next_lane
            .store((lane + 1) % INBOUND_LANE_COUNT, Ordering::Release);
    }
}

impl QuicClient {
    pub async fn connect(config: QuicClientConfig) -> Result<Self, TransportError> {
        if config.server_name.is_empty() {
            return Err(TransportError::InvalidConfig(
                "server name must not be empty".to_owned(),
            ));
        }
        config.validate_timeouts()?;
        let idle_timeout = quinn::IdleTimeout::try_from(config.dead_timeout)
            .map_err(|error| TransportError::InvalidConfig(error.to_string()))?;
        let mut transport = quinn::TransportConfig::default();
        transport
            .keep_alive_interval(Some(config.keep_alive))
            .max_idle_timeout(Some(idle_timeout));
        let mut client_config = config.client_config.clone();
        client_config.transport_config(Arc::new(transport));

        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().map_err(|error| {
            TransportError::InvalidConfig(format!("client bind address: {error}"))
        })?)
        .map_err(|error| TransportError::InvalidConfig(error.to_string()))?;
        endpoint.set_default_client_config(client_config);
        let connection = endpoint
            .connect(config.relay_addr, &config.server_name)
            .map_err(|error| TransportError::Connection(error.to_string()))?
            .await
            .map_err(map_connection_error)?;

        let (control_sender, control_receiver) = mpsc::channel(INBOUND_RELIABLE_QUEUE_CAPACITY);
        let (input_sender, input_receiver) = mpsc::channel(INBOUND_RELIABLE_QUEUE_CAPACITY);
        let (video_config_sender, video_config_receiver) =
            mpsc::channel(INBOUND_RELIABLE_QUEUE_CAPACITY);
        let (video_datagram_sender, video_datagram_receiver) =
            mpsc::channel(INBOUND_DATAGRAM_QUEUE_CAPACITY);
        let (cursor_datagram_sender, cursor_datagram_receiver) =
            mpsc::channel(INBOUND_DATAGRAM_QUEUE_CAPACITY);
        let (closed_sender, closed_receiver) = mpsc::channel(INBOUND_CLOSED_QUEUE_CAPACITY);
        let inbound_senders = InboundSenders {
            control: control_sender,
            input: input_sender,
            video_config: video_config_sender,
            video_datagram: video_datagram_sender,
            cursor_datagram: cursor_datagram_sender,
            closed: closed_sender,
        };
        let inner = Arc::new(ClientInner {
            connection: connection.clone(),
            joined: AtomicBool::new(false),
            join_lock: Mutex::new(()),
            control: Mutex::new(None),
            input: Mutex::new(None),
            video_config: Mutex::new(None),
            events: InboundReceivers::new(
                control_receiver,
                input_receiver,
                video_config_receiver,
                video_datagram_receiver,
                cursor_datagram_receiver,
                closed_receiver,
            ),
        });
        tokio::spawn(read_connection(connection, inbound_senders));
        Ok(Self {
            _endpoint: endpoint,
            inner,
        })
    }

    pub async fn join(&self, join: RelayJoin) -> Result<(), TransportError> {
        let _join_guard = self.inner.join_lock.lock().await;
        if self.inner.joined.load(Ordering::Acquire) {
            return Err(TransportError::AlreadyJoined);
        }
        let (mut send, mut receive) = self
            .inner
            .connection
            .open_bi()
            .await
            .map_err(map_connection_error)?;
        let envelope = join.encode();
        send.write_all(&(envelope.len() as u32).to_be_bytes())
            .await
            .map_err(map_write_error)?;
        send.write_all(&envelope).await.map_err(map_write_error)?;
        send.finish()
            .map_err(|error| TransportError::Stream(error.to_string()))?;

        let mut response = [0; 1];
        receive
            .read_exact(&mut response)
            .await
            .map_err(|error| map_read_exact_error(&self.inner.connection, error))?;
        if response[0] != 0 {
            return Err(TransportError::JoinRejected(JoinRejectCode::from_byte(
                response[0],
            )));
        }
        self.inner.joined.store(true, Ordering::Release);
        Ok(())
    }

    pub async fn lookup_directory(
        &self,
        lookup: RelayDirectoryLookup,
    ) -> Result<Vec<u8>, TransportError> {
        let _join_guard = self.inner.join_lock.lock().await;
        if self.inner.joined.load(Ordering::Acquire) {
            return Err(TransportError::AlreadyJoined);
        }
        let (mut send, mut receive) = self
            .inner
            .connection
            .open_bi()
            .await
            .map_err(map_connection_error)?;
        let envelope = lookup.encode();
        send.write_all(&(envelope.len() as u32).to_be_bytes())
            .await
            .map_err(map_write_error)?;
        send.write_all(&envelope).await.map_err(map_write_error)?;
        send.finish()
            .map_err(|error| TransportError::Stream(error.to_string()))?;

        let mut status = [0; 1];
        receive
            .read_exact(&mut status)
            .await
            .map_err(|error| map_read_exact_error(&self.inner.connection, error))?;
        match status[0] {
            DIRECTORY_LOOKUP_FOUND => {
                let mut length = [0; 2];
                receive
                    .read_exact(&mut length)
                    .await
                    .map_err(|error| map_read_exact_error(&self.inner.connection, error))?;
                let length = u16::from_be_bytes(length) as usize;
                if length == 0 || length > MAX_DIRECTORY_INVITATION_BYTES {
                    return Err(TransportError::Malformed);
                }
                let mut invitation = vec![0; length];
                receive
                    .read_exact(&mut invitation)
                    .await
                    .map_err(|error| map_read_exact_error(&self.inner.connection, error))?;
                Ok(invitation)
            }
            DIRECTORY_LOOKUP_NOT_FOUND => Err(TransportError::DirectoryNotFound),
            DIRECTORY_LOOKUP_RATE_LIMITED => Err(TransportError::DirectoryRateLimited),
            DIRECTORY_LOOKUP_MALFORMED => Err(TransportError::Malformed),
            _ => Err(TransportError::Malformed),
        }
    }

    pub async fn send_control(&self, bytes: Vec<u8>) -> Result<(), TransportError> {
        self.send_reliable(ChannelKind::Control, bytes).await
    }

    pub async fn send_input(&self, bytes: Vec<u8>) -> Result<(), TransportError> {
        self.send_reliable(ChannelKind::Input, bytes).await
    }

    pub async fn send_video_config(&self, bytes: Vec<u8>) -> Result<(), TransportError> {
        self.send_reliable(ChannelKind::VideoConfig, bytes).await
    }

    pub async fn send_video_datagram(&self, bytes: Vec<u8>) -> Result<(), TransportError> {
        self.send_datagram(ChannelKind::VideoDatagram, bytes)
    }

    pub async fn send_cursor_datagram(&self, bytes: Vec<u8>) -> Result<(), TransportError> {
        self.send_datagram(ChannelKind::CursorDatagram, bytes)
    }

    pub async fn next_event(&self) -> Result<TransportEvent, TransportError> {
        loop {
            let start = self.inner.events.next_lane.load(Ordering::Acquire);
            for offset in 0..INBOUND_LANE_COUNT {
                let lane = (start + offset) % INBOUND_LANE_COUNT;
                if !self.inner.events.lane_open(lane) {
                    continue;
                }
                match self.inner.events.try_recv_lane(lane) {
                    LanePoll::Event(event) => {
                        self.inner.events.set_next_lane(lane);
                        return Ok(event);
                    }
                    LanePoll::Closed => self.inner.events.close_lane(lane),
                    LanePoll::Empty | LanePoll::Locked => {}
                }
            }
            if !self.inner.events.has_open_lane() {
                return Err(TransportError::Closed);
            }

            tokio::select! {
                event = recv_lane(&self.inner.events.input), if self.inner.events.input_open.load(Ordering::Acquire) => {
                    if let Some(event) = event {
                        self.inner.events.set_next_lane(1);
                        return Ok(event);
                    }
                    self.inner.events.close_lane(1);
                }
                event = recv_lane(&self.inner.events.control), if self.inner.events.control_open.load(Ordering::Acquire) => {
                    if let Some(event) = event {
                        self.inner.events.set_next_lane(0);
                        return Ok(event);
                    }
                    self.inner.events.close_lane(0);
                }
                event = recv_lane(&self.inner.events.video_config), if self.inner.events.video_config_open.load(Ordering::Acquire) => {
                    if let Some(event) = event {
                        self.inner.events.set_next_lane(2);
                        return Ok(event);
                    }
                    self.inner.events.close_lane(2);
                }
                event = recv_lane(&self.inner.events.video_datagram), if self.inner.events.video_datagram_open.load(Ordering::Acquire) => {
                    if let Some(event) = event {
                        self.inner.events.set_next_lane(3);
                        return Ok(event);
                    }
                    self.inner.events.close_lane(3);
                }
                event = recv_lane(&self.inner.events.cursor_datagram), if self.inner.events.cursor_datagram_open.load(Ordering::Acquire) => {
                    if let Some(event) = event {
                        self.inner.events.set_next_lane(4);
                        return Ok(event);
                    }
                    self.inner.events.close_lane(4);
                }
                event = recv_lane(&self.inner.events.closed), if self.inner.events.closed_open.load(Ordering::Acquire) => {
                    if let Some(event) = event {
                        self.inner.events.set_next_lane(5);
                        return Ok(event);
                    }
                    self.inner.events.close_lane(5);
                }
            }
        }
    }

    pub async fn next_control(&self) -> Result<Vec<u8>, TransportError> {
        receive_payload(&self.inner.events.control, |event| match event {
            TransportEvent::Control(bytes) => Some(bytes),
            _ => None,
        })
        .await
    }

    pub async fn next_input(&self) -> Result<Vec<u8>, TransportError> {
        receive_payload(&self.inner.events.input, |event| match event {
            TransportEvent::Input(bytes) => Some(bytes),
            _ => None,
        })
        .await
    }

    pub async fn next_video_config(&self) -> Result<Vec<u8>, TransportError> {
        receive_payload(&self.inner.events.video_config, |event| match event {
            TransportEvent::VideoConfig(bytes) => Some(bytes),
            _ => None,
        })
        .await
    }

    pub async fn next_video_datagram(&self) -> Result<Vec<u8>, TransportError> {
        receive_payload(&self.inner.events.video_datagram, |event| match event {
            TransportEvent::VideoDatagram(bytes) => Some(bytes),
            _ => None,
        })
        .await
    }

    pub async fn next_cursor_datagram(&self) -> Result<Vec<u8>, TransportError> {
        receive_payload(&self.inner.events.cursor_datagram, |event| match event {
            TransportEvent::CursorDatagram(bytes) => Some(bytes),
            _ => None,
        })
        .await
    }

    async fn send_reliable(
        &self,
        channel: ChannelKind,
        bytes: Vec<u8>,
    ) -> Result<(), TransportError> {
        if bytes.len() > MAX_RELIABLE_MESSAGE_BYTES {
            return Err(TransportError::MessageTooLarge {
                actual: bytes.len(),
                maximum: MAX_RELIABLE_MESSAGE_BYTES,
            });
        }
        self.ensure_joined()?;
        let stream_lock = match channel {
            ChannelKind::Control => &self.inner.control,
            ChannelKind::Input => &self.inner.input,
            ChannelKind::VideoConfig => &self.inner.video_config,
            ChannelKind::VideoDatagram | ChannelKind::CursorDatagram => {
                return Err(TransportError::Malformed);
            }
        };
        let mut stream = stream_lock.lock().await;
        if stream.is_none() {
            let (mut send, _receive) = self
                .inner
                .connection
                .open_bi()
                .await
                .map_err(map_connection_error)?;
            send.write_all(&[channel as u8])
                .await
                .map_err(map_write_error)?;
            *stream = Some(send);
        }
        let Some(outbound) = stream.as_mut() else {
            return Err(TransportError::Closed);
        };
        let length = (bytes.len() as u32).to_be_bytes();
        if let Err(error) = outbound.write_all(&length).await {
            *stream = None;
            return Err(map_write_error(error));
        }
        if let Err(error) = outbound.write_all(&bytes).await {
            *stream = None;
            return Err(map_write_error(error));
        }
        Ok(())
    }

    fn send_datagram(&self, channel: ChannelKind, bytes: Vec<u8>) -> Result<(), TransportError> {
        if bytes.len() > MAX_DATAGRAM_BYTES {
            return Err(TransportError::MessageTooLarge {
                actual: bytes.len(),
                maximum: MAX_DATAGRAM_BYTES,
            });
        }
        self.ensure_joined()?;
        let mut frame = Vec::with_capacity(bytes.len() + 1);
        frame.push(channel as u8);
        frame.extend_from_slice(&bytes);
        self.inner
            .connection
            .send_datagram(Bytes::from(frame))
            .map_err(map_datagram_error)
    }

    fn ensure_joined(&self) -> Result<(), TransportError> {
        if self.inner.joined.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(TransportError::NotJoined)
        }
    }
}

impl Drop for QuicClient {
    fn drop(&mut self) {
        self.inner
            .connection
            .close(quinn::VarInt::from_u32(0), b"DeskLink client closed");
    }
}

fn map_connection_error(error: quinn::ConnectionError) -> TransportError {
    match error {
        quinn::ConnectionError::ApplicationClosed(close)
            if close.error_code == quinn::VarInt::from_u32(RELAY_CONNECTION_LIMIT_CLOSE_CODE) =>
        {
            TransportError::ConnectionLimit
        }
        quinn::ConnectionError::ConnectionClosed(close)
            if u64::from(close.error_code) == u64::from(RELAY_CONNECTION_LIMIT_CLOSE_CODE) =>
        {
            TransportError::ConnectionLimit
        }
        error => TransportError::Connection(error.to_string()),
    }
}

fn map_read_exact_error(
    connection: &quinn::Connection,
    error: quinn::ReadExactError,
) -> TransportError {
    match error {
        quinn::ReadExactError::ReadError(quinn::ReadError::ConnectionLost(error)) => {
            map_connection_error(error)
        }
        quinn::ReadExactError::FinishedEarly(read) => connection
            .close_reason()
            .map(map_connection_error)
            .unwrap_or_else(|| {
                TransportError::Connection(format!("stream finished early ({read} bytes read)"))
            }),
        error => TransportError::Connection(error.to_string()),
    }
}

fn map_write_error(error: quinn::WriteError) -> TransportError {
    match error {
        quinn::WriteError::ConnectionLost(error) => map_connection_error(error),
        error => TransportError::Stream(error.to_string()),
    }
}

fn map_datagram_error(error: quinn::SendDatagramError) -> TransportError {
    match error {
        quinn::SendDatagramError::ConnectionLost(error) => map_connection_error(error),
        error => TransportError::Datagram(error.to_string()),
    }
}

async fn recv_lane(receiver: &Mutex<mpsc::Receiver<TransportEvent>>) -> Option<TransportEvent> {
    receiver.lock().await.recv().await
}

async fn receive_payload(
    receiver: &Mutex<mpsc::Receiver<TransportEvent>>,
    project: fn(TransportEvent) -> Option<Vec<u8>>,
) -> Result<Vec<u8>, TransportError> {
    match recv_lane(receiver).await {
        Some(event) => project(event).ok_or(TransportError::Malformed),
        None => Err(TransportError::Closed),
    }
}

async fn read_connection(connection: quinn::Connection, events: InboundSenders) {
    loop {
        tokio::select! {
            accepted = connection.accept_bi() => {
                let Ok((_send, receive)) = accepted else {
                    emit_closed(&events.closed, "reliable stream accept failed");
                    break;
                };
                let events = events.clone();
                tokio::spawn(read_reliable_stream(connection.clone(), receive, events));
            }
            datagram = connection.read_datagram() => {
                let Ok(datagram) = datagram else {
                    emit_closed(&events.closed, "datagram receive failed");
                    break;
                };
                match decode_datagram(datagram.as_ref()) {
                    Ok(event) => {
                        let sender = match &event {
                            TransportEvent::VideoDatagram(_) => &events.video_datagram,
                            TransportEvent::CursorDatagram(_) => &events.cursor_datagram,
                            _ => unreachable!(),
                        };
                        // Datagram delivery is intentionally lossy at this bounded boundary:
                        // drop the newest packet when its channel is saturated, while reliable
                        // channels retain backpressure only within their own queue.
                        let _ = sender.try_send(event);
                    }
                    Err(()) => {
                        connection.close(quinn::VarInt::from_u32(3), b"malformed datagram");
                        emit_closed(&events.closed, "malformed datagram");
                        break;
                    }
                }
            }
            closed = connection.closed() => {
                emit_closed(&events.closed, closed.to_string());
                break;
            }
        }
    }
}

fn emit_closed(events: &mpsc::Sender<TransportEvent>, reason: impl Into<String>) {
    let _ = events.try_send(TransportEvent::Closed {
        reason: reason.into(),
    });
}

async fn read_reliable_stream(
    connection: quinn::Connection,
    mut receive: quinn::RecvStream,
    events: InboundSenders,
) {
    let mut channel = [0; 1];
    match receive.read_exact(&mut channel).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(0)) => {
            let _ = receive.stop(quinn::VarInt::from_u32(1));
            emit_closed(&events.closed, "empty reliable stream");
            return;
        }
        Err(_) => {
            let _ = receive.stop(quinn::VarInt::from_u32(1));
            emit_closed(&events.closed, "malformed reliable stream");
            return;
        }
    }
    let Ok(channel) = ChannelKind::try_from(channel[0]) else {
        let _ = receive.stop(quinn::VarInt::from_u32(1));
        connection.close(quinn::VarInt::from_u32(3), b"unknown reliable channel");
        emit_closed(&events.closed, "unknown reliable channel");
        return;
    };
    if !channel.is_reliable() {
        let _ = receive.stop(quinn::VarInt::from_u32(1));
        connection.close(quinn::VarInt::from_u32(3), b"invalid reliable channel");
        emit_closed(&events.closed, "invalid reliable channel");
        return;
    }
    let sender = match channel {
        ChannelKind::Control => events.control,
        ChannelKind::Input => events.input,
        ChannelKind::VideoConfig => events.video_config,
        ChannelKind::VideoDatagram | ChannelKind::CursorDatagram => unreachable!(),
    };
    let mut message_seen = false;
    loop {
        let mut length = [0; 4];
        match receive.read_exact(&mut length).await {
            Ok(()) => {}
            Err(quinn::ReadExactError::FinishedEarly(0)) if message_seen => return,
            Err(_) => {
                let _ = receive.stop(quinn::VarInt::from_u32(1));
                emit_closed(&events.closed, "malformed reliable message");
                return;
            }
        }
        let length = u32::from_be_bytes(length) as usize;
        if length > MAX_RELIABLE_MESSAGE_BYTES {
            let _ = receive.stop(quinn::VarInt::from_u32(1));
            connection.close(quinn::VarInt::from_u32(3), b"reliable message too large");
            emit_closed(&events.closed, "reliable message too large");
            return;
        }
        let mut bytes = vec![0; length];
        match receive.read_exact(&mut bytes).await {
            Ok(()) => {}
            Err(_) => {
                let _ = receive.stop(quinn::VarInt::from_u32(1));
                emit_closed(&events.closed, "malformed reliable message");
                return;
            }
        }
        let event = match channel {
            ChannelKind::Control => TransportEvent::Control(bytes),
            ChannelKind::Input => TransportEvent::Input(bytes),
            ChannelKind::VideoConfig => TransportEvent::VideoConfig(bytes),
            ChannelKind::VideoDatagram | ChannelKind::CursorDatagram => unreachable!(),
        };
        if sender.send(event).await.is_err() {
            return;
        }
        message_seen = true;
    }
}

fn decode_datagram(bytes: &[u8]) -> Result<TransportEvent, ()> {
    if bytes.is_empty() || bytes.len() - 1 > MAX_DATAGRAM_BYTES {
        return Err(());
    }
    let payload = bytes[1..].to_vec();
    match ChannelKind::try_from(bytes[0]) {
        Ok(ChannelKind::VideoDatagram) => Ok(TransportEvent::VideoDatagram(payload)),
        Ok(ChannelKind::CursorDatagram) => Ok(TransportEvent::CursorDatagram(payload)),
        _ => Err(()),
    }
}
