use std::path::PathBuf;

#[cfg(windows)]
pub(crate) fn local_app_data_path() -> Option<PathBuf> {
    use windows::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
    };

    let path = unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None).ok()? };
    let result = unsafe { path.to_string().ok().map(PathBuf::from) };
    unsafe { CoTaskMemFree(Some(path.as_ptr().cast())) };
    result
}

#[cfg(not(windows))]
pub(crate) fn local_app_data_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::local_app_data_path;

    #[test]
    fn current_user_local_app_data_is_available() {
        let path = local_app_data_path().expect("current user local app data should be available");
        assert!(path.is_absolute());
    }
}
