use std::{env, error::Error, io, net::SocketAddr, time::Duration};

use desklink_transport::{QuicClient, QuicClientConfig};

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    let relay_address = arguments
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "缺少中继 IP:端口"))?
        .parse::<SocketAddr>()?;
    let server_name = arguments
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "缺少 TLS 服务器名称"))?;
    if arguments.next().is_some() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "参数过多").into());
    }

    let config = QuicClientConfig::new(relay_address, server_name.clone())?;
    let client = tokio::time::timeout(PROBE_TIMEOUT, QuicClient::connect(config))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "QUIC/TLS 握手超时"))??;
    drop(client);

    println!("中继连接成功：{relay_address}，TLS 服务器名称：{server_name}");
    Ok(())
}
