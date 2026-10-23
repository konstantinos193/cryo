//! Self-update: check the release feed for a newer installer, fetch it
//! in the background, and hand it to Windows. Everything network- or
//! disk-bound runs on a worker thread off the event loop — the caller
//! gets one `Outcome` back through a callback (AGENTS.md §5, §9).
//!
//! `github` is the feed adapter. The manifest is a wire type: it lives
//! at this boundary, gets version-checked, and only then becomes an
//! `Update` — a downloaded, checksum-verified installer path.

mod github;

use std::path::PathBuf;

use semver::Version;
use serde::Deserialize;
use thiserror::Error;

/// The `latest.json` attached to every GitHub release. Wire format —
/// strict fields, parsed once at the boundary.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub version: String,
    pub installer_url: String,
    pub sha256: String,
}

/// A release newer than the running build: installer already downloaded
/// and checksum-verified, ready to launch.
#[derive(Debug)]
pub struct Update {
    pub version: Version,
    pub installer: PathBuf,
}

/// What a background check found. `UpToDate` matters only for manual
/// checks — automatic ones stay silent unless an update is `Ready`.
#[derive(Debug)]
pub enum Outcome {
    Ready(Update),
    UpToDate,
    Failed(UpdateError),
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UpdateError {
    #[error("fetching release manifest: {0}")]
    FetchManifest(String),
    #[error("parsing release manifest: {0}")]
    ParseManifest(String),
    #[error("release version {0:?} is not semver")]
    BadVersion(String, #[source] semver::Error),
    #[error("downloading installer: {0}")]
    Download(String),
    #[error("writing installer to disk")]
    WriteInstaller(#[source] std::io::Error),
    #[error("verifying installer")]
    VerifyInstaller(#[source] std::io::Error),
    #[error("installer sha256 mismatch")]
    Checksum,
    #[error("launching installer")]
    Launch(#[source] std::io::Error),
}

/// Check for an update on a worker thread; exactly one `notify` call
/// fires with the outcome. If a newer version exists the installer is
/// downloaded and verified before `Ready` is reported, so applying it
/// later is instant.
pub fn spawn_check(notify: impl Fn(Outcome) + Send + 'static) {
    std::thread::spawn(move || notify(fetch_ready_update()));
}

/// Launch the downloaded installer and let it take over. The flags run
/// Inno fully silent, close cryo through Restart Manager and relaunch it
/// after install — the caller should exit the event loop right after.
pub fn launch_installer(update: &Update) -> Result<(), UpdateError> {
    std::process::Command::new(&update.installer)
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/CLOSEAPPLICATIONS",
        ])
        .spawn()
        .map_err(UpdateError::Launch)?;
    Ok(())
}

fn fetch_ready_update() -> Outcome {
    match ready_update() {
        Ok(Some(u)) => Outcome::Ready(u),
        Ok(None) => Outcome::UpToDate,
        Err(e) => Outcome::Failed(e),
    }
}

fn ready_update() -> Result<Option<Update>, UpdateError> {
    let manifest = github::latest_manifest()?;
    let version = Version::parse(&manifest.version)
        .map_err(|e| UpdateError::BadVersion(manifest.version.clone(), e))?;
    let current =
        Version::parse(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION is always semver");
    if version <= current {
        return Ok(None);
    }
    let installer = github::download_installer(&manifest)?;
    Ok(Some(Update { version, installer }))
}
