//! Recognising a Chrome Web Store install attempt.
//!
//! Clicking "Add to Chrome" makes the store ask the engine for a `.crx`. The
//! WebView2 runtime carries no working endpoint for that: the request goes to
//! `permanently-removed.invalid`, a host under a TLD reserved by RFC 2606 that
//! can never resolve. The navigation therefore fails as "host not found",
//! which is true and useless — the address is a placeholder, and retrying it
//! will fail forever.
//!
//! This module turns such a request back into the one fact worth having: the
//! id of the extension someone was trying to install.

use super::ExtensionError;

/// A Chrome extension id: 32 characters drawn from `a`-`p`, which is how
/// Chromium renders the first 16 bytes of the SHA-256 of the signing key.
/// A newtype because it is about to be used to build a URL and a directory
/// name, and a bare `String` would make both of those trusting (AGENTS §8).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExtensionId(String);

impl ExtensionId {
    /// The only way to make one. Anything that is not exactly 32 `a`-`p`
    /// characters is refused, so an id can be pasted into a path or a query
    /// without further checking.
    pub fn parse(raw: &str) -> Result<Self, ExtensionError> {
        let ok = raw.len() == 32 && raw.bytes().all(|b| (b'a'..=b'p').contains(&b));
        if ok {
            Ok(Self(raw.to_string()))
        } else {
            Err(ExtensionError::Id(raw.to_string()))
        }
    }

    pub fn get(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ExtensionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The placeholder host the runtime substitutes for the store's download
/// endpoint. Matched by name because it is the clearest single signal, but
/// never the only one — see `install_request`.
const REMOVED_HOST: &str = "permanently-removed.invalid";

/// Google's real CRX endpoint — the one the runtime's placeholder stands in
/// for. Asked directly, it answers with a redirect to the package.
const DOWNLOAD_ENDPOINT: &str = "https://clients2.google.com/service/update2/crx";

/// The Chromium version the download is asked for on behalf of. Used only
/// when a request did not carry one of its own; the store varies what it
/// serves by version, so passing through what was asked for is better than
/// inventing a number.
const FALLBACK_PRODVERSION: &str = "155.0.0.0";

/// A recognised store install, with whatever the store told us about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallRequest {
    pub id: ExtensionId,
    /// The Chromium version the store asked on behalf of, when the request
    /// carried one.
    pub prodversion: Option<String>,
}

/// Where to fetch a package from. Built from our own constants and a
/// validated id, so nothing an attacker controls reaches the URL unchecked.
pub fn download_url(request: &InstallRequest) -> String {
    let prodversion = request
        .prodversion
        .as_deref()
        .filter(|v| v.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
        .unwrap_or(FALLBACK_PRODVERSION);
    format!(
        "{DOWNLOAD_ENDPOINT}?response=redirect&acceptformat=crx3&prodversion={prodversion}\
         &x=id%3D{}%26installsource%3Dondemand%26uc",
        request.id
    )
}

/// Read a store address or a bare id typed into the Extensions pane. Accepts
/// what a person actually has to hand — the page URL they were looking at, or
/// the id out of it — and nothing else.
pub fn parse_input(input: &str) -> Result<InstallRequest, ExtensionError> {
    let trimmed = input.trim();
    if let Ok(id) = ExtensionId::parse(trimmed) {
        return Ok(InstallRequest {
            id,
            prodversion: None,
        });
    }
    if let Some(found) = install_request(trimmed) {
        return Ok(found);
    }
    // A store detail page: the id is the last path segment that is one.
    if let Ok(parsed) = url::Url::parse(trimmed) {
        let host_ok = parsed.host_str().is_some_and(|h| {
            h.ends_with("chromewebstore.google.com") || h.ends_with("chrome.google.com")
        });
        if host_ok {
            if let Some(segments) = parsed.path_segments() {
                for segment in segments.rev() {
                    if let Ok(id) = ExtensionId::parse(segment) {
                        return Ok(InstallRequest {
                            id,
                            prodversion: None,
                        });
                    }
                }
            }
        }
    }
    Err(ExtensionError::Id(trimmed.to_string()))
}

/// The extension id a failed navigation was trying to install, if that is
/// what it was.
///
/// Two independent signals, either of which is enough, because the host is
/// Microsoft's placeholder and could change while the query stays the same:
/// the placeholder host itself, or a query that declares a CRX download
/// (`prod=chromiumcrx`, or an `acceptformat` naming crx). Both still require
/// an `x=id=<id>` parameter, since without an id there is nothing to say.
pub fn install_request(url: &str) -> Option<InstallRequest> {
    let parsed = url::Url::parse(url).ok()?;
    let mut looks_like_crx = parsed.host_str() == Some(REMOVED_HOST);
    let mut id = None;
    let mut prodversion = None;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "prod" if value == "chromiumcrx" => looks_like_crx = true,
            "prodversion" if !value.is_empty() => prodversion = Some(value.to_string()),
            "acceptformat" if value.split(',').any(|f| f.trim() == "crx3") => {
                looks_like_crx = true;
            }
            // `x` is itself a urlencoded parameter string: `id=<id>&uc` and
            // friends. The outer decode already ran, so split it plainly.
            "x" => {
                for part in value.split('&') {
                    if let Some(raw) = part.strip_prefix("id=") {
                        id = ExtensionId::parse(raw).ok();
                    }
                }
            }
            _ => {}
        }
    }
    let id = looks_like_crx.then_some(id).flatten()?;
    Some(InstallRequest { id, prodversion })
}

/// Fetch and verify a package on a worker thread. Network work never runs on
/// the event loop (AGENTS §9); the result comes back through the callback,
/// which the app turns into an `AppEvent`.
pub fn spawn_fetch(
    request: InstallRequest,
    progress: impl Fn(u64, Option<u64>) + Send + 'static,
    done: impl FnOnce(Result<crate::extensions::crx::VerifiedCrx, ExtensionError>) + Send + 'static,
) {
    let spawned = std::thread::Builder::new()
        .name("cryo-extension-fetch".into())
        .spawn(move || done(fetch(&request, &progress)));
    if let Err(e) = spawned {
        crate::report!("could not start the extension fetch: {e}");
    }
}

/// How often the worker reports progress. A 25 MiB package in 256 KiB steps
/// is about a hundred messages — enough for a bar that moves, few enough that
/// the event loop is not doing nothing else.
const PROGRESS_STEP: u64 = 256 * 1024;

/// One package, fetched and checked. The size cap is here rather than at the
/// caller because this is the only place that reads from the network: without
/// it, a hostile or broken endpoint decides how much memory cryo allocates.
fn fetch(
    request: &InstallRequest,
    progress: &(impl Fn(u64, Option<u64>) + ?Sized),
) -> Result<crate::extensions::crx::VerifiedCrx, ExtensionError> {
    use std::io::Read;

    const MAX_PACKAGE: u64 = 128 * 1024 * 1024;

    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(15)))
        .user_agent(concat!("cryo/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent();
    let response = agent
        .get(download_url(request))
        .call()
        .map_err(|e| ExtensionError::Fetch(e.to_string()))?;
    // The store does send a length; where it does not, the bar runs
    // indeterminate rather than inventing a total.
    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&n| n > 0 && n <= MAX_PACKAGE);

    // Read in steps so the prompt can show movement. A 25 MiB extension over
    // a slow line is twenty seconds of nothing otherwise, which reads as a
    // hang however honest the spinner.
    let mut reader = response.into_body().into_reader().take(MAX_PACKAGE + 1);
    let mut bytes = Vec::with_capacity(total.unwrap_or(4 * 1024 * 1024) as usize);
    let mut buffer = [0u8; 64 * 1024];
    let mut announced = 0u64;
    progress(0, total);
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| ExtensionError::Fetch(e.to_string()))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        let received = bytes.len() as u64;
        if received - announced >= PROGRESS_STEP {
            announced = received;
            progress(received, total);
        }
    }
    if bytes.len() as u64 > MAX_PACKAGE {
        return Err(ExtensionError::Fetch(
            "the package is larger than 128 MiB".into(),
        ));
    }
    // The last step lands on the real total, so the bar does not stop at 98%
    // while the signature is being checked.
    progress(bytes.len() as u64, total);

    let verified = crate::extensions::crx::verify(&bytes)?;
    // The store is asked for one extension; a package that says it is another
    // is refused rather than installed under the name that was requested.
    if verified.id() != &request.id {
        return Err(ExtensionError::Fetch(format!(
            "asked for {} and got a package signed as {}",
            request.id,
            verified.id()
        )));
    }
    Ok(verified)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real address cryo was sent to when "Add to Chrome" was clicked on
    /// MetaMask's store page, trimmed of nothing.
    const METAMASK: &str = "https://permanently-removed.invalid/?response=redirect&os=win&arch=x64&prod=chromiumcrx&prodchannel=&prodversion=155.0.4283.62&lang=en-US&acceptformat=crx3,puff&x=id%3Dnkbihfbeogaeaoehlefnkodbefgpgknn%26installsource%3Dondemand%26uc&authuser=0";

    #[test]
    fn the_real_store_install_url_yields_the_extension_id() {
        let id = install_request(METAMASK).expect("recognised");
        assert_eq!(id.id.get(), "nkbihfbeogaeaoehlefnkodbefgpgknn");
    }

    #[test]
    fn the_query_alone_is_enough_when_the_placeholder_host_changes() {
        let moved = METAMASK.replace(REMOVED_HOST, "some-other-placeholder.example");
        assert_eq!(
            install_request(&moved).map(|i| i.id.get().to_string()),
            Some("nkbihfbeogaeaoehlefnkodbefgpgknn".to_string())
        );
        // Google's own endpoint, which is what a fetch would really use.
        let real = "https://clients2.google.com/service/update2/crx?response=redirect&acceptformat=crx3&prodversion=155&x=id%3Dnkbihfbeogaeaoehlefnkodbefgpgknn%26uc";
        assert!(install_request(real).is_some());
    }

    #[test]
    fn an_ordinary_failed_address_is_not_an_install() {
        for url in [
            "https://example.com/",
            "https://permanently-removed.invalid/",
            "https://chromewebstore.google.com/detail/metamask/nkbihfbeogaeaoehlefnkodbefgpgknn",
            "not a url at all",
        ] {
            assert_eq!(install_request(url), None, "{url}");
        }
    }

    #[test]
    fn a_crx_request_naming_no_id_says_nothing() {
        let no_id = "https://permanently-removed.invalid/?prod=chromiumcrx&acceptformat=crx3";
        assert_eq!(install_request(no_id), None);
    }

    #[test]
    fn an_id_that_is_not_an_id_is_refused_rather_than_carried() {
        for raw in [
            "",
            "short",
            // 32 characters, but `z` is outside the a-p alphabet.
            "zkbihfbeogaeaoehlefnkodbefgpgknn",
            // Right alphabet, wrong length.
            "nkbihfbeogaeaoehlefnkodbefgpgkn",
            "../../../etc/passwd",
            "NKBIHFBEOGAEAOEHLEFNKODBEFGPGKNN",
        ] {
            assert!(ExtensionId::parse(raw).is_err(), "{raw}");
        }
        assert!(ExtensionId::parse("nkbihfbeogaeaoehlefnkodbefgpgknn").is_ok());
    }

    #[test]
    fn the_download_url_carries_the_id_and_the_version_that_was_asked_for() {
        let request = install_request(METAMASK).expect("recognised");
        assert_eq!(request.prodversion.as_deref(), Some("155.0.4283.62"));
        let url = download_url(&request);
        assert!(url.starts_with("https://clients2.google.com/service/update2/crx?"));
        assert!(url.contains("acceptformat=crx3"));
        assert!(url.contains("prodversion=155.0.4283.62"));
        assert!(url.contains("x=id%3Dnkbihfbeogaeaoehlefnkodbefgpgknn%26"));
    }

    #[test]
    fn a_prodversion_that_is_not_a_version_is_replaced_rather_than_passed_on() {
        // Whatever the store said, only digits and dots reach the URL.
        let request = InstallRequest {
            id: ExtensionId::parse("nkbihfbeogaeaoehlefnkodbefgpgknn").unwrap(),
            prodversion: Some("155 & rm -rf".into()),
        };
        let url = download_url(&request);
        assert!(url.contains(&format!("prodversion={FALLBACK_PRODVERSION}")));
        assert!(!url.contains("rm -rf"));
    }

    #[test]
    fn the_pane_accepts_an_id_a_store_page_or_the_failed_install_address() {
        let expected = "nkbihfbeogaeaoehlefnkodbefgpgknn";
        for input in [
            "nkbihfbeogaeaoehlefnkodbefgpgknn",
            "  nkbihfbeogaeaoehlefnkodbefgpgknn  ",
            "https://chromewebstore.google.com/detail/metamask/nkbihfbeogaeaoehlefnkodbefgpgknn",
            "https://chromewebstore.google.com/detail/metamask/nkbihfbeogaeaoehlefnkodbefgpgknn?hl=en",
            "https://chrome.google.com/webstore/detail/metamask/nkbihfbeogaeaoehlefnkodbefgpgknn",
            METAMASK,
        ] {
            let request = parse_input(input).unwrap_or_else(|_| panic!("accepts {input}"));
            assert_eq!(request.id.get(), expected, "{input}");
        }
    }

    #[test]
    fn the_pane_refuses_anything_that_does_not_name_an_extension() {
        for input in [
            "",
            "   ",
            "metamask",
            "https://example.com/",
            // An id-shaped segment on somebody else's host is not a store page.
            "https://evil.example/detail/x/nkbihfbeogaeaoehlefnkodbefgpgknn",
            "https://chromewebstore.google.com/detail/metamask",
        ] {
            assert!(parse_input(input).is_err(), "{input:?}");
        }
    }

    #[test]
    fn an_install_url_carrying_a_bad_id_is_not_treated_as_an_install() {
        let crafted =
            "https://permanently-removed.invalid/?prod=chromiumcrx&x=id%3D..%252Fescape%26uc";
        assert_eq!(install_request(crafted), None);
    }

    /// The real thing: ask Google for MetaMask and check the signature with
    /// the real CNG path. Ignored because it needs the network, same as the
    /// live probes — run with
    /// `cargo test real_web_store_package -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_web_store_package_verifies_and_a_tampered_one_does_not() {
        let request = parse_input("nkbihfbeogaeaoehlefnkodbefgpgknn").expect("id");
        let package = fetch(&request, &|_, _| {}).expect("fetched and verified");
        assert_eq!(package.id(), &request.id);
        let archive = package.archive().to_vec();
        assert!(archive.starts_with(b"PK"), "the archive is a zip");
        println!(
            "WEB_STORE_PROBE id={} archive={} bytes",
            package.id(),
            archive.len()
        );

        // And it unpacks into something the catalogue will own.
        let root = std::env::temp_dir().join(format!("cryo-store-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let catalogue = crate::extensions::Catalogue::new(root.clone());
        let installed = catalogue.install_package(&package).expect("unpacks");
        println!(
            "WEB_STORE_PROBE installed {} {} (manifest v{}) at {}",
            installed.name(),
            installed.version(),
            installed.manifest_version(),
            installed.dir().display()
        );
        assert_eq!(installed.slug(), request.id.get());
        assert!(installed.dir().join("manifest.json").is_file());
        assert_eq!(catalogue.list().expect("lists").len(), 1);
        let _ = std::fs::remove_dir_all(&root);

        // Flip one byte of the archive and the signature must stop matching.
        // Fetched once and tampered locally, so this costs no second request.
        let mut bytes = Vec::new();
        let agent = ureq::Agent::config_builder().build().new_agent();
        let response = agent
            .get(download_url(&request))
            .call()
            .expect("second get");
        use std::io::Read;
        response
            .into_body()
            .into_reader()
            .read_to_end(&mut bytes)
            .expect("read");
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        let refused = crate::extensions::crx::verify(&bytes)
            .unwrap_err()
            .to_string();
        println!("WEB_STORE_PROBE tampered rejected: {refused}");
        assert!(refused.contains("no signature from the key"), "{refused}");
    }
}
