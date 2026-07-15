use std::{
    env,
    error::Error,
    fs::File,
    io::{self, BufReader},
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
    Ok(RelayConfig {
        session_ttl: Duration::from_secs(session_ttl_s),
        ..RelayConfig::default()
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let address = env::var("DESKLINK_RELAY_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:4433".to_owned())
        .parse()?;
    let relay = Arc::new(RelayServer::bind(address, server_config()?, relay_config()?).await?);
    eprintln!("DeskLink relay listening on {}", relay.local_addr()?);
    relay.run().await?;
    Ok(())
}
