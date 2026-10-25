//! Password-fill policy: canonical web origins and credential selection.
//!
//! DOM observation and engine messages are adapters in `app`; this module
//! only decides whether a saved sign-in belongs to a web page.

use url::Url;

use super::{Credential, Credentials};

/// A web security origin suitable for password matching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteOrigin(String);

impl SiteOrigin {
    pub fn parse(input: &str) -> Option<Self> {
        let url = Url::parse(input).ok()?;
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            return None;
        }
        Some(Self(url.origin().ascii_serialization()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub(super) fn for_page<'a>(credentials: &'a Credentials, page: &SiteOrigin) -> Vec<&'a Credential> {
    credentials
        .rows()
        .iter()
        .filter(|credential| SiteOrigin::parse(credential.origin()).as_ref() == Some(page))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{Draft, Saved};

    fn credential(origin: &str, username: &str) -> Credentials {
        let mut credentials = Credentials::default();
        let saved = credentials
            .add(&Draft {
                origin: origin.into(),
                username: username.into(),
                secret: "secret".into(),
                note: String::new(),
                updated_at: 1,
            })
            .unwrap();
        assert_eq!(saved, Saved::Created);
        credentials
    }

    #[test]
    fn paths_share_their_web_origin() {
        let rows = credential("https://example.test/login", "ann");
        let page = SiteOrigin::parse("https://example.test/account?q=1").unwrap();
        assert_eq!(for_page(&rows, &page).len(), 1);
    }

    #[test]
    fn scheme_host_and_port_are_security_boundaries() {
        let rows = credential("https://example.test", "ann");
        for other in [
            "http://example.test",
            "https://login.example.test",
            "https://example.test.evil.invalid",
            "https://example.test:444",
        ] {
            let page = SiteOrigin::parse(other).unwrap();
            assert!(for_page(&rows, &page).is_empty(), "matched {other}");
        }
    }

    #[test]
    fn default_ports_and_host_case_canonicalize() {
        let rows = credential("https://EXAMPLE.test:443/login", "ann");
        let page = SiteOrigin::parse("https://example.test").unwrap();
        assert_eq!(for_page(&rows, &page).len(), 1);
    }

    #[test]
    fn non_web_origins_never_fill() {
        for value in [
            "about:blank",
            "file:///tmp/login.html",
            "data:text/html,x",
            "android://hash@app.example",
        ] {
            assert!(SiteOrigin::parse(value).is_none(), "accepted {value}");
        }
    }
}
