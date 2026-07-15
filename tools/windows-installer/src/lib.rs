use std::path::{Path, PathBuf};

pub const PRODUCT_NAME: &str = "DeskLink";
pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APPLICATION_FILE_NAME: &str = "DeskLink.exe";
pub const HOST_FILE_NAME: &str = "desklink-windows.exe";
pub const UNINSTALLER_FILE_NAME: &str = "DeskLinkUninstall.exe";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallLayout {
    pub install_directory: PathBuf,
    pub application: PathBuf,
    pub host: PathBuf,
    pub uninstaller: PathBuf,
    pub start_menu_shortcut: PathBuf,
    pub data_directory: PathBuf,
}

impl InstallLayout {
    pub fn from_user_roots(local_app_data: &Path, roaming_app_data: &Path) -> Self {
        let install_directory = local_app_data.join("Programs").join(PRODUCT_NAME);
        Self {
            application: install_directory.join(APPLICATION_FILE_NAME),
            host: install_directory.join(HOST_FILE_NAME),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_user_layout_separates_program_files_from_preserved_data() {
        let layout = InstallLayout::from_user_roots(
            Path::new(r"C:\Users\Owner\AppData\Local"),
            Path::new(r"C:\Users\Owner\AppData\Roaming"),
        );
        assert_eq!(
            layout.application,
            PathBuf::from(r"C:\Users\Owner\AppData\Local\Programs\DeskLink\DeskLink.exe")
        );
        assert_eq!(
            layout.host,
            PathBuf::from(r"C:\Users\Owner\AppData\Local\Programs\DeskLink\desklink-windows.exe")
        );
        assert_eq!(
            layout.data_directory,
            PathBuf::from(r"C:\Users\Owner\AppData\Local\DeskLink")
        );
        assert!(!layout.data_directory.starts_with(&layout.install_directory));
        assert!(
            layout
                .start_menu_shortcut
                .ends_with(r"Start Menu\Programs\DeskLink.lnk")
        );
    }

    #[test]
    fn startup_and_uninstall_commands_quote_paths_and_contain_no_credentials() {
        let layout = InstallLayout::from_user_roots(
            Path::new(r"C:\Users\Desk Link\AppData\Local"),
            Path::new(r"C:\Users\Desk Link\AppData\Roaming"),
        );
        assert_eq!(
            layout.startup_command(),
            r#""C:\Users\Desk Link\AppData\Local\Programs\DeskLink\DeskLink.exe" --startup"#
        );
        assert_eq!(
            layout.uninstall_command(),
            r#""C:\Users\Desk Link\AppData\Local\Programs\DeskLink\DeskLinkUninstall.exe" --uninstall"#
        );
        assert!(!layout.startup_command().contains("AUTH_KEY"));
        assert!(!layout.startup_command().contains("PAIRING"));
    }
}
