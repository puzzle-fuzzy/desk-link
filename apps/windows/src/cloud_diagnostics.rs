use std::{ffi::c_void, fs, path::PathBuf, ptr, thread, time::Duration};

use blake2::{Blake2s256, Digest as _};
use desklink_crypto::{DeviceIdentity, SessionId};
use rand_core::OsRng;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use windows::{
    Win32::Networking::WinHttp::{
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_FLAG_SECURE_DEFAULTS,
        WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
        WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReceiveResponse,
        WinHttpSendRequest, WinHttpSetTimeouts,
    },
    core::PCWSTR,
};

use crate::{
    diagnostic_sharing::WindowsDiagnosticSharing, diagnostics::DiagnosticLog,
    identity::WindowsIdentityStore, storage::local_app_data_path,
};

const ENDPOINT_HOST: &str = "p2p.yxswy.com";
const ENDPOINT_PATH: &str = "/desklink-diagnostics/v1/batches";
const SIGNATURE_DOMAIN: &[u8] = b"desklink-cloud-diagnostics-v1\0";
const INSTALLATION_DOMAIN: &[u8] = b"desklink-diagnostic-installation-v1\0";
const CORRELATION_DOMAIN: &[u8] = b"desklink-diagnostic-correlation-v1\0";
const MAX_BATCH_BYTES: usize = 48 * 1_024;
const MAX_BATCH_EVENTS: usize = 100;
const HEALTHY_INTERVAL: Duration = Duration::from_secs(60);
const DISABLED_INTERVAL: Duration = Duration::from_secs(15);
const MAX_RETRY_INTERVAL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSource {
    Host,
    Controller,
}

impl DiagnosticSource {
    const fn label(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Controller => "controller",
        }
    }

    const fn correlation_file(self) -> &'static str {
        match self {
            Self::Host => "host-correlation.txt",
            Self::Controller => "controller-correlation.txt",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct DiagnosticUploadSummary {
    pub uploaded_sources: u32,
    pub uploaded_events: u32,
}

#[derive(Debug, Error)]
pub enum CloudDiagnosticError {
    #[error("diagnostic sharing is not enabled")]
    SharingDisabled,
    #[error("diagnostic storage is unavailable")]
    Storage,
    #[error("diagnostic identity is unavailable")]
    Identity,
    #[error("diagnostic batch could not be encoded")]
    Encoding,
    #[error("diagnostic upload failed")]
    Network,
    #[error("diagnostic service returned HTTP {0}")]
    HttpStatus(u32),
}

#[derive(Serialize)]
struct DiagnosticBatch<'a> {
    schema: u8,
    app_version: &'a str,
    platform: &'static str,
    source: &'static str,
    installation_id: String,
    correlation_id: Option<String>,
    events: Vec<Value>,
}

struct SignedRequest {
    body: Vec<u8>,
    public_key: String,
    signature: String,
    batch_id: String,
    event_count: usize,
}

pub fn set_session_correlation(
    source: DiagnosticSource,
    session_id: SessionId,
) -> Result<(), CloudDiagnosticError> {
    let path = correlation_path(source)?;
    let parent = path.parent().ok_or(CloudDiagnosticError::Storage)?;
    fs::create_dir_all(parent).map_err(|_| CloudDiagnosticError::Storage)?;
    fs::write(path, correlation_identifier(session_id.as_bytes()))
        .map_err(|_| CloudDiagnosticError::Storage)
}

pub fn start_background_uploader() {
    let _ = thread::Builder::new()
        .name("desklink-diagnostics".to_owned())
        .spawn(|| {
            let mut retry_interval = HEALTHY_INTERVAL;
            loop {
                let sharing = WindowsDiagnosticSharing::for_current_user()
                    .and_then(|sharing| sharing.is_enabled());
                match sharing {
                    Ok(true) => {
                        if upload_all_once().is_ok() {
                            retry_interval = HEALTHY_INTERVAL;
                        } else {
                            retry_interval =
                                retry_interval.saturating_mul(2).min(MAX_RETRY_INTERVAL);
                        }
                        thread::sleep(retry_interval);
                    }
                    _ => {
                        retry_interval = HEALTHY_INTERVAL;
                        thread::sleep(DISABLED_INTERVAL);
                    }
                }
            }
        });
}

pub fn upload_all_once() -> Result<DiagnosticUploadSummary, CloudDiagnosticError> {
    let sharing =
        WindowsDiagnosticSharing::for_current_user().map_err(|_| CloudDiagnosticError::Storage)?;
    if !sharing
        .is_enabled()
        .map_err(|_| CloudDiagnosticError::Storage)?
    {
        return Err(CloudDiagnosticError::SharingDisabled);
    }
    let identity = WindowsIdentityStore::for_current_user()
        .map_err(|_| CloudDiagnosticError::Identity)?
        .load_or_create(&mut OsRng)
        .map_err(|_| CloudDiagnosticError::Identity)?;
    let mut summary = DiagnosticUploadSummary::default();
    for source in [DiagnosticSource::Host, DiagnosticSource::Controller] {
        let Some(request) = build_request(source, &identity)? else {
            continue;
        };
        send_request(&request)?;
        summary.uploaded_sources = summary.uploaded_sources.saturating_add(1);
        summary.uploaded_events = summary
            .uploaded_events
            .saturating_add(request.event_count.try_into().unwrap_or(u32::MAX));
    }
    Ok(summary)
}

fn build_request(
    source: DiagnosticSource,
    identity: &DeviceIdentity,
) -> Result<Option<SignedRequest>, CloudDiagnosticError> {
    let log = match source {
        DiagnosticSource::Host => DiagnosticLog::for_current_user(),
        DiagnosticSource::Controller => DiagnosticLog::controller_for_current_user(),
    }
    .map_err(|_| CloudDiagnosticError::Storage)?;
    let lines = log
        .recent_sanitized_lines()
        .map_err(|_| CloudDiagnosticError::Storage)?;
    let mut events = lines
        .into_iter()
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter(|event| event.is_object())
        .rev()
        .take(MAX_BATCH_EVENTS)
        .collect::<Vec<_>>();
    events.reverse();
    if events.is_empty() {
        return Ok(None);
    }
    let public_key = *identity.verify_key().as_bytes();
    let installation_id = installation_identifier(&public_key);
    let correlation_id = read_correlation(source);
    let mut batch = DiagnosticBatch {
        schema: 1,
        app_version: env!("CARGO_PKG_VERSION"),
        platform: "windows",
        source: source.label(),
        installation_id,
        correlation_id,
        events,
    };
    let mut body = serde_json::to_vec(&batch).map_err(|_| CloudDiagnosticError::Encoding)?;
    while body.len() > MAX_BATCH_BYTES && batch.events.len() > 1 {
        batch.events.remove(0);
        body = serde_json::to_vec(&batch).map_err(|_| CloudDiagnosticError::Encoding)?;
    }
    if body.len() > MAX_BATCH_BYTES {
        return Err(CloudDiagnosticError::Encoding);
    }
    let mut signed_payload = Vec::with_capacity(SIGNATURE_DOMAIN.len() + body.len());
    signed_payload.extend_from_slice(SIGNATURE_DOMAIN);
    signed_payload.extend_from_slice(&body);
    let signature = identity.sign(&signed_payload).to_bytes();
    Ok(Some(SignedRequest {
        event_count: batch.events.len(),
        batch_id: hex(&Blake2s256::digest(&body)),
        public_key: hex(&public_key),
        signature: hex(&signature),
        body,
    }))
}

fn send_request(request: &SignedRequest) -> Result<(), CloudDiagnosticError> {
    let agent = wide("DeskLink-Windows-Diagnostics/1");
    let host = wide(ENDPOINT_HOST);
    let method = wide("POST");
    let path = wide(ENDPOINT_PATH);
    let session = InternetHandle::new(unsafe {
        WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            WINHTTP_FLAG_SECURE_DEFAULTS,
        )
    })?;
    unsafe {
        WinHttpSetTimeouts(session.0, 5_000, 5_000, 10_000, 10_000)
            .map_err(|_| CloudDiagnosticError::Network)?;
    }
    let connection =
        InternetHandle::new(unsafe { WinHttpConnect(session.0, PCWSTR(host.as_ptr()), 443, 0) })?;
    let request_handle = InternetHandle::new(unsafe {
        WinHttpOpenRequest(
            connection.0,
            PCWSTR(method.as_ptr()),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            ptr::null(),
            WINHTTP_FLAG_SECURE,
        )
    })?;
    let headers = format!(
        "Content-Type: application/json\r\nX-DeskLink-Public-Key: {}\r\nX-DeskLink-Signature: {}\r\nX-DeskLink-Batch-Id: {}\r\n",
        request.public_key, request.signature, request.batch_id
    )
    .encode_utf16()
    .collect::<Vec<_>>();
    let body_len = u32::try_from(request.body.len()).map_err(|_| CloudDiagnosticError::Encoding)?;
    unsafe {
        WinHttpSendRequest(
            request_handle.0,
            Some(&headers),
            Some(request.body.as_ptr().cast::<c_void>()),
            body_len,
            body_len,
            0,
        )
        .map_err(|_| CloudDiagnosticError::Network)?;
        WinHttpReceiveResponse(request_handle.0, ptr::null_mut())
            .map_err(|_| CloudDiagnosticError::Network)?;
    }
    let mut status = 0_u32;
    let mut status_bytes = u32::try_from(std::mem::size_of::<u32>()).unwrap_or(4);
    let mut index = 0_u32;
    unsafe {
        WinHttpQueryHeaders(
            request_handle.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast::<c_void>()),
            &mut status_bytes,
            &mut index,
        )
        .map_err(|_| CloudDiagnosticError::Network)?;
    }
    if !(200..300).contains(&status) {
        return Err(CloudDiagnosticError::HttpStatus(status));
    }
    Ok(())
}

fn correlation_path(source: DiagnosticSource) -> Result<PathBuf, CloudDiagnosticError> {
    local_app_data_path()
        .map(|root| {
            root.join("DeskLink")
                .join("logs")
                .join(source.correlation_file())
        })
        .ok_or(CloudDiagnosticError::Storage)
}

fn read_correlation(source: DiagnosticSource) -> Option<String> {
    let value = fs::read_to_string(correlation_path(source).ok()?).ok()?;
    let value = value.trim().to_ascii_lowercase();
    (value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(value)
}

fn installation_identifier(public_key: &[u8; 32]) -> String {
    let mut hasher = Blake2s256::new();
    hasher.update(INSTALLATION_DOMAIN);
    hasher.update(public_key);
    hex(&hasher.finalize()[..16])
}

fn correlation_identifier(session_id: &[u8; 16]) -> String {
    let mut hasher = Blake2s256::new();
    hasher.update(CORRELATION_DOMAIN);
    hasher.update(session_id);
    hex(&hasher.finalize()[..16])
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

struct InternetHandle(*mut c_void);

impl InternetHandle {
    fn new(handle: *mut c_void) -> Result<Self, CloudDiagnosticError> {
        if handle.is_null() {
            Err(CloudDiagnosticError::Network)
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for InternetHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use desklink_crypto::DeviceIdentity;
    use ed25519_dalek::Signature;

    use super::*;

    #[test]
    fn correlation_is_stable_without_exposing_the_session_identifier() {
        let session = [0xabu8; 16];
        let correlation = correlation_identifier(&session);
        assert_eq!(correlation.len(), 32);
        assert_eq!(correlation, correlation_identifier(&session));
        assert_ne!(correlation, hex(&session));
    }

    #[test]
    fn signed_payload_uses_the_same_domain_as_the_service() {
        let identity = DeviceIdentity::from_secret_key([2; 16], &[7; 32]);
        let body = br#"{"schema":1}"#;
        let mut payload = SIGNATURE_DOMAIN.to_vec();
        payload.extend_from_slice(body);
        let signature = identity.sign(&payload).to_bytes();
        assert!(identity.verify(&payload, &Signature::from_bytes(&signature)));
        assert!(!identity.verify(body, &Signature::from_bytes(&signature)));
        assert_eq!(
            installation_identifier(identity.verify_key().as_bytes()).len(),
            32
        );
    }
}
