use std::{env, error::Error, io, net::SocketAddr, time::Instant};

use desklink_transport::{QuicClient, QuicClientConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut arguments = env::args().skip(1);
    let relay_address = arguments
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing relay IP:port"))?
        .parse::<SocketAddr>()?;
    let server_name = arguments
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing TLS server name"))?;
    if arguments.next().is_some() {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "unexpected extra argument").into(),
        );
    }

    let started = Instant::now();
    let client =
        QuicClient::connect(QuicClientConfig::new(relay_address, server_name.clone())?).await?;
    drop(client);
    println!(
        "DeskLink relay TLS/QUIC probe passed: {relay_address} ({server_name}) in {} ms",
        started.elapsed().as_millis().max(1)
    );
    Ok(())
}
