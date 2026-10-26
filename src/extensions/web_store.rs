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

/// The extension id a failed navigation was trying to install, if that is
/// what it was.
///
/// Two independent signals, either of which is enough, because the host is
/// Microsoft's placeholder and could change while the query stays the same:
/// the placeholder host itself, or a query that declares a CRX download
/// (`prod=chromiumcrx`, or an `acceptformat` naming crx). Both still require
/// an `x=id=<id>` parameter, since without an id there is nothing to say.
pub fn install_request(url: &str) -> Option<ExtensionId> {
    let parsed = url::Url::parse(url).ok()?;
    let mut looks_like_crx = parsed.host_str() == Some(REMOVED_HOST);
    let mut id = None;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "prod" if value == "chromiumcrx" => looks_like_crx = true,
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
    looks_like_crx.then_some(id).flatten()
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
        assert_eq!(id.get(), "nkbihfbeogaeaoehlefnkodbefgpgknn");
    }

    #[test]
    fn the_query_alone_is_enough_when_the_placeholder_host_changes() {
        let moved = METAMASK.replace(REMOVED_HOST, "some-other-placeholder.example");
        assert_eq!(
            install_request(&moved).map(|i| i.get().to_string()),
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
    fn an_install_url_carrying_a_bad_id_is_not_treated_as_an_install() {
        let crafted =
            "https://permanently-removed.invalid/?prod=chromiumcrx&x=id%3D..%252Fescape%26uc";
        assert_eq!(install_request(crafted), None);
    }
}
