use std::{
    cmp::Ordering,
    path::{Path, PathBuf},
};

pub mod update;
#[cfg(windows)]
pub mod update_registration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Install,
    Update,
    Uninstall,
    UninstallWorker,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Options {
    pub action: Action,
    pub quiet: bool,
    pub no_autostart: bool,
    pub remove_data: bool,
}

impl Options {
    pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self {
            action: Action::Install,
            quiet: false,
            no_autostart: false,
            remove_data: false,
        };
        for argument in arguments {
            let action = match argument.as_str() {
                "--quiet" => {
                    options.quiet = true;
                    continue;
                }
                "--no-autostart" => {
                    options.no_autostart = true;
                    continue;
                }
                "--remove-data" => {
                    options.remove_data = true;
                    continue;
                }
                "--update" => Action::Update,
                "--uninstall" => Action::Uninstall,
                "--uninstall-worker" => Action::UninstallWorker,
                _ => return Err(format!("未知的安装器选项：{argument}")),
            };
            if options.action != Action::Install && options.action != action {
                return Err("安装器操作选项不能同时使用".to_owned());
            }
            options.action = action;
        }
        if matches!(options.action, Action::Install | Action::Update) && options.remove_data {
            return Err("--remove-data 只能与 --uninstall 一起使用".to_owned());
        }
        if options.action == Action::Update && options.no_autostart {
            return Err("升级会保留原有的开机启动设置，不能指定 --no-autostart".to_owned());
        }
        Ok(options)
    }

    pub fn show_errors(self) -> bool {
        self.action == Action::Update || !self.quiet
    }

    pub fn show_success(self) -> bool {
        !self.quiet && !matches!(self.action, Action::Update | Action::UninstallWorker)
    }
}

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

/// Returns whether a WebView2 Evergreen runtime version can be used by the
/// Windows control surface. The registry reports `0.0.0.0` when the runtime
/// is not installed, so every component must parse and at least one must be
/// non-zero.
pub fn webview2_version_is_suitable(value: &str) -> bool {
    let mut parts = value.trim().split('.');
    let Some(first) = parts.next().and_then(|part| part.parse::<u64>().ok()) else {
        return false;
    };
    let Some(second) = parts.next().and_then(|part| part.parse::<u64>().ok()) else {
        return false;
    };
    let Some(third) = parts.next().and_then(|part| part.parse::<u64>().ok()) else {
        return false;
    };
    let Some(fourth) = parts.next().and_then(|part| part.parse::<u64>().ok()) else {
        return false;
    };
    parts.next().is_none() && (first, second, third, fourth) != (0, 0, 0, 0)
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

    #[test]
    fn webview2_runtime_versions_fail_closed() {
        assert!(webview2_version_is_suitable("120.0.2210.55"));
        assert!(!webview2_version_is_suitable("0.0.0.0"));
        assert!(!webview2_version_is_suitable(""));
        assert!(!webview2_version_is_suitable("120.0.2210"));
        assert!(!webview2_version_is_suitable("120.0.2210.55.extra"));
    }

    #[test]
    fn update_keeps_success_quiet_but_never_hides_errors() {
        let options = Options::parse(["--update", "--quiet"].map(str::to_owned)).unwrap();
        assert_eq!(options.action, Action::Update);
        assert!(!options.show_success());
        assert!(options.show_errors());
        assert!(!options.no_autostart);
        assert!(!options.remove_data);
        let normal = Options::parse(["--quiet", "--no-autostart"].map(str::to_owned)).unwrap();
        assert_eq!(normal.action, Action::Install);
        assert!(!normal.show_errors());
        assert!(normal.no_autostart);
    }

    #[test]
    fn update_rejects_data_removal_startup_changes_and_conflicting_actions() {
        for arguments in [
            vec!["--update", "--remove-data"],
            vec!["--update", "--no-autostart"],
            vec!["--update", "--uninstall"],
            vec!["--uninstall", "--update"],
            vec!["--update", "--uninstall-worker"],
        ] {
            assert!(Options::parse(arguments.into_iter().map(str::to_owned)).is_err());
        }
        assert!(Options::parse(["--uninstall", "--remove-data"].map(str::to_owned)).is_ok());
    }
}
