use std::{
    env,
    error::Error,
    ffi::OsStr,
    fs::File,
    io::{self, BufReader},
    net::SocketAddr,
    path::Path,
    sync::Arc,
    time::Duration,
};

use desklink_relay::{RelayConfig, RelayServer};
use quinn::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

const DEFAULT_SESSION_TTL_S: u64 = 86_400;
const MIN_SESSION_TTL_S: u64 = 60;
const MAX_SESSION_TTL_S: u64 = 2_592_000;
const DEFAULT_MAX_CONNECTIONS: usize = 1_024;
const DEFAULT_MAX_SESSIONS: usize = 1_024;
const MAX_CONFIGURED_LIMIT: usize = 100_000;
const CAPACITY_LOG_INTERVAL: Duration = Duration::from_secs(60);

fn development_server_config() -> Result<ServerConfig, Box<dyn Error + Send + Sync>> {
    let server_name =
        env::var("DESKLINK_RELAY_DEV_SERVER_NAME").unwrap_or_else(|_| "localhost".to_owned());
    let certificate = rcgen::generate_simple_self_signed(vec![server_name])?;
    let certificate_der = certificate.cert.der().to_vec();
    let key_der = certificate.key_pair.serialize_der();
    Ok(ServerConfig::with_single_cert(
        vec![CertificateDer::from(certificate_der)],
        PrivateKeyDer::Pkcs8(key_der.into()),
    )?)
}

fn pem_server_config(
    certificate_path: &Path,
    private_key_path: &Path,
) -> Result<ServerConfig, Box<dyn Error + Send + Sync>> {
    let mut certificate_reader = BufReader::new(File::open(certificate_path)?);
    let certificates =
        rustls_pemfile::certs(&mut certificate_reader).collect::<Result<Vec<_>, _>>()?;
    if certificates.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "DESKLINK_RELAY_CERT_PEM contains no certificates",
        )
        .into());
    }
    let mut private_key_reader = BufReader::new(File::open(private_key_path)?);
    let private_key = rustls_pemfile::private_key(&mut private_key_reader)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "DESKLINK_RELAY_KEY_PEM contains no supported private key",
        )
    })?;
    Ok(ServerConfig::with_single_cert(certificates, private_key)?)
}

fn server_config() -> Result<ServerConfig, Box<dyn Error + Send + Sync>> {
    match (
        env::var_os("DESKLINK_RELAY_CERT_PEM"),
        env::var_os("DESKLINK_RELAY_KEY_PEM"),
    ) {
        (Some(certificate), Some(private_key)) => {
            pem_server_config(Path::new(&certificate), Path::new(&private_key))
        }
        (None, None) => {
            eprintln!(
                "warning: using a development self-signed relay certificate; Windows clients require a trusted certificate for cross-PC use"
            );
            development_server_config()
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "DESKLINK_RELAY_CERT_PEM and DESKLINK_RELAY_KEY_PEM must be set together",
        )
        .into()),
    }
}

fn relay_config() -> Result<RelayConfig, Box<dyn Error + Send + Sync>> {
    let session_ttl_s = env::var("DESKLINK_RELAY_SESSION_TTL_S")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?
        .unwrap_or(DEFAULT_SESSION_TTL_S);
    if !(MIN_SESSION_TTL_S..=MAX_SESSION_TTL_S).contains(&session_ttl_s) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "DESKLINK_RELAY_SESSION_TTL_S must be between {MIN_SESSION_TTL_S} and {MAX_SESSION_TTL_S}"
            ),
        )
        .into());
    }
    let max_connections = bounded_limit(
        "DESKLINK_RELAY_MAX_CONNECTIONS",
        env::var("DESKLINK_RELAY_MAX_CONNECTIONS").ok().as_deref(),
        DEFAULT_MAX_CONNECTIONS,
    )?;
    let max_sessions = bounded_limit(
        "DESKLINK_RELAY_MAX_SESSIONS",
        env::var("DESKLINK_RELAY_MAX_SESSIONS").ok().as_deref(),
        DEFAULT_MAX_SESSIONS,
    )?;
    Ok(RelayConfig {
        session_ttl: Duration::from_secs(session_ttl_s),
        max_connections,
        max_sessions,
        ..RelayConfig::default()
    })
}

fn bounded_limit(
    name: &str,
    value: Option<&str>,
    default: usize,
) -> Result<usize, Box<dyn Error + Send + Sync>> {
    let value = value
        .map(str::parse::<usize>)
        .transpose()
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} must be an integer between 1 and {MAX_CONFIGURED_LIMIT}"),
            )
        })?
        .unwrap_or(default);
    if !(1..=MAX_CONFIGURED_LIMIT).contains(&value) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be between 1 and {MAX_CONFIGURED_LIMIT}"),
        )
        .into());
    }
    Ok(value)
}

fn relay_address() -> Result<SocketAddr, Box<dyn Error + Send + Sync>> {
    Ok(env::var("DESKLINK_RELAY_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:4433".to_owned())
        .parse()?)
}

fn check_configuration() -> Result<(), Box<dyn Error + Send + Sync>> {
    let _ = relay_address()?;
    let _ = server_config()?;
    let _ = relay_config()?;
    eprintln!("DeskLink relay configuration is valid");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    if env::args_os().nth(1).as_deref() == Some(OsStr::new("--check-config")) {
        return check_configuration();
    }
    let address = relay_address()?;
    let relay = Arc::new(RelayServer::bind(address, server_config()?, relay_config()?).await?);
    eprintln!("DeskLink relay listening on {}", relay.local_addr()?);
    let metrics_relay = relay.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(CAPACITY_LOG_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let snapshot = metrics_relay.capacity_snapshot();
            eprintln!(
                "relay_capacity active_sessions={} attached_participants={} accepted_connections={} max_sessions={} max_connections={}",
                snapshot.active_sessions,
                snapshot.attached_participants,
                snapshot.accepted_connections,
                snapshot.max_sessions,
                snapshot.max_connections,
            );
        }
    });
    relay.run().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::bounded_limit;

    #[test]
    fn configured_limits_are_bounded_and_have_a_default() {
        assert_eq!(bounded_limit("LIMIT", None, 128).unwrap(), 128);
        assert_eq!(bounded_limit("LIMIT", Some("512"), 128).unwrap(), 512);
        assert!(bounded_limit("LIMIT", Some("0"), 128).is_err());
        assert!(bounded_limit("LIMIT", Some("100001"), 128).is_err());
        assert!(bounded_limit("LIMIT", Some("many"), 128).is_err());
    }
}
