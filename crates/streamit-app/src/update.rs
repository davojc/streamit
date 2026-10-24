//! Updates from the GitHub releases of davojc/streamit.
//!
//! Each release carries one Windows exe, built with GStreamer delay-loaded, so it runs
//! on the playout PC and on any other PC. Installing replaces the running exe in place; the new version runs from the next start. The window offers it
//! only while no service is running (the same rule as the simulation switch).
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Duration;

const LATEST_URL: &str = "https://api.github.com/repos/davojc/streamit/releases/latest";
pub const RELEASES_PAGE: &str = "https://github.com/davojc/streamit/releases";
/// The release file StreamIt updates from.
pub const ASSET: &str = "streamit-windows-x64.exe";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// A newer release this build can move to.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    pub page: String,
    url: String,
    /// Hex SHA-256 of the file, when GitHub reports one.
    sha256: Option<String>,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

/// Reads GitHub's latest-release answer: the release to move to, or `None` when
/// `current` is as new or newer.
pub fn newer(json: &str, current: &str, asset: &str) -> Result<Option<Release>, String> {
    let r: GhRelease = serde_json::from_str(json).map_err(|e| format!("unexpected answer from GitHub: {e}"))?;
    let current = Version::parse(current).map_err(|e| format!("this build's version {current}: {e}"))?;
    let tag = r.tag_name.trim_start_matches('v');
    let version = Version::parse(tag).map_err(|e| format!("release {}: {e}", r.tag_name))?;
    if r.draft || r.prerelease || version <= current {
        return Ok(None);
    }
    let a = r
        .assets
        .into_iter()
        .find(|a| a.name == asset)
        .ok_or_else(|| format!("release {} has no {asset} yet", r.tag_name))?;
    let sha256 = a.digest.and_then(|d| d.strip_prefix("sha256:").map(str::to_ascii_lowercase));
    Ok(Some(Release { version, page: r.html_url, url: a.browser_download_url, sha256 }))
}

fn client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("StreamIt/", env!("CARGO_PKG_VERSION")))
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())
}

/// Asks GitHub for the latest release. `None` when up to date (or nothing is released yet).
pub async fn check() -> Result<Option<Release>, String> {
    let res = client(CHECK_TIMEOUT)?
        .get(LATEST_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("could not reach GitHub: {e}"))?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let res = res.error_for_status().map_err(|e| format!("GitHub: {e}"))?;
    let body = res.text().await.map_err(|e| format!("GitHub: {e}"))?;
    newer(&body, CURRENT, ASSET)
}

/// The downloaded file is the whole file GitHub described, and a Windows program.
pub fn verify(bytes: &[u8], sha256: Option<&str>) -> Result<(), String> {
    if let Some(want) = sha256 {
        let got = hex::encode(Sha256::digest(bytes));
        if got != want {
            return Err("the download was damaged (checksum mismatch); nothing was changed".into());
        }
    }
    if !bytes.starts_with(b"MZ") {
        return Err("the download is not a Windows program; nothing was changed".into());
    }
    Ok(())
}

/// Downloads the release and puts it in place of the running exe. Returns the exe's
/// path, to start the new version from.
pub async fn install(r: Release) -> Result<PathBuf, String> {
    // Read before the swap: afterwards the running file has been moved aside.
    let exe = std::env::current_exe().map_err(|e| format!("where StreamIt is installed: {e}"))?;
    let bytes = client(DOWNLOAD_TIMEOUT)?
        .get(&r.url)
        .send()
        .await
        .and_then(|res| res.error_for_status())
        .map_err(|e| format!("download failed: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("download failed: {e}"))?;
    verify(&bytes, r.sha256.as_deref())?;
    let staged = exe.with_extension("update");
    let exe_for_swap = exe.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::write(&staged, &bytes).map_err(|e| format!("could not write {}: {e}", staged.display()))?;
        let swapped = self_replace::self_replace(&staged)
            .map_err(|e| format!("could not replace {}: {e}", exe_for_swap.display()));
        let _ = std::fs::remove_file(&staged);
        swapped
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(exe)
}

/// Starts the installed exe. The caller then closes this window; the new one finds the
/// Stream Deck when this one lets go of it.
pub fn start(exe: &PathBuf) -> std::io::Result<()> {
    std::process::Command::new(exe).spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, assets: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/davojc/streamit/releases/tag/{tag}",
                "draft":false,"prerelease":false,"assets":[{assets}]}}"#
        )
    }

    const ASSETS: &str = r#"
        {"name":"notes.txt","browser_download_url":"https://x/notes.txt","digest":null},
        {"name":"streamit-windows-x64.exe","browser_download_url":"https://x/streamit.exe","digest":"sha256:AB12"}"#;

    #[test]
    fn a_newer_release_is_offered_with_its_exe() {
        let r = newer(&release("v1.2.0", ASSETS), "1.1.0", "streamit-windows-x64.exe").unwrap().unwrap();
        assert_eq!(r.version, Version::new(1, 2, 0));
        assert_eq!(r.url, "https://x/streamit.exe", "only the exe, never another file");
        assert_eq!(r.sha256.as_deref(), Some("ab12"));
    }

    #[test]
    fn same_or_older_releases_are_not_offered() {
        assert_eq!(newer(&release("v1.1.0", ASSETS), "1.1.0", "streamit-windows-x64.exe").unwrap(), None);
        assert_eq!(newer(&release("1.0.9", ASSETS), "1.1.0", "streamit-windows-x64.exe").unwrap(), None);
        assert_eq!(newer(&release("v1.1.0", ASSETS), "1.10.0", "streamit-windows-x64.exe").unwrap(), None);
    }

    #[test]
    fn drafts_and_prereleases_are_skipped() {
        let pre = release("v2.0.0", ASSETS).replace(r#""prerelease":false"#, r#""prerelease":true"#);
        assert_eq!(newer(&pre, "1.1.0", "streamit-windows-x64.exe").unwrap(), None);
    }

    #[test]
    fn a_release_still_uploading_says_so() {
        let e = newer(&release("v1.2.0", ""), "1.1.0", "streamit-windows-x64.exe").unwrap_err();
        assert!(e.contains("no streamit-windows-x64.exe yet"), "{e}");
        assert!(newer("<html>", "1.1.0", "x").is_err());
        assert!(newer(&release("latest", ASSETS), "1.1.0", "x").is_err());
    }

    #[test]
    fn a_damaged_or_wrong_download_is_refused() {
        let exe = b"MZ\x90\x00rest";
        let sum = hex::encode(Sha256::digest(exe));
        assert!(verify(exe, Some(&sum)).is_ok());
        assert!(verify(exe, None).is_ok());
        assert!(verify(b"MZ\x90\x00resT", Some(&sum)).unwrap_err().contains("checksum"));
        assert!(verify(b"<html>not found</html>", None).unwrap_err().contains("not a Windows program"));
    }

    #[test]
    fn this_build_knows_its_version() {
        Version::parse(CURRENT).unwrap();
    }
}
