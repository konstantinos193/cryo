//! GitHub Releases adapter: fetches `latest.json` and the installer it
//! names. `releases/latest/download/<asset>` always resolves to the
//! newest tag, so the feed URL never changes; the manifest itself pins
//! the exact tag URL for the installer it describes.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::{ReleaseManifest, UpdateError};

const MANIFEST_URL: &str =
    "https://github.com/konstantinos193/cryo/releases/latest/download/latest.json";

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .user_agent(concat!("cryo/", env!("CARGO_PKG_VERSION")))
        .build()
}

pub fn latest_manifest() -> Result<ReleaseManifest, UpdateError> {
    let body = agent()
        .get(MANIFEST_URL)
        .call()
        .map_err(|e| UpdateError::FetchManifest(e.to_string()))?
        .into_string()
        .map_err(|e| UpdateError::FetchManifest(e.to_string()))?;
    serde_json::from_str(&body).map_err(|e| UpdateError::ParseManifest(e.to_string()))
}

/// Download the installer the manifest names into `%TEMP%`, then verify
/// its sha256 against the manifest. A file already sitting there with a
/// matching hash is reused — a re-check never downloads twice.
pub fn download_installer(manifest: &ReleaseManifest) -> Result<PathBuf, UpdateError> {
    let path = std::env::temp_dir().join(format!("cryo-update-{}.exe", manifest.version));
    if path.exists() && sha256_hex(&path).is_ok_and(|h| h == manifest.sha256) {
        return Ok(path);
    }

    let response = agent()
        .get(&manifest.installer_url)
        .call()
        .map_err(|e| UpdateError::Download(e.to_string()))?;
    let mut reader = response.into_reader();
    let mut file = std::fs::File::create(&path).map_err(UpdateError::WriteInstaller)?;
    std::io::copy(&mut reader, &mut file).map_err(UpdateError::WriteInstaller)?;
    file.flush().map_err(UpdateError::WriteInstaller)?;
    drop(file);

    if sha256_hex(&path)? != manifest.sha256 {
        // A corrupt download must not survive to a re-check — the delete
        // is best-effort, but a failure gets a note, not silence.
        if let Err(e) = std::fs::remove_file(&path) {
            crate::report!("could not remove bad installer {}: {e}", path.display());
        }
        return Err(UpdateError::Checksum);
    }
    Ok(path)
}

fn sha256_hex(path: &std::path::Path) -> Result<String, UpdateError> {
    let bytes = std::fs::read(path).map_err(UpdateError::VerifyInstaller)?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}
