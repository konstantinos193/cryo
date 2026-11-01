//! Character reference decoding: `&amp;`, `&#x27;`, `&nbsp;`.
//!
//! Two jobs the tokenizer delegates here, because both are table lookups
//! with fiddly spec rules and neither is a state machine:
//!
//! 1. **Named references** match by *longest prefix*, not by scanning to a
//!    `;`. `&notit;` is `¬it;` — "not" matches, the rest is text. Any
//!    implementation that looks for the semicolon first gets this wrong.
//! 2. **Numeric references** remap a specific set of code points.
//!    `&#128;` is not U+0080; it is `€`, because of a 1990s
//!    windows-1252 mistake the web is now permanently built on.
//!
//! Coverage: the full Latin-1 supplement, the punctuation, currency, math
//! and Greek blocks, and the markup-critical five. That is what documents
//! actually contain. The WHATWG table has 2231 entries including every
//! `&bnequiv;`-class mathematical alias; the long tail is mechanical to
//! add and absent until a real page needs it, which is a deliberate
//! omission rather than an oversight — see [`NAMED`].

use crate::engine::cryo_impl::encoding::windows_1252;

/// Named references, as `(name including any trailing `;`, replacement)`.
///
/// Both the semicolon-terminated and the legacy bare forms appear where
/// the spec lists both: `&amp` without the semicolon is valid in text and
/// the table carries it, which is why names are stored with their
/// punctuation rather than stripped.
///
/// Sorted longest-first within each starting letter is *not* required —
/// [`match_named`] scans for the longest match explicitly — but entries
/// are grouped by block to keep the table reviewable.
static NAMED: &[(&str, &str)] = &[
    // The five that change how markup parses.
    ("amp;", "&"),
    ("amp", "&"),
    ("lt;", "<"),
    ("lt", "<"),
    ("gt;", ">"),
    ("gt", ">"),
    ("quot;", "\""),
    ("quot", "\""),
    ("apos;", "'"),
    // Spaces and dashes.
    ("nbsp;", "\u{a0}"),
    ("nbsp", "\u{a0}"),
    ("ensp;", "\u{2002}"),
    ("emsp;", "\u{2003}"),
    ("thinsp;", "\u{2009}"),
    ("zwnj;", "\u{200c}"),
    ("zwj;", "\u{200d}"),
    ("lrm;", "\u{200e}"),
    ("rlm;", "\u{200f}"),
    ("ndash;", "\u{2013}"),
    ("mdash;", "\u{2014}"),
    ("shy;", "\u{ad}"),
    // Quotes.
    ("lsquo;", "\u{2018}"),
    ("rsquo;", "\u{2019}"),
    ("sbquo;", "\u{201a}"),
    ("ldquo;", "\u{201c}"),
    ("rdquo;", "\u{201d}"),
    ("bdquo;", "\u{201e}"),
    ("lsaquo;", "\u{2039}"),
    ("rsaquo;", "\u{203a}"),
    ("laquo;", "\u{ab}"),
    ("laquo", "\u{ab}"),
    ("raquo;", "\u{bb}"),
    ("raquo", "\u{bb}"),
    // Punctuation and marks.
    ("hellip;", "\u{2026}"),
    ("bull;", "\u{2022}"),
    ("middot;", "\u{b7}"),
    ("middot", "\u{b7}"),
    ("dagger;", "\u{2020}"),
    ("Dagger;", "\u{2021}"),
    ("prime;", "\u{2032}"),
    ("Prime;", "\u{2033}"),
    ("iexcl;", "\u{a1}"),
    ("iexcl", "\u{a1}"),
    ("iquest;", "\u{bf}"),
    ("iquest", "\u{bf}"),
    ("sect;", "\u{a7}"),
    ("sect", "\u{a7}"),
    ("para;", "\u{b6}"),
    ("para", "\u{b6}"),
    ("brvbar;", "\u{a6}"),
    ("brvbar", "\u{a6}"),
    ("uml;", "\u{a8}"),
    ("uml", "\u{a8}"),
    ("macr;", "\u{af}"),
    ("macr", "\u{af}"),
    ("acute;", "\u{b4}"),
    ("acute", "\u{b4}"),
    ("cedil;", "\u{b8}"),
    ("cedil", "\u{b8}"),
    ("ordf;", "\u{aa}"),
    ("ordf", "\u{aa}"),
    ("ordm;", "\u{ba}"),
    ("ordm", "\u{ba}"),
    // Currency and legal.
    ("cent;", "\u{a2}"),
    ("cent", "\u{a2}"),
    ("pound;", "\u{a3}"),
    ("pound", "\u{a3}"),
    ("curren;", "\u{a4}"),
    ("curren", "\u{a4}"),
    ("yen;", "\u{a5}"),
    ("yen", "\u{a5}"),
    ("euro;", "\u{20ac}"),
    ("copy;", "\u{a9}"),
    ("copy", "\u{a9}"),
    ("reg;", "\u{ae}"),
    ("reg", "\u{ae}"),
    ("trade;", "\u{2122}"),
    // Maths and arrows.
    ("deg;", "\u{b0}"),
    ("deg", "\u{b0}"),
    ("plusmn;", "\u{b1}"),
    ("plusmn", "\u{b1}"),
    ("sup1;", "\u{b9}"),
    ("sup1", "\u{b9}"),
    ("sup2;", "\u{b2}"),
    ("sup2", "\u{b2}"),
    ("sup3;", "\u{b3}"),
    ("sup3", "\u{b3}"),
    ("frac14;", "\u{bc}"),
    ("frac14", "\u{bc}"),
    ("frac12;", "\u{bd}"),
    ("frac12", "\u{bd}"),
    ("frac34;", "\u{be}"),
    ("frac34", "\u{be}"),
    ("times;", "\u{d7}"),
    ("times", "\u{d7}"),
    ("divide;", "\u{f7}"),
    ("divide", "\u{f7}"),
    ("minus;", "\u{2212}"),
    ("lowast;", "\u{2217}"),
    ("ne;", "\u{2260}"),
    ("le;", "\u{2264}"),
    ("ge;", "\u{2265}"),
    ("asymp;", "\u{2248}"),
    ("equiv;", "\u{2261}"),
    ("prop;", "\u{221d}"),
    ("infin;", "\u{221e}"),
    ("radic;", "\u{221a}"),
    ("sum;", "\u{2211}"),
    ("prod;", "\u{220f}"),
    ("int;", "\u{222b}"),
    ("part;", "\u{2202}"),
    ("nabla;", "\u{2207}"),
    ("isin;", "\u{2208}"),
    ("notin;", "\u{2209}"),
    ("cap;", "\u{2229}"),
    ("cup;", "\u{222a}"),
    ("sub;", "\u{2282}"),
    ("sup;", "\u{2283}"),
    ("forall;", "\u{2200}"),
    ("exist;", "\u{2203}"),
    ("empty;", "\u{2205}"),
    ("not;", "\u{ac}"),
    ("not", "\u{ac}"),
    ("and;", "\u{2227}"),
    ("or;", "\u{2228}"),
    ("larr;", "\u{2190}"),
    ("uarr;", "\u{2191}"),
    ("rarr;", "\u{2192}"),
    ("darr;", "\u{2193}"),
    ("harr;", "\u{2194}"),
    ("lArr;", "\u{21d0}"),
    ("uArr;", "\u{21d1}"),
    ("rArr;", "\u{21d2}"),
    ("dArr;", "\u{21d3}"),
    ("hArr;", "\u{21d4}"),
    // Latin-1 letters.
    ("Agrave;", "\u{c0}"),
    ("Aacute;", "\u{c1}"),
    ("Acirc;", "\u{c2}"),
    ("Atilde;", "\u{c3}"),
    ("Auml;", "\u{c4}"),
    ("Aring;", "\u{c5}"),
    ("AElig;", "\u{c6}"),
    ("Ccedil;", "\u{c7}"),
    ("Egrave;", "\u{c8}"),
    ("Eacute;", "\u{c9}"),
    ("Ecirc;", "\u{ca}"),
    ("Euml;", "\u{cb}"),
    ("Igrave;", "\u{cc}"),
    ("Iacute;", "\u{cd}"),
    ("Icirc;", "\u{ce}"),
    ("Iuml;", "\u{cf}"),
    ("ETH;", "\u{d0}"),
    ("Ntilde;", "\u{d1}"),
    ("Ograve;", "\u{d2}"),
    ("Oacute;", "\u{d3}"),
    ("Ocirc;", "\u{d4}"),
    ("Otilde;", "\u{d5}"),
    ("Ouml;", "\u{d6}"),
    ("Oslash;", "\u{d8}"),
    ("Ugrave;", "\u{d9}"),
    ("Uacute;", "\u{da}"),
    ("Ucirc;", "\u{db}"),
    ("Uuml;", "\u{dc}"),
    ("Yacute;", "\u{dd}"),
    ("THORN;", "\u{de}"),
    ("szlig;", "\u{df}"),
    ("agrave;", "\u{e0}"),
    ("aacute;", "\u{e1}"),
    ("acirc;", "\u{e2}"),
    ("atilde;", "\u{e3}"),
    ("auml;", "\u{e4}"),
    ("aring;", "\u{e5}"),
    ("aelig;", "\u{e6}"),
    ("ccedil;", "\u{e7}"),
    ("egrave;", "\u{e8}"),
    ("eacute;", "\u{e9}"),
    ("ecirc;", "\u{ea}"),
    ("euml;", "\u{eb}"),
    ("igrave;", "\u{ec}"),
    ("iacute;", "\u{ed}"),
    ("icirc;", "\u{ee}"),
    ("iuml;", "\u{ef}"),
    ("eth;", "\u{f0}"),
    ("ntilde;", "\u{f1}"),
    ("ograve;", "\u{f2}"),
    ("oacute;", "\u{f3}"),
    ("ocirc;", "\u{f4}"),
    ("otilde;", "\u{f5}"),
    ("ouml;", "\u{f6}"),
    ("oslash;", "\u{f8}"),
    ("ugrave;", "\u{f9}"),
    ("uacute;", "\u{fa}"),
    ("ucirc;", "\u{fb}"),
    ("uuml;", "\u{fc}"),
    ("yacute;", "\u{fd}"),
    ("thorn;", "\u{fe}"),
    ("yuml;", "\u{ff}"),
    ("OElig;", "\u{152}"),
    ("oelig;", "\u{153}"),
    ("Scaron;", "\u{160}"),
    ("scaron;", "\u{161}"),
    ("Yuml;", "\u{178}"),
    ("fnof;", "\u{192}"),
    ("circ;", "\u{2c6}"),
    ("tilde;", "\u{2dc}"),
    // Greek, which mathematical and scientific prose leans on.
    ("Alpha;", "\u{391}"),
    ("Beta;", "\u{392}"),
    ("Gamma;", "\u{393}"),
    ("Delta;", "\u{394}"),
    ("Epsilon;", "\u{395}"),
    ("Zeta;", "\u{396}"),
    ("Eta;", "\u{397}"),
    ("Theta;", "\u{398}"),
    ("Iota;", "\u{399}"),
    ("Kappa;", "\u{39a}"),
    ("Lambda;", "\u{39b}"),
    ("Mu;", "\u{39c}"),
    ("Nu;", "\u{39d}"),
    ("Xi;", "\u{39e}"),
    ("Omicron;", "\u{39f}"),
    ("Pi;", "\u{3a0}"),
    ("Rho;", "\u{3a1}"),
    ("Sigma;", "\u{3a3}"),
    ("Tau;", "\u{3a4}"),
    ("Upsilon;", "\u{3a5}"),
    ("Phi;", "\u{3a6}"),
    ("Chi;", "\u{3a7}"),
    ("Psi;", "\u{3a8}"),
    ("Omega;", "\u{3a9}"),
    ("alpha;", "\u{3b1}"),
    ("beta;", "\u{3b2}"),
    ("gamma;", "\u{3b3}"),
    ("delta;", "\u{3b4}"),
    ("epsilon;", "\u{3b5}"),
    ("zeta;", "\u{3b6}"),
    ("eta;", "\u{3b7}"),
    ("theta;", "\u{3b8}"),
    ("iota;", "\u{3b9}"),
    ("kappa;", "\u{3ba}"),
    ("lambda;", "\u{3bb}"),
    ("mu;", "\u{3bc}"),
    ("nu;", "\u{3bd}"),
    ("xi;", "\u{3be}"),
    ("omicron;", "\u{3bf}"),
    ("pi;", "\u{3c0}"),
    ("rho;", "\u{3c1}"),
    ("sigmaf;", "\u{3c2}"),
    ("sigma;", "\u{3c3}"),
    ("tau;", "\u{3c4}"),
    ("upsilon;", "\u{3c5}"),
    ("phi;", "\u{3c6}"),
    ("chi;", "\u{3c7}"),
    ("psi;", "\u{3c8}"),
    ("omega;", "\u{3c9}"),
];

/// The longest named reference in [`NAMED`], after the `&`. Bounds the
/// prefix window [`match_named`] has to consider.
const MAX_NAMED_LEN: usize = 8;

/// A decoded reference: its replacement text and how many characters
/// after the `&` it consumed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub text: String,
    pub consumed: usize,
}

/// Match the longest named reference at the start of `rest`, which is the
/// input *after* the `&`.
///
/// Longest-prefix, per spec: for `notin` both `not` and `notin;` are
/// candidates and the longer wins, while for `notit;` only `not` matches
/// and `it;` stays as text.
pub fn match_named(rest: &str) -> Option<Reference> {
    let window: String = rest.chars().take(MAX_NAMED_LEN).collect();
    let mut best: Option<Reference> = None;
    for (name, replacement) in NAMED {
        if !window.starts_with(name) {
            continue;
        }
        let consumed = name.chars().count();
        if best.as_ref().is_none_or(|b| consumed > b.consumed) {
            best = Some(Reference {
                text: (*replacement).to_owned(),
                consumed,
            });
        }
    }
    best
}

/// Map a numeric reference's raw value to the character it actually means.
///
/// The spec's substitutions, in order: null and out-of-range become the
/// replacement character, as do lone surrogates (they cannot be encoded
/// at all), and the C1 range takes its windows-1252 meaning.
pub fn numeric_replacement(code: u32) -> char {
    const REPLACEMENT: char = '\u{fffd}';
    match code {
        0 => REPLACEMENT,
        // Authoring tools emitted `&#147;` for a curly quote for years,
        // meaning the windows-1252 byte rather than the C1 control at
        // U+0093. Every browser honours it, so the same table that
        // decodes a windows-1252 document decides this too.
        0x80..=0x9f => windows_1252(code as u8),
        // Surrogate halves are not scalar values.
        0xd800..=0xdfff => REPLACEMENT,
        0x110000.. => REPLACEMENT,
        // Noncharacters and controls are a parse error but decode as
        // themselves; `from_u32` has already ruled out the rest.
        _ => char::from_u32(code).unwrap_or(REPLACEMENT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(input: &str) -> Option<(String, usize)> {
        match_named(input).map(|r| (r.text, r.consumed))
    }

    #[test]
    fn the_markup_critical_five_decode() {
        for (input, want) in [
            ("amp;", "&"),
            ("lt;", "<"),
            ("gt;", ">"),
            ("quot;", "\""),
            ("apos;", "'"),
        ] {
            assert_eq!(
                named(input).map(|(t, _)| t),
                Some(want.to_owned()),
                "{input}"
            );
        }
    }

    #[test]
    fn longest_prefix_wins_over_a_shorter_match() {
        // `not` and `notin;` both match; the longer one is correct.
        assert_eq!(named("notin;"), Some(("\u{2209}".into(), 6)));
    }

    #[test]
    fn an_unterminated_reference_still_matches_its_legacy_form() {
        // The classic spec example: `&notit;` is `¬it;`, not an error.
        assert_eq!(named("notit;"), Some(("\u{ac}".into(), 3)));
    }

    #[test]
    fn a_reference_without_a_legacy_form_needs_its_semicolon() {
        assert_eq!(named("euro"), None);
        assert_eq!(named("euro;"), Some(("\u{20ac}".into(), 5)));
    }

    #[test]
    fn unknown_names_do_not_match() {
        assert_eq!(named("bogusref;"), None);
        assert_eq!(named(""), None);
    }

    #[test]
    fn trailing_text_after_a_reference_is_left_alone() {
        let found = match_named("amp;rest").expect("amp; matches");
        assert_eq!(found.text, "&");
        assert_eq!(found.consumed, 4);
    }

    #[test]
    fn named_matching_is_case_sensitive() {
        // `&Dagger;` and `&dagger;` are different characters, so folding
        // case here would silently corrupt documents.
        assert_eq!(named("dagger;"), Some(("\u{2020}".into(), 7)));
        assert_eq!(named("Dagger;"), Some(("\u{2021}".into(), 7)));
    }

    #[test]
    fn the_c1_range_takes_its_windows_1252_meaning() {
        assert_eq!(numeric_replacement(0x80), '\u{20ac}', "euro, not U+0080");
        assert_eq!(numeric_replacement(0x93), '\u{201c}', "curly open quote");
        assert_eq!(numeric_replacement(0x99), '\u{2122}', "trademark");
    }

    #[test]
    fn null_surrogates_and_overlarge_values_become_the_replacement_char() {
        assert_eq!(numeric_replacement(0), '\u{fffd}');
        assert_eq!(numeric_replacement(0xd800), '\u{fffd}');
        assert_eq!(numeric_replacement(0xdfff), '\u{fffd}');
        assert_eq!(numeric_replacement(0x110000), '\u{fffd}');
        assert_eq!(numeric_replacement(u32::MAX), '\u{fffd}');
    }

    #[test]
    fn ordinary_code_points_decode_to_themselves() {
        assert_eq!(numeric_replacement(0x41), 'A');
        assert_eq!(numeric_replacement(0x27), '\'');
        assert_eq!(numeric_replacement(0x1f600), '\u{1f600}');
        assert_eq!(numeric_replacement(0x10ffff), '\u{10ffff}');
    }

    #[test]
    fn the_table_declares_its_own_longest_entry() {
        let longest = NAMED
            .iter()
            .map(|(name, _)| name.chars().count())
            .max()
            .expect("the table is not empty");
        assert_eq!(
            longest, MAX_NAMED_LEN,
            "MAX_NAMED_LEN must match the table, or long names stop matching"
        );
    }
}
