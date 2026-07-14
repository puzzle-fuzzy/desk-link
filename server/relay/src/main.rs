use std::{env, error::Error, sync::Arc};

use desklink_relay::{RelayConfig, RelayServer};
use quinn::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

fn server_config() -> Result<ServerConfig, Box<dyn Error + Send + Sync>> {
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
    let certificate_der = certificate.cert.der().to_vec();
    let key_der = certificate.key_pair.serialize_der();
    Ok(ServerConfig::with_single_cert(
        vec![CertificateDer::from(certificate_der)],
        PrivateKeyDer::Pkcs8(key_der.into()),
    )?)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let address = env::var("DESKLINK_RELAY_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:4433".to_owned())
        .parse()?;
    let relay =
        Arc::new(RelayServer::bind(address, server_config()?, RelayConfig::default()).await?);
    relay.run().await?;
    Ok(())
}
