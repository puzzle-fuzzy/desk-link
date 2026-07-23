use std::{
    cmp::Ordering,
    path::{Path, PathBuf},
};

pub const PRODUCT_NAME: &str = "DeskLink";
pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APPLICATION_FILE_NAME: &str = "DeskLink.exe";
pub const UNINSTALLER_FILE_NAME: &str = "DeskLinkUninstall.exe";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallLayout {
    pub install_directory: PathBuf,
    pub application: PathBuf,
    pub uninstaller: PathBuf,
    pub start_menu_shortcut: PathBuf,
    pub data_directory: PathBuf,
}

impl InstallLayout {
    pub fn from_user_roots(local_app_data: &Path, roaming_app_data: &Path) -> Self {
        let install_directory = local_app_data.join("Programs").join(PRODUCT_NAME);
        Self {
            application: install_directory.join(APPLICATION_FILE_NAME),
            uninstaller: install_directory.join(UNINSTALLER_FILE_NAME),
            start_menu_shortcut: roaming_app_data
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join("DeskLink.lnk"),
            data_directory: local_app_data.join(PRODUCT_NAME),
            install_directory,
        }
    }

    pub fn startup_command(&self) -> String {
        format!("{} --startup", quote_executable(&self.application))
    }

    pub fn uninstall_command(&self) -> String {
        format!("{} --uninstall", quote_executable(&self.uninstaller))
    }
}

pub fn quote_executable(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

/// Compares two public DeskLink release versions.
///
/// Installer versions deliberately use the strict `major.minor.patch` form. Returning
/// `None` for an unknown value preserves repair compatibility with very old builds
/// that may have written a non-standard registry value.
pub fn compare_release_versions(left: &str, right: &str) -> Option<Ordering> {
    Some(parse_release_version(left)?.cmp(&parse_release_version(right)?))
}

fn parse_release_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.trim().split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_user_layout_separates_program_files_from_preserved_data() {
        let local = Path::new("user-local");
        let roaming = Path::new("user-roaming");
        let layout = InstallLayout::from_user_roots(local, roaming);
        assert_eq!(
            layout.application,
            local.join("Programs").join("DeskLink").join("DeskLink.exe")
        );
        assert_eq!(layout.data_directory, local.join("DeskLink"));
        assert!(!layout.data_directory.starts_with(&layout.install_directory));
        assert_eq!(
            layout.start_menu_shortcut,
            roaming
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join("DeskLink.lnk")
        );
    }

    #[test]
    fn startup_and_uninstall_commands_quote_paths_and_contain_no_credentials() {
        let local = Path::new("Desk Link").join("Local Data");
        let roaming = Path::new("Desk Link").join("Roaming Data");
        let layout = InstallLayout::from_user_roots(&local, &roaming);
        assert_eq!(
            layout.startup_command(),
            format!("\"{}\" --startup", layout.application.display())
        );
        assert_eq!(
            layout.uninstall_command(),
            format!("\"{}\" --uninstall", layout.uninstaller.display())
        );
        assert!(!layout.startup_command().contains("AUTH_KEY"));
        assert!(!layout.startup_command().contains("PAIRING"));
    }

    #[test]
    fn release_versions_are_compared_numerically() {
        assert_eq!(
            compare_release_versions("0.1.25", "0.1.24"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_release_versions("0.10.0", "0.9.99"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_release_versions("1.0.0", "1.0.0"),
            Some(Ordering::Equal)
        );
    }

    #[test]
    fn non_release_versions_are_not_guessed() {
        assert_eq!(compare_release_versions("0.1", "0.1.25"), None);
        assert_eq!(compare_release_versions("v0.1.25", "0.1.25"), None);
        assert_eq!(compare_release_versions("0.1.25-beta", "0.1.25"), None);
    }
}
