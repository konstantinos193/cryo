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

/// `ListAccounts`, the endpoint Chromium's own account button reads —
/// with the same parameters it sends (`gaia_urls.cc`). `gpsia=1` isn't
/// optional, and neither is the shape: a bare GET gets a 400 — the
/// request is a form POST with a `www.google.com` Origin
/// (`GaiaAuthFetcher::StartListAccounts`). `laf=b64bin` is the only
/// format still served; the reply is a base64 `ListAccountsResponse`
/// protobuf, not the `)]}'` JSON it used to be.
const LIST_ACCOUNTS_URL: &str =
    "https://accounts.google.com/ListAccounts?gpsia=1&source=ChromiumBrowser&json=standard&laf=b64bin";

/// One signed-in Google identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    /// Display name, or the email when Google sends no name.
    pub name: String,
    pub email: String,
    /// Account picture as a `data:` URL — `None` makes the UI draw the
    /// name's initial instead.
    pub avatar: Option<String>,
}

/// Who the profile is signed in as — `None` means signed out. A jar
/// can hold several live accounts (Google multi-login); `accounts`
/// keeps them in ListAccounts order, where the first is Google's
/// default — `authuser=0` on every Google property.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub accounts: Vec<Account>,
}

/// Read the session out of a profile jar. `None` when the jar holds no
/// live sign-in or the question can't be asked (offline, engine
/// unreachable) — a dead check looks like signed-out rather than a
/// stale "signed in".
pub fn fetch(cookies: &[Cookie]) -> Option<Session> {
    // `SID` is the canary Google plants on sign-in; without it the round
    // trip can't name an account, so don't make it.
    if !cookies.iter().any(|c| c.name == "SID") {
        crate::report!("account check: jar holds no SID cookie");
        return None;
    }
    let header = cookies
        .iter()
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ");
    let mut response = match agent()
        .post(LIST_ACCOUNTS_URL)
        .header("Cookie", &header)
        .header("Origin", "https://www.google.com")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(" ")
    {
        Ok(r) => r,
        Err(e) => {
            crate::report!("account check: ListAccounts request failed: {e:?}");
            return None;
        }
    };
    let body = match response.body_mut().read_to_string() {
        Ok(b) => b,
        Err(e) => {
            crate::report!("account check: ListAccounts body unreadable: {e:?}");
            return None;
        }
    };
    let accounts = parse_list_accounts(&body);
    if accounts.is_empty() {
        crate::report!("account check: ListAccounts reply named no account: {body:.200}");
        return None;
    }
    let accounts = accounts
        .into_iter()
        .map(|(name, email, photo)| Account {
            name,
            email,
            avatar: photo.as_deref().and_then(fetch_avatar),
        })
        .collect();
    Some(Session { accounts })
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

/// One protobuf field's payload — only the wire types the
/// `ListAccountsResponse` schema uses: varint (0) and
/// length-delimited (2).
enum Field<'a> {
    Var(u64),
    Bytes(&'a [u8]),
}

/// The ListAccounts reply: base64 of a `ListAccountsResponse` proto.
/// Each `account` (field 1, length-delimited) carries the display name
/// (2), email (3) and photo URL (4) as strings, plus `valid_session`
/// (9) and `signed_out` (14) flags — an entry that fails either check
/// is a leftover cookie, not a live session. Chromium's own projection
/// (`list_accounts_response.proto`) reads only a subset; field numbers
/// here come from the wire as actually served. Returns every live
/// account in jar order — multi-login lists them all.
fn parse_list_accounts(body: &str) -> Vec<(String, String, Option<String>)> {
    let mut out = Vec::new();
    let Some(raw) = crate::base64_decode(body.trim()) else {
        return out;
    };
    let Some(fields) = proto_fields(&raw) else {
        return out;
    };
    for field in fields {
        let (1, Field::Bytes(msg)) = field else {
            continue;
        };
        let Some(f) = proto_fields(msg) else { continue };
        let signed_out = var_field(&f, 14) == Some(1);
        let valid = var_field(&f, 9) != Some(0);
        if signed_out || !valid {
            continue;
        }
        let Some(email) = str_field(&f, 3) else {
            continue;
        };
        let name = str_field(&f, 2).filter(|s| !s.is_empty()).unwrap_or(email);
        let photo = str_field(&f, 4)
            .filter(|s| s.starts_with("https://") && s.contains("googleusercontent.com"));
        out.push((
            name.to_string(),
            email.to_string(),
            photo.map(str::to_string),
        ));
    }
    out
}

/// Field `tag`'s string value in a parsed message, if present.
fn str_field<'a>(fields: &[(u64, Field<'a>)], tag: u64) -> Option<&'a str> {
    fields.iter().find_map(|(t, f)| match (t, f) {
        (t, Field::Bytes(b)) if *t == tag => std::str::from_utf8(b).ok(),
        _ => None,
    })
}

/// Field `tag`'s varint value in a parsed message, if present.
fn var_field(fields: &[(u64, Field<'_>)], tag: u64) -> Option<u64> {
    fields.iter().find_map(|(t, f)| match (t, f) {
        (t, Field::Var(v)) if *t == tag => Some(*v),
        _ => None,
    })
}

/// Walk a protobuf buffer into (tag, payload) pairs. `None` on
/// malformed wire data — a truncated varint, a length running past
/// the buffer, or a wire type the schema never carries.
fn proto_fields(buf: &[u8]) -> Option<Vec<(u64, Field<'_>)>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        let key = varint(buf, &mut i)?;
        let field = match key & 7 {
            0 => Field::Var(varint(buf, &mut i)?),
            2 => {
                let len = usize::try_from(varint(buf, &mut i)?).ok()?;
                let bytes = buf.get(i..i.checked_add(len)?)?;
                i += len;
                Field::Bytes(bytes)
            }
            _ => return None,
        };
        out.push((key >> 3, field));
    }
    Some(out)
}

fn varint(buf: &[u8], i: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let b = *buf.get(*i)?;
        *i += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
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

    /// Encode `tag`'s varint field — test-side mirror of the wire the
    /// parser reads.
    fn tvar(tag: u64, v: u64) -> Vec<u8> {
        let mut out = vec![(tag << 3) as u8];
        let mut v = v;
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            out.push(if v == 0 { b } else { b | 0x80 });
            if v == 0 {
                return out;
            }
        }
    }

    /// Encode `tag`'s length-delimited field.
    fn tstr(tag: u64, s: &str) -> Vec<u8> {
        let mut out = vec![(tag << 3 | 2) as u8, s.len() as u8];
        out.extend_from_slice(s.as_bytes());
        out
    }

    /// A `ListAccountsResponse` body: each account proto wrapped in
    /// field 1 and the whole thing base64'd, as the wire serves it.
    fn reply(accounts: &[&[u8]]) -> String {
        let mut raw = Vec::new();
        for a in accounts {
            raw.push(0x0a); // field 1, length-delimited
            raw.push(a.len() as u8);
            raw.extend_from_slice(a);
        }
        crate::base64(&raw)
    }

    #[test]
    fn a_reply_names_the_first_account() {
        let mut account = tvar(1, 1);
        account.extend(tstr(2, "Konst"));
        account.extend(tstr(3, "konst@example.com"));
        account.extend(tstr(4, "https://lh3.googleusercontent.com/a/abc=s64"));
        let accounts = parse_list_accounts(&reply(&[&account]));
        assert_eq!(accounts.len(), 1);
        let (name, email, photo) = &accounts[0];
        assert_eq!(name, "Konst");
        assert_eq!(email, "konst@example.com");
        assert_eq!(
            photo.as_deref(),
            Some("https://lh3.googleusercontent.com/a/abc=s64")
        );
    }

    #[test]
    fn a_reply_without_photo_still_names_the_account() {
        let account = [tstr(2, "A B"), tstr(3, "a@b.com")].concat();
        let accounts = parse_list_accounts(&reply(&[&account]));
        assert_eq!(
            accounts.as_slice(),
            &[("A B".to_string(), "a@b.com".to_string(), None)]
        );
    }

    #[test]
    fn a_missing_name_falls_back_to_the_email() {
        let account = tstr(3, "a@b.com");
        let accounts = parse_list_accounts(&reply(&[&account]));
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].0, "a@b.com");
        assert_eq!(accounts[0].1, "a@b.com");
    }

    #[test]
    fn multi_login_lists_every_live_account() {
        let first = [tstr(2, "A B"), tstr(3, "a@b.com")].concat();
        let second = [tstr(2, "C D"), tstr(3, "c@d.com")].concat();
        let accounts = parse_list_accounts(&reply(&[&first, &second]));
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[1].0, "C D");
        assert_eq!(accounts[1].1, "c@d.com");
    }

    #[test]
    fn a_dead_or_leftover_cookie_is_no_session() {
        // `signed_out` (14) and `valid_session` (9) mark jar leftovers,
        // not live accounts — and the parser falls through to the next
        // entry before giving up.
        let mut dead = tstr(3, "gone@example.com");
        dead.extend(tvar(14, 1));
        let mut expired = tstr(3, "stale@example.com");
        expired.extend(tvar(9, 0));
        let mut live = tstr(3, "live@example.com");
        live.extend(tvar(9, 1));
        let accounts = parse_list_accounts(&reply(&[&dead, &expired, &live]));
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].0, "live@example.com");
    }

    #[test]
    fn an_empty_account_list_is_no_session() {
        assert!(parse_list_accounts(&reply(&[])).is_empty());
        assert!(parse_list_accounts(&reply(&[&tstr(2, "no email")])).is_empty());
        assert!(parse_list_accounts("not proto").is_empty());
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
