use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use bytes::Bytes;
use tokio::sync::{Mutex, mpsc};

use crate::{
    ChannelKind, JOIN_ENVELOPE_BYTES, JoinRejectCode, MAX_DATAGRAM_BYTES,
    MAX_RELIABLE_MESSAGE_BYTES, QuicClientConfig, RelayJoin, TransportError, TransportEvent,
};

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
    events: Mutex<mpsc::Receiver<TransportEvent>>,
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
            .map_err(|error| TransportError::Connection(error.to_string()))?;

        let (event_sender, event_receiver) = mpsc::channel(128);
        let inner = Arc::new(ClientInner {
            connection: connection.clone(),
            joined: AtomicBool::new(false),
            join_lock: Mutex::new(()),
            control: Mutex::new(None),
            input: Mutex::new(None),
            video_config: Mutex::new(None),
            events: Mutex::new(event_receiver),
        });
        tokio::spawn(read_connection(connection, event_sender));
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
            .map_err(|error| TransportError::Connection(error.to_string()))?;
        let envelope = join.encode();
        send.write_all(&(JOIN_ENVELOPE_BYTES as u32).to_be_bytes())
            .await
            .map_err(|error| TransportError::Stream(error.to_string()))?;
        send.write_all(&envelope)
            .await
            .map_err(|error| TransportError::Stream(error.to_string()))?;
        send.finish()
            .map_err(|error| TransportError::Stream(error.to_string()))?;

        let mut response = [0; 1];
        receive
            .read_exact(&mut response)
            .await
            .map_err(|error| TransportError::Connection(error.to_string()))?;
        if response[0] != 0 {
            return Err(TransportError::JoinRejected(JoinRejectCode::from_byte(
                response[0],
            )));
        }
        self.inner.joined.store(true, Ordering::Release);
        Ok(())
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
        let mut events = self.inner.events.lock().await;
        events.recv().await.ok_or(TransportError::Closed)
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
                .map_err(|error| TransportError::Connection(error.to_string()))?;
            send.write_all(&[channel as u8])
                .await
                .map_err(|error| TransportError::Stream(error.to_string()))?;
            *stream = Some(send);
        }
        let Some(outbound) = stream.as_mut() else {
            return Err(TransportError::Closed);
        };
        let length = (bytes.len() as u32).to_be_bytes();
        if let Err(error) = outbound.write_all(&length).await {
            *stream = None;
            return Err(TransportError::Stream(error.to_string()));
        }
        if let Err(error) = outbound.write_all(&bytes).await {
            *stream = None;
            return Err(TransportError::Stream(error.to_string()));
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
            .map_err(|error| TransportError::Datagram(error.to_string()))
    }

    fn ensure_joined(&self) -> Result<(), TransportError> {
        if self.inner.joined.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(TransportError::NotJoined)
        }
    }
}

async fn read_connection(connection: quinn::Connection, events: mpsc::Sender<TransportEvent>) {
    loop {
        tokio::select! {
            accepted = connection.accept_bi() => {
                let Ok((_send, receive)) = accepted else {
                    emit_closed(&events, "reliable stream accept failed".to_owned()).await;
                    break;
                };
                let events = events.clone();
                tokio::spawn(read_reliable_stream(connection.clone(), receive, events));
            }
            datagram = connection.read_datagram() => {
                let Ok(datagram) = datagram else {
                    emit_closed(&events, "datagram receive failed".to_owned()).await;
                    break;
                };
                match decode_datagram(datagram.as_ref()) {
                    Ok(event) => {
                        if events.send(event).await.is_err() { break; }
                    }
                    Err(()) => {
                        connection.close(quinn::VarInt::from_u32(3), b"malformed datagram");
                        emit_closed(&events, "malformed datagram".to_owned()).await;
                        break;
                    }
                }
            }
            closed = connection.closed() => {
                emit_closed(&events, closed.to_string()).await;
                break;
            }
        }
    }
}

async fn emit_closed(events: &mpsc::Sender<TransportEvent>, reason: String) {
    let _ = events.send(TransportEvent::Closed { reason }).await;
}

async fn read_reliable_stream(
    connection: quinn::Connection,
    mut receive: quinn::RecvStream,
    events: mpsc::Sender<TransportEvent>,
) {
    let mut channel = [0; 1];
    match receive.read_exact(&mut channel).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(0)) => return,
        Err(_) => {
            let _ = receive.stop(quinn::VarInt::from_u32(1));
            emit_closed(&events, "malformed reliable stream".to_owned()).await;
            return;
        }
    }
    let Ok(channel) = ChannelKind::try_from(channel[0]) else {
        let _ = receive.stop(quinn::VarInt::from_u32(1));
        connection.close(quinn::VarInt::from_u32(3), b"unknown reliable channel");
        emit_closed(&events, "unknown reliable channel".to_owned()).await;
        return;
    };
    if !channel.is_reliable() {
        let _ = receive.stop(quinn::VarInt::from_u32(1));
        connection.close(quinn::VarInt::from_u32(3), b"invalid reliable channel");
        emit_closed(&events, "invalid reliable channel".to_owned()).await;
        return;
    }
    loop {
        let mut length = [0; 4];
        match receive.read_exact(&mut length).await {
            Ok(()) => {}
            Err(quinn::ReadExactError::FinishedEarly(0)) => return,
            Err(_) => {
                let _ = receive.stop(quinn::VarInt::from_u32(1));
                emit_closed(&events, "malformed reliable message".to_owned()).await;
                return;
            }
        }
        let length = u32::from_be_bytes(length) as usize;
        if length > MAX_RELIABLE_MESSAGE_BYTES {
            let _ = receive.stop(quinn::VarInt::from_u32(1));
            connection.close(quinn::VarInt::from_u32(3), b"reliable message too large");
            emit_closed(&events, "reliable message too large".to_owned()).await;
            return;
        }
        let mut bytes = vec![0; length];
        match receive.read_exact(&mut bytes).await {
            Ok(()) => {}
            Err(_) => {
                let _ = receive.stop(quinn::VarInt::from_u32(1));
                emit_closed(&events, "malformed reliable message".to_owned()).await;
                return;
            }
        }
        let event = match channel {
            ChannelKind::Control => TransportEvent::Control(bytes),
            ChannelKind::Input => TransportEvent::Input(bytes),
            ChannelKind::VideoConfig => TransportEvent::VideoConfig(bytes),
            ChannelKind::VideoDatagram | ChannelKind::CursorDatagram => unreachable!(),
        };
        if events.send(event).await.is_err() {
            return;
        }
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
