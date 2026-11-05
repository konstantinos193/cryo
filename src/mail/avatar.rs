//! Sender pictures: who an address looks like.
//!
//! Mail carries no picture of its sender, so a client has to find one,
//! the way eM Client does. In order: the picture the reader chose for
//! this address; Gravatar, for the person (by a SHA-256 of the address,
//! the only form Gravatar ever sees); then the organization's icon, from
//! its own website (`site_icon`) or failing that DuckDuckGo's icon
//! service. `None` leaves the page's initial-letter disc in place.
//!
//! Only the reader's choice is theirs to keep; the rest is a cache. Every
//! answer lands in the store, a miss included, so a list of two hundred
//! senders costs its network once, then nothing for a fortnight. Icons
//! are cached per registered name, not per address: a sender's website
//! hears from the reader at most once a fortnight however much they
//! send, which keeps this from working as a read receipt.

use std::time::Duration;

use sha2::{Digest, Sha256};

use super::site_icon::{self, Fetch};
use super::store::FileStore;
use super::MailError;

/// A disc of 64px needs kilobytes; a larger answer isn't a small picture.
const MAX_BYTES: u64 = 128 * 1024;

/// A picture the reader chose has been shrunk by the page; past this it
/// didn't come from there.
const MAX_CHOSEN_BYTES: usize = 256 * 1024;

/// Lookups run between the worker's real jobs, so one slow sender holds
/// an opening message up by at most this long.
const BUDGET: Duration = Duration::from_secs(6);

/// Domains shared by strangers — their logo says nothing about the
/// person, and every row wearing the same "G" is worse than a letter.
const SHARED_DOMAINS: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "outlook.com",
    "hotmail.com",
    "hotmail.gr",
    "live.com",
    "msn.com",
    "yahoo.com",
    "yahoo.gr",
    "ymail.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
    "gmx.com",
    "gmx.net",
    "mail.com",
    "zoho.com",
    "otenet.gr",
    "windowslive.com",
    "cosmotemail.gr",
    "hol.gr",
    "forthnet.gr",
];

/// A picture for an address, ready for the page.
pub struct Found {
    /// A `data:` URL.
    pub src: String,
    /// The reader's own choice, which stands even with lookups off.
    pub chosen: bool,
}

/// The picture for `addr`. `lookup` is the reader's permission to ask
/// anyone; without it only their own choice can answer.
pub(super) fn look_up(store: &FileStore, addr: &str, lookup: bool) -> Option<Found> {
    let addr = addr.trim().to_ascii_lowercase();
    let domain = domain_of(&addr)?;
    let key = hash(&addr);
    if let Some(bytes) = store.load_chosen(&key) {
        return Some(Found {
            src: data_url(&bytes)?,
            chosen: true,
        });
    }
    if !lookup {
        return None;
    }
    let fetch = Fetch::within(BUDGET);
    let bytes = cached(store, &key, || gravatar(&fetch, &key));
    let bytes = if bytes.is_empty() {
        let site = site_icon::registrable(domain);
        if SHARED_DOMAINS.contains(&site) {
            return None;
        }
        cached(store, &hash(&format!("site:{site}")), || {
            organization_icon(&fetch, site)
        })
    } else {
        bytes
    };
    Some(Found {
        src: data_url(&bytes)?,
        chosen: false,
    })
}

/// Set or, with an empty `photo`, clear the picture the reader chose for
/// `addr`. Takes the page's `data:` URL and keeps the bytes it holds.
pub(super) fn choose(store: &FileStore, addr: &str, photo: &str) -> Result<(), MailError> {
    let addr = addr.trim().to_ascii_lowercase();
    if domain_of(&addr).is_none() {
        return Err(MailError::Invalid(format!("{addr} is not an address")));
    }
    let key = hash(&addr);
    if photo.is_empty() {
        return store.clear_chosen(&key);
    }
    store.save_chosen(&key, &decode_picture(photo)?)
}

fn decode_picture(photo: &str) -> Result<Vec<u8>, MailError> {
    use base64::Engine;
    let refused = || MailError::Invalid("that picture can't be used".into());
    let payload = photo
        .strip_prefix("data:image/")
        .and_then(|rest| rest.split_once(";base64,"))
        .map(|(_, payload)| payload)
        .ok_or_else(refused)?;
    if payload.len() > MAX_CHOSEN_BYTES {
        return Err(refused());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|_| refused())?;
    sniff(&bytes).ok_or_else(refused)?;
    Ok(bytes)
}

/// The store's answer, or `ask`'s, remembered. An empty `Vec` is "nobody
/// has one".
fn cached(store: &FileStore, key: &str, ask: impl FnOnce() -> Option<Vec<u8>>) -> Vec<u8> {
    if let Some(bytes) = store.load_face(key) {
        return bytes;
    }
    let bytes = ask().unwrap_or_default();
    if let Err(e) = store.save_face(key, &bytes) {
        crate::report!("mail face: {e}");
    }
    bytes
}

fn data_url(bytes: &[u8]) -> Option<String> {
    let mime = sniff(bytes)?;
    Some(format!("data:{mime};base64,{}", crate::base64(bytes)))
}

/// The address's domain, when it is safe to put in a URL. Addresses come
/// from mail headers, which anyone can write.
fn domain_of(addr: &str) -> Option<&str> {
    let (local, domain) = addr.rsplit_once('@')?;
    let plausible = !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with(['.', '-'])
        && !domain.contains("..")
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    plausible.then_some(domain)
}

fn hash(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn image(fetch: &Fetch, url: &str) -> Option<Vec<u8>> {
    let bytes = fetch.image(url, MAX_BYTES)?;
    sniff(&bytes).map(|_| bytes)
}

/// `d=404` asks for a refusal rather than a generated placeholder.
fn gravatar(fetch: &Fetch, key: &str) -> Option<Vec<u8>> {
    image(
        fetch,
        &format!("https://gravatar.com/avatar/{key}?s=96&d=404"),
    )
}

fn organization_icon(fetch: &Fetch, site: &str) -> Option<Vec<u8>> {
    site_icon::candidates(fetch, site)
        .iter()
        .find_map(|url| image(fetch, url))
        .or_else(|| {
            image(
                fetch,
                &format!("https://icons.duckduckgo.com/ip3/{site}.ico"),
            )
        })
}

/// What the bytes really are — never what the server claimed. Raster
/// formats only: this ends up in a `data:` URL on a page holding the
/// mailbox's `window.ipc`.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        [0, 0, 1, 0, ..] => Some("image/x-icon"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    const PNG: &[u8] = b"\x89PNGbytes";

    fn png_url() -> String {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(PNG)
        )
    }

    /// Remembered misses for an address and its site: enough that a
    /// lookup answers without the network.
    fn seed_miss(store: &FileStore, addr: &str, site: &str) {
        store.save_face(&hash(addr), b"").unwrap();
        store
            .save_face(&hash(&format!("site:{site}")), b"")
            .unwrap();
    }

    #[test]
    fn only_plausible_addresses_reach_a_url() {
        assert_eq!(domain_of("ann@a.test"), Some("a.test"));
        for hostile in [
            "ann",
            "@a.test",
            "ann@localhost",
            "ann@a.test/../x",
            "ann@a.test?x=1",
            "ann@a b.test",
            "ann@.a.test",
            "ann@a..test",
            "ann@-a.test",
        ] {
            assert_eq!(domain_of(hostile), None, "{hostile}");
        }
    }

    #[test]
    fn the_hash_is_gravatars() {
        // Gravatar's documented SHA-256 of the trimmed, lower-cased address.
        assert_eq!(
            hash("test@example.com"),
            "973dfe463ec85785f5f95af5ba3906eedb2d931c24e69824a89ea65dba4e813b"
        );
    }

    #[test]
    fn only_raster_images_pass() {
        assert_eq!(sniff(b"\x89PNG\r\n"), Some("image/png"));
        assert_eq!(sniff(b"\xFF\xD8\xFF\xE0"), Some("image/jpeg"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8"), Some("image/webp"));
        assert_eq!(sniff(b"<svg onload=alert(1)>"), None);
        assert_eq!(sniff(b"<html>"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn a_remembered_answer_needs_no_network() {
        let dir = crate::test_dir("mail-avatar");
        let store = FileStore::new(dir.clone());
        store.save_face(&hash("ann@a.test"), PNG).unwrap();
        seed_miss(&store, "bob@a.test", "a.test");
        let ann = look_up(&store, " Ann@A.test ", true).unwrap();
        assert!(ann.src.starts_with("data:image/png;base64,"));
        assert!(!ann.chosen);
        assert!(look_up(&store, "bob@a.test", true).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn colleagues_share_one_cached_icon() {
        let dir = crate::test_dir("mail-avatar-site");
        let store = FileStore::new(dir.clone());
        store.save_face(&hash("bob@mail.a.test"), b"").unwrap();
        store.save_face(&hash("site:a.test"), PNG).unwrap();
        assert!(look_up(&store, "bob@mail.a.test", true).is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_shared_domain_never_lends_its_logo() {
        let dir = crate::test_dir("mail-avatar-shared");
        let store = FileStore::new(dir.clone());
        store.save_face(&hash("ann@gmail.com"), b"").unwrap();
        store.save_face(&hash("bob@help.yahoo.com"), b"").unwrap();
        // Would be returned if consulted.
        store.save_face(&hash("site:gmail.com"), PNG).unwrap();
        store.save_face(&hash("site:yahoo.com"), PNG).unwrap();
        assert!(look_up(&store, "ann@gmail.com", true).is_none());
        assert!(look_up(&store, "bob@help.yahoo.com", true).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_readers_choice_beats_everything_and_needs_no_permission() {
        let dir = crate::test_dir("mail-avatar-chosen");
        let store = FileStore::new(dir.clone());
        let addr = "ann@a.test";
        store.save_face(&hash(addr), b"\xFF\xD8\xFFlookup").unwrap();
        assert!(look_up(&store, addr, false).is_none(), "lookups are off");
        choose(&store, addr, &png_url()).unwrap();
        for lookup in [true, false] {
            let found = look_up(&store, addr, lookup).unwrap();
            assert!(found.chosen);
            assert_eq!(found.src, png_url());
        }
        choose(&store, addr, "").unwrap();
        let back = look_up(&store, addr, true).unwrap();
        assert!(!back.chosen, "clearing falls back to the lookup");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_chosen_picture_must_really_be_one() {
        let dir = crate::test_dir("mail-avatar-refuse");
        let store = FileStore::new(dir.clone());
        let svg = base64::engine::general_purpose::STANDARD.encode("<svg onload=alert(1)>");
        for bad in [
            "javascript:alert(1)".to_string(),
            "data:text/html;base64,AAAA".to_string(),
            format!("data:image/svg+xml;base64,{svg}"),
            "data:image/png;base64,!!!".to_string(),
        ] {
            assert!(choose(&store, "ann@a.test", &bad).is_err(), "{bad}");
        }
        assert!(choose(&store, "not-an-address", &png_url()).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
