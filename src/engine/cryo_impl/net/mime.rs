//! Content types: what a server says a response is.
//!
//! The parse is small; the *judgement* is the point. A `Content-Type`
//! is advisory and often wrong, and what a browser does about that is a
//! security decision, not a convenience one — treating a server's
//! `text/plain` as HTML is how a file upload becomes a cross-site
//! scripting hole.
//!
//! So this module answers two separate questions and keeps them apart:
//!
//! - **What did the server say?** [`MimeType::parse`], verbatim.
//! - **What should we do with it?** [`MimeType::resource_kind`], which
//!   is deliberately conservative: anything not recognised is treated
//!   as a download rather than guessed at.
//!
//! No content sniffing of bodies. A server that mislabels its HTML gets
//! plain text, which is a visible bug on one page; a sniffer that
//! promotes text to HTML is a vulnerability on every page.

/// A parsed `Content-Type`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MimeType {
    /// `text` in `text/html`, lowercased.
    pub kind: String,
    /// `html` in `text/html`, lowercased.
    pub subtype: String,
    /// Parameters in order, names lowercased and values as written.
    pub parameters: Vec<(String, String)>,
}

impl MimeType {
    /// Parse a `Content-Type` header value.
    ///
    /// `None` for anything without a `/`, which is not a media type
    /// however hopefully it was sent.
    pub fn parse(header: &str) -> Option<Self> {
        let mut parts = header.split(';');
        let essence = parts.next()?.trim();
        let (kind, subtype) = essence.split_once('/')?;
        if kind.is_empty() || subtype.is_empty() {
            return None;
        }

        let parameters = parts
            .filter_map(|parameter| {
                let (name, value) = parameter.split_once('=')?;
                let value = value.trim();
                // Quoted values are common on charset, and the quotes
                // are not part of the value.
                let value = value
                    .strip_prefix('"')
                    .and_then(|inner| inner.strip_suffix('"'))
                    .unwrap_or(value);
                Some((name.trim().to_ascii_lowercase(), value.to_owned()))
            })
            .collect();

        Some(Self {
            kind: kind.to_ascii_lowercase(),
            subtype: subtype.to_ascii_lowercase(),
            parameters,
        })
    }

    /// `text/html` — the type without its parameters.
    pub fn essence(&self) -> String {
        format!("{}/{}", self.kind, self.subtype)
    }

    pub fn parameter(&self, name: &str) -> Option<&str> {
        self.parameters
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The declared charset, if any.
    pub fn charset(&self) -> Option<&str> {
        self.parameter("charset")
    }

    /// What this engine should do with a resource of this type.
    ///
    /// Unrecognised types become [`ResourceKind::Download`] rather than
    /// being guessed at — see the module note on why sniffing is worse
    /// than being wrong here.
    pub fn resource_kind(&self) -> ResourceKind {
        match (self.kind.as_str(), self.subtype.as_str()) {
            ("text", "html") | ("application", "xhtml+xml") => ResourceKind::Html,
            ("text", "css") => ResourceKind::Css,
            // The `text/javascript` spelling is the standard one; the
            // rest are legacy and still served every day.
            ("text", "javascript" | "ecmascript")
            | ("application", "javascript" | "ecmascript" | "x-javascript") => ResourceKind::Script,
            ("image", _) => ResourceKind::Image,
            ("font", _) | ("application", "font-woff" | "font-woff2") => ResourceKind::Font,
            ("text", _) => ResourceKind::PlainText,
            _ => ResourceKind::Download,
        }
    }

    /// Whether this is a type the engine renders as a document.
    pub fn is_document(&self) -> bool {
        matches!(
            self.resource_kind(),
            ResourceKind::Html | ResourceKind::PlainText
        )
    }
}

/// What the engine does with a fetched resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResourceKind {
    Html,
    Css,
    Script,
    Image,
    Font,
    /// Text that is not markup — shown in a `<pre>`, not parsed.
    PlainText,
    /// Anything else. The engine does not render it.
    Download,
}

/// `text/html; charset=utf-8`, for a response with no stated type.
///
/// Used only where the transport has no notion of one — a `file:` read,
/// say. A server that omits the header gets [`ResourceKind::Download`]
/// instead, because silence from a server is not the same as a local
/// file having no header to give.
pub fn html() -> MimeType {
    MimeType {
        kind: "text".to_owned(),
        subtype: "html".to_owned(),
        parameters: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_type_parses() {
        let mime = MimeType::parse("text/html").expect("parses");
        assert_eq!(mime.kind, "text");
        assert_eq!(mime.subtype, "html");
        assert_eq!(mime.essence(), "text/html");
        assert!(mime.parameters.is_empty());
    }

    #[test]
    fn types_fold_case_but_parameter_values_do_not() {
        let mime = MimeType::parse("TEXT/HTML; CharSet=UTF-8").expect("parses");
        assert_eq!(mime.essence(), "text/html");
        // The name folds so lookup works; the value is the label, and
        // the encoding table folds it itself.
        assert_eq!(mime.charset(), Some("UTF-8"));
    }

    #[test]
    fn a_charset_parameter_is_read() {
        let mime = MimeType::parse("text/html; charset=utf-8").expect("parses");
        assert_eq!(mime.charset(), Some("utf-8"));
    }

    #[test]
    fn a_quoted_charset_loses_its_quotes() {
        let mime = MimeType::parse("text/html; charset=\"utf-8\"").expect("parses");
        assert_eq!(mime.charset(), Some("utf-8"));
    }

    #[test]
    fn whitespace_around_parameters_is_ignored() {
        let mime = MimeType::parse("  text/html ;  charset = utf-8  ").expect("parses");
        assert_eq!(mime.essence(), "text/html");
        assert_eq!(mime.charset(), Some("utf-8"));
    }

    #[test]
    fn several_parameters_are_all_kept() {
        let mime =
            MimeType::parse("multipart/form-data; boundary=xyz; charset=utf-8").expect("parses");
        assert_eq!(mime.parameter("boundary"), Some("xyz"));
        assert_eq!(mime.charset(), Some("utf-8"));
    }

    #[test]
    fn a_missing_parameter_is_absent_not_empty() {
        let mime = MimeType::parse("text/html").expect("parses");
        assert_eq!(mime.charset(), None);
        assert_eq!(mime.parameter("boundary"), None);
    }

    #[test]
    fn something_without_a_slash_is_not_a_media_type() {
        assert!(MimeType::parse("html").is_none());
        assert!(MimeType::parse("").is_none());
        assert!(MimeType::parse("/html").is_none());
        assert!(MimeType::parse("text/").is_none());
    }

    #[test]
    fn a_malformed_parameter_is_skipped_not_fatal() {
        // A parameter with no `=` is dropped; the type still parses.
        let mime = MimeType::parse("text/html; broken; charset=utf-8").expect("parses");
        assert_eq!(mime.charset(), Some("utf-8"));
        assert_eq!(mime.parameters.len(), 1);
    }

    #[test]
    fn document_types_are_recognised() {
        for header in ["text/html", "application/xhtml+xml"] {
            let mime = MimeType::parse(header).expect("parses");
            assert_eq!(mime.resource_kind(), ResourceKind::Html, "{header}");
            assert!(mime.is_document());
        }
    }

    #[test]
    fn stylesheets_and_scripts_are_recognised() {
        assert_eq!(
            MimeType::parse("text/css").expect("parses").resource_kind(),
            ResourceKind::Css
        );
        for header in [
            "text/javascript",
            "application/javascript",
            "application/x-javascript",
            "text/ecmascript",
        ] {
            assert_eq!(
                MimeType::parse(header).expect("parses").resource_kind(),
                ResourceKind::Script,
                "{header}"
            );
        }
    }

    #[test]
    fn images_and_fonts_are_recognised_by_their_type_alone() {
        for header in ["image/png", "image/jpeg", "image/svg+xml", "image/webp"] {
            assert_eq!(
                MimeType::parse(header).expect("parses").resource_kind(),
                ResourceKind::Image,
                "{header}"
            );
        }
        for header in ["font/ttf", "font/woff2", "application/font-woff"] {
            assert_eq!(
                MimeType::parse(header).expect("parses").resource_kind(),
                ResourceKind::Font,
                "{header}"
            );
        }
    }

    #[test]
    fn plain_text_is_shown_but_not_parsed_as_markup() {
        let mime = MimeType::parse("text/plain").expect("parses");
        assert_eq!(mime.resource_kind(), ResourceKind::PlainText);
        assert!(mime.is_document(), "it is displayed...");
        assert_ne!(
            mime.resource_kind(),
            ResourceKind::Html,
            "...but not as HTML"
        );
    }

    #[test]
    fn an_unknown_type_is_a_download_not_a_guess() {
        // The security decision: never promote an unknown type.
        for header in ["application/octet-stream", "application/zip", "x-thing/y"] {
            let mime = MimeType::parse(header).expect("parses");
            assert_eq!(mime.resource_kind(), ResourceKind::Download, "{header}");
            assert!(!mime.is_document());
        }
    }

    #[test]
    fn a_charset_does_not_change_what_a_type_is() {
        let bare = MimeType::parse("text/html").expect("parses");
        let with_charset = MimeType::parse("text/html; charset=utf-8").expect("parses");
        assert_eq!(bare.resource_kind(), with_charset.resource_kind());
        assert_eq!(bare.essence(), with_charset.essence());
    }

    #[test]
    fn the_html_default_is_a_usable_document_type() {
        let mime = html();
        assert_eq!(mime.essence(), "text/html");
        assert_eq!(mime.resource_kind(), ResourceKind::Html);
        assert_eq!(mime.charset(), None);
    }
}
