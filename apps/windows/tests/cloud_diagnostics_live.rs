#![cfg(windows)]

use apps_windows::{
    cloud_diagnostics::upload_all_once_without_session_correlation,
    diagnostic_sharing::WindowsDiagnosticSharing,
    diagnostics::{DiagnosticEvent, DiagnosticLog, DiagnosticOperation},
};

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
    // This probe validates ingestion only. Keep it uncorrelated so a synthetic
    // event cannot be merged into a real user session in the managed report.
    DiagnosticLog::controller_for_current_user()
        .unwrap()
        .record(&DiagnosticEvent::OperationSucceeded(
            DiagnosticOperation::RelayProbe,
        ))
        .unwrap();

    let result = upload_all_once_without_session_correlation().unwrap();
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
