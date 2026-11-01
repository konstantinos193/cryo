//! Working out a document's encoding before it can be read.
//!
//! The circularity: a page declares its encoding *inside itself*, in
//! `<meta charset>`, which cannot be read without knowing the encoding.
//! The spec cuts the knot by scanning the raw bytes for the declaration
//! treating them as ASCII — which works because every encoding a page
//! might be in agrees with ASCII on the letters `charset` is spelled
//! with.
//!
//! The scan is bounded to [`SNIFF_LIMIT`] bytes. A declaration after
//! that is ignored, as browsers ignore it, because a page cannot be
//! allowed to change encoding half way through: everything before the
//! declaration has already been decoded.

use super::Encoding;

/// How far into a document to look for a `<meta charset>`.
///
/// 1024 bytes, which is what the spec requires and what browsers do. A
/// declaration below that is too late to act on.
pub const SNIFF_LIMIT: usize = 1024;

/// How a document's encoding was decided, for reporting and debugging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodingSource {
    /// A byte order mark, which outranks everything.
    ByteOrderMark,
    /// The transport's `Content-Type` charset.
    Transport,
    /// A `<meta charset>` in the document.
    Document,
    /// Nothing said, so the default was used.
    Default,
}

/// A byte order mark at the start, and how many bytes it occupies.
///
/// A BOM wins over every other signal, including an HTTP header that
/// contradicts it: it is unambiguous, and a server's guess is not.
pub fn sniff_bom(bytes: &[u8]) -> Option<(Encoding, usize)> {
    match bytes {
        [0xef, 0xbb, 0xbf, ..] => Some((Encoding::Utf8, 3)),
        [0xff, 0xfe, ..] => Some((Encoding::Utf16Le, 2)),
        [0xfe, 0xff, ..] => Some((Encoding::Utf16Be, 2)),
        _ => None,
    }
}

/// Scan the first [`SNIFF_LIMIT`] bytes for a `<meta>` charset
/// declaration, in either of the two spellings pages use:
///
/// ```text
///   <meta charset="utf-8">
///   <meta http-equiv="Content-Type" content="text/html; charset=utf-8">
/// ```
pub fn sniff_meta_charset(bytes: &[u8]) -> Option<Encoding> {
    let window = &bytes[..bytes.len().min(SNIFF_LIMIT)];
    // ASCII-lowercased for matching; every encoding worth sniffing
    // agrees with ASCII on these letters.
    let text: String = window
        .iter()
        .map(|&byte| char::from(byte.to_ascii_lowercase()))
        .collect();

    let mut from = 0;
    while let Some(found) = text[from..].find("charset") {
        let after = from + found + "charset".len();
        if let Some(encoding) = read_label(&text[after..]) {
            return Some(encoding);
        }
        from = after;
    }
    None
}

/// Read a charset label following a `charset` keyword.
///
/// Returns `None` when what follows is not a label at all — a page may
/// contain the word in prose, and only an `=` makes it a declaration.
fn read_label(rest: &str) -> Option<Encoding> {
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();

    let (quote, rest) = match rest.strip_prefix('"') {
        Some(inner) => (Some('"'), inner),
        None => match rest.strip_prefix('\'') {
            Some(inner) => (Some('\''), inner),
            None => (None, rest),
        },
    };
    let end = match quote {
        Some(quote) => rest.find(quote)?,
        // Unquoted, so it ends at whitespace or at the tag's end.
        None => rest
            .find(|c: char| c.is_whitespace() || c == ';' || c == '>' || c == '/')
            .unwrap_or(rest.len()),
    };
    Encoding::for_label(&rest[..end])
}

/// Decide a document's encoding and decode it.
///
/// `declared` is the transport's charset, if it gave one. Returns the
/// text, the encoding used, and where that decision came from.
///
/// The byte order mark is **consumed**, not left in the text: it is a
/// signal about the bytes, not content, and leaving it would put an
/// invisible character at the very start of every such document — where
/// it would become a text node before `<html>`.
pub fn detect(bytes: &[u8], declared: Option<&str>) -> (String, Encoding, EncodingSource) {
    if let Some((encoding, length)) = sniff_bom(bytes) {
        return (
            encoding.decode(&bytes[length..]),
            encoding,
            EncodingSource::ByteOrderMark,
        );
    }
    if let Some(encoding) = declared.and_then(Encoding::for_label) {
        return (encoding.decode(bytes), encoding, EncodingSource::Transport);
    }
    if let Some(encoding) = sniff_meta_charset(bytes) {
        return (encoding.decode(bytes), encoding, EncodingSource::Document);
    }
    // The web's default for undeclared content. Not UTF-8: a legacy
    // page is far likelier to be windows-1252, and reading it as UTF-8
    // replaces every accented character rather than bending one.
    let encoding = Encoding::Windows1252;
    (encoding.decode(bytes), encoding, EncodingSource::Default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_utf8_byte_order_mark_is_recognised() {
        assert_eq!(
            sniff_bom(&[0xef, 0xbb, 0xbf, b'h']),
            Some((Encoding::Utf8, 3))
        );
    }

    #[test]
    fn both_utf16_byte_order_marks_are_recognised() {
        assert_eq!(sniff_bom(&[0xff, 0xfe]), Some((Encoding::Utf16Le, 2)));
        assert_eq!(sniff_bom(&[0xfe, 0xff]), Some((Encoding::Utf16Be, 2)));
    }

    #[test]
    fn ordinary_text_has_no_byte_order_mark() {
        assert_eq!(sniff_bom(b"<!DOCTYPE html>"), None);
        assert_eq!(sniff_bom(b""), None);
        assert_eq!(sniff_bom(&[0xef]), None, "a truncated mark is not one");
    }

    #[test]
    fn a_meta_charset_is_found_in_either_spelling() {
        assert_eq!(
            sniff_meta_charset(b"<html><head><meta charset=\"utf-8\">"),
            Some(Encoding::Utf8)
        );
        assert_eq!(
            sniff_meta_charset(
                b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=windows-1252\">"
            ),
            Some(Encoding::Windows1252)
        );
    }

    #[test]
    fn a_meta_charset_may_be_unquoted_or_single_quoted() {
        assert_eq!(
            sniff_meta_charset(b"<meta charset=utf-8>"),
            Some(Encoding::Utf8)
        );
        assert_eq!(
            sniff_meta_charset(b"<meta charset='utf-8'>"),
            Some(Encoding::Utf8)
        );
        assert_eq!(
            sniff_meta_charset(b"<meta charset=utf-8 />"),
            Some(Encoding::Utf8)
        );
    }

    #[test]
    fn a_meta_charset_is_matched_case_insensitively() {
        assert_eq!(
            sniff_meta_charset(b"<META CHARSET=\"UTF-8\">"),
            Some(Encoding::Utf8)
        );
    }

    #[test]
    fn the_word_charset_in_prose_is_not_a_declaration() {
        // Only an `=` makes it one.
        assert_eq!(sniff_meta_charset(b"<p>our charset policy</p>"), None);
        assert_eq!(sniff_meta_charset(b"<p>charset</p>"), None);
    }

    #[test]
    fn a_later_declaration_is_found_after_an_unusable_one() {
        // The first mention is prose; the real declaration follows.
        let html = b"<!-- charset discussion --><meta charset=\"utf-8\">";
        assert_eq!(sniff_meta_charset(html), Some(Encoding::Utf8));
    }

    #[test]
    fn an_unsupported_declared_charset_is_not_used() {
        assert_eq!(sniff_meta_charset(b"<meta charset=\"shift_jis\">"), None);
    }

    #[test]
    fn a_declaration_past_the_limit_is_ignored() {
        // Too late to act on: everything before it is already decoded.
        let mut html = vec![b' '; SNIFF_LIMIT];
        html.extend_from_slice(b"<meta charset=\"utf-8\">");
        assert_eq!(sniff_meta_charset(&html), None);

        // One byte earlier and it is found.
        let mut just_inside = vec![b' '; SNIFF_LIMIT - 30];
        just_inside.extend_from_slice(b"<meta charset=\"utf-8\">");
        assert_eq!(sniff_meta_charset(&just_inside), Some(Encoding::Utf8));
    }

    #[test]
    fn a_byte_order_mark_beats_a_contradicting_header() {
        // The mark is unambiguous; a server's charset is a guess.
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice("café".as_bytes());
        let (text, encoding, source) = detect(&bytes, Some("windows-1252"));
        assert_eq!(text, "café");
        assert_eq!(encoding, Encoding::Utf8);
        assert_eq!(source, EncodingSource::ByteOrderMark);
    }

    #[test]
    fn the_byte_order_mark_is_consumed_not_left_in_the_text() {
        // Otherwise it becomes a stray text node before `<html>`.
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice(b"<html>");
        let (text, _, _) = detect(&bytes, None);
        assert_eq!(text, "<html>");
        assert!(!text.starts_with('\u{feff}'));
    }

    #[test]
    fn a_header_charset_beats_a_meta_declaration() {
        let html = b"<meta charset=\"utf-8\">";
        let (_, encoding, source) = detect(html, Some("windows-1252"));
        assert_eq!(encoding, Encoding::Windows1252);
        assert_eq!(source, EncodingSource::Transport);
    }

    #[test]
    fn a_meta_declaration_is_used_when_the_transport_says_nothing() {
        let html = "<meta charset=\"utf-8\"><p>café".as_bytes();
        let (text, encoding, source) = detect(html, None);
        assert_eq!(encoding, Encoding::Utf8);
        assert_eq!(source, EncodingSource::Document);
        assert!(text.ends_with("café"));
    }

    #[test]
    fn an_unusable_header_charset_falls_through_to_the_document() {
        let html = "<meta charset=\"utf-8\"><p>café".as_bytes();
        let (_, encoding, source) = detect(html, Some("x-unknown-encoding"));
        assert_eq!(encoding, Encoding::Utf8);
        assert_eq!(source, EncodingSource::Document);
    }

    #[test]
    fn undeclared_content_defaults_to_windows_1252() {
        // The web's real default. 0xe9 is `é` there, and invalid UTF-8.
        let (text, encoding, source) = detect(&[b'c', b'a', b'f', 0xe9], None);
        assert_eq!(text, "café");
        assert_eq!(encoding, Encoding::Windows1252);
        assert_eq!(source, EncodingSource::Default);
    }

    #[test]
    fn an_empty_document_still_decodes() {
        let (text, _, source) = detect(b"", None);
        assert_eq!(text, "");
        assert_eq!(source, EncodingSource::Default);
    }

    #[test]
    fn a_utf16_document_decodes_through_its_mark() {
        let mut bytes = vec![0xff, 0xfe];
        for unit in "hi".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let (text, encoding, _) = detect(&bytes, None);
        assert_eq!(text, "hi");
        assert_eq!(encoding, Encoding::Utf16Le);
    }
}
