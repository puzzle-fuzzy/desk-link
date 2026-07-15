use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

use apps_windows::{
    configuration::{HostConnectionSettings, WindowsConnectionSettingsStore},
    diagnostics::{DiagnosticEvent, DiagnosticLog, DiagnosticOperation},
};
use desklink_relay::{RelayConfig, RelayServer};
use desklink_transport::{QuicClient, QuicClientConfig, TransportError};
use quinn::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::Serialize;

pub const LAN_RELAY_SERVER_NAME: &str = "desklink-lan";
const LAN_RELAY_PORT: u16 = 4433;
const PAIRING_PACKAGE_HEADER: &str = "DESKLINK-PAIR-1";
const RELAY_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

static LOCAL_RELAY: LazyLock<Mutex<LocalRelaySupervisor>> =
    LazyLock::new(|| Mutex::new(LocalRelaySupervisor::default()));

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum LocalRelayRuntimeState {
    #[default]
    Inactive,
    Starting,
    Ready,
    Failed,
}

#[derive(Default)]
struct LocalRelaySupervisor {
    generation: u64,
    state: LocalRelayRuntimeState,
    abort: Option<tokio::task::AbortHandle>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanAddressSummary {
    pub relay_address: String,
    pub interface_name: String,
    pub is_primary: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayStatusSummary {
    pub mode: &'static str,
    pub state: &'static str,
    pub title: String,
    pub detail: String,
    pub port: Option<u16>,
    pub addresses: Vec<LanAddressSummary>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayProbeResult {
    pub title: String,
    pub detail: String,
    pub relay_address: String,
    pub elapsed_ms: u128,
}

pub fn start_if_configured() {
    let configured_for_lan = WindowsConnectionSettingsStore::for_current_user()
        .and_then(|store| store.load())
        .ok()
        .flatten()
        .is_some_and(|settings| is_lan_relay(&settings));
    if !configured_for_lan {
        stop_local_relay();
        return;
    }

    let generation = {
        let Ok(mut supervisor) = LOCAL_RELAY.lock() else {
            return;
        };
        if supervisor.abort.is_some() {
            return;
        }
        supervisor.generation = supervisor.generation.wrapping_add(1);
        supervisor.state = LocalRelayRuntimeState::Starting;
        supervisor.generation
    };

    let task = tauri::async_runtime::spawn(async move {
        match bind_local_relay().await {
            Ok(relay) => {
                update_local_relay_state(generation, LocalRelayRuntimeState::Ready, false);
                record_local_relay_event(DiagnosticEvent::OperationSucceeded(
                    DiagnosticOperation::LocalRelayStartup,
                ));
                let reason = relay.run().await.map_or_else(
                    |error| error.to_string(),
                    |_| "local relay stopped unexpectedly".to_owned(),
                );
                record_local_relay_event(DiagnosticEvent::OperationFailed {
                    operation: DiagnosticOperation::LocalRelayStartup,
                    reason,
                });
                update_local_relay_state(generation, LocalRelayRuntimeState::Failed, true);
            }
            Err(error) => {
                record_local_relay_event(DiagnosticEvent::OperationFailed {
                    operation: DiagnosticOperation::LocalRelayStartup,
                    reason: error.to_string(),
                });
                update_local_relay_state(generation, LocalRelayRuntimeState::Failed, true);
            }
        }
    });
    let abort = task.inner().abort_handle();
    drop(task);
    let Ok(mut supervisor) = LOCAL_RELAY.lock() else {
        abort.abort();
        return;
    };
    if supervisor.generation == generation && supervisor.state != LocalRelayRuntimeState::Failed {
        supervisor.abort = Some(abort);
    } else {
        abort.abort();
    }
}

fn stop_local_relay() {
    let Ok(mut supervisor) = LOCAL_RELAY.lock() else {
        return;
    };
    supervisor.generation = supervisor.generation.wrapping_add(1);
    if let Some(abort) = supervisor.abort.take() {
        abort.abort();
    }
    supervisor.state = LocalRelayRuntimeState::Inactive;
}

fn update_local_relay_state(generation: u64, state: LocalRelayRuntimeState, clear_abort: bool) {
    let Ok(mut supervisor) = LOCAL_RELAY.lock() else {
        return;
    };
    if supervisor.generation != generation {
        return;
    }
    supervisor.state = state;
    if clear_abort {
        supervisor.abort = None;
    }
}

pub fn client_config(
    relay_address: SocketAddr,
    server_name: &str,
) -> Result<QuicClientConfig, TransportError> {
    if is_lan_endpoint(relay_address, server_name) {
        QuicClientConfig::new_lan(relay_address, server_name.to_owned())
    } else {
        QuicClientConfig::new(relay_address, server_name.to_owned())
    }
}

pub fn status(settings: Option<&HostConnectionSettings>) -> RelayStatusSummary {
    let Some(settings) = settings else {
        return RelayStatusSummary {
            mode: "unconfigured",
            state: "inactive",
            title: "尚未配置中继".to_owned(),
            detail: "保存连接设置后，DeskLink 会在这里检查中继可用性。".to_owned(),
            port: None,
            addresses: Vec::new(),
        };
    };
    if !is_lan_relay(settings) {
        return RelayStatusSummary {
            mode: "external",
            state: "ready",
            title: "使用外部中继".to_owned(),
            detail: "两台电脑都将连接到已保存的中继服务器。".to_owned(),
            port: Some(settings.relay_address().port()),
            addresses: Vec::new(),
        };
    }

    let addresses = lan_ipv4_addresses(settings.relay_address().port());
    let runtime_state = LOCAL_RELAY
        .lock()
        .map(|supervisor| supervisor.state)
        .unwrap_or(LocalRelayRuntimeState::Failed);
    if addresses.is_empty() && runtime_state == LocalRelayRuntimeState::Ready {
        return RelayStatusSummary {
            mode: "lan",
            state: "offline",
            title: "没有可用的局域网地址".to_owned(),
            detail: "请连接 Wi-Fi 或有线网络后刷新状态，再创建配对连接码。".to_owned(),
            port: Some(settings.relay_address().port()),
            addresses,
        };
    }
    let (state, title, detail) = match runtime_state {
        LocalRelayRuntimeState::Inactive | LocalRelayRuntimeState::Starting => (
            "starting",
            "正在启动局域网中继",
            "DeskLink 正在监听本机 UDP 端口，通常几秒内即可使用。",
        ),
        LocalRelayRuntimeState::Ready => (
            "ready",
            "局域网中继已就绪",
            "创建连接码时可选择与另一台电脑处于同一网络的地址。",
        ),
        LocalRelayRuntimeState::Failed => (
            "failed",
            "局域网中继启动失败",
            "无法监听 UDP 4433，端口可能被占用。关闭占用程序或重新启动 DeskLink 后再试。",
        ),
    };
    RelayStatusSummary {
        mode: "lan",
        state,
        title: title.to_owned(),
        detail: detail.to_owned(),
        port: Some(settings.relay_address().port()),
        addresses,
    }
}

pub async fn probe(
    relay_address: SocketAddr,
    server_name: &str,
) -> Result<RelayProbeResult, String> {
    let lan = is_lan_endpoint(relay_address, server_name);
    let config = client_config(relay_address, server_name)
        .map_err(|_| "中继地址或 TLS 服务器名称无效，请检查连接码是否完整。".to_owned())?;
    let started = Instant::now();
    match tokio::time::timeout(RELAY_PROBE_TIMEOUT, QuicClient::connect(config)).await {
        Ok(Ok(client)) => {
            drop(client);
            Ok(RelayProbeResult {
                title: "中继服务器可以连接".to_owned(),
                detail: if lan {
                    "已完成局域网 QUIC 握手，可以继续安全连接并在主机上批准此电脑。"
                        .to_owned()
                } else {
                    "已完成 QUIC 和 TLS 握手，可以继续安全连接。".to_owned()
                },
                relay_address: relay_address.to_string(),
                elapsed_ms: started.elapsed().as_millis().max(1),
            })
        }
        Ok(Err(_)) | Err(_) if lan => Err(
            "无法到达主机电脑的局域网中继。请确认两台电脑连接同一局域网、主机 DeskLink 正在运行，并允许 Windows 防火墙的专用网络访问；访客 Wi-Fi 还可能隔离设备。"
                .to_owned(),
        ),
        Ok(Err(_)) | Err(_) => Err(
            "无法连接中继服务器。请检查地址、网络连接、TLS 服务器名称和服务器运行状态。"
                .to_owned(),
        ),
    }
}

pub fn pairing_package(settings: &HostConnectionSettings, invitation: &str) -> Result<String, ()> {
    let relay_address = advertised_relay_address(settings).ok_or(())?;
    let server_name = if is_lan_relay(settings) {
        LAN_RELAY_SERVER_NAME
    } else {
        settings.server_name()
    };
    Ok(format!(
        "{PAIRING_PACKAGE_HEADER}\n{relay_address}\n{server_name}\n{invitation}"
    ))
}

fn is_lan_relay(settings: &HostConnectionSettings) -> bool {
    is_lan_endpoint(settings.relay_address(), settings.server_name())
}

pub(crate) fn is_lan_endpoint(relay_address: SocketAddr, server_name: &str) -> bool {
    matches!(server_name, "localhost" | LAN_RELAY_SERVER_NAME)
        && is_private_or_loopback(relay_address.ip())
}

fn is_private_or_loopback(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_private() || address.is_loopback(),
        IpAddr::V6(address) => address.is_unique_local() || address.is_loopback(),
    }
}

fn advertised_relay_address(settings: &HostConnectionSettings) -> Option<SocketAddr> {
    let address = settings.relay_address();
    if !address.ip().is_loopback() {
        return Some(address);
    }
    preferred_lan_ipv4().map(|ip| SocketAddr::new(IpAddr::V4(ip), address.port()))
}

fn preferred_lan_ipv4() -> Option<Ipv4Addr> {
    let primary = primary_lan_ipv4();
    let mut addresses = system_lan_ipv4_addresses();
    sort_lan_candidates(&mut addresses, primary);
    addresses.first().map(|candidate| candidate.address)
}

fn primary_lan_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(address) if !address.is_loopback() && !address.is_unspecified() => Some(address),
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LanIpv4Candidate {
    address: Ipv4Addr,
    interface_name: String,
    is_virtual: bool,
}

fn lan_ipv4_addresses(port: u16) -> Vec<LanAddressSummary> {
    let primary = primary_lan_ipv4();
    let mut addresses = system_lan_ipv4_addresses();
    sort_lan_candidates(&mut addresses, primary);
    let recommended = addresses.first().map(|candidate| candidate.address);
    addresses
        .into_iter()
        .map(|candidate| LanAddressSummary {
            relay_address: SocketAddr::new(IpAddr::V4(candidate.address), port).to_string(),
            interface_name: candidate.interface_name,
            is_primary: Some(candidate.address) == recommended,
        })
        .collect()
}

fn sort_lan_candidates(addresses: &mut [LanIpv4Candidate], primary: Option<Ipv4Addr>) {
    addresses.sort_by(|left, right| {
        left.is_virtual
            .cmp(&right.is_virtual)
            .then_with(|| {
                let left_primary = Some(left.address) == primary;
                let right_primary = Some(right.address) == primary;
                right_primary.cmp(&left_primary)
            })
            .then_with(|| left.interface_name.cmp(&right.interface_name))
            .then_with(|| left.address.octets().cmp(&right.address.octets()))
    });
}

fn interface_name_is_virtual(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    [
        "virtual",
        "vethernet",
        "hyper-v",
        "wsl",
        "vmware",
        "virtualbox",
        "tailscale",
        "wireguard",
        "vpn",
        "tap-",
        "loopback",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

#[cfg(windows)]
fn system_lan_ipv4_addresses() -> Vec<LanIpv4Candidate> {
    use std::mem::size_of;

    use windows::Win32::{
        Foundation::ERROR_BUFFER_OVERFLOW,
        NetworkManagement::{
            IpHelper::{
                GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
                GetAdaptersAddresses, IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH,
            },
            Ndis::IfOperStatusUp,
        },
        Networking::WinSock::{AF_INET, SOCKADDR_IN},
    };

    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut byte_count = 0u32;
    let sizing =
        unsafe { GetAdaptersAddresses(AF_INET.0.into(), flags, None, None, &mut byte_count) };
    if sizing != ERROR_BUFFER_OVERFLOW.0 || byte_count == 0 {
        return Vec::new();
    }
    let word_count = (byte_count as usize).div_ceil(size_of::<usize>());
    let mut storage = vec![0usize; word_count];
    let first = storage.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    let status = unsafe {
        GetAdaptersAddresses(AF_INET.0.into(), flags, None, Some(first), &mut byte_count)
    };
    if status != 0 {
        return Vec::new();
    }

    let mut output = Vec::new();
    let mut adapter = first;
    while !adapter.is_null() {
        let current = unsafe { &*adapter };
        if current.OperStatus == IfOperStatusUp && current.IfType != IF_TYPE_SOFTWARE_LOOPBACK {
            let interface_name = if current.FriendlyName.is_null() {
                "未命名网络".to_owned()
            } else {
                unsafe { current.FriendlyName.to_string() }
                    .unwrap_or_else(|_| "未命名网络".to_owned())
            };
            let mut unicast = current.FirstUnicastAddress;
            while !unicast.is_null() {
                let address = unsafe { &*unicast }.Address;
                if !address.lpSockaddr.is_null()
                    && address.iSockaddrLength as usize >= size_of::<SOCKADDR_IN>()
                {
                    let socket_address = unsafe { &*address.lpSockaddr.cast::<SOCKADDR_IN>() };
                    if socket_address.sin_family == AF_INET {
                        let octets = unsafe { socket_address.sin_addr.S_un.S_un_b };
                        let ipv4 =
                            Ipv4Addr::new(octets.s_b1, octets.s_b2, octets.s_b3, octets.s_b4);
                        if ipv4.is_private() && !ipv4.is_loopback() && !ipv4.is_unspecified() {
                            output.push(LanIpv4Candidate {
                                address: ipv4,
                                interface_name: interface_name.clone(),
                                is_virtual: interface_name_is_virtual(&interface_name),
                            });
                        }
                    }
                }
                unicast = unsafe { (*unicast).Next };
            }
        }
        adapter = current.Next;
    }
    output.sort_by(|left, right| {
        left.address
            .octets()
            .cmp(&right.address.octets())
            .then_with(|| left.interface_name.cmp(&right.interface_name))
    });
    output.dedup_by(|left, right| left.address == right.address);
    output
}

#[cfg(not(windows))]
fn system_lan_ipv4_addresses() -> Vec<LanIpv4Candidate> {
    primary_lan_ipv4()
        .filter(Ipv4Addr::is_private)
        .map(|address| {
            vec![LanIpv4Candidate {
                address,
                interface_name: "局域网".to_owned(),
                is_virtual: false,
            }]
        })
        .unwrap_or_default()
}

async fn bind_local_relay() -> Result<Arc<RelayServer>, Box<dyn std::error::Error + Send + Sync>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let server_config = local_relay_server_config()?;
    Ok(Arc::new(
        RelayServer::bind(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), LAN_RELAY_PORT),
            server_config,
            RelayConfig::default(),
        )
        .await?,
    ))
}

fn local_relay_server_config() -> Result<ServerConfig, Box<dyn std::error::Error + Send + Sync>> {
    let certificate = rcgen::generate_simple_self_signed(vec![
        LAN_RELAY_SERVER_NAME.to_owned(),
        "localhost".to_owned(),
    ])?;
    let certificate_der = certificate.cert.der().to_vec();
    let key_der = certificate.key_pair.serialize_der();
    Ok(ServerConfig::with_single_cert(
        vec![CertificateDer::from(certificate_der)],
        PrivateKeyDer::Pkcs8(key_der.into()),
    )?)
}

fn record_local_relay_event(event: DiagnosticEvent) {
    if let Ok(diagnostics) = DiagnosticLog::for_current_user() {
        let _ = diagnostics.record(&event);
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use desklink_crypto::SessionId;
    use desklink_relay::{RelayConfig, RelayServer};
    use desklink_transport::{QuicClient, RelayJoin, TransportEvent};

    use super::{
        LAN_RELAY_SERVER_NAME, client_config, interface_name_is_virtual, is_lan_endpoint,
        local_relay_server_config, probe, status,
    };

    #[test]
    fn unconfigured_relay_status_has_no_network_addresses() {
        let summary = status(None);

        assert_eq!(summary.mode, "unconfigured");
        assert_eq!(summary.state, "inactive");
        assert!(summary.addresses.is_empty());
    }

    #[test]
    fn virtual_adapter_names_do_not_become_the_recommended_lan_address() {
        assert!(interface_name_is_virtual(
            "vEthernet (WSL (Hyper-V firewall))"
        ));
        assert!(interface_name_is_virtual("Tailscale Tunnel"));
        assert!(!interface_name_is_virtual("Wi-Fi"));
        assert!(!interface_name_is_virtual("以太网"));
    }

    #[test]
    fn lan_certificate_mode_is_restricted_to_private_addresses_and_explicit_names() {
        assert!(is_lan_endpoint(
            "192.168.1.10:4433".parse().unwrap(),
            LAN_RELAY_SERVER_NAME
        ));
        assert!(is_lan_endpoint(
            "127.0.0.1:4433".parse().unwrap(),
            "localhost"
        ));
        assert!(!is_lan_endpoint(
            "203.0.113.10:4433".parse().unwrap(),
            LAN_RELAY_SERVER_NAME
        ));
        assert!(!is_lan_endpoint(
            "192.168.1.10:4433".parse().unwrap(),
            "relay.example.com"
        ));
    }

    #[tokio::test]
    async fn embedded_lan_relay_matches_host_and_controller_traffic() {
        let relay = Arc::new(
            RelayServer::bind(
                "127.0.0.1:0".parse().unwrap(),
                local_relay_server_config().unwrap(),
                RelayConfig::default(),
            )
            .await
            .unwrap(),
        );
        let address = relay.local_addr().unwrap();
        let task_relay = relay.clone();
        let task = tokio::spawn(async move { task_relay.run().await });
        let probe_result = probe(address, LAN_RELAY_SERVER_NAME).await.unwrap();
        assert_eq!(probe_result.relay_address, address.to_string());
        let session_id = SessionId::from_bytes([17; 16]);
        let authentication = [23; 32];

        let host = QuicClient::connect(client_config(address, LAN_RELAY_SERVER_NAME).unwrap())
            .await
            .unwrap();
        host.join(RelayJoin::host(session_id, authentication))
            .await
            .unwrap();
        let controller =
            QuicClient::connect(client_config(address, LAN_RELAY_SERVER_NAME).unwrap())
                .await
                .unwrap();
        controller
            .join(RelayJoin::controller(session_id, authentication))
            .await
            .unwrap();

        host.send_control(vec![4, 5, 6]).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), controller.next_event())
                .await
                .unwrap()
                .unwrap(),
            TransportEvent::Control(vec![4, 5, 6])
        );
        task.abort();
    }
}
