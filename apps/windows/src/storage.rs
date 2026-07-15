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
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
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
    use super::{downloads_path, local_app_data_path};

    #[test]
    fn current_user_local_app_data_is_available() {
        let path = local_app_data_path().expect("current user local app data should be available");
        assert!(path.is_absolute());
    }

    #[test]
    fn current_user_downloads_folder_is_available() {
        let path = downloads_path().expect("current user downloads folder should be available");
        assert!(path.is_absolute());
    }
}
