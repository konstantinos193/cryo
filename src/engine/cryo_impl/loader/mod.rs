//! Loading a page: a URL becomes a document with all its styles.
//!
//! The context that coordinates the others. [`net`](super::net) fetches,
//! [`html`](super::html) parses, [`css`](super::css) cascades — and
//! something has to know that a document's `<link rel=stylesheet>`
//! elements are themselves URLs that must be fetched before the page
//! can be styled. That knowledge lives here rather than in any of the
//! three, because it belongs to none of them.
//!
//! Order matters and is not obvious: **stylesheets apply in document
//! order**, and a `<link>` and a `<style>` interleave. A page that
//! writes
//!
//! ```html
//!   <link rel=stylesheet href=theme.css>
//!   <style>h1 { color: red }</style>
//! ```
//!
//! expects the inline rule to win over the file's, so the sheets are
//! collected in tree order and inserted in that order — not files first
//! and inline after, which would silently invert every override a page
//! makes.
//!
//! A sheet that fails to fetch is **skipped, not fatal**. A page with
//! one dead stylesheet still renders with the others, which is what
//! every browser does and what makes a site survive a CDN outage.
//!
//! Not yet loaded: images, scripts, fonts and `@import`. Each needs its
//! own handling once there is something to do with it.

pub mod fonts;
pub mod images;

use super::css::cascade::{Origin, RuleSet};
use super::css::media::Viewport;
use super::dom::{Document, NodeId};
use super::html::parse as parse_html;
use super::net::{url, Fetch, FetchError, ResourceKind};

/// A page, fetched and ready to style.
pub struct LoadedPage {
    /// The URL the document actually came from, after redirects.
    /// Relative links resolve against this.
    pub base_url: String,
    pub document: Document,
    /// The user-agent sheet plus every author sheet, in order.
    pub rules: RuleSet,
    /// Stylesheet URLs that could not be fetched. Reported rather than
    /// hidden: a page missing its styles looks broken, and the cause
    /// should not take a packet capture to find.
    pub failed_stylesheets: Vec<(String, String)>,
}

impl LoadedPage {
    /// How many author stylesheets were applied.
    pub fn stylesheet_count(&self) -> usize {
        self.rules.len()
    }
}

/// Where a stylesheet came from, so it can be sorted into tree order.
enum SheetSource {
    /// A `<style>` element's text, already in hand.
    Inline(String),
    /// A `<link rel=stylesheet>`, to be fetched.
    External(String),
}

/// Fetch `url`, parse it, and load every stylesheet it references.
///
/// `user_agent_css` is the engine's own default sheet, inserted first
/// so that author rules win over it at equal specificity.
pub fn load_page(
    net: &dyn Fetch,
    url: &str,
    user_agent_css: &str,
    viewport: Viewport,
) -> Result<LoadedPage, FetchError> {
    let resource = net.fetch(url)?;
    if resource.kind() != ResourceKind::Html {
        return Err(FetchError::Malformed(format!(
            "{} is {}, not a document",
            resource.url,
            resource.mime.essence()
        )));
    }

    let document = parse_html(&resource.text().text);
    let base_url = document_base(&document, &resource.url);

    let mut rules = RuleSet::for_viewport(viewport);
    rules.insert_css(Origin::UserAgent, user_agent_css);
    let mut failed_stylesheets = Vec::new();

    for source in collect_sheets(&document) {
        match source {
            // An inline sheet's relative URLs resolve against the
            // document, since that is where it was written.
            SheetSource::Inline(css) => rules.insert_css_from(Origin::Author, &css, &base_url),
            SheetSource::External(reference) => {
                let Some(absolute) = url::resolve(&base_url, &reference) else {
                    failed_stylesheets.push((reference, "unresolvable address".to_owned()));
                    continue;
                };
                if !url::is_fetchable(&absolute) {
                    failed_stylesheets.push((absolute, "scheme not fetched".to_owned()));
                    continue;
                }
                match net.fetch(&absolute) {
                    // A sheet served as the wrong type is still used:
                    // misconfigured servers are common, and the cost of
                    // trusting one here is a parse that finds no rules.
                    Ok(sheet) => {
                        let sheet_url = sheet.url.clone();
                        rules.insert_css_from(Origin::Author, &sheet.text().text, &sheet_url)
                    }
                    Err(error) => failed_stylesheets.push((absolute, error.to_string())),
                }
            }
        }
    }

    Ok(LoadedPage {
        base_url,
        document,
        rules,
        failed_stylesheets,
    })
}

/// The URL relative references resolve against.
///
/// A `<base href>` overrides the document's own address — the one place
/// a page gets to say where its links point.
fn document_base(document: &Document, fetched_from: &str) -> String {
    document
        .first_html_element("base")
        .and_then(|id| document.element(id)?.attr("href").map(ToOwned::to_owned))
        .and_then(|href| url::resolve(fetched_from, &href))
        .unwrap_or_else(|| fetched_from.to_owned())
}

/// Every stylesheet the document references, in tree order.
fn collect_sheets(document: &Document) -> Vec<SheetSource> {
    document
        .descendants(document.root())
        .filter_map(|id| sheet_at(document, id))
        .collect()
}

fn sheet_at(document: &Document, id: NodeId) -> Option<SheetSource> {
    if document.is_html_element(id, "style") {
        let css = document.text_content(id);
        return (!css.trim().is_empty()).then_some(SheetSource::Inline(css));
    }
    if !document.is_html_element(id, "link") {
        return None;
    }
    let element = document.element(id)?;
    // `rel` is a space-separated token list, and a sheet may be marked
    // `alternate stylesheet` — which is not applied unless chosen.
    let rel = element.attr("rel").unwrap_or_default().to_ascii_lowercase();
    let tokens: Vec<&str> = rel.split_ascii_whitespace().collect();
    if !tokens.contains(&"stylesheet") || tokens.contains(&"alternate") {
        return None;
    }
    element
        .attr("href")
        .map(|href| SheetSource::External(href.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::computed::Display;
    use crate::engine::cryo_impl::css::selector::MatchState;
    use crate::engine::cryo_impl::css::style_tree::StyleTree;
    use crate::engine::cryo_impl::css::value::Color;
    use crate::engine::cryo_impl::net::MemoryFetch;

    const UA: &str = "html, body, p, h1 { display: block }";

    /// The computed style of the first `<{tag}>` on a loaded page.
    fn style_of(
        page: &LoadedPage,
        tag: &str,
    ) -> crate::engine::cryo_impl::css::computed::ComputedStyle {
        let styles = StyleTree::resolve(
            &page.document,
            &page.rules,
            &MatchState::default(),
            &Default::default(),
        );
        let element = page
            .document
            .first_html_element(tag)
            .unwrap_or_else(|| panic!("no <{tag}>"));
        styles
            .get(element)
            .unwrap_or_else(|| panic!("<{tag}> was not styled"))
            .clone()
    }

    #[test]
    fn a_page_with_no_styles_loads() {
        let net = MemoryFetch::new().html("https://a.test/", "<p>hi</p>");
        let page = load_page(&net, "https://a.test/", UA, Viewport::DEFAULT).expect("loads");
        assert_eq!(page.base_url, "https://a.test/");
        assert!(page.failed_stylesheets.is_empty());
        assert_eq!(
            style_of(&page, "p").display,
            Display::Block,
            "the UA sheet applied"
        );
    }

    #[test]
    fn an_inline_style_element_applies() {
        let net = MemoryFetch::new().html(
            "https://a.test/",
            "<style>p { color: red }</style><p>hi</p>",
        );
        let page = load_page(&net, "https://a.test/", UA, Viewport::DEFAULT).expect("loads");
        assert_eq!(style_of(&page, "p").color, Color::rgb(255, 0, 0));
    }

    #[test]
    fn an_external_stylesheet_is_fetched_and_applied() {
        // The whole point of this module.
        let net = MemoryFetch::new()
            .html(
                "https://a.test/",
                "<link rel=stylesheet href=/s.css><p>hi</p>",
            )
            .serve("https://a.test/s.css", "text/css", b"p { color: blue }");
        let page = load_page(&net, "https://a.test/", UA, Viewport::DEFAULT).expect("loads");
        assert!(page.failed_stylesheets.is_empty());
        assert_eq!(style_of(&page, "p").color, Color::rgb(0, 0, 255));
    }

    #[test]
    fn a_relative_href_resolves_against_the_document() {
        let net = MemoryFetch::new()
            .html(
                "https://a.test/dir/page.html",
                "<link rel=stylesheet href=s.css><p>hi</p>",
            )
            .serve("https://a.test/dir/s.css", "text/css", b"p { color: blue }");
        let page =
            load_page(&net, "https://a.test/dir/page.html", UA, Viewport::DEFAULT).expect("loads");
        assert_eq!(style_of(&page, "p").color, Color::rgb(0, 0, 255));
    }

    #[test]
    fn a_stylesheet_resolves_against_where_a_redirect_landed() {
        // Not against the URL requested: the assets live beside the
        // document that was actually served.
        let net = MemoryFetch::new()
            .redirect("https://a.test/old", "https://a.test/new/index.html")
            .html(
                "https://a.test/new/index.html",
                "<link rel=stylesheet href=s.css><p>hi</p>",
            )
            .serve("https://a.test/new/s.css", "text/css", b"p { color: blue }");
        let page = load_page(&net, "https://a.test/old", UA, Viewport::DEFAULT).expect("loads");
        assert_eq!(page.base_url, "https://a.test/new/index.html");
        assert_eq!(style_of(&page, "p").color, Color::rgb(0, 0, 255));
    }

    #[test]
    fn a_base_element_overrides_where_links_point() {
        let net = MemoryFetch::new()
            .html(
                "https://a.test/dir/page.html",
                "<base href=\"https://cdn.test/\"><link rel=stylesheet href=s.css><p>hi</p>",
            )
            .serve("https://cdn.test/s.css", "text/css", b"p { color: blue }");
        let page =
            load_page(&net, "https://a.test/dir/page.html", UA, Viewport::DEFAULT).expect("loads");
        assert_eq!(page.base_url, "https://cdn.test/");
        assert_eq!(style_of(&page, "p").color, Color::rgb(0, 0, 255));
    }

    #[test]
    fn sheets_apply_in_document_order() {
        // The inline rule comes second, so it wins. Loading files first
        // would invert every override a page makes.
        let net = MemoryFetch::new()
            .html(
                "https://a.test/",
                "<link rel=stylesheet href=/s.css><style>p { color: lime }</style><p>hi</p>",
            )
            .serve("https://a.test/s.css", "text/css", b"p { color: red }");
        let page = load_page(&net, "https://a.test/", UA, Viewport::DEFAULT).expect("loads");
        assert_eq!(style_of(&page, "p").color, Color::rgb(0, 255, 0));
    }

    #[test]
    fn an_earlier_inline_sheet_loses_to_a_later_file() {
        // And the other way round, to prove the order is real.
        let net = MemoryFetch::new()
            .html(
                "https://a.test/",
                "<style>p { color: lime }</style><link rel=stylesheet href=/s.css><p>hi</p>",
            )
            .serve("https://a.test/s.css", "text/css", b"p { color: red }");
        let page = load_page(&net, "https://a.test/", UA, Viewport::DEFAULT).expect("loads");
        assert_eq!(style_of(&page, "p").color, Color::rgb(255, 0, 0));
    }

    #[test]
    fn a_stylesheet_that_fails_is_skipped_not_fatal() {
        // One dead CDN must not blank the page.
        let net = MemoryFetch::new().html(
            "https://a.test/",
            "<link rel=stylesheet href=/gone.css><style>p { color: red }</style><p>hi</p>",
        );
        let page = load_page(&net, "https://a.test/", UA, Viewport::DEFAULT).expect("still loads");
        assert_eq!(page.failed_stylesheets.len(), 1);
        assert!(page.failed_stylesheets[0].0.ends_with("/gone.css"));
        assert_eq!(style_of(&page, "p").color, Color::rgb(255, 0, 0));
    }

    #[test]
    fn a_link_that_is_not_a_stylesheet_is_ignored() {
        let net = MemoryFetch::new().html(
            "https://a.test/",
            "<link rel=icon href=/favicon.ico><link rel=preload href=/x.js><p>hi</p>",
        );
        let page = load_page(&net, "https://a.test/", UA, Viewport::DEFAULT).expect("loads");
        assert!(page.failed_stylesheets.is_empty(), "neither was fetched");
    }

    #[test]
    fn an_alternate_stylesheet_is_not_applied() {
        // It is an offer, not a default.
        let net = MemoryFetch::new()
            .html(
                "https://a.test/",
                "<link rel=\"alternate stylesheet\" href=/alt.css><p>hi</p>",
            )
            .serve("https://a.test/alt.css", "text/css", b"p { color: red }");
        let page = load_page(&net, "https://a.test/", UA, Viewport::DEFAULT).expect("loads");
        assert_ne!(style_of(&page, "p").color, Color::rgb(255, 0, 0));
    }

    #[test]
    fn a_rel_token_list_is_matched_by_token_not_substring() {
        let net = MemoryFetch::new()
            .html(
                "https://a.test/",
                "<link rel=\"preload stylesheet\" href=/s.css><p>hi</p>",
            )
            .serve("https://a.test/s.css", "text/css", b"p { color: blue }");
        let page = load_page(&net, "https://a.test/", UA, Viewport::DEFAULT).expect("loads");
        assert_eq!(style_of(&page, "p").color, Color::rgb(0, 0, 255));
    }

    #[test]
    fn an_unfetchable_scheme_is_reported_rather_than_requested() {
        let net = MemoryFetch::new().html(
            "https://a.test/",
            "<link rel=stylesheet href=\"data:text/css,p{color:red}\"><p>hi</p>",
        );
        let page = load_page(&net, "https://a.test/", UA, Viewport::DEFAULT).expect("loads");
        assert_eq!(page.failed_stylesheets.len(), 1);
        assert!(page.failed_stylesheets[0].1.contains("scheme"));
    }

    #[test]
    fn a_link_with_no_href_is_ignored() {
        let net = MemoryFetch::new().html("https://a.test/", "<link rel=stylesheet><p>hi</p>");
        let page = load_page(&net, "https://a.test/", UA, Viewport::DEFAULT).expect("loads");
        assert!(page.failed_stylesheets.is_empty());
    }

    #[test]
    fn an_empty_style_element_is_not_counted() {
        let net = MemoryFetch::new().html("https://a.test/", "<style>   </style><p>hi</p>");
        let page = load_page(&net, "https://a.test/", UA, Viewport::DEFAULT).expect("loads");
        // Only the UA sheet's rules are present.
        assert_eq!(page.stylesheet_count(), 1);
    }

    #[test]
    fn several_sheets_all_apply() {
        let net = MemoryFetch::new()
            .html(
                "https://a.test/",
                "<link rel=stylesheet href=/a.css><link rel=stylesheet href=/b.css><p>hi</p>",
            )
            .serve("https://a.test/a.css", "text/css", b"p { color: red }")
            .serve("https://a.test/b.css", "text/css", b"p { font-size: 30px }");
        let page = load_page(&net, "https://a.test/", UA, Viewport::DEFAULT).expect("loads");
        let style = style_of(&page, "p");
        assert_eq!(style.color, Color::rgb(255, 0, 0));
        assert_eq!(style.font_size, 30.0);
    }

    #[test]
    fn something_that_is_not_a_document_is_refused() {
        let net = MemoryFetch::new().serve("https://a.test/x.zip", "application/zip", b"PK");
        assert!(load_page(&net, "https://a.test/x.zip", UA, Viewport::DEFAULT).is_err());
    }

    #[test]
    fn a_page_that_cannot_be_fetched_reports_why() {
        let net = MemoryFetch::new();
        assert!(matches!(
            load_page(&net, "https://nowhere.test/", UA, Viewport::DEFAULT),
            Err(FetchError::HostNotFound(_))
        ));
    }
}
