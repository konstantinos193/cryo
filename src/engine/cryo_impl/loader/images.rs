//! Finding and fetching the images a page asks for.
//!
//! An image reaches a page two ways — an `<img src>` in the markup, or
//! a `url()` among a box's `background-image` layers — and both are
//! known only *after* the styles have been computed, because a
//! stylesheet decides most of them. So this runs between styling and
//! paint, which is also where a browser puts it.
//!
//! ```text
//!   document + styles ──▶ urls ──▶ fetch ──▶ decode ──▶ Store
//! ```
//!
//! Each URL is fetched once however many boxes use it, which matters:
//! a page that tiles one picture across twenty elements should make
//! one request, not twenty.
//!
//! A failure here is not a page failure. An image that 404s, or
//! arrives in a format this engine cannot read, leaves a hole and the
//! rest of the page still renders — the same thing a browser does.

use std::collections::BTreeSet;

use crate::engine::cryo_impl::css::style_tree::StyleTree;
use crate::engine::cryo_impl::dom::Document;
use crate::engine::cryo_impl::image::{self, Store};
use crate::engine::cryo_impl::net::{url, Fetch};

/// Every image URL the page refers to, resolved against `base` and
/// each one only once.
///
/// Returned in a set so the order is stable and the duplicates are
/// already gone.
pub fn urls(document: &Document, styles: &StyleTree, base: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();

    for node in document.descendants(document.root()) {
        // `<img src>`: the markup's own way of asking.
        if document.is_html_element(node, "img") {
            if let Some(source) = document
                .element(node)
                .and_then(|element| element.attr("src"))
            {
                insert_resolved(&mut found, base, source);
            }
        }
        // `background-image: url(...)`, which is where most of a
        // modern page's pictures come from.
        let Some(style) = styles.get(node) else {
            continue;
        };
        for layer in &style.background_image {
            if let Some(source) = layer.url() {
                insert_resolved(&mut found, base, source);
            }
        }
    }
    found
}

/// Resolve one reference and keep it if it is something we could ask
/// for.
///
/// A `data:` URI or a relative path that will not resolve is dropped
/// here rather than failing a fetch later.
fn insert_resolved(found: &mut BTreeSet<String>, base: &str, reference: &str) {
    if reference.trim().is_empty() {
        return;
    }
    let Some(absolute) = url::resolve(base, reference) else {
        return;
    };
    if url::is_fetchable(&absolute) {
        found.insert(absolute);
    }
}

/// Fetch and decode every image the page refers to.
///
/// Returns the store and, beside it, what could not be had — so a
/// caller can say why a picture is missing instead of only showing a
/// gap.
pub fn load(
    net: &dyn Fetch,
    document: &Document,
    styles: &StyleTree,
    base: &str,
) -> (Store, Vec<(String, String)>) {
    let mut store = Store::new();
    let mut failed = Vec::new();

    for address in urls(document, styles, base) {
        match fetch_one(net, &address) {
            Ok(image) => store.insert(address, image),
            Err(why) => failed.push((address, why)),
        }
    }
    (store, failed)
}

fn fetch_one(net: &dyn Fetch, address: &str) -> Result<image::Image, String> {
    let resource = net.fetch(address).map_err(|error| error.to_string())?;
    image::decode(&resource.body).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::cascade::{Origin, RuleSet};
    use crate::engine::cryo_impl::css::computed::ComputeContext;
    use crate::engine::cryo_impl::css::selector::MatchState;
    use crate::engine::cryo_impl::html::parse as parse_html;

    /// The image URLs of a page at `https://a.test/dir/page.html`.
    fn found(markup: &str, css: &str) -> Vec<String> {
        let document = parse_html(markup);
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::Author, css);
        let styles = StyleTree::resolve(
            &document,
            &rules,
            &MatchState::default(),
            &ComputeContext::DEFAULT,
        );
        urls(&document, &styles, "https://a.test/dir/page.html")
            .into_iter()
            .collect()
    }

    #[test]
    fn an_img_src_is_found_and_resolved() {
        assert_eq!(
            found("<img src=cat.png>", ""),
            ["https://a.test/dir/cat.png"]
        );
    }

    #[test]
    fn a_background_url_is_found_too() {
        assert_eq!(
            found(
                "<div id=a></div>",
                "#a { background-image: url(/hero.jpg) }"
            ),
            ["https://a.test/hero.jpg"]
        );
    }

    #[test]
    fn a_url_used_twice_is_asked_for_once() {
        // Twenty boxes tiling one picture is one request.
        assert_eq!(
            found(
                "<div class=x></div><div class=x></div>",
                ".x { background-image: url(tile.png) }"
            ),
            ["https://a.test/dir/tile.png"]
        );
    }

    #[test]
    fn a_gradient_layer_asks_for_nothing() {
        assert!(found(
            "<div id=a></div>",
            "#a { background-image: linear-gradient(red, blue) }"
        )
        .is_empty());
    }

    #[test]
    fn both_layers_of_a_list_are_seen() {
        // The hero pattern: a photograph over a gradient.
        assert_eq!(
            found(
                "<div id=a></div>",
                "#a { background-image: url(hero.jpg), linear-gradient(red, blue) }"
            ),
            ["https://a.test/dir/hero.jpg"]
        );
    }

    #[test]
    fn an_unfetchable_reference_is_dropped_rather_than_tried() {
        assert!(found("<img src=''>", "").is_empty());
        assert!(found(
            "<div id=a></div>",
            "#a { background-image: url(data:image/png;base64,iVBORw0KGgo=) }"
        )
        .is_empty());
    }

    #[test]
    fn an_image_in_a_stylesheet_and_one_in_the_markup_both_arrive() {
        let mut both = found(
            "<img src=a.png><div id=b></div>",
            "#b { background-image: url(c.png) }",
        );
        both.sort();
        assert_eq!(
            both,
            ["https://a.test/dir/a.png", "https://a.test/dir/c.png"]
        );
    }
}
