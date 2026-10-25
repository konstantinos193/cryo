//! Address input normalization and local file URL encoding.
use crate::shell::search::SearchEngine;

/// Turn address-bar text into a URL: full URLs pass through, domain-looking
/// input gets `https://`, anything else becomes a search on `engine`.
pub fn normalize_nav_input(input: &str, engine: &SearchEngine) -> String {
    let s = input.trim();
    if s.is_empty() {
        return "about:blank".to_string();
    }
    if s.contains("://")
        || s.starts_with("about:")
        || s.starts_with("data:")
        || s.starts_with("file:")
    {
        return s.to_string();
    }
    // A pasted Windows path becomes a file URL, like in any real
    // browser — without this the drive letter's dot (in `file.html`)
    // reads as a host and the path is asked of the web.
    if let Some(url) = local_path_url(s) {
        return url;
    }
    if !s.contains(' ') && s.contains('.') {
        return format!("https://{s}");
    }
    engine.search_url(s)
}

/// Chrome's Ctrl+Enter: a bare word becomes `www.<word>.com`; anything
/// already dotted or multi-word falls back to normal normalization.
pub fn canonize_nav_input(input: &str, engine: &SearchEngine) -> String {
    let s = input.trim();
    if let Some(url) = local_path_url(s) {
        return url;
    }
    if !s.is_empty() && !s.contains(' ') && !s.contains('.') && !s.contains("://") {
        return format!("https://www.{s}.com");
    }
    normalize_nav_input(s, engine)
}

/// `C:\dir\f` or `C:/dir/f` → `file:///C:/dir/f`; `\\srv\dir` →
/// `file://srv/dir`. Anything else is not a path.
fn local_path_url(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let drive =
        b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
    if drive {
        return Some(format!("file:///{}", encode_path(s)));
    }
    if let Some(unc) = s.strip_prefix(r"\\") {
        return Some(format!("file://{}", encode_path(unc)));
    }
    None
}

/// Percent-encode what a `file:` URL cannot carry — path separators are
/// normalized to `/` first, and non-ASCII bytes escape per octet so a
/// Greek or Cyrillic folder name still opens.
fn encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.replace('\\', "/").bytes() {
        match b {
            b' ' | b'%' | b'#' | b'?' | b'<' | b'>' | b'"' | b'|' | 0x00..=0x1F | 0x7F..=0xFF => {
                out.push_str(&format!("%{b:02X}"))
            }
            _ => out.push(b as char),
        }
    }
    out
}
