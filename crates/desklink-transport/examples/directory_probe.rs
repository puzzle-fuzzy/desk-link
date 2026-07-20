use std::{
    env,
    error::Error,
    io,
    net::SocketAddr,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use desklink_crypto::SessionId;
use desklink_protocol::PROTOCOL_VERSION;
use desklink_transport::{
    QuicClient, QuicClientConfig, RelayDirectoryLookup, RelayDirectoryRegistration, RelayJoin,
    TransportError,
};

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
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let nonce_bytes = nonce.to_be_bytes();
    let device_id = 100_000_000_000 + (nonce as u64 % 900_000_000_000);
    let access_code = *b"AB2DEF3G";
    let wrong_code = *b"AB2DEF4G";
    let invitation = format!("desklink-directory-probe:{nonce}").into_bytes();
    let mut authentication = [0_u8; 32];
    authentication[..16].copy_from_slice(&nonce_bytes);
    authentication[16..].copy_from_slice(&nonce_bytes);
    authentication[31] ^= 0xa5;

    let config = QuicClientConfig::new(relay_address, server_name.clone())?;
    let host = QuicClient::connect(config.clone()).await?;
    let registration =
        RelayDirectoryRegistration::new(device_id, access_code, invitation.clone(), 30)?;
    let join = RelayJoin::host_with_participant(
        SessionId::from_bytes(nonce_bytes),
        authentication,
        nonce_bytes,
    )
    .with_directory_registration(registration)?;
    host.join(join).await?;

    let wrong = QuicClient::connect(config.clone()).await?;
    if wrong
        .lookup_directory(RelayDirectoryLookup::new(device_id, wrong_code)?)
        .await
        != Err(TransportError::DirectoryNotFound)
    {
        return Err(io::Error::other("wrong directory password was not rejected safely").into());
    }

    let incompatible = QuicClient::connect(config.clone()).await?;
    if incompatible
        .lookup_directory(RelayDirectoryLookup::new_for_protocol(
            device_id,
            access_code,
            PROTOCOL_VERSION + 1,
        )?)
        .await
        != Err(TransportError::DirectoryProtocolMismatch {
            controller: Some(PROTOCOL_VERSION + 1),
            host: Some(PROTOCOL_VERSION),
        })
    {
        return Err(io::Error::other(
            "directory did not reject an incompatible application protocol",
        )
        .into());
    }

    let correct = QuicClient::connect(config).await?;
    let resolved = correct
        .lookup_directory(RelayDirectoryLookup::new(device_id, access_code)?)
        .await?;
    if resolved != invitation {
        return Err(io::Error::other("directory lookup returned a different invitation").into());
    }

    // A successful directory response alone does not prove that an actual remote-control
    // session can exchange messages in both directions. Exercise the exact join and reliable
    // control lane used by the Windows host and controller so production relay regressions are
    // caught before an installer is handed to users.
    let controller =
        QuicClient::connect(QuicClientConfig::new(relay_address, server_name.clone())?).await?;
    controller
        .join(RelayJoin::controller_with_participant(
            SessionId::from_bytes(nonce_bytes),
            authentication,
            [0x5a; 16],
        ))
        .await?;
    controller
        .send_control(b"controller-hello".to_vec())
        .await?;
    let controller_hello =
        tokio::time::timeout(std::time::Duration::from_secs(5), host.next_control())
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "controller-to-host relay timed out",
                )
            })??;
    if controller_hello != b"controller-hello" {
        return Err(io::Error::other("controller-to-host relay payload changed").into());
    }
    host.send_control(b"host-hello".to_vec()).await?;
    let host_hello =
        tokio::time::timeout(std::time::Duration::from_secs(5), controller.next_control())
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "host-to-controller relay timed out",
                )
            })??;
    if host_hello != b"host-hello" {
        return Err(io::Error::other("host-to-controller relay payload changed").into());
    }

    println!(
        "DeskLink relay directory and bidirectional control probe passed: {relay_address} ({server_name}) in {} ms",
        started.elapsed().as_millis().max(1)
    );
    Ok(())
}
