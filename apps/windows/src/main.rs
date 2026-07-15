#![cfg_attr(all(windows, feature = "installer-gui"), windows_subsystem = "windows")]

#[cfg(windows)]
use std::{
    env,
    error::Error,
    fmt, io,
    net::SocketAddr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
use apps_windows::{
    configuration::{
        HostConnectionSettings, WindowsConnectionSettingsDialog, WindowsConnectionSettingsStore,
        request_running_host_shutdown,
    },
    diagnostics::{DiagnosticEvent, DiagnosticLog},
    identity::WindowsIdentityStore,
    runtime::{ControllerAuthorizer, HostLifecycleEvent, HostLifecycleObserver, HostSupervisor},
    tray::WindowsTrayApplication,
    trusted::{
        WindowsControllerAuthorizer, WindowsPairingAuthorizer, WindowsTrustedControllerStore,
    },
    window::WindowsLocalApprovalDialog,
};
#[cfg(windows)]
use desklink_crypto::{MAX_PAIRING_TTL_S, PairingInvite};
#[cfg(windows)]
use desklink_transport::QuicClientConfig;
#[cfg(windows)]
use ed25519_dalek::VerifyingKey;
#[cfg(windows)]
use rand_core::OsRng;

#[cfg(windows)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let arguments = env::args_os().collect::<Vec<_>>();
    let startup_mode = arguments.iter().any(|argument| argument == "--startup");
    let configure_mode = arguments.iter().any(|argument| argument == "--configure");
    let relay_override = read_optional_env("DESKLINK_RELAY_ADDR")?
        .map(|value| value.parse::<SocketAddr>())
        .transpose()?;
    let server_name_override = read_optional_env("DESKLINK_RELAY_SERVER_NAME")?;
    let stream_id_override = read_optional_env("DESKLINK_STREAM_ID")?
        .map(|value| value.parse::<u64>())
        .transpose()?;
    let pairing_mode = read_bool_env("DESKLINK_PAIRING_MODE")?;
    let development_fallback = read_optional_hex_env::<32>("DESKLINK_PEER_VERIFY_KEY")?
        .map(|bytes| VerifyingKey::from_bytes(&bytes))
        .transpose()?;

    if pairing_mode && development_fallback.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "DESKLINK_PAIRING_MODE and DESKLINK_PEER_VERIFY_KEY cannot be used together",
        )
        .into());
    }
    if pairing_mode && configure_mode {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--configure cannot be combined with DESKLINK_PAIRING_MODE",
        )
        .into());
    }
    if !pairing_mode
        && development_fallback.is_some()
        && env::var("DESKLINK_APPROVE_SESSION").as_deref() != Ok("1")
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the development key fallback requires DESKLINK_APPROVE_SESSION=1 after confirming the controller identity",
        )
        .into());
    }

    let trusted = WindowsTrustedControllerStore::for_current_user()?;
    if read_bool_env("DESKLINK_MANAGE_TRUST")? {
        manage_trusted_controllers(&trusted)?;
        return Ok(());
    }
    let connection_store = WindowsConnectionSettingsStore::for_current_user()?;
    let mut connection_load_error = None;
    let mut persisted_connection = match connection_store.load() {
        Ok(settings) => settings,
        Err(error) => {
            connection_load_error = Some(format!(
                "hosting configuration could not be loaded: {error}"
            ));
            None
        }
    };
    let environment_connection = if pairing_mode {
        None
    } else {
        read_environment_connection(
            relay_override,
            server_name_override.as_deref(),
            stream_id_override,
        )?
    };
    let needs_first_run_configuration = !pairing_mode
        && environment_connection.is_none()
        && persisted_connection.is_none()
        && !startup_mode;
    if configure_mode || needs_first_run_configuration {
        let saved = WindowsConnectionSettingsDialog::show(
            &connection_store,
            persisted_connection.as_ref(),
        )?;
        if !saved {
            return Ok(());
        }
        request_running_host_shutdown()?;
        persisted_connection = connection_store.load()?;
        connection_load_error = None;
    }

    let (relay_addr, server_name, stream_id, connection) = if pairing_mode {
        let relay_addr = relay_override
            .or_else(|| {
                persisted_connection
                    .as_ref()
                    .map(HostConnectionSettings::relay_address)
            })
            .unwrap_or_else(|| {
                "127.0.0.1:4433"
                    .parse()
                    .expect("valid default relay address")
            });
        let server_name = server_name_override
            .or_else(|| {
                persisted_connection
                    .as_ref()
                    .map(|settings| settings.server_name().to_owned())
            })
            .unwrap_or_else(|| "localhost".to_owned());
        let stream_id = stream_id_override
            .or_else(|| {
                persisted_connection
                    .as_ref()
                    .map(HostConnectionSettings::stream_id)
            })
            .unwrap_or(1);
        (relay_addr, server_name, stream_id, None)
    } else {
        let connection = environment_connection.or(persisted_connection);
        let Some(connection) = connection else {
            let reason = connection_load_error.unwrap_or_else(|| {
                "hosting configuration is missing; open Connection settings from the tray menu"
                    .to_owned()
            });
            run_unconfigured_startup(trusted, reason).await?;
            return Ok(());
        };
        (
            connection.relay_address(),
            connection.server_name().to_owned(),
            connection.stream_id(),
            Some(connection),
        )
    };
    let identity = WindowsIdentityStore::for_current_user()?.load_or_create(&mut OsRng)?;
    let (session_id, authentication, authorizer, expires_at_unix_s): (
        _,
        _,
        Arc<dyn ControllerAuthorizer>,
        _,
    ) = if pairing_mode {
        let now_unix_s = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "system clock is before Unix epoch",
                )
            })?
            .as_secs();
        let invite = PairingInvite::new(&identity, now_unix_s, MAX_PAIRING_TTL_S)?;
        print_pairing_material(&invite)?;
        let session_id = invite.session_id();
        let authentication = *invite.relay_authentication();
        let expires_at_unix_s = invite.expires_at_unix_s();
        let authorizer = WindowsPairingAuthorizer::new(
            trusted.clone(),
            invite,
            Box::new(WindowsLocalApprovalDialog),
        );
        (
            session_id,
            authentication,
            Arc::new(authorizer),
            Some(expires_at_unix_s),
        )
    } else {
        let connection = connection
            .as_ref()
            .expect("non-pairing connections are resolved before host startup");
        let session_id = connection.session_id();
        let authentication = *connection.authentication();
        let authorizer = match development_fallback {
            Some(expected) => {
                WindowsControllerAuthorizer::with_development_fallback(trusted.clone(), expected)
            }
            None => WindowsControllerAuthorizer::new(trusted.clone()),
        };
        (session_id, authentication, Arc::new(authorizer), None)
    };

    let diagnostics = DiagnosticLog::for_current_user()?;
    record_diagnostic(
        &diagnostics,
        &DiagnosticEvent::ApplicationStarted { pairing_mode },
    );
    let tray = WindowsTrayApplication::start_with_diagnostics(trusted, diagnostics.clone())?;
    let tray_handle = tray.handle();
    if pairing_mode {
        tray_handle.show();
    }
    let status_handle = tray_handle.clone();
    let lifecycle_diagnostics = diagnostics.clone();
    let observer: Arc<dyn HostLifecycleObserver> = Arc::new(move |event: HostLifecycleEvent| {
        record_diagnostic(
            &lifecycle_diagnostics,
            &DiagnosticEvent::Lifecycle(event.clone()),
        );
        status_handle.publish(event);
    });
    let supervisor = HostSupervisor::new(
        QuicClientConfig::new(relay_addr, server_name)?,
        session_id,
        authentication,
        stream_id,
        identity,
        authorizer,
        expires_at_unix_s,
    )?
    .with_observer(observer)
    .run();
    let mut exit = tray.exit_receiver();
    let supervisor_result = {
        tokio::pin!(supervisor);
        tokio::select! {
            result = &mut supervisor => {
                tray_handle.show();
                while !*exit.borrow() && exit.changed().await.is_ok() {}
                Some(result)
            }
            changed = exit.changed() => {
                let _ = changed;
                None
            }
        }
    };
    tray.shutdown();
    record_diagnostic(&diagnostics, &DiagnosticEvent::ApplicationStopped);
    if let Some(result) = supervisor_result {
        result?;
    }
    Ok(())
}

#[cfg(windows)]
async fn run_unconfigured_startup(
    trusted: WindowsTrustedControllerStore,
    reason: String,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let diagnostics = DiagnosticLog::for_current_user()?;
    record_diagnostic(
        &diagnostics,
        &DiagnosticEvent::ApplicationStarted {
            pairing_mode: false,
        },
    );
    let tray = WindowsTrayApplication::start_with_diagnostics(trusted, diagnostics.clone())?;
    tray.handle()
        .publish(HostLifecycleEvent::Stopped { reason });
    let mut exit = tray.exit_receiver();
    while !*exit.borrow() && exit.changed().await.is_ok() {}
    tray.shutdown();
    record_diagnostic(&diagnostics, &DiagnosticEvent::ApplicationStopped);
    Ok(())
}

#[cfg(windows)]
fn record_diagnostic(diagnostics: &DiagnosticLog, event: &DiagnosticEvent) {
    if diagnostics.record(event).is_err() {
        eprintln!("DeskLink could not update its local diagnostic log.");
    }
}

#[cfg(windows)]
fn read_environment_connection(
    relay_override: Option<SocketAddr>,
    server_name_override: Option<&str>,
    stream_id_override: Option<u64>,
) -> io::Result<Option<HostConnectionSettings>> {
    let session_id = read_optional_env("DESKLINK_SESSION_ID")?;
    let authentication = read_optional_env("DESKLINK_AUTH_KEY")?;
    let (Some(session_id), Some(authentication)) = (session_id, authentication) else {
        if env::var_os("DESKLINK_SESSION_ID").is_some()
            || env::var_os("DESKLINK_AUTH_KEY").is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "DESKLINK_SESSION_ID and DESKLINK_AUTH_KEY must be provided together",
            ));
        }
        return Ok(None);
    };
    HostConnectionSettings::from_text(
        &relay_override
            .unwrap_or_else(|| {
                "127.0.0.1:4433"
                    .parse()
                    .expect("valid default relay address")
            })
            .to_string(),
        server_name_override.unwrap_or("localhost"),
        &session_id,
        &authentication,
        None,
        &stream_id_override.unwrap_or(1).to_string(),
    )
    .map(Some)
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

#[cfg(windows)]
fn read_optional_env(name: &str) -> io::Result<Option<String>> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must contain Unicode text"),
        )),
    }
}

#[cfg(windows)]
fn read_hex_env<const N: usize>(name: &str) -> io::Result<[u8; N]> {
    let value = env::var(name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("missing required environment variable {name}"),
        )
    })?;
    if value.len() != N * 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{name} must contain exactly {} hexadecimal characters",
                N * 2
            ),
        ));
    }
    let mut output = [0_u8; N];
    for (index, byte) in output.iter_mut().enumerate() {
        let offset = index * 2;
        *byte = u8::from_str_radix(&value[offset..offset + 2], 16).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} contains non-hexadecimal characters"),
            )
        })?;
    }
    Ok(output)
}

#[cfg(windows)]
fn read_optional_hex_env<const N: usize>(name: &str) -> io::Result<Option<[u8; N]>> {
    if env::var_os(name).is_none() {
        return Ok(None);
    }
    read_hex_env(name).map(Some)
}

#[cfg(windows)]
fn read_bool_env(name: &str) -> io::Result<bool> {
    match env::var(name) {
        Err(env::VarError::NotPresent) => Ok(false),
        Ok(value) if value == "0" => Ok(false),
        Ok(value) if value == "1" => Ok(true),
        Ok(_) | Err(env::VarError::NotUnicode(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be 0 or 1"),
        )),
    }
}

#[cfg(windows)]
fn print_pairing_material(invite: &PairingInvite) -> io::Result<()> {
    use std::io::Write as _;

    let encoded = invite
        .encode()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut stderr = io::stderr().lock();
    writeln!(
        stderr,
        "DeskLink pairing is active for at most {MAX_PAIRING_TTL_S} seconds. Treat the following values as secrets:"
    )?;
    writeln!(
        stderr,
        "DESKLINK_SESSION_ID={}",
        HexBytes(invite.session_id().as_bytes())
    )?;
    writeln!(
        stderr,
        "DESKLINK_AUTH_KEY={}",
        HexBytes(invite.relay_authentication())
    )?;
    writeln!(
        stderr,
        "DESKLINK_HOST_VERIFY_KEY={}",
        HexBytes(invite.host_verify_key().as_bytes())
    )?;
    writeln!(
        stderr,
        "DESKLINK_PAIRING_INVITE={}",
        HexBytes(encoded.as_bytes())
    )?;
    stderr.flush()
}

#[cfg(windows)]
fn manage_trusted_controllers(
    store: &WindowsTrustedControllerStore,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let records = store.list()?;
    if records.is_empty() {
        eprintln!("DeskLink has no trusted controllers for the current Windows user.");
        return Ok(());
    }
    for record in records {
        if WindowsLocalApprovalDialog::confirm_revocation(record.device_id(), record.verify_key()) {
            store.revoke(record.fingerprint())?;
            eprintln!("Revoked one DeskLink controller.");
        }
    }
    Ok(())
}

#[cfg(windows)]
struct HexBytes<'a>(&'a [u8]);

#[cfg(windows)]
impl fmt::Display for HexBytes<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(not(windows))]
fn main() {}
