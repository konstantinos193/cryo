//! Which font families a page actually asks for.
//!
//! A font set is built once per page and then borrowed immutably
//! through layout and paint, so it has to be complete before either
//! starts. That means knowing the families up front — and only the
//! computed styles know them, because a stylesheet decides nearly
//! every `font-family` on a real page.
//!
//! Why it matters more than it sounds: a family that is *not* in the
//! set falls through to the fallback, and a fallback supplies one
//! regular face. So a page whose stack this machine cannot satisfy
//! loses not just the shape of its letters but every weight
//! distinction in it — labels, headings and body all drawn at 400.
//! Asking for the families by name at least lets the substitution
//! table in [`system`](super::super::paint::font::system) find a close
//! relative with a bold of its own.
//!
//! Generic families are always included: they are what the stack falls
//! back to, and a page that names none of them still gets one from the
//! user-agent sheet.

use std::collections::BTreeSet;

use crate::engine::cryo_impl::css::font_face::{FontFace, Source};
use crate::engine::cryo_impl::css::style_tree::StyleTree;
use crate::engine::cryo_impl::dom::Document;
use crate::engine::cryo_impl::font_file;
use crate::engine::cryo_impl::net::{url, Fetch};
use crate::engine::cryo_impl::paint::font::outline_font::{FontSet, OutlineFont};
use crate::engine::cryo_impl::paint::font::system::SystemFonts;

/// The generics every page can reach, whether or not it names them.
const GENERICS: &[&str] = &["sans-serif", "serif", "monospace"];

/// Every family named by any element's computed style, plus the
/// generics.
///
/// Returned in a set, so a family shared by a thousand elements is
/// looked for once.
pub fn families(document: &Document, styles: &StyleTree) -> BTreeSet<String> {
    let mut found: BTreeSet<String> = GENERICS.iter().map(|name| (*name).to_owned()).collect();
    for node in document.descendants(document.root()) {
        let Some(style) = styles.get(node) else {
            continue;
        };
        for family in &style.font_family {
            // `inherit` reaches the computed style as a literal when
            // nothing resolved it; it is not a family and no machine
            // has a file for it.
            if family.eq_ignore_ascii_case("inherit") {
                continue;
            }
            found.insert(family.clone());
        }
    }
    found
}

/// Build the font set a page needs: the machine's fonts for the
/// families it names, and the files its own `@font-face` rules point
/// at.
///
/// The page's own faces go in **after** the system ones and win,
/// which is the point of declaring them: a designer who ships Inter
/// means Inter, not whatever the machine has by that name.
///
/// A face that will not load is skipped rather than fatal, and the
/// reason is returned. A missing webfont leaves a page readable in a
/// fallback; refusing to render leaves it blank.
pub fn load(
    net: &dyn Fetch,
    document: &Document,
    styles: &StyleTree,
    rules: &[FontFace],
) -> (FontSet, Vec<(String, String)>) {
    let wanted: Vec<String> = families(document, styles).into_iter().collect();
    let system = SystemFonts::new();
    let mut set = system.load_set(&wanted);

    let mut failed = Vec::new();
    for face in rules {
        match load_face(net, &system, face) {
            Ok(font) => set.insert(&face.family, font),
            Err(why) => failed.push((face.family.clone(), why)),
        }
    }
    (set, failed)
}

/// Try a face's sources in order and keep the first that opens.
fn load_face(
    net: &dyn Fetch,
    system: &SystemFonts,
    face: &FontFace,
) -> Result<OutlineFont, String> {
    let mut last = String::from("no usable source");
    for source in &face.sources {
        match source {
            Source::Local(family) => {
                if let Some(font) = system.load_family(family) {
                    return Ok(font.with_family(&face.family));
                }
                last = format!("no local {family}");
            }
            Source::Url(reference) => match fetch_font(net, reference, &face.base) {
                Ok(font) => return Ok(font.with_family(&face.family)),
                Err(why) => last = why,
            },
        }
    }
    Err(last)
}

fn fetch_font(net: &dyn Fetch, reference: &str, base: &str) -> Result<OutlineFont, String> {
    let address = url::resolve(base, reference).ok_or("unresolvable address")?;
    let resource = net
        .fetch(&address)
        .map_err(|error| format!("{address}: {error}"))?;
    let sfnt = font_file::to_sfnt(&resource.body).map_err(|error| format!("{address}: {error}"))?;
    OutlineFont::from_bytes(sfnt, "webfont").map_err(|error| format!("{address}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::css::cascade::{Origin, RuleSet};
    use crate::engine::cryo_impl::css::computed::ComputeContext;
    use crate::engine::cryo_impl::css::selector::MatchState;
    use crate::engine::cryo_impl::html::parse as parse_html;

    fn named(markup: &str, css: &str) -> BTreeSet<String> {
        let document = parse_html(markup);
        let mut rules = RuleSet::new();
        rules.insert_css(Origin::Author, css);
        let styles = StyleTree::resolve(
            &document,
            &rules,
            &MatchState::default(),
            &ComputeContext::DEFAULT,
        );
        families(&document, &styles)
    }

    #[test]
    fn the_generics_are_always_there() {
        let found = named("<p>hi</p>", "");
        for generic in GENERICS {
            assert!(found.contains(*generic), "{generic} missing from {found:?}");
        }
    }

    #[test]
    fn a_family_a_rule_names_is_collected() {
        let found = named("<p id=a>hi</p>", "#a { font-family: Inter, Arial }");
        assert!(found.contains("Inter"), "{found:?}");
        assert!(found.contains("Arial"), "{found:?}");
    }

    #[test]
    fn a_family_is_collected_once_however_many_elements_use_it() {
        let found = named("<p>a</p><p>b</p><p>c</p>", "p { font-family: Poppins }");
        assert_eq!(
            found.iter().filter(|name| *name == "Poppins").count(),
            1,
            "{found:?}"
        );
    }

    #[test]
    fn inherit_is_not_a_family() {
        // It reaches the computed style as a literal when nothing
        // resolved it, and no machine has a file called `inherit`.
        let found = named("<p id=a>hi</p>", "#a { font-family: inherit }");
        assert!(!found.contains("inherit"), "{found:?}");
    }
}
