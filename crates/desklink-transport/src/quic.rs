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
    streams: Mutex<OutboundStreams>,
    events: Mutex<mpsc::Receiver<TransportEvent>>,
}

#[derive(Default)]
struct OutboundStreams {
    control: Option<quinn::SendStream>,
    input: Option<quinn::SendStream>,
    video_config: Option<quinn::SendStream>,
}

impl QuicClient {
    pub async fn connect(config: QuicClientConfig) -> Result<Self, TransportError> {
        if config.server_name.is_empty() {
            return Err(TransportError::InvalidConfig(
                "server name must not be empty".to_owned(),
            ));
        }
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
            streams: Mutex::new(OutboundStreams::default()),
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
        let mut streams = self.inner.streams.lock().await;
        let stream = match channel {
            ChannelKind::Control => &mut streams.control,
            ChannelKind::Input => &mut streams.input,
            ChannelKind::VideoConfig => &mut streams.video_config,
            ChannelKind::VideoDatagram | ChannelKind::CursorDatagram => {
                return Err(TransportError::Malformed);
            }
        };
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
        let Some(stream) = stream.as_mut() else {
            return Err(TransportError::Closed);
        };
        let length = (bytes.len() as u32).to_be_bytes();
        if let Err(error) = stream.write_all(&length).await {
            return Err(TransportError::Stream(error.to_string()));
        }
        if let Err(error) = stream.write_all(&bytes).await {
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
                let Ok((_send, receive)) = accepted else { break; };
                let events = events.clone();
                tokio::spawn(read_reliable_stream(receive, events));
            }
            datagram = connection.read_datagram() => {
                let Ok(datagram) = datagram else { break; };
                if let Some(event) = decode_datagram(datagram.as_ref()) {
                    if events.send(event).await.is_err() { break; }
                }
            }
            closed = connection.closed() => {
                let _ = events.send(TransportEvent::Closed { reason: closed.to_string() }).await;
                break;
            }
        }
    }
}

async fn read_reliable_stream(
    mut receive: quinn::RecvStream,
    events: mpsc::Sender<TransportEvent>,
) {
    let mut channel = [0; 1];
    if receive.read_exact(&mut channel).await.is_err() {
        return;
    }
    let Ok(channel) = ChannelKind::try_from(channel[0]) else {
        return;
    };
    if !channel.is_reliable() {
        return;
    }
    loop {
        let mut length = [0; 4];
        if receive.read_exact(&mut length).await.is_err() {
            return;
        }
        let length = u32::from_be_bytes(length) as usize;
        if length > MAX_RELIABLE_MESSAGE_BYTES {
            return;
        }
        let mut bytes = vec![0; length];
        if receive.read_exact(&mut bytes).await.is_err() {
            return;
        }
        let event = match channel {
            ChannelKind::Control => TransportEvent::Control(bytes),
            ChannelKind::Input => TransportEvent::Input(bytes),
            ChannelKind::VideoConfig => TransportEvent::VideoConfig(bytes),
            ChannelKind::VideoDatagram | ChannelKind::CursorDatagram => return,
        };
        if events.send(event).await.is_err() {
            return;
        }
    }
}

fn decode_datagram(bytes: &[u8]) -> Option<TransportEvent> {
    if bytes.is_empty() || bytes.len() - 1 > MAX_DATAGRAM_BYTES {
        return None;
    }
    let payload = bytes[1..].to_vec();
    match ChannelKind::try_from(bytes[0]) {
        Ok(ChannelKind::VideoDatagram) => Some(TransportEvent::VideoDatagram(payload)),
        Ok(ChannelKind::CursorDatagram) => Some(TransportEvent::CursorDatagram(payload)),
        _ => None,
    }
}
