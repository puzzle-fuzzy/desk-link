use std::time::Duration;

use reqwest::{
    Client, StatusCode, Url,
    header::{ACCEPT, USER_AGENT},
    redirect::{Attempt, Policy},
};
use serde::{Deserialize, Serialize};

const RELEASE_API_URL: &str = "https://api.github.com/repos/puzzle-fuzzy/desk-link/releases/latest";
const RELEASE_DOWNLOAD_PREFIX: &str = "/puzzle-fuzzy/desk-link/releases/download/";
const INSTALLER_MINIMUM_BYTES: u64 = 1_000_000;
const RELEASE_RESPONSE_MAXIMUM_BYTES: usize = 1_000_000;
const MANIFEST_MAXIMUM_BYTES: usize = 100_000;

#[derive(Debug, Serialize)]
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

#[derive(Debug, Deserialize)]
struct GithubRelease {
    draft: bool,
    prerelease: bool,
    tag_name: String,
    published_at: Option<String>,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

#[derive(Debug)]
struct ReleaseCandidate {
    version: String,
    published_at: Option<String>,
    installer_name: String,
    installer_size: u64,
    manifest_url: Url,
}

#[derive(Debug, Deserialize)]
struct InstallerManifest {
    schema: u8,
    version: String,
    signed: bool,
    passed: bool,
    installer: ManifestInstaller,
}

#[derive(Debug, Deserialize)]
struct ManifestInstaller {
    file_name: String,
    size_bytes: u64,
    sha256: String,
}

pub async fn check() -> Result<WindowsReleaseSource, String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(12))
        .redirect(Policy::custom(allow_release_redirect))
        .build()
        .map_err(|_| "update client could not be created".to_owned())?;
    let response = client
        .get(RELEASE_API_URL)
        .header(
            USER_AGENT,
            format!("DeskLink/{}", env!("CARGO_PKG_VERSION")),
        )
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| "GitHub release request failed".to_owned())?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(WindowsReleaseSource::Unavailable {
            reason: "noRelease",
        });
    }
    if !response.status().is_success() {
        return Err(format!(
            "GitHub release request returned {}",
            response.status()
        ));
    }
    let release_bytes = limited_response(response, RELEASE_RESPONSE_MAXIMUM_BYTES).await?;
    let release: GithubRelease = serde_json::from_slice(&release_bytes)
        .map_err(|_| "GitHub release response was invalid".to_owned())?;
    let candidate = match inspect_release(release) {
        Ok(candidate) => candidate,
        Err(reason) => return Ok(WindowsReleaseSource::Unavailable { reason }),
    };

    let response = client
        .get(candidate.manifest_url.clone())
        .header(
            USER_AGENT,
            format!("DeskLink/{}", env!("CARGO_PKG_VERSION")),
        )
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| "Windows release manifest request failed".to_owned())?;
    if !response.status().is_success() {
        return Ok(WindowsReleaseSource::Unavailable {
            reason: "unverifiedWindowsRelease",
        });
    }
    let manifest_bytes = limited_response(response, MANIFEST_MAXIMUM_BYTES).await?;
    let manifest: InstallerManifest = match serde_json::from_slice(&manifest_bytes) {
        Ok(manifest) => manifest,
        Err(_) => {
            return Ok(WindowsReleaseSource::Unavailable {
                reason: "unverifiedWindowsRelease",
            });
        }
    };
    if !is_verified_manifest(&candidate, &manifest) {
        return Ok(WindowsReleaseSource::Unavailable {
            reason: "unverifiedWindowsRelease",
        });
    }
    Ok(WindowsReleaseSource::Release {
        latest_version: candidate.version,
        published_at: candidate.published_at,
    })
}

fn allow_release_redirect(attempt: Attempt<'_>) -> reqwest::redirect::Action {
    if attempt.previous().len() >= 5 {
        return attempt.stop();
    }
    let url = attempt.url();
    let allowed_host = matches!(
        url.host_str(),
        Some("api.github.com" | "github.com" | "release-assets.githubusercontent.com")
    );
    if url.scheme() == "https" && allowed_host {
        attempt.follow()
    } else {
        attempt.stop()
    }
}

async fn limited_response(
    response: reqwest::Response,
    limit: usize,
) -> Result<bytes::Bytes, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("Windows release response exceeded its size limit".to_owned());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "Windows release response could not be read".to_owned())?;
    if bytes.len() > limit {
        return Err("Windows release response exceeded its size limit".to_owned());
    }
    Ok(bytes)
}

fn inspect_release(release: GithubRelease) -> Result<ReleaseCandidate, &'static str> {
    if release.draft || release.prerelease {
        return Err("invalidRelease");
    }
    let version = normalize_release_version(&release.tag_name).ok_or("invalidRelease")?;
    let installer_name = format!("DeskLinkSetup-{version}-x64.exe");
    let installer = release
        .assets
        .iter()
        .find(|asset| asset.name == installer_name && asset.size >= INSTALLER_MINIMUM_BYTES)
        .ok_or("incompleteWindowsRelease")?;
    let manifest = release
        .assets
        .iter()
        .find(|asset| {
            asset.name == "windows-installer-manifest.json"
                && asset.size > 0
                && asset.size <= MANIFEST_MAXIMUM_BYTES as u64
        })
        .ok_or("incompleteWindowsRelease")?;
    let manifest_url = trusted_release_download(&manifest.browser_download_url)
        .ok_or("incompleteWindowsRelease")?;
    if trusted_release_download(&installer.browser_download_url).is_none() {
        return Err("incompleteWindowsRelease");
    }
    let published_at = release
        .published_at
        .filter(|value| !value.is_empty() && value.len() <= 64);
    Ok(ReleaseCandidate {
        version,
        published_at,
        installer_name,
        installer_size: installer.size,
        manifest_url,
    })
}

fn trusted_release_download(value: &str) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    (url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.path().starts_with(RELEASE_DOWNLOAD_PREFIX))
    .then_some(url)
}

fn is_verified_manifest(candidate: &ReleaseCandidate, manifest: &InstallerManifest) -> bool {
    manifest.schema == 1
        && manifest.signed
        && manifest.passed
        && normalize_release_version(&manifest.version).as_deref()
            == Some(candidate.version.as_str())
        && manifest.installer.file_name == candidate.installer_name
        && manifest.installer.size_bytes == candidate.installer_size
        && manifest.installer.sha256.len() == 64
        && manifest
            .installer
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn normalize_release_version(value: &str) -> Option<String> {
    let version = value.trim().strip_prefix('v').unwrap_or(value.trim());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str) -> GithubRelease {
        let installer_name = format!("DeskLinkSetup-{version}-x64.exe");
        GithubRelease {
            draft: false,
            prerelease: false,
            tag_name: format!("v{version}"),
            published_at: Some("2026-07-19T08:00:00Z".to_owned()),
            assets: vec![
                GithubAsset {
                    name: installer_name.clone(),
                    size: 24_000_000,
                    browser_download_url: format!(
                        "https://github.com/puzzle-fuzzy/desk-link/releases/download/v{version}/{installer_name}"
                    ),
                },
                GithubAsset {
                    name: "windows-installer-manifest.json".to_owned(),
                    size: 700,
                    browser_download_url: format!(
                        "https://github.com/puzzle-fuzzy/desk-link/releases/download/v{version}/windows-installer-manifest.json"
                    ),
                },
            ],
        }
    }

    #[test]
    fn stable_release_requires_matching_windows_assets_on_the_fixed_repository() {
        let candidate = inspect_release(release("0.1.42")).unwrap();
        assert_eq!(candidate.version, "0.1.42");
        let mut redirected = release("0.1.42");
        redirected.assets[0].browser_download_url = "https://example.com/app.exe".to_owned();
        assert_eq!(
            inspect_release(redirected).unwrap_err(),
            "incompleteWindowsRelease"
        );
    }

    #[test]
    fn draft_prerelease_and_non_semver_tags_are_rejected() {
        let mut draft = release("0.1.42");
        draft.draft = true;
        assert_eq!(inspect_release(draft).unwrap_err(), "invalidRelease");
        assert!(normalize_release_version("0.1.42-beta.1").is_none());
        assert!(normalize_release_version("01.1.42").is_none());
    }

    #[test]
    fn manifest_must_be_signed_and_match_the_exact_installer() {
        let candidate = inspect_release(release("0.1.42")).unwrap();
        let mut manifest = InstallerManifest {
            schema: 1,
            version: "0.1.42".to_owned(),
            signed: true,
            passed: true,
            installer: ManifestInstaller {
                file_name: "DeskLinkSetup-0.1.42-x64.exe".to_owned(),
                size_bytes: 24_000_000,
                sha256: "a".repeat(64),
            },
        };
        assert!(is_verified_manifest(&candidate, &manifest));
        manifest.signed = false;
        assert!(!is_verified_manifest(&candidate, &manifest));
        manifest.signed = true;
        manifest.installer.size_bytes += 1;
        assert!(!is_verified_manifest(&candidate, &manifest));
    }

    #[test]
    fn release_source_uses_the_frontend_camel_case_contract() {
        let value = serde_json::to_value(WindowsReleaseSource::Release {
            latest_version: "0.1.42".to_owned(),
            published_at: None,
        })
        .unwrap();
        assert_eq!(value["kind"], "release");
        assert_eq!(value["latestVersion"], "0.1.42");
        assert!(value.get("latest_version").is_none());
    }
}
