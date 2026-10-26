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
    diagnostic_sharing::WindowsDiagnosticSharing,
    diagnostics::{DiagnosticLog, normalize_u64_diagnostic_fields},
    identity::WindowsIdentityStore,
    storage::local_app_data_path,
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
) -> Result<SessionCorrelationGuard, CloudDiagnosticError> {
    let path = correlation_path(source)?;
    let parent = path.parent().ok_or(CloudDiagnosticError::Storage)?;
    fs::create_dir_all(parent).map_err(|_| CloudDiagnosticError::Storage)?;
    let expected = correlation_identifier(session_id.as_bytes());
    fs::write(path, &expected).map_err(|_| CloudDiagnosticError::Storage)?;
    Ok(SessionCorrelationGuard { source, expected })
}

/// Removes a correlation marker only when it still belongs to the session that
/// created this guard. A newer connection can therefore safely replace an old
/// one without the old task clearing the newer session's diagnostics link.
pub struct SessionCorrelationGuard {
    source: DiagnosticSource,
    expected: String,
}

impl Drop for SessionCorrelationGuard {
    fn drop(&mut self) {
        let Ok(path) = correlation_path(self.source) else {
            return;
        };
        let Ok(value) = fs::read_to_string(&path) else {
            return;
        };
        if correlation_marker_matches(&value, &self.expected) {
            let _ = fs::remove_file(path);
        }
    }
}

fn correlation_marker_matches(value: &str, expected: &str) -> bool {
    value.trim().eq_ignore_ascii_case(expected)
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
    upload_all_once_with_correlation(true)
}

/// Uploads a batch for infrastructure probes without inheriting a persisted
/// user-session marker. The payload remains signed and sanitized; only the
/// optional server-side session correlation is omitted.
pub fn upload_all_once_without_session_correlation()
-> Result<DiagnosticUploadSummary, CloudDiagnosticError> {
    upload_all_once_with_correlation(false)
}

fn upload_all_once_with_correlation(
    include_session_correlation: bool,
) -> Result<DiagnosticUploadSummary, CloudDiagnosticError> {
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
        let Some(request) = build_request(source, &identity, include_session_correlation)? else {
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
    include_session_correlation: bool,
) -> Result<Option<SignedRequest>, CloudDiagnosticError> {
    let log = match source {
        DiagnosticSource::Host => DiagnosticLog::for_current_user(),
        DiagnosticSource::Controller => DiagnosticLog::controller_for_current_user(),
    }
    .map_err(|_| CloudDiagnosticError::Storage)?;
    build_request_from_log(source, identity, &log, include_session_correlation)
}

fn build_request_from_log(
    source: DiagnosticSource,
    identity: &DeviceIdentity,
    log: &DiagnosticLog,
    include_session_correlation: bool,
) -> Result<Option<SignedRequest>, CloudDiagnosticError> {
    let lines = log
        .recent_sanitized_lines()
        .map_err(|_| CloudDiagnosticError::Storage)?;
    let mut events = lines
        .into_iter()
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter(|event| event.is_object())
        .rev()
        .take(MAX_BATCH_EVENTS)
        .map(|mut event| {
            normalize_u64_diagnostic_fields(&mut event)
                .map_err(|_| CloudDiagnosticError::Encoding)?;
            Ok(event)
        })
        .collect::<Result<Vec<_>, CloudDiagnosticError>>()?;
    events.reverse();
    if events.is_empty() {
        return Ok(None);
    }
    let public_key = *identity.verify_key().as_bytes();
    let installation_id = installation_identifier(&public_key);
    let correlation_id = include_session_correlation
        .then(|| read_correlation(source))
        .flatten();
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
    use desklink_protocol::H264Profile;
    use ed25519_dalek::Signature;
    use std::{
        collections::{BTreeMap, BTreeSet},
        io::Write as _,
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::{
        configuration::{ConnectionSettingsInputError, HostConnectionSettings},
        diagnostics::{DiagnosticEvent, H264ProfileProbe, diagnostic_u64_fields},
        runtime::{
            CursorDisableReason, DirectListenerStopReason, HostLifecycleEvent, HostMediaDiagnostic,
        },
    };

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

    #[test]
    fn correlation_guard_only_matches_its_own_marker() {
        assert!(correlation_marker_matches(" ABCD ", "abcd"));
        assert!(!correlation_marker_matches("abcd0", "abcd"));
        assert!(!correlation_marker_matches("abcd", "efgh"));
    }

    struct FixtureDirectory(PathBuf);

    impl Drop for FixtureDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(serde::Serialize)]
    struct ConfiguredStreamFixture {
        schema: u8,
        u64_fields_by_event: BTreeMap<String, Vec<String>>,
        cases: Vec<ConfiguredStreamCase>,
        historical_cases: Vec<HistoricalStreamCase>,
    }

    #[derive(serde::Deserialize)]
    struct LegacyConfiguredStreamFixture {
        schema: u8,
        cases: Vec<ConfiguredStreamCase>,
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct ConfiguredStreamCase {
        name: String,
        configured_stream_id: String,
        log_lines: Vec<String>,
        request: FixtureRequest,
    }

    #[derive(serde::Serialize)]
    struct HistoricalStreamCase {
        name: String,
        configured_stream_id: String,
        log_lines: Vec<String>,
        legacy_request: FixtureRequest,
        request: FixtureRequest,
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct FixtureRequest {
        body_utf8: String,
        public_key: String,
        signature: String,
        batch_id: String,
        event_count: usize,
    }

    fn fixture_u64(value: &Value) -> u64 {
        if let Some(text) = value.as_str() {
            let parsed = text.parse::<u64>().expect("canonical decimal u64");
            assert_eq!(text, parsed.to_string());
            parsed
        } else {
            value.as_u64().expect("exact nonnegative Rust JSON u64")
        }
    }

    fn fixture_request(request: SignedRequest) -> FixtureRequest {
        FixtureRequest {
            // Preserve actual signed body bytes. No parsing/stringifying or
            // signing in another language occurs in this fixture boundary.
            body_utf8: String::from_utf8(request.body).unwrap(),
            public_key: request.public_key,
            signature: request.signature,
            batch_id: request.batch_id,
            event_count: request.event_count,
        }
    }

    fn verify_fixture_request(identity: &DeviceIdentity, request: &FixtureRequest) -> Value {
        let body = request.body_utf8.as_bytes();
        let batch: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(batch["source"], "host");
        assert!(batch["correlation_id"].is_null());
        assert_eq!(
            batch["events"].as_array().unwrap().len(),
            request.event_count
        );
        assert_eq!(request.public_key, hex(identity.verify_key().as_bytes()));
        assert_eq!(request.batch_id, hex(&Blake2s256::digest(body)));
        assert_eq!(
            batch["installation_id"],
            installation_identifier(identity.verify_key().as_bytes())
        );
        let signature_bytes = request
            .signature
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect::<Vec<_>>();
        let signature = Signature::from_slice(&signature_bytes).unwrap();
        let mut signed_payload = SIGNATURE_DOMAIN.to_vec();
        signed_payload.extend_from_slice(body);
        assert!(identity.verify(&signed_payload, &signature));
        assert!(!identity.verify(body, &signature));
        batch
    }

    fn write_historical_lines(path: &std::path::Path, lines: &[String]) -> Vec<u8> {
        let mut bytes = lines.join("\n").into_bytes();
        bytes.push(b'\n');
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_data().unwrap();
        bytes
    }

    #[test]
    fn configured_stream_logs_produce_actual_signed_batch_fixture() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temporary = std::env::temp_dir().join(format!(
            "desklink-configured-stream-fixture-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&temporary).unwrap();
        let temporary = FixtureDirectory(temporary);
        // Synthetic test identity only. Never read or persist a Windows identity,
        // a sharing preference, a correlation marker, or a current-user log.
        let identity = DeviceIdentity::from_secret_key([2; 16], &[7; 32]);
        let mut cases = Vec::new();
        let mut u64_fields_by_event = BTreeMap::new();
        for (name, stream_text, counter) in [
            ("safe", "9007199254740991", 3),
            ("two_pow_53", "9007199254740992", 3),
            ("two_pow_53_plus_one", "9007199254740993", 3),
            ("u64_max", "18446744073709551615", 3),
            ("all_u64_max", "7", u64::MAX),
            // Zero applies to counters; stream 7 remains a nonzero identity.
            ("zero_counters", "7", 0),
        ] {
            let settings = HostConnectionSettings::from_text(
                "127.0.0.1:4433",
                "localhost",
                "00112233445566778899aabbccddeeff",
                "",
                Some([0x31; 32]),
                stream_text,
            )
            .expect("production configuration accepts every tested nonzero u64 stream");
            let stream_id = settings.stream_id();
            assert_eq!(stream_id.to_string(), stream_text);
            let log = DiagnosticLog::new(temporary.0.join(format!("{name}.jsonl")));
            for event in [
                DiagnosticEvent::Lifecycle(HostLifecycleEvent::Connecting {
                    attempt: 1,
                    stream_id,
                }),
                DiagnosticEvent::Lifecycle(HostLifecycleEvent::Available { stream_id }),
                DiagnosticEvent::Lifecycle(HostLifecycleEvent::Connected { stream_id }),
                DiagnosticEvent::HostMedia(HostMediaDiagnostic::CursorSend {
                    stream_id,
                    send_attempts: counter,
                    send_errors: counter,
                    disabled: true,
                    reason: CursorDisableReason::CapabilityOrSize,
                }),
                DiagnosticEvent::HostMedia(HostMediaDiagnostic::DirectListenerStopped {
                    stream_id,
                    retired_active: true,
                    reason: DirectListenerStopReason::ListenerClosed,
                }),
                DiagnosticEvent::ControllerVideoMetrics {
                    attempt: 1,
                    stream_id: Some(stream_id),
                    video_path: "relay".to_owned(),
                    video_path_rtt_ms: Some(3),
                    video_path_loss_basis_points: Some(3),
                    video_path_fallback_reason: None,
                    received_video_packets: counter,
                    dropped_video_packets: counter,
                    completed_frames: counter,
                    delivered_video_frames: counter,
                    video_ipc_overflow_drops: counter,
                    video_ipc_keyframe_replacements: counter,
                    input_backpressure_count: counter,
                },
                DiagnosticEvent::ControllerRenderMetrics {
                    stream_id,
                    video_width: 64,
                    video_height: 64,
                    video_path: "relay".to_owned(),
                    received_frames: counter,
                    submitted_frames: counter,
                    displayed_frames: counter,
                    malformed_frames: counter,
                    decoder_recoveries: 3,
                    video_pull_failures: 3,
                    first_frame_ms: Some(counter),
                    displayed_fps_x100: Some(3_000),
                    max_frame_gap_ms: Some(counter),
                    coalesced_frame_drops: counter,
                    h264_profile: H264Profile::Main,
                    profile_probe: H264ProfileProbe::Supported,
                    profile_probe_ms: Some(counter),
                    profile_fallback_reason: None,
                },
            ] {
                // Actual log encoding, append, rotation and sync_data execute.
                log.record(&event).unwrap();
            }
            let log_lines = log.recent_sanitized_lines().unwrap();
            assert_eq!(log_lines.len(), 7);
            let events = log_lines
                .iter()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            for event in &events {
                assert_eq!(fixture_u64(&event["stream_id"]), stream_id, "{name}");
                let event_name = event["event"].as_str().unwrap();
                let fields = diagnostic_u64_fields(event_name);
                assert!(
                    !fields.is_empty(),
                    "missing production field table for {event_name}"
                );
                u64_fields_by_event.insert(
                    event_name.to_owned(),
                    fields
                        .iter()
                        .map(|field| (*field).to_owned())
                        .collect::<Vec<_>>(),
                );
                for field in fields {
                    let expected = if *field == "stream_id" {
                        stream_id
                    } else {
                        counter
                    };
                    assert_eq!(fixture_u64(&event[*field]), expected, "{name}.{field}");
                    assert_eq!(
                        event[*field].as_str(),
                        Some(expected.to_string().as_str()),
                        "{name}.{field}"
                    );
                }
            }

            // The false flag short-circuits correlation lookup. All event
            // selection, JSON body serialization, limits, hash and signature
            // below are the same production implementation used by the uploader.
            let request = fixture_request(
                build_request_from_log(DiagnosticSource::Host, &identity, &log, false)
                    .unwrap()
                    .unwrap(),
            );
            let batch = verify_fixture_request(&identity, &request);
            assert_eq!(batch["events"], Value::Array(events));
            assert_eq!(request.event_count, 7);
            cases.push(ConfiguredStreamCase {
                name: name.to_owned(),
                configured_stream_id: stream_text.to_owned(),
                log_lines,
                request,
            });
        }
        assert_eq!(u64_fields_by_event.len(), 7);
        assert_eq!(
            u64_fields_by_event
                .values()
                .flatten()
                .collect::<BTreeSet<_>>()
                .len(),
            18
        );

        // Historical input is the immutable artifact produced by the previous
        // real Rust uploader. Restore those original lines byte-for-byte,
        // rather than invoking the new encoder or recreating events by hand.
        let legacy: LegacyConfiguredStreamFixture = serde_json::from_str(include_str!(
            "../tests/fixtures/diagnostic-u64-legacy-producer.json"
        ))
        .unwrap();
        assert_eq!(legacy.schema, 1);
        assert_eq!(
            legacy
                .cases
                .iter()
                .map(|case| case.name.as_str())
                .collect::<Vec<_>>(),
            cases
                .iter()
                .map(|case| case.name.as_str())
                .collect::<Vec<_>>()
        );
        let mut historical_cases = Vec::new();
        for old in legacy.cases {
            let path = temporary.0.join(format!("historical-{}.jsonl", old.name));
            let original_bytes = write_historical_lines(&path, &old.log_lines);
            let log = DiagnosticLog::new(&path);
            assert_eq!(fs::read(&path).unwrap(), original_bytes);
            assert_eq!(log.recent_sanitized_lines().unwrap(), old.log_lines);
            let old_batch = verify_fixture_request(&identity, &old.request);
            let old_events = old
                .log_lines
                .iter()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(old_batch["events"], Value::Array(old_events.clone()));

            let request = fixture_request(
                build_request_from_log(DiagnosticSource::Host, &identity, &log, false)
                    .unwrap()
                    .unwrap(),
            );
            let batch = verify_fixture_request(&identity, &request);
            let new_events = batch["events"].as_array().unwrap();
            assert_eq!(new_events.len(), old_events.len());
            assert_eq!(request.event_count, old.request.event_count);
            for (before, after) in old_events.iter().zip(new_events) {
                let before = before.as_object().unwrap();
                let after = after.as_object().unwrap();
                assert_eq!(
                    before.keys().collect::<Vec<_>>(),
                    after.keys().collect::<Vec<_>>()
                );
                let event_name = before["event"].as_str().unwrap();
                let fields = diagnostic_u64_fields(event_name);
                for (field, value) in before {
                    if fields.contains(&field.as_str()) {
                        assert_eq!(
                            after[field].as_str(),
                            Some(fixture_u64(value).to_string().as_str())
                        );
                    } else {
                        assert_eq!(&after[field], value, "{}.{field}", old.name);
                    }
                }
            }
            // Normalization never edits the old on-disk bytes. It produces a
            // newly serialized and newly signed batch after reading them.
            assert_eq!(fs::read(&path).unwrap(), original_bytes);
            assert_eq!(log.recent_sanitized_lines().unwrap(), old.log_lines);
            assert_ne!(request.body_utf8, old.request.body_utf8);
            assert_ne!(request.signature, old.request.signature);
            assert_ne!(request.batch_id, old.request.batch_id);
            historical_cases.push(HistoricalStreamCase {
                name: old.name,
                configured_stream_id: old.configured_stream_id,
                log_lines: old.log_lines,
                legacy_request: old.request,
                request,
            });
        }

        // Exercise the identity/counter one boundary through the same actual
        // configuration/log/uploader route without changing the six pinned
        // current/historical fixture case names. A zero stream is never legal.
        let one = HostConnectionSettings::from_text(
            "127.0.0.1:4433",
            "localhost",
            "00112233445566778899aabbccddeeff",
            "",
            Some([0x31; 32]),
            "1",
        )
        .unwrap();
        assert!(matches!(
            HostConnectionSettings::from_text(
                "127.0.0.1:4433",
                "localhost",
                "00112233445566778899aabbccddeeff",
                "",
                Some([0x31; 32]),
                "0",
            ),
            Err(ConnectionSettingsInputError::InvalidStreamId)
        ));
        let one_log = DiagnosticLog::new(temporary.0.join("one.jsonl"));
        one_log
            .record(&DiagnosticEvent::HostMedia(
                HostMediaDiagnostic::CursorSend {
                    stream_id: one.stream_id(),
                    send_attempts: 1,
                    send_errors: 1,
                    disabled: true,
                    reason: CursorDisableReason::CapabilityOrSize,
                },
            ))
            .unwrap();
        let one_request = fixture_request(
            build_request_from_log(DiagnosticSource::Host, &identity, &one_log, false)
                .unwrap()
                .unwrap(),
        );
        let one_batch = verify_fixture_request(&identity, &one_request);
        for field in diagnostic_u64_fields("host_cursor_send") {
            assert_eq!(one_batch["events"][0][*field].as_str(), Some("1"));
        }

        // Unknown events and keys remain intact for the fixed service schema
        // to reject. Known valid u64 fields still normalize in that batch.
        let unknown_lines = [
            r#"{"event":"unknown_future","stream_id":18446744073709551615,"unknown":"retain"}"#.to_owned(),
            r#"{"event":"host_cursor_send","stream_id":7,"send_attempts":3,"send_errors":1,"unknown":1.5}"#.to_owned(),
        ];
        let unknown_path = temporary.0.join("unknown-values.jsonl");
        let unknown_bytes = write_historical_lines(&unknown_path, &unknown_lines);
        let unknown_log = DiagnosticLog::new(&unknown_path);
        let unknown_request = fixture_request(
            build_request_from_log(DiagnosticSource::Host, &identity, &unknown_log, false)
                .unwrap()
                .unwrap(),
        );
        let unknown_batch = verify_fixture_request(&identity, &unknown_request);
        assert_eq!(
            unknown_batch["events"][0],
            serde_json::from_str::<Value>(&unknown_lines[0]).unwrap()
        );
        assert_eq!(unknown_batch["events"].as_array().unwrap().len(), 2);
        assert_eq!(unknown_batch["events"][1]["stream_id"], "7");
        assert_eq!(unknown_batch["events"][1]["send_attempts"], "3");
        assert_eq!(unknown_batch["events"][1]["send_errors"], "1");
        assert_eq!(unknown_batch["events"][1]["unknown"], 1.5);
        assert_eq!(fs::read(unknown_path).unwrap(), unknown_bytes);

        // Known malformed fields fail the whole selected batch. In particular,
        // float 1.0 must not reach JavaScript as an apparently safe integer.
        let malformed = [
            r#"{"event":"host_cursor_send","stream_id":"01","send_attempts":3,"send_errors":1}"#,
            r#"{"event":"host_cursor_send","stream_id":7,"send_attempts":-1,"send_errors":1}"#,
            r#"{"event":"host_cursor_send","stream_id":7,"send_attempts":1.0,"send_errors":1}"#,
            r#"{"event":"host_cursor_send","stream_id":7,"send_attempts":3,"send_errors":1.5}"#,
            r#"{"event":"host_cursor_send","stream_id":"18446744073709551616","send_attempts":3,"send_errors":1}"#,
            r#"{"event":"host_cursor_send","stream_id":0,"send_attempts":3,"send_errors":1}"#,
            r#"{"event":"host_cursor_send","stream_id":"0","send_attempts":3,"send_errors":1}"#,
            r#"{"event":"host_cursor_send","stream_id":7,"send_attempts":true,"send_errors":null}"#,
        ];
        let valid_line = one_log.recent_sanitized_lines().unwrap().remove(0);
        for (index, invalid_line) in malformed.iter().enumerate() {
            let path = temporary.0.join(format!("invalid-known-{index}.jsonl"));
            let original_bytes =
                write_historical_lines(&path, &[valid_line.clone(), (*invalid_line).to_owned()]);
            assert!(matches!(
                build_request_from_log(
                    DiagnosticSource::Host,
                    &identity,
                    &DiagnosticLog::new(&path),
                    false,
                ),
                Err(CloudDiagnosticError::Encoding)
            ));
            assert_eq!(fs::read(path).unwrap(), original_bytes);
        }

        // Apply validation after the original newest-100 selection. An older
        // malformed event outside the budget cannot stop current valid events,
        // while the same malformed event within that budget fails atomically.
        let mut budget_lines = vec![malformed[2].to_owned()];
        budget_lines.extend(std::iter::repeat_n(valid_line, MAX_BATCH_EVENTS));
        let budget_path = temporary.0.join("invalid-outside-budget.jsonl");
        let budget_bytes = write_historical_lines(&budget_path, &budget_lines);
        let budget_request = fixture_request(
            build_request_from_log(
                DiagnosticSource::Host,
                &identity,
                &DiagnosticLog::new(&budget_path),
                false,
            )
            .unwrap()
            .unwrap(),
        );
        let budget_batch = verify_fixture_request(&identity, &budget_request);
        assert_eq!(budget_request.event_count, MAX_BATCH_EVENTS);
        assert_eq!(
            budget_batch["events"].as_array().unwrap().len(),
            MAX_BATCH_EVENTS
        );
        assert_eq!(fs::read(budget_path).unwrap(), budget_bytes);
        budget_lines.push(malformed[2].to_owned());
        let selected_path = temporary.0.join("invalid-inside-budget.jsonl");
        let selected_bytes = write_historical_lines(&selected_path, &budget_lines);
        assert!(matches!(
            build_request_from_log(
                DiagnosticSource::Host,
                &identity,
                &DiagnosticLog::new(&selected_path),
                false,
            ),
            Err(CloudDiagnosticError::Encoding)
        ));
        assert_eq!(fs::read(selected_path).unwrap(), selected_bytes);

        let fixture = ConfiguredStreamFixture {
            schema: 2,
            u64_fields_by_event,
            cases,
            historical_cases,
        };
        // Normal test runs never export anything. An explicit unique target
        // preserves the legacy artifact if the serializer changes later.
        if let Some(output) = std::env::var_os("DESKLINK_DIAGNOSTIC_FIXTURE_OUT") {
            let output = PathBuf::from(output);
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            let mut contents = serde_json::to_vec_pretty(&fixture).unwrap();
            contents.push(b'\n');
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&output)
                .expect("fixture target must be explicitly chosen and not already exist");
            file.write_all(&contents).unwrap();
            file.sync_all().unwrap();
            println!(
                "offline actual signed diagnostic fixture: {}",
                output.display()
            );
        }
    }
}
