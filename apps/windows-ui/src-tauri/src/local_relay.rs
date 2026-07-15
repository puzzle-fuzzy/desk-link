use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use apps_windows::configuration::HostConnectionSettings;
use desklink_transport::{QuicClient, QuicClientConfig, TransportError};
use serde::Serialize;

pub const MANAGED_RELAY_ADDRESS: &str = "101.35.246.159:4433";
pub const MANAGED_RELAY_SERVER_NAME: &str = "turn.p2p.yxswy.com";
const PAIRING_PACKAGE_HEADER: &str = "DESKLINK-PAIR-1";
const RELAY_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayStatusSummary {
    pub mode: &'static str,
    pub state: &'static str,
    pub title: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayProbeResult {
    pub title: String,
    pub detail: String,
    pub relay_address: String,
    pub elapsed_ms: u128,
}

pub fn client_config(
    relay_address: SocketAddr,
    server_name: &str,
) -> Result<QuicClientConfig, TransportError> {
    QuicClientConfig::new(relay_address, server_name.to_owned())
}

pub fn status(settings: Option<&HostConnectionSettings>) -> RelayStatusSummary {
    let Some(settings) = settings else {
        return RelayStatusSummary {
            mode: "unconfigured",
            state: "inactive",
            title: "尚未启用远程连接".to_owned(),
            detail: "启用后，DeskLink 会通过受系统信任证书保护的公网中继连接两台电脑。".to_owned(),
        };
    };
    let managed = settings.relay_address().to_string() == MANAGED_RELAY_ADDRESS
        && settings
            .server_name()
            .eq_ignore_ascii_case(MANAGED_RELAY_SERVER_NAME);
    RelayStatusSummary {
        mode: "external",
        state: "ready",
        title: if managed {
            "DeskLink 公网中继已配置".to_owned()
        } else {
            "自建中继已配置".to_owned()
        },
        detail: if managed {
            "两台电脑可以位于不同网络，业务内容仍由端到端加密保护。".to_owned()
        } else {
            "两台电脑都将连接到已保存、且证书受 Windows 信任的中继服务器。".to_owned()
        },
    }
}

pub async fn probe(
    relay_address: SocketAddr,
    server_name: &str,
) -> Result<RelayProbeResult, String> {
    let config = client_config(relay_address, server_name)
        .map_err(|_| "中继地址或 TLS 服务器名称无效，请检查连接码是否完整。".to_owned())?;
    let started = Instant::now();
    match tokio::time::timeout(RELAY_PROBE_TIMEOUT, QuicClient::connect(config)).await {
        Ok(Ok(client)) => {
            drop(client);
            Ok(RelayProbeResult {
                title: "中继服务器可以连接".to_owned(),
                detail: "已完成 QUIC 和 TLS 身份校验，可以继续安全连接。".to_owned(),
                relay_address: relay_address.to_string(),
                elapsed_ms: started.elapsed().as_millis().max(1),
            })
        }
        Ok(Err(_)) | Err(_) => Err(
            "无法连接中继服务器。请检查网络连接，稍后重试；如果使用自建中继，请同时检查地址、UDP 端口和 TLS 证书名称。"
                .to_owned(),
        ),
    }
}

pub fn pairing_package(settings: &HostConnectionSettings, invitation: &str) -> String {
    format!(
        "{PAIRING_PACKAGE_HEADER}\n{}\n{}\n{invitation}",
        settings.relay_address(),
        settings.server_name()
    )
}

#[cfg(test)]
mod tests {
    use apps_windows::configuration::HostConnectionSettings;

    use super::{MANAGED_RELAY_ADDRESS, MANAGED_RELAY_SERVER_NAME, pairing_package, status};

    fn managed_settings() -> HostConnectionSettings {
        HostConnectionSettings::from_text(
            MANAGED_RELAY_ADDRESS,
            MANAGED_RELAY_SERVER_NAME,
            "00112233445566778899aabbccddeeff",
            &"11".repeat(32),
            None,
            "1",
        )
        .unwrap()
    }

    #[test]
    fn managed_relay_status_is_external() {
        let summary = status(Some(&managed_settings()));
        assert_eq!(summary.mode, "external");
        assert_eq!(summary.state, "ready");
        assert!(summary.title.contains("公网中继"));
    }

    #[test]
    fn pairing_package_carries_the_tls_verified_public_endpoint() {
        let package = pairing_package(&managed_settings(), "signed-invitation");
        assert!(package.starts_with("DESKLINK-PAIR-1\n101.35.246.159:4433\n"));
        assert!(package.contains(MANAGED_RELAY_SERVER_NAME));
    }
}
