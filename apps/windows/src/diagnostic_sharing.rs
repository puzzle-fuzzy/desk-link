use std::{
    fs::{self, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::storage::local_app_data_path;

const ENABLED_MARKER: &[u8] = b"desklink-diagnostics-sharing-v1\n";

#[derive(Clone, Debug)]
pub struct WindowsDiagnosticSharing {
    path: PathBuf,
}

#[derive(Debug, Error)]
pub enum WindowsDiagnosticSharingError {
    #[error("diagnostic sharing storage path is unavailable")]
    MissingStoragePath,
    #[error("diagnostic sharing preference could not be read or written: {0}")]
    Io(#[from] io::Error),
    #[error("diagnostic sharing preference is malformed")]
    InvalidPreference,
}

impl WindowsDiagnosticSharing {
    pub fn for_current_user() -> Result<Self, WindowsDiagnosticSharingError> {
        let local_app_data =
            local_app_data_path().ok_or(WindowsDiagnosticSharingError::MissingStoragePath)?;
        Ok(Self::new(
            local_app_data
                .join("DeskLink")
                .join("diagnostics-sharing.enabled"),
        ))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn is_enabled(&self) -> Result<bool, WindowsDiagnosticSharingError> {
        match fs::read(&self.path) {
            Ok(value) if value == ENABLED_MARKER => Ok(true),
            Ok(_) => Err(WindowsDiagnosticSharingError::InvalidPreference),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<(), WindowsDiagnosticSharingError> {
        if !enabled {
            return match fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            };
        }
        let parent = self
            .path
            .parent()
            .ok_or(WindowsDiagnosticSharingError::MissingStoragePath)?;
        fs::create_dir_all(parent)?;
        let temporary = self.path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(ENABLED_MARKER)?;
        file.sync_all()?;
        drop(file);
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        fs::rename(temporary, &self.path)?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn sharing_is_explicit_and_fails_closed_for_malformed_state() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "desklink-diagnostic-sharing-{}-{unique}",
            std::process::id()
        ));
        let sharing = WindowsDiagnosticSharing::new(directory.join("enabled"));
        assert!(!sharing.is_enabled().unwrap());
        sharing.set_enabled(true).unwrap();
        assert!(sharing.is_enabled().unwrap());
        std::fs::write(sharing.path(), b"unexpected").unwrap();
        assert!(matches!(
            sharing.is_enabled(),
            Err(WindowsDiagnosticSharingError::InvalidPreference)
        ));
        sharing.set_enabled(false).unwrap();
        assert!(!sharing.is_enabled().unwrap());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
