use ed25519_dalek::{Signature, VerifyingKey};
use rand_core::{OsRng, RngCore};
use reqwest::{
    Client, StatusCode, Url,
    header::ACCEPT,
    redirect::{Attempt, Policy},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::ipc::Channel;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::watch,
};

const RELEASE_API_URL: &str = "https://api.github.com/repos/puzzle-fuzzy/desk-link/releases/latest";
const RELEASE_DOWNLOAD_PREFIX: &str = "/puzzle-fuzzy/desk-link/releases/download/";
const MANIFEST_NAME: &str = "windows-installer-manifest.json";
const SIGNATURE_NAME: &str = "windows-installer-manifest.json.sig";
const SIGNATURE_DOMAIN: &[u8] = b"desklink-windows-update-manifest-v1\0";
const PUBLIC_KEY: &str = include_str!("../update-public-key.hex");
const INSTALLER_MINIMUM_BYTES: u64 = 1_000_000;
const INSTALLER_MAXIMUM_BYTES: u64 = 512 * 1_024 * 1_024;
const RELEASE_RESPONSE_MAXIMUM_BYTES: usize = 1_000_000;
const MANIFEST_MAXIMUM_BYTES: usize = 100_000;
const CANCELLED: &str = "更新已取消，当前版本仍可继续使用。";

#[derive(Debug, Serialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WindowsReleaseSource {
    Release {
        latest_version: String,
        published_at: Option<String>,
    },
    Unavailable {
        reason: &'static str,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsUpdateProgress {
    stage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    downloaded_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_bytes: Option<u64>,
}
impl WindowsUpdateProgress {
    fn new(stage: &'static str, version: Option<&str>) -> Self {
        Self {
            stage,
            version: version.map(str::to_owned),
            downloaded_bytes: None,
            total_bytes: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    draft: bool,
    prerelease: bool,
    tag_name: String,
    published_at: Option<String>,
    assets: Vec<GithubAsset>,
}
#[derive(Clone, Debug, Deserialize)]
struct GithubAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}
#[derive(Debug)]
struct ReleaseCandidate {
    version: String,
    published_at: Option<String>,
    installer: GithubAsset,
    installer_url: Url,
    manifest_url: Url,
    signature_url: Url,
}
#[derive(Debug, Deserialize)]
struct InstallerManifest {
    schema: u8,
    target: String,
    version: String,
    passed: bool,
    update_mode: String,
    installer: ManifestInstaller,
}
#[derive(Debug, Deserialize)]
struct ManifestInstaller {
    file_name: String,
    size_bytes: u64,
    sha256: String,
}

// Test endpoints cannot be constructed by production IPC or environment values.
#[derive(Clone)]
enum ReleaseSource {
    Github,
    #[cfg(test)]
    Local(Url),
}
impl ReleaseSource {
    fn api_url(&self) -> Url {
        match self {
            Self::Github => Url::parse(RELEASE_API_URL).expect("fixed release URL"),
            #[cfg(test)]
            Self::Local(base) => base.join("latest").unwrap(),
        }
    }
    fn asset_url(&self, value: &str, tag: &str, name: &str) -> Option<Url> {
        let url = Url::parse(value).ok()?;
        let valid_origin = match self {
            Self::Github => {
                url.scheme() == "https"
                    && url.host_str() == Some("github.com")
                    && url.port_or_known_default() == Some(443)
            }
            #[cfg(test)]
            Self::Local(base) => url.origin() == base.origin(),
        };
        (valid_origin
            && url.path() == format!("{RELEASE_DOWNLOAD_PREFIX}{tag}/{name}")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none())
        .then_some(url)
    }
    fn allow_redirect(&self, attempt: Attempt<'_>) -> reqwest::redirect::Action {
        if attempt.previous().len() >= 5 {
            return attempt.stop();
        }
        let url = attempt.url();
        let valid = match self {
            Self::Github => {
                url.scheme() == "https"
                    && url.port_or_known_default() == Some(443)
                    && matches!(
                        url.host_str(),
                        Some(
                            "api.github.com"
                                | "github.com"
                                | "release-assets.githubusercontent.com"
                        )
                    )
            }
            #[cfg(test)]
            Self::Local(base) => url.origin() == base.origin(),
        };
        if valid && url.username().is_empty() && url.password().is_none() {
            attempt.follow()
        } else {
            attempt.stop()
        }
    }
    fn client(&self) -> Result<Client, String> {
        let source = self.clone();
        let builder = Client::builder()
            .connect_timeout(Duration::from_secs(12))
            .timeout(Duration::from_secs(300))
            .redirect(Policy::custom(move |attempt| {
                source.allow_redirect(attempt)
            }))
            .user_agent(format!("DeskLink/{}", env!("CARGO_PKG_VERSION")));
        #[cfg(test)]
        let builder = if matches!(self, Self::Local(_)) {
            builder.no_proxy()
        } else {
            builder
        };
        builder.build().map_err(|_| "无法创建更新连接。".to_owned())
    }
}

pub async fn check() -> Result<WindowsReleaseSource, String> {
    cleanup_stale_downloads(&std::env::temp_dir().join("DeskLinkUpdates"));
    let (_sender, mut cancellation) = watch::channel(false);
    let source = ReleaseSource::Github;
    let client = source.client()?;
    match verified_release(&source, &client, PUBLIC_KEY, &mut cancellation).await? {
        Ok((candidate, _)) => Ok(WindowsReleaseSource::Release {
            latest_version: candidate.version,
            published_at: candidate.published_at,
        }),
        Err(reason) => Ok(WindowsReleaseSource::Unavailable { reason }),
    }
}
async fn cancelled(receiver: &mut watch::Receiver<bool>) {
    if *receiver.borrow() {
        return;
    }
    while receiver.changed().await.is_ok() {
        if *receiver.borrow() {
            return;
        }
    }
    std::future::pending::<()>().await;
}
async fn get(
    client: &Client,
    url: Url,
    cancellation: &mut watch::Receiver<bool>,
) -> Result<reqwest::Response, String> {
    tokio::select! { biased;
        _ = cancelled(cancellation) => Err(CANCELLED.to_owned()),
        response = client.get(url).header(ACCEPT, "application/json").timeout(Duration::from_secs(12)).send() => response.map_err(|_| "无法读取更新信息，请稍后重试。".to_owned()),
    }
}
async fn limited_response(
    mut response: reqwest::Response,
    limit: usize,
    cancellation: &mut watch::Receiver<bool>,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("更新信息超过大小限制。".to_owned());
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::select! { biased;
            _ = cancelled(cancellation) => return Err(CANCELLED.to_owned()),
            chunk = response.chunk() => chunk.map_err(|_| "更新信息读取失败。".to_owned())?,
        };
        let Some(chunk) = chunk else {
            return Ok(bytes);
        };
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err("更新信息超过大小限制。".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
}
async fn verified_release(
    source: &ReleaseSource,
    client: &Client,
    public_key: &str,
    cancellation: &mut watch::Receiver<bool>,
) -> Result<Result<(ReleaseCandidate, InstallerManifest), &'static str>, String> {
    let response = get(client, source.api_url(), cancellation).await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(Err("noRelease"));
    }
    if !response.status().is_success() {
        return Err("更新服务暂时不可用。".to_owned());
    }
    let bytes = limited_response(response, RELEASE_RESPONSE_MAXIMUM_BYTES, cancellation).await?;
    let release: GithubRelease =
        serde_json::from_slice(&bytes).map_err(|_| "更新信息格式无效。".to_owned())?;
    let candidate = match inspect_release(source, release) {
        Ok(candidate) => candidate,
        Err(reason) => return Ok(Err(reason)),
    };
    let manifest = get(client, candidate.manifest_url.clone(), cancellation).await?;
    if !manifest.status().is_success() {
        return Ok(Err("unverifiedWindowsRelease"));
    }
    let manifest = limited_response(manifest, MANIFEST_MAXIMUM_BYTES, cancellation).await?;
    let signature = get(client, candidate.signature_url.clone(), cancellation).await?;
    if !signature.status().is_success() {
        return Ok(Err("unverifiedWindowsRelease"));
    }
    let signature = limited_response(signature, 130, cancellation).await?;
    // Authenticate the exact bytes before parsing JSON; signed=true is not proof.
    if !verify_manifest_signature(public_key, &manifest, &signature) {
        return Ok(Err("unverifiedWindowsRelease"));
    }
    let manifest: InstallerManifest = match serde_json::from_slice(&manifest) {
        Ok(manifest) => manifest,
        Err(_) => return Ok(Err("unverifiedWindowsRelease")),
    };
    if !is_verified_manifest(&candidate, &manifest) {
        return Ok(Err("unverifiedWindowsRelease"));
    }
    Ok(Ok((candidate, manifest)))
}
fn inspect_release(
    source: &ReleaseSource,
    release: GithubRelease,
) -> Result<ReleaseCandidate, &'static str> {
    if release.draft || release.prerelease {
        return Err("invalidRelease");
    }
    let version = normalize_release_version(&release.tag_name).ok_or("invalidRelease")?;
    let installer_name = format!("DeskLinkSetup-{version}-x64.exe");
    let asset = |name: &str| {
        let mut matches = release.assets.iter().filter(|asset| asset.name == name);
        let value = matches.next()?;
        matches.next().is_none().then_some(value.clone())
    };
    let installer = asset(&installer_name)
        .filter(|asset| (INSTALLER_MINIMUM_BYTES..=INSTALLER_MAXIMUM_BYTES).contains(&asset.size))
        .ok_or("incompleteWindowsRelease")?;
    let manifest = asset(MANIFEST_NAME)
        .filter(|asset| asset.size > 0 && asset.size <= MANIFEST_MAXIMUM_BYTES as u64)
        .ok_or("incompleteWindowsRelease")?;
    let signature = asset(SIGNATURE_NAME)
        .filter(|asset| matches!(asset.size, 128..=130))
        .ok_or("incompleteWindowsRelease")?;
    let installer_url = source
        .asset_url(
            &installer.browser_download_url,
            &release.tag_name,
            &installer.name,
        )
        .ok_or("incompleteWindowsRelease")?;
    let manifest_url = source
        .asset_url(
            &manifest.browser_download_url,
            &release.tag_name,
            MANIFEST_NAME,
        )
        .ok_or("incompleteWindowsRelease")?;
    let signature_url = source
        .asset_url(
            &signature.browser_download_url,
            &release.tag_name,
            SIGNATURE_NAME,
        )
        .ok_or("incompleteWindowsRelease")?;
    let published_at = release
        .published_at
        .filter(|value| !value.is_empty() && value.len() <= 64);
    Ok(ReleaseCandidate {
        version,
        published_at,
        installer,
        installer_url,
        manifest_url,
        signature_url,
    })
}
fn decode_hex<const N: usize>(value: &[u8]) -> Option<[u8; N]> {
    let value = value
        .strip_suffix(b"\r\n")
        .or_else(|| value.strip_suffix(b"\n"))
        .unwrap_or(value);
    if value.len() != N * 2
        || !value
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return None;
    }
    let mut output = [0; N];
    for (output, pair) in output.iter_mut().zip(value.chunks_exact(2)) {
        *output = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(output)
}
fn verify_manifest_signature(public_key: &str, manifest: &[u8], signature: &[u8]) -> bool {
    let Some(public_key) = decode_hex::<32>(public_key.as_bytes()) else {
        return false;
    };
    let Ok(public_key) = VerifyingKey::from_bytes(&public_key) else {
        return false;
    };
    let Some(signature) = decode_hex::<64>(signature) else {
        return false;
    };
    let mut message = Vec::with_capacity(SIGNATURE_DOMAIN.len() + manifest.len());
    message.extend_from_slice(SIGNATURE_DOMAIN);
    message.extend_from_slice(manifest);
    public_key
        .verify_strict(&message, &Signature::from_bytes(&signature))
        .is_ok()
}
fn is_verified_manifest(candidate: &ReleaseCandidate, manifest: &InstallerManifest) -> bool {
    manifest.schema == 1
        && manifest.target == "x86_64-pc-windows-msvc"
        && manifest.passed
        && manifest.update_mode == "perUserV1"
        && manifest.version == candidate.version
        && manifest.installer.file_name == candidate.installer.name
        && manifest.installer.size_bytes == candidate.installer.size
        && decode_hex::<32>(manifest.installer.sha256.as_bytes()).is_some()
        && manifest.installer.sha256.len() == 64
}
fn normalize_release_version(value: &str) -> Option<String> {
    let version = value.strip_prefix('v').unwrap_or(value);
    let parts = version.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        return None;
    }
    let parsed = parts
        .iter()
        .map(|part| {
            if part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                return None;
            }
            part.parse::<u64>().ok()
        })
        .collect::<Option<Vec<_>>>()?;
    Some(format!("{}.{}.{}", parsed[0], parsed[1], parsed[2]))
}
fn newer_than(candidate: &str, current: &str) -> bool {
    let version = |value: &str| -> Option<Vec<u64>> {
        normalize_release_version(value)?
            .split('.')
            .map(|part| part.parse().ok())
            .collect()
    };
    matches!((version(candidate), version(current)), (Some(candidate), Some(current)) if candidate > current)
}

struct ActiveUpdate {
    cancellation: watch::Sender<bool>,
    committed: bool,
}
enum LaunchedInstaller {
    Process(Child),
    #[cfg(test)]
    HandedOff,
}
#[derive(Clone, Default)]
pub struct UpdateManager {
    active: Arc<Mutex<Option<ActiveUpdate>>>,
}
struct UpdateLease {
    active: Arc<Mutex<Option<ActiveUpdate>>>,
    committed: bool,
}
impl Drop for UpdateLease {
    fn drop(&mut self) {
        if !self.committed {
            *self
                .active
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = None;
        }
    }
}
impl UpdateManager {
    fn begin(&self) -> Result<(UpdateLease, watch::Receiver<bool>), String> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| "更新任务暂时不可用。".to_owned())?;
        if active.is_some() {
            return Err("更新任务已在进行，请勿重复操作。".to_owned());
        }
        let (sender, receiver) = watch::channel(false);
        *active = Some(ActiveUpdate {
            cancellation: sender,
            committed: false,
        });
        Ok((
            UpdateLease {
                active: self.active.clone(),
                committed: false,
            },
            receiver,
        ))
    }
    pub fn cancel(&self) -> bool {
        let Ok(active) = self.active.lock() else {
            return false;
        };
        let Some(active) = active.as_ref().filter(|active| !active.committed) else {
            return false;
        };
        active.cancellation.send(true).is_ok()
    }
    pub async fn install(&self, progress: Channel<WindowsUpdateProgress>) -> Result<(), String> {
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| "无法定位当前用户的安装目录。".to_owned())?;
        let installation = Installation {
            executable: std::env::current_exe().map_err(|_| "无法定位当前客户端。".to_owned())?,
            local_app_data: local,
            staging_root: std::env::temp_dir().join("DeskLinkUpdates"),
        };
        self.install_with(
            ReleaseSource::Github,
            PUBLIC_KEY,
            env!("CARGO_PKG_VERSION"),
            &installation,
            &|event| {
                let _ = progress.send(event);
            },
            &|path| {
                Command::new(path)
                    .arg("--update")
                    .spawn()
                    .map(LaunchedInstaller::Process)
                    .map_err(|_| "无法启动更新安装器，当前版本仍可继续使用。".to_owned())
            },
        )
        .await
    }
    async fn install_with(
        &self,
        source: ReleaseSource,
        public_key: &str,
        current_version: &str,
        installation: &Installation,
        progress: &(dyn Fn(WindowsUpdateProgress) + Sync),
        launch: &(dyn Fn(&Path) -> Result<LaunchedInstaller, String> + Sync),
    ) -> Result<(), String> {
        let (mut lease, mut cancellation) = self.begin()?;
        validate_installation(installation)?;
        progress(WindowsUpdateProgress::new("checking", None));
        let client = source.client()?;
        let (candidate, manifest) =
            verified_release(&source, &client, public_key, &mut cancellation)
                .await?
                .map_err(|_| "暂时没有可验证的新版 Windows 更新。".to_owned())?;
        if !newer_than(&candidate.version, current_version) {
            return Err("当前已是此通道的最新版本。".to_owned());
        }
        let mut staged =
            StagedInstaller::create(&installation.staging_root, &candidate.installer.name)?;
        let download = download_installer(
            &client,
            &candidate,
            &manifest,
            &mut staged,
            &mut cancellation,
            progress,
        )
        .await;
        if let Err(error) = download {
            staged.close().await;
            return Err(error);
        }
        // Serialize cancellation with CreateProcess: after launch the installer
        // owns the handoff and cancellation cannot report a false success.
        progress(WindowsUpdateProgress::new(
            "installing",
            Some(&candidate.version),
        ));
        let launched = {
            let mut active = self
                .active
                .lock()
                .map_err(|_| "更新任务暂时不可用。".to_owned())?;
            let current = active.as_mut().ok_or_else(|| CANCELLED.to_owned())?;
            if *current.cancellation.borrow() {
                return Err(CANCELLED.to_owned());
            }
            let launched = launch(&staged.path)?;
            staged.keep = true;
            current.committed = true;
            lease.committed = true;
            launched
        };
        #[cfg(not(test))]
        let LaunchedInstaller::Process(mut child) = launched;
        #[cfg(test)]
        let mut child = match launched {
            LaunchedInstaller::Process(child) => child,
            LaunchedInstaller::HandedOff => return Ok(()),
        };
        // The installer closes the old application only after its preflight
        // succeeds. Keep it usable if the child fails before that handoff.
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    lease.committed = false;
                    staged.keep = false;
                    staged.close().await;
                    return Err(if status.success() {
                        "更新安装器已结束，但更新尚未完成。当前版本仍可使用，请重试更新。"
                    } else {
                        "更新安装器未能完成升级。当前版本仍可使用，请重试更新。"
                    }
                    .to_owned());
                }
                Ok(None) => tokio::time::sleep(Duration::from_millis(100)).await,
                Err(_) => {
                    return Err("无法确认安装器状态，请先关闭安装器再重新打开客户端。".to_owned());
                }
            }
        }
    }
}
struct Installation {
    executable: PathBuf,
    local_app_data: PathBuf,
    staging_root: PathBuf,
}
fn validate_installation(installation: &Installation) -> Result<(), String> {
    let expected = installation
        .local_app_data
        .join("Programs")
        .join("DeskLink")
        .join("DeskLink.exe");
    let actual = fs::canonicalize(&installation.executable).ok();
    if actual.is_none() || actual != fs::canonicalize(expected).ok() {
        return Err("请从已安装的 DeskLink 客户端执行更新；开发版不会覆盖已安装程序。".to_owned());
    }
    Ok(())
}
struct StagedInstaller {
    directory: PathBuf,
    path: PathBuf,
    file: Option<tokio::fs::File>,
    keep: bool,
}
impl StagedInstaller {
    async fn close(&mut self) {
        if let Some(file) = self.file.take() {
            // Wait for any Tokio filesystem operation that was interrupted by
            // cancellation before deleting the private partial download.
            drop(file.into_std().await);
        }
    }
    fn create(root: &Path, name: &str) -> Result<Self, String> {
        fs::create_dir_all(root).map_err(|_| "无法创建更新下载目录。".to_owned())?;
        let mut random = [0; 16];
        OsRng.fill_bytes(&mut random);
        let directory = root.join(
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        );
        fs::create_dir(&directory).map_err(|_| "无法创建独立更新目录。".to_owned())?;
        let path = directory.join(name);
        let mut staged = Self {
            directory,
            path,
            file: None,
            keep: false,
        };
        // Deny writes/deletes until CreateProcess to prevent a verification /
        // execution race. The child may open the verified executable for reads.
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(1)
            .open(&staged.path)
            .map_err(|_| "无法创建更新安装包。".to_owned())?;
        staged.file = Some(tokio::fs::File::from_std(file));
        Ok(staged)
    }
}
impl Drop for StagedInstaller {
    fn drop(&mut self) {
        self.file.take();
        if !self.keep {
            let _ = fs::remove_file(&self.path);
            let _ = fs::remove_dir(&self.directory);
        }
    }
}
async fn download_installer(
    client: &Client,
    candidate: &ReleaseCandidate,
    manifest: &InstallerManifest,
    staged: &mut StagedInstaller,
    cancellation: &mut watch::Receiver<bool>,
    progress: &(dyn Fn(WindowsUpdateProgress) + Sync),
) -> Result<(), String> {
    let mut response = tokio::select! { biased;
        _ = cancelled(cancellation) => return Err(CANCELLED.to_owned()),
        response = client.get(candidate.installer_url.clone()).header(ACCEPT, "application/octet-stream").send() => response.map_err(|_| "更新下载失败，请重试。".to_owned())?,
    };
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|size| size != candidate.installer.size)
    {
        return Err("更新安装包大小或下载响应无效。".to_owned());
    }
    let mut downloaded = 0_u64;
    let mut reported = 0_u64;
    let mut last_report = std::time::Instant::now();
    let mut hasher = Sha256::new();
    let file = staged.file.as_mut().expect("new staged file");
    let event = |downloaded| WindowsUpdateProgress {
        stage: "downloading",
        version: Some(candidate.version.clone()),
        downloaded_bytes: Some(downloaded),
        total_bytes: Some(candidate.installer.size),
    };
    progress(event(0));
    loop {
        let chunk = tokio::select! { biased;
            _ = cancelled(cancellation) => return Err(CANCELLED.to_owned()),
            chunk = response.chunk() => chunk.map_err(|_| "更新下载中断，请重试。".to_owned())?,
        };
        let Some(chunk) = chunk else {
            break;
        };
        if chunk.len() as u64 > candidate.installer.size.saturating_sub(downloaded) {
            return Err("更新安装包超过声明大小。".to_owned());
        }
        tokio::select! { biased;
            _ = cancelled(cancellation) => return Err(CANCELLED.to_owned()),
            result = file.write_all(&chunk) => result.map_err(|_| "更新安装包写入失败。".to_owned())?,
        }
        hasher.update(&chunk);
        downloaded += chunk.len() as u64;
        if reported == 0
            || downloaded == candidate.installer.size
            || last_report.elapsed() >= Duration::from_millis(100)
        {
            progress(event(downloaded));
            reported = downloaded;
            last_report = std::time::Instant::now();
        }
    }
    if *cancellation.borrow() {
        return Err(CANCELLED.to_owned());
    }
    progress(WindowsUpdateProgress::new(
        "verifying",
        Some(&candidate.version),
    ));
    if downloaded != candidate.installer.size
        || format!("{:x}", hasher.finalize()) != manifest.installer.sha256
    {
        return Err("更新安装包校验失败，已保留当前版本。".to_owned());
    }
    file.flush()
        .await
        .map_err(|_| "更新安装包写入失败。".to_owned())?;
    file.sync_all()
        .await
        .map_err(|_| "更新安装包写入失败。".to_owned())?;
    // A writable handle prevents Windows from creating an executable image.
    // Reopen read-only, then verify again under that immutable launch lock so
    // a replacement in the close/reopen interval cannot bypass verification.
    staged.file.take();
    staged.file = Some(open_launch_lock(&staged.path)?);
    let read_lock = staged.file.as_mut().expect("read-only launch lock");
    let mut actual_size = 0_u64;
    let mut actual_hash = Sha256::new();
    let mut buffer = [0; 65_536];
    loop {
        let count = tokio::select! { biased;
            _ = cancelled(cancellation) => return Err(CANCELLED.to_owned()),
            count = read_lock.read(&mut buffer) => count.map_err(|_| "无法读取已下载的更新安装包。".to_owned())?,
        };
        if count == 0 {
            break;
        }
        actual_size += count as u64;
        if actual_size > candidate.installer.size {
            return Err("更新安装包在验证时发生变化。".to_owned());
        }
        actual_hash.update(&buffer[..count]);
    }
    if actual_size != candidate.installer.size
        || format!("{:x}", actual_hash.finalize()) != manifest.installer.sha256
    {
        return Err("更新安装包在验证时发生变化。".to_owned());
    }
    Ok(())
}

fn open_launch_lock(path: &Path) -> Result<tokio::fs::File, String> {
    OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(path)
        .map(tokio::fs::File::from_std)
        .map_err(|_| "无法锁定已下载的更新安装包。".to_owned())
}

fn cleanup_stale_downloads(root: &Path) {
    use std::os::windows::fs::MetadataExt;
    let plain = |path: &Path| {
        fs::symlink_metadata(path)
            .ok()
            .filter(|metadata| metadata.file_attributes() & 0x400 == 0)
    };
    if !plain(root).is_some_and(|metadata| metadata.is_dir()) {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for directory in entries.take(32).flatten() {
        let name = directory.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if decode_hex::<16>(name.as_bytes()).is_none()
            || name.len() != 32
            || !plain(&directory.path()).is_some_and(|metadata| metadata.is_dir())
        {
            continue;
        }
        let Ok(files) = fs::read_dir(directory.path()) else {
            continue;
        };
        let files = files.take(2).collect::<Result<Vec<_>, _>>();
        let Ok(files) = files else {
            continue;
        };
        let [file] = files.as_slice() else {
            continue;
        };
        let file_name = file.file_name();
        let Some(version) = file_name
            .to_str()
            .and_then(|name| name.strip_prefix("DeskLinkSetup-"))
            .and_then(|name| name.strip_suffix("-x64.exe"))
        else {
            continue;
        };
        let Some(metadata) = plain(&file.path()).filter(|metadata| metadata.is_file()) else {
            continue;
        };
        if normalize_release_version(version).as_deref() != Some(version)
            || !metadata
                .modified()
                .ok()
                .and_then(|time| time.elapsed().ok())
                .is_some_and(|age| age >= Duration::from_secs(86_400))
        {
            continue;
        }
        // Only delete this recognized cache file and its now-empty directory;
        // never recurse into arbitrary or reparse-point directory contents.
        if fs::remove_file(file.path()).is_ok() {
            let _ = fs::remove_dir(directory.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::{
        collections::HashMap,
        os::windows::process::CommandExt,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use tokio::{net::TcpListener, task::JoinSet};

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
    fn fixture_key() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }
    fn signature(bytes: &[u8]) -> Vec<u8> {
        let mut message = SIGNATURE_DOMAIN.to_vec();
        message.extend_from_slice(bytes);
        format!("{}\n", hex(&fixture_key().sign(&message).to_bytes())).into_bytes()
    }
    fn temporary_directory() -> PathBuf {
        let mut random = [0; 16];
        OsRng.fill_bytes(&mut random);
        let directory =
            std::env::temp_dir().join(format!("desklink-updater-test-{}", hex(&random)));
        fs::create_dir(&directory).unwrap();
        directory
    }
    #[derive(Clone, Copy)]
    enum BodyMode {
        Normal,
        Corrupt,
        Truncated,
        Overlong,
        Slow,
        TamperedManifest,
        HarmlessPe,
    }
    struct HttpFixture {
        source: ReleaseSource,
        public_key: String,
        installation: Installation,
        directory: PathBuf,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for HttpFixture {
        fn drop(&mut self) {
            self.task.abort();
            assert_eq!(
                self.directory.parent(),
                Some(std::env::temp_dir().as_path())
            );
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
    impl HttpFixture {
        async fn new(version: &str, mode: BodyMode) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
            let source = ReleaseSource::Local(base.clone());
            let installer_name = format!("DeskLinkSetup-{version}-x64.exe");
            let payload = if matches!(mode, BodyMode::HarmlessPe) {
                let command =
                    PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe");
                let mut bytes = fs::read(command).unwrap();
                // Preserve the production size boundary with a harmless PE overlay.
                bytes.resize(bytes.len().max(INSTALLER_MINIMUM_BYTES as usize + 1_024), 0);
                bytes
            } else {
                vec![0x53; INSTALLER_MINIMUM_BYTES as usize + 1_024]
            };
            let mut manifest = serde_json::to_vec(&serde_json::json!({
                "schema":1,"target":"x86_64-pc-windows-msvc","version":version,"passed":true,
                "signed":false,"source_dirty":true,"update_mode":"perUserV1",
                "installer":{"file_name":installer_name,"size_bytes":payload.len(),"sha256":format!("{:x}",Sha256::digest(&payload))}
            })).unwrap();
            let signed = signature(&manifest);
            if matches!(mode, BodyMode::TamperedManifest) {
                manifest.push(b' ');
            }
            let path = |name: &str| format!("{RELEASE_DOWNLOAD_PREFIX}v{version}/{name}");
            let asset = |name: &str, size: usize| serde_json::json!({"name":name,"size":size,"browser_download_url":base.join(&path(name)).unwrap().as_str()});
            let release = serde_json::to_vec(&serde_json::json!({"draft":false,"prerelease":false,"tag_name":format!("v{version}"),"published_at":null,"assets":[asset(&installer_name,payload.len()),asset(MANIFEST_NAME,manifest.len()),asset(SIGNATURE_NAME,signed.len())]})).unwrap();
            let mut routes = HashMap::new();
            routes.insert("/latest".to_owned(), release);
            routes.insert(path(MANIFEST_NAME), manifest);
            routes.insert(path(SIGNATURE_NAME), signed);
            let installer_path = path(&installer_name);
            let mut actual_payload = payload.clone();
            match mode {
                BodyMode::Corrupt => actual_payload[0] ^= 1,
                BodyMode::Truncated => {
                    actual_payload.pop();
                }
                BodyMode::Overlong => actual_payload.push(0),
                _ => {}
            }
            routes.insert(installer_path.clone(), actual_payload);
            let routes = Arc::new(routes);
            let task = tokio::spawn(async move {
                let mut connections = JoinSet::new();
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            let Ok((mut stream, _)) = accepted else { break; };
                            let routes = routes.clone(); let installer_path = installer_path.clone();
                            connections.spawn(async move {
                                let mut request = Vec::new(); let mut byte = [0];
                                while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
                                    if stream.read_exact(&mut byte).await.is_err() { return; } request.push(byte[0]);
                                }
                                let request = String::from_utf8_lossy(&request);
                                let path = request.split_whitespace().nth(1).unwrap_or("/");
                                let Some(body) = routes.get(path) else { let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await; return; };
                                let installer = path == installer_path;
                                // Omit Content-Length on package bodies so truncated /
                                // oversized responses exercise the streaming boundary.
                                let header = if installer { "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_owned() } else { format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()) };
                                if stream.write_all(header.as_bytes()).await.is_err() { return; }
                                if installer && matches!(mode, BodyMode::Slow) {
                                    let _ = stream.write_all(&body[..1_024]).await;
                                    tokio::time::sleep(Duration::from_secs(60)).await;
                                } else { let _ = stream.write_all(body).await; }
                            });
                        }
                        _ = connections.join_next(), if !connections.is_empty() => {}
                    }
                }
            });
            let directory = temporary_directory();
            let local_app_data = directory.join("Local");
            let executable = local_app_data.join("Programs/DeskLink/DeskLink.exe");
            fs::create_dir_all(executable.parent().unwrap()).unwrap();
            fs::write(&executable, b"isolated installed fixture").unwrap();
            let installation = Installation {
                executable,
                local_app_data,
                staging_root: directory.join("downloads"),
            };
            Self {
                source,
                public_key: hex(fixture_key().verifying_key().as_bytes()),
                installation,
                directory,
                task,
            }
        }
        async fn install(
            &self,
            manager: &UpdateManager,
            current: &str,
            progress: &(dyn Fn(WindowsUpdateProgress) + Sync),
            launch: &(dyn Fn(&Path) -> Result<LaunchedInstaller, String> + Sync),
        ) -> Result<(), String> {
            manager
                .install_with(
                    self.source.clone(),
                    &self.public_key,
                    current,
                    &self.installation,
                    progress,
                    launch,
                )
                .await
        }
        fn download_count(&self) -> usize {
            fs::read_dir(&self.installation.staging_root)
                .map(|entries| entries.count())
                .unwrap_or(0)
        }
    }

    #[test]
    fn python_signature_vector_matches_raw_bytes_domain_and_strict_key() {
        let raw = br#"{"schema":1,"version":"0.1.92"}"#;
        let public = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
        let signature = b"6b75f65cfb8488a64b451b3779c025197989fd65a61cbd1073fe86bef4d3ee6cedddc3952f43f83d4b18a6ebaae70ec07d67cf293490e4a7b101838489916c01\n";
        assert!(verify_manifest_signature(public, raw, signature));
        assert!(verify_manifest_signature(
            &format!("{public}\r\n"),
            raw,
            &[&signature[..128], b"\r\n"].concat()
        ));
        assert!(!verify_manifest_signature(
            &format!(" {public}"),
            raw,
            signature
        ));
        assert!(!verify_manifest_signature(PUBLIC_KEY, raw, signature));
        let mut tampered = raw.to_vec();
        tampered.push(b' ');
        assert!(!verify_manifest_signature(public, &tampered, signature));
        assert!(!verify_manifest_signature(public, raw, &signature[..127]));
        assert!(decode_hex::<32>(PUBLIC_KEY.as_bytes()).is_some());
    }
    #[test]
    fn exact_repository_tag_asset_and_numeric_version_boundaries_fail_closed() {
        let source = ReleaseSource::Github;
        let valid = "https://github.com/puzzle-fuzzy/desk-link/releases/download/v1.2.3/windows-installer-manifest.json";
        assert!(source.asset_url(valid, "v1.2.3", MANIFEST_NAME).is_some());
        for value in [
            valid.replace("https:", "http:"),
            valid.replace("github.com/", "github.com:444/"),
            valid.replace("v1.2.3", "v1.2.2"),
            format!("{valid}?x=1"),
            valid.replace("github.com/", "user@github.com/"),
            format!("{valid}#x"),
        ] {
            assert!(source.asset_url(&value, "v1.2.3", MANIFEST_NAME).is_none());
        }
        assert!(newer_than("0.10.0", "0.9.99"));
        assert!(!newer_than("0.1.92", "0.1.92"));
        assert!(!newer_than("0.1.91", "0.1.92"));
        for value in [
            "1.2",
            "01.2.3",
            "1.2.3-beta",
            " 1.2.3",
            "18446744073709551616.0.0",
        ] {
            assert!(normalize_release_version(value).is_none());
        }
    }
    #[test]
    fn signed_manifest_still_requires_the_exact_target_update_mode_and_asset() {
        let version = "1.2.3";
        let installer_name = format!("DeskLinkSetup-{version}-x64.exe");
        let asset = |name: &str, size: u64| GithubAsset {
            name: name.to_owned(),
            size,
            browser_download_url: format!(
                "https://github.com{RELEASE_DOWNLOAD_PREFIX}v{version}/{name}"
            ),
        };
        let release = || GithubRelease {
            draft: false,
            prerelease: false,
            tag_name: format!("v{version}"),
            published_at: None,
            assets: vec![
                asset(&installer_name, INSTALLER_MINIMUM_BYTES),
                asset(MANIFEST_NAME, 512),
                asset(SIGNATURE_NAME, 129),
            ],
        };
        let candidate = inspect_release(&ReleaseSource::Github, release()).unwrap();
        let value = serde_json::json!({"schema":1,"target":"x86_64-pc-windows-msvc","version":version,"passed":true,"update_mode":"perUserV1","installer":{"file_name":installer_name,"size_bytes":INSTALLER_MINIMUM_BYTES,"sha256":"a".repeat(64)}});
        assert!(is_verified_manifest(
            &candidate,
            &serde_json::from_value(value.clone()).unwrap()
        ));
        for (field, invalid) in [
            ("schema", serde_json::json!(2)),
            ("target", serde_json::json!("aarch64-pc-windows-msvc")),
            ("version", serde_json::json!("1.2.2")),
            ("passed", serde_json::json!(false)),
            ("update_mode", serde_json::json!("legacy")),
        ] {
            let mut changed = value.clone();
            changed[field] = invalid;
            assert!(!is_verified_manifest(
                &candidate,
                &serde_json::from_value(changed).unwrap()
            ));
        }
        for (field, invalid) in [
            ("file_name", serde_json::json!("other.exe")),
            ("size_bytes", serde_json::json!(INSTALLER_MINIMUM_BYTES + 1)),
            ("sha256", serde_json::json!("A".repeat(64))),
        ] {
            let mut changed = value.clone();
            changed["installer"][field] = invalid;
            assert!(!is_verified_manifest(
                &candidate,
                &serde_json::from_value(changed).unwrap()
            ));
        }
        let mut duplicate = release();
        duplicate.assets.push(asset(SIGNATURE_NAME, 129));
        assert!(inspect_release(&ReleaseSource::Github, duplicate).is_err());
        let mut missing = release();
        missing.assets.pop();
        assert!(inspect_release(&ReleaseSource::Github, missing).is_err());
        let mut draft = release();
        draft.draft = true;
        assert!(inspect_release(&ReleaseSource::Github, draft).is_err());
    }
    #[test]
    fn cache_cleanup_only_removes_old_recognized_packages_without_recursing() {
        use std::fs::FileTimes;
        let root = temporary_directory();
        for (name, file, old) in [
            ("11".repeat(16), "DeskLinkSetup-1.2.3-x64.exe", true),
            ("22".repeat(16), "DeskLinkSetup-1.2.4-x64.exe", false),
            ("33".repeat(16), "user-notes.txt", true),
        ] {
            let directory = root.join(name);
            fs::create_dir(&directory).unwrap();
            let path = directory.join(file);
            fs::write(&path, b"fixture").unwrap();
            if old {
                fs::File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_times(
                        FileTimes::new().set_modified(
                            std::time::SystemTime::now() - Duration::from_secs(172_800),
                        ),
                    )
                    .unwrap();
            }
        }
        cleanup_stale_downloads(&root);
        assert!(!root.join("11".repeat(16)).exists());
        assert!(
            root.join("22".repeat(16))
                .join("DeskLinkSetup-1.2.4-x64.exe")
                .is_file()
        );
        assert!(root.join("33".repeat(16)).join("user-notes.txt").is_file());
        assert_eq!(root.parent(), Some(std::env::temp_dir().as_path()));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "requires DESKLINK_TEST_UPDATE_MANIFEST pointing to an explicitly built local signed package"]
    fn actual_built_manifest_verifies_with_embedded_public_key() {
        use std::io::Read;
        let path = PathBuf::from(
            std::env::var_os("DESKLINK_TEST_UPDATE_MANIFEST")
                .expect("select a local build manifest explicitly"),
        );
        let raw = fs::read(&path).unwrap();
        assert!(raw.len() <= MANIFEST_MAXIMUM_BYTES);
        let signature = fs::read(path.with_file_name(SIGNATURE_NAME)).unwrap();
        assert!(
            verify_manifest_signature(PUBLIC_KEY, &raw, &signature),
            "built update must be authorized by the embedded product key"
        );
        let manifest: InstallerManifest = serde_json::from_slice(&raw).unwrap();
        assert_eq!(
            normalize_release_version(&manifest.version).as_deref(),
            Some(manifest.version.as_str())
        );
        let installer_name = format!("DeskLinkSetup-{}-x64.exe", manifest.version);
        assert_eq!(manifest.installer.file_name, installer_name);
        let url = Url::parse("https://github.com/puzzle-fuzzy/desk-link/releases/latest").unwrap();
        let candidate = ReleaseCandidate {
            version: manifest.version.clone(),
            published_at: None,
            installer: GithubAsset {
                name: installer_name.clone(),
                size: manifest.installer.size_bytes,
                browser_download_url: url.to_string(),
            },
            installer_url: url.clone(),
            manifest_url: url.clone(),
            signature_url: url,
        };
        assert!(
            (INSTALLER_MINIMUM_BYTES..=INSTALLER_MAXIMUM_BYTES)
                .contains(&manifest.installer.size_bytes)
        );
        assert!(is_verified_manifest(&candidate, &manifest));
        let mut file = fs::File::open(path.parent().unwrap().join(installer_name)).unwrap();
        assert_eq!(
            file.metadata().unwrap().len(),
            manifest.installer.size_bytes
        );
        let mut hasher = Sha256::new();
        let mut buffer = [0; 65_536];
        loop {
            let count = file.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        assert_eq!(
            format!("{:x}", hasher.finalize()),
            manifest.installer.sha256
        );
    }
    #[tokio::test]
    async fn signed_update_downloads_checks_hash_and_commits_only_once() {
        let fixture = HttpFixture::new("1.2.3", BodyMode::Normal).await;
        let manager = UpdateManager::default();
        let launched = AtomicUsize::new(0);
        let stages = Mutex::new(Vec::new());
        fixture
            .install(
                &manager,
                "1.2.2",
                &|event| stages.lock().unwrap().push(event.stage),
                &|path| {
                    assert_eq!(
                        fs::read(path).unwrap().len(),
                        INSTALLER_MINIMUM_BYTES as usize + 1_024
                    );
                    assert!(OpenOptions::new().write(true).open(path).is_err());
                    launched.fetch_add(1, Ordering::SeqCst);
                    Ok(LaunchedInstaller::HandedOff)
                },
            )
            .await
            .unwrap();
        assert_eq!(launched.load(Ordering::SeqCst), 1);
        assert!(!manager.cancel());
        assert!(manager.begin().is_err());
        let stages = stages.lock().unwrap();
        assert_eq!(stages.first(), Some(&"checking"));
        assert!(stages.contains(&"downloading"));
        assert!(stages.contains(&"verifying"));
        assert_eq!(stages.last(), Some(&"installing"));
    }
    #[tokio::test]
    async fn old_equal_tampered_and_wrong_size_packages_never_launch_or_leave_partial_files() {
        for (version, mode) in [
            ("1.2.2", BodyMode::Normal),
            ("1.2.1", BodyMode::Normal),
            ("1.2.3", BodyMode::Corrupt),
            ("1.2.3", BodyMode::Truncated),
            ("1.2.3", BodyMode::Overlong),
            ("1.2.3", BodyMode::TamperedManifest),
        ] {
            let fixture = HttpFixture::new(version, mode).await;
            let manager = UpdateManager::default();
            assert!(
                fixture
                    .install(&manager, "1.2.2", &|_| {}, &|_| panic!(
                        "invalid update must never launch"
                    ))
                    .await
                    .is_err()
            );
            assert_eq!(fixture.download_count(), 0);
            assert!(manager.begin().is_ok());
        }
    }
    #[tokio::test]
    async fn cancellation_during_streaming_is_prompt_single_flight_and_cleans_the_partial() {
        let fixture = HttpFixture::new("1.2.3", BodyMode::Slow).await;
        let manager = UpdateManager::default();
        let cancel_after_chunk = |event: WindowsUpdateProgress| {
            if event.downloaded_bytes.is_some_and(|count| count > 0) {
                assert!(manager.begin().is_err());
                assert!(manager.cancel());
            }
        };
        let (result, duplicate) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(
                fixture.install(&manager, "1.2.2", &cancel_after_chunk, &|_| panic!(
                    "cancelled update must never launch"
                ),),
                fixture.install(&manager, "1.2.2", &|_| {}, &|_| panic!(
                    "duplicate update must never launch"
                ))
            )
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap_err(), CANCELLED);
        assert!(duplicate.unwrap_err().contains("重复"));
        assert_eq!(fixture.download_count(), 0);
        assert!(manager.begin().is_ok());
    }
    #[tokio::test]
    async fn cancellation_at_handoff_and_launch_failure_leave_the_current_app_running() {
        let fixture = HttpFixture::new("1.2.3", BodyMode::Normal).await;
        let manager = UpdateManager::default();
        assert_eq!(
            fixture
                .install(
                    &manager,
                    "1.2.2",
                    &|event| {
                        if event.stage == "installing" {
                            assert!(manager.cancel());
                        }
                    },
                    &|_| panic!("cancel before commit must win")
                )
                .await
                .unwrap_err(),
            CANCELLED
        );
        assert_eq!(fixture.download_count(), 0);
        assert!(
            fixture
                .install(&manager, "1.2.2", &|_| {}, &|_| Err(
                    "injected launch failure".to_owned()
                ))
                .await
                .is_err()
        );
        assert_eq!(fixture.download_count(), 0);
        assert!(fixture.installation.executable.is_file());
    }
    #[tokio::test]
    async fn debug_and_portable_paths_cannot_overwrite_the_installed_copy() {
        let mut fixture = HttpFixture::new("1.2.3", BodyMode::Normal).await;
        fixture.installation.executable = fixture.directory.join("debug.exe");
        fs::write(&fixture.installation.executable, b"debug").unwrap();
        assert!(
            fixture
                .install(
                    &UpdateManager::default(),
                    "1.2.2",
                    &|_| panic!("must reject before network"),
                    &|_| panic!("must reject before launch")
                )
                .await
                .unwrap_err()
                .contains("开发版")
        );
    }
    #[tokio::test]
    async fn checking_can_be_cancelled_before_any_download() {
        let fixture = HttpFixture::new("1.2.3", BodyMode::Normal).await;
        let manager = UpdateManager::default();
        assert_eq!(
            fixture
                .install(
                    &manager,
                    "1.2.2",
                    &|event| {
                        if event.stage == "checking" {
                            assert!(manager.cancel());
                        }
                    },
                    &|_| panic!("cancelled check must not launch")
                )
                .await
                .unwrap_err(),
            CANCELLED
        );
        assert_eq!(fixture.download_count(), 0);
    }
    #[tokio::test]
    async fn real_downloaded_pe_exit_allows_retry_without_closing_current_app() {
        for code in ["exit 7", "exit 0"] {
            let fixture = HttpFixture::new("1.2.3", BodyMode::HarmlessPe).await;
            let manager = UpdateManager::default();
            let error = fixture
                .install(&manager, "1.2.2", &|_| {}, &|path| {
                    Command::new(path)
                        .args(["/d", "/c", code])
                        .creation_flags(0x0800_0000)
                        .spawn()
                        .map(LaunchedInstaller::Process)
                        .map_err(|error| error.to_string())
                })
                .await
                .unwrap_err();
            assert!(error.contains("当前版本仍可使用"));
            assert!(fixture.installation.executable.is_file());
            assert_eq!(fixture.download_count(), 0);
            assert!(manager.begin().is_ok());
        }
    }
    #[tokio::test]
    async fn read_only_launch_lock_allows_real_harmless_windows_pe_execution() {
        let directory = temporary_directory();
        let path = directory.join("probe.exe");
        let command =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe");
        fs::copy(command, &path).unwrap();
        let lock = open_launch_lock(&path).unwrap();
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        let status = Command::new(&path)
            .args(["/d", "/c", "exit 0"])
            .creation_flags(0x0800_0000)
            .status()
            .unwrap();
        assert!(status.success());
        drop(lock);
        assert_eq!(directory.parent(), Some(std::env::temp_dir().as_path()));
        fs::remove_dir_all(directory).unwrap();
    }
}
