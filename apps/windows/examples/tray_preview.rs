#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::{env, fs, time::Duration};

    use apps_windows::{
        runtime::HostLifecycleEvent, tray::WindowsTrayApplication,
        trusted::WindowsTrustedControllerStore,
    };

    let store_path = env::temp_dir()
        .join("DeskLink")
        .join("tray-preview-trusted-controllers.bin");
    if let Some(parent) = store_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&store_path, b"invalid preview trust store")?;
    let store = WindowsTrustedControllerStore::new(&store_path);
    let tray = WindowsTrayApplication::start(store)?;
    let handle = tray.handle();
    handle.publish(HostLifecycleEvent::Reconnecting {
        retry: 2,
        maximum_retries: 6,
        delay: Duration::from_millis(1_000),
        reason: "The relay is temporarily unavailable. DeskLink will keep trying securely."
            .to_owned(),
    });
    handle.show();

    let mut exit = tray.exit_receiver();
    while !*exit.borrow() && exit.changed().await.is_ok() {}
    tray.shutdown();
    let _ = fs::remove_file(store_path);
    Ok(())
}

#[cfg(not(windows))]
fn main() {}
