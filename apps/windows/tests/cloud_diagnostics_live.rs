#![cfg(windows)]

use apps_windows::{
    cloud_diagnostics::{DiagnosticSource, set_session_correlation, upload_all_once},
    diagnostic_sharing::WindowsDiagnosticSharing,
    diagnostics::{DiagnosticEvent, DiagnosticLog, DiagnosticOperation},
};
use desklink_crypto::SessionId;

#[test]
#[ignore = "live HTTPS diagnostic ingestion probe; run explicitly before publishing a Windows installer"]
fn windows_signed_diagnostic_batch_reaches_managed_service() {
    let sharing = WindowsDiagnosticSharing::for_current_user().unwrap();
    let previous = sharing.is_enabled().unwrap_or(false);
    let _restore = SharingRestore {
        sharing: sharing.clone(),
        previous,
    };
    sharing.set_enabled(true).unwrap();
    set_session_correlation(
        DiagnosticSource::Controller,
        SessionId::from_bytes([0x5a; 16]),
    )
    .unwrap();
    DiagnosticLog::controller_for_current_user()
        .unwrap()
        .record(&DiagnosticEvent::OperationFailed {
            operation: DiagnosticOperation::RelayProbe,
            reason: "managed cloud diagnostic live probe".to_owned(),
        })
        .unwrap();

    let result = upload_all_once().unwrap();
    assert!(result.uploaded_sources >= 1);
    assert!(result.uploaded_events >= 1);
}

struct SharingRestore {
    sharing: WindowsDiagnosticSharing,
    previous: bool,
}

impl Drop for SharingRestore {
    fn drop(&mut self) {
        let _ = self.sharing.set_enabled(self.previous);
    }
}
