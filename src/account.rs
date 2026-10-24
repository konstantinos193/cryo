//! The Google session behind the chrome's account button: who — if
//! anyone — the profile's cookie jar signs pages in as. Detection asks
//! `accounts.google.com/ListAccounts`, the same endpoint the Chromium
//! family polls for its own profile button, so the answer matches what
//! the sites themselves see: a SID cookie alone can outlive the session
//! it stood for, but ListAccounts only names live accounts.
//!
//! This is a top-level context, not `shell`: HTTP, endpoint knowledge
//! and foreign-wire parsing are session plumbing, not window UI — the
//! chrome button is only the surface that displays [`Session`].
//!
//! `fetch` is blocking — it runs on a worker, never the event loop.

use std::time::Duration;

use crate::engine::Cookie;

/// The URI the jar is read for: `accounts.google.com`, so the cookies
/// that matter (`SID` and friends) come back — it's also the host
/// `fetch` talks to, so the same set is exactly what gets sent.
pub const JAR_URL: &str = "https://accounts.google.com";

/// `ListAccounts`, the endpoint Chromium's own account button reads.
/// `source=ChromiumBrowser` is what real browsers send and the answer
/// doesn't depend on it; `json=standard` picks the parseable reply.
const LIST_ACCOUNTS_URL: &str =
    "https://accounts.google.com/ListAccounts?source=ChromiumBrowser&json=standard";

/// Who the profile is signed in as — `None` means signed out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// Display name, or the email when Google sends no name.
    pub name: String,
    pub email: String,
    /// Account picture as a `data:` URL — `None` makes the UI draw the
    /// name's initial instead.
    pub avatar: Option<String>,
}

/// Read the session out of a profile jar. `None` when the jar holds no
/// live sign-in or the question can't be asked (offline, engine
/// unreachable) — a dead check looks like signed-out rather than a
/// stale "signed in".
pub fn fetch(cookies: &[Cookie]) -> Option<Session> {
    // `SID` is the canary Google plants on sign-in; without it the round
    // trip can't name an account, so don't make it.
    if !cookies.iter().any(|c| c.name == "SID") {
        return None;
    }
    let header = cookies
        .iter()
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ");
    let mut response = agent()
        .get(LIST_ACCOUNTS_URL)
        .header("Cookie", &header)
        .call()
        .ok()?;
    let body = response.body_mut().read_to_string().ok()?;
    let (name, email, photo) = parse_list_accounts(&body)?;
    let avatar = photo.as_deref().and_then(fetch_avatar);
    Some(Session {
        name,
        email,
        avatar,
    })
}

/// Whether `url` sits on a Google property — sign-ins, sign-outs and
/// OAuth hops all route through `accounts.google.com`, so a navigation
/// under `google.com` is the signal the session may have changed.
pub fn is_google_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = rest.split(['/', '?', '#', ':']).next().unwrap_or("");
    host == "google.com" || host.ends_with(".google.com")
}

/// The ListAccounts reply: `)]}'` followed by
/// `["gaia.l.a.r", [], [[<id>, <email>, <name>, <given>, ...], ...]]`.
/// Field positions are Google's wire and have drifted before, so the
/// parse picks by shape — email is the first `@` string, name the
/// first plain word string, photo any googleusercontent URL — rather
/// than by index.
fn parse_list_accounts(body: &str) -> Option<(String, String, Option<String>)> {
    let json = &body[body.find('[')?..];
    let root: serde_json::Value = serde_json::from_str(json).ok()?;
    let account = root.get(2)?.as_array()?.first()?.as_array()?;
    let strings = || account.iter().filter_map(|v| v.as_str());
    let email = strings().find(|s| s.contains('@'))?.to_string();
    let name = strings()
        .find(|s| {
            !s.contains('@')
                && !s.starts_with("http")
                && !s.chars().all(|c| c.is_ascii_digit() || c == '.')
                && !s.is_empty()
        })
        .unwrap_or(email.as_str())
        .to_string();
    let photo = strings()
        .find(|s| s.starts_with("https://") && s.contains("googleusercontent.com"))
        .map(str::to_string);
    Some((name, email, photo))
}

/// Download the account picture, capped so a hostile URL can't stream
/// the web into memory — `None` degrades to the initial-letter circle.
/// The cap is the product rule made literal: the button paints a 32px
/// circle, and a `data:` URL ~4/3 the bytes sits in [`Session`] for the
/// window's life — kilobytes of avatar, not a third of a megabyte.
fn fetch_avatar(url: &str) -> Option<String> {
    let mut response = agent().get(url).call().ok()?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(32 * 1024)
        .read_to_vec()
        .ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some(format!("data:image/jpeg;base64,{}", crate::base64(&bytes)))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent(concat!("cryo/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_names_the_first_account() {
        let body = ")]}'\n[\"gaia.l.a.r\", [], \
                    [[\"117\", \"konst@example.com\", \"Konst\", \"K\", 1, 0, 0, \
                    \"https://lh3.googleusercontent.com/a/abc=s64\"]]]";
        let (name, email, photo) = parse_list_accounts(body).unwrap();
        assert_eq!(name, "Konst");
        assert_eq!(email, "konst@example.com");
        assert_eq!(
            photo.as_deref(),
            Some("https://lh3.googleusercontent.com/a/abc=s64")
        );
    }

    #[test]
    fn a_reply_without_photo_still_names_the_account() {
        let body = "[\"gaia.l.a.r\", [], [[\"117\", \"a@b.com\", \"A B\", \"A\", 1, 0, 0]]]";
        let (name, email, photo) = parse_list_accounts(body).unwrap();
        assert_eq!((name, email, photo), ("A B".into(), "a@b.com".into(), None));
    }

    #[test]
    fn a_missing_name_falls_back_to_the_email() {
        let body = "[\"gaia.l.a.r\", [], [[\"117\", \"a@b.com\"]]]";
        let (name, email, _) = parse_list_accounts(body).unwrap();
        assert_eq!(name, "a@b.com");
        assert_eq!(email, "a@b.com");
    }

    #[test]
    fn an_empty_account_list_is_no_session() {
        assert!(parse_list_accounts("[\"gaia.l.a.r\", [], []]").is_none());
        assert!(parse_list_accounts("not json").is_none());
    }

    #[test]
    fn google_urls_cover_sign_in_and_sign_out_hosts() {
        for url in [
            "https://accounts.google.com/signin",
            "https://myaccount.google.com/",
            "https://google.com",
            "https://www.google.com/search?q=x",
            "http://google.com",
        ] {
            assert!(is_google_url(url), "{url}");
        }
        for url in [
            "https://reddit.com",
            "https://google.com.evil.example",
            "https://example.com/google.com",
        ] {
            assert!(!is_google_url(url), "{url}");
        }
    }
}
