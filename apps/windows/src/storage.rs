use std::path::PathBuf;

#[cfg(windows)]
pub(crate) fn local_app_data_path() -> Option<PathBuf> {
    use windows::Win32::UI::Shell::FOLDERID_LocalAppData;

    known_folder_path(&FOLDERID_LocalAppData)
}

#[cfg(windows)]
pub(crate) fn downloads_path() -> Option<PathBuf> {
    use windows::Win32::UI::Shell::FOLDERID_Downloads;

    known_folder_path(&FOLDERID_Downloads)
}

#[cfg(windows)]
fn known_folder_path(folder_id: &windows::core::GUID) -> Option<PathBuf> {
    use windows::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{KF_FLAG_DEFAULT, SHGetKnownFolderPath},
    };

    let path = unsafe { SHGetKnownFolderPath(folder_id, KF_FLAG_DEFAULT, None).ok()? };
    let result = unsafe { path.to_string().ok().map(PathBuf::from) };
    unsafe { CoTaskMemFree(Some(path.as_ptr().cast())) };
    result
}

#[cfg(not(windows))]
pub(crate) fn local_app_data_path() -> Option<PathBuf> {
    non_windows_local_app_data_path(
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
        cfg!(target_os = "macos"),
    )
}

#[cfg(not(windows))]
fn non_windows_local_app_data_path(
    local_app_data: Option<PathBuf>,
    xdg_data_home: Option<PathBuf>,
    home: Option<PathBuf>,
    is_macos: bool,
) -> Option<PathBuf> {
    local_app_data.or(xdg_data_home).or_else(|| {
        home.map(|path| {
            if is_macos {
                path.join("Library").join("Application Support")
            } else {
                path.join(".local").join("share")
            }
        })
    })
}

#[cfg(not(windows))]
pub(crate) fn downloads_path() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .map(|path| path.join("Downloads"))
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::{downloads_path, local_app_data_path};

    #[cfg(not(windows))]
    use super::non_windows_local_app_data_path;

    #[cfg(not(windows))]
    use std::path::PathBuf;

    #[cfg(windows)]
    #[test]
    fn current_user_local_app_data_is_available() {
        let path = local_app_data_path().expect("current user local app data should be available");
        assert!(path.is_absolute());
    }

    #[cfg(windows)]
    #[test]
    fn current_user_downloads_folder_is_available() {
        let path = downloads_path().expect("current user downloads folder should be available");
        assert!(path.is_absolute());
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_tooling_uses_native_data_roots_without_windows_environment() {
        let home = PathBuf::from("/Users/desklink");
        assert_eq!(
            non_windows_local_app_data_path(None, None, Some(home.clone()), true),
            Some(home.join("Library").join("Application Support"))
        );
        assert_eq!(
            non_windows_local_app_data_path(None, None, Some(home.clone()), false),
            Some(home.join(".local").join("share"))
        );
        assert_eq!(
            non_windows_local_app_data_path(
                None,
                Some(PathBuf::from("/data/xdg")),
                Some(home),
                false,
            ),
            Some(PathBuf::from("/data/xdg"))
        );
    }
}
