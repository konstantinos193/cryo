//! The palette every shell document shares, in the three themes the
//! settings page offers.
//!
//! One file holds every colour — `theme/tokens.css`, one `:root` block
//! per theme, served once and linked by each page before its own sheet.
//! Page stylesheets spend the tokens; none of them declares one, and
//! none of them writes a literal colour, because a literal is a colour
//! no theme can change.
//!
//! The pick itself lives in the shell origin's localStorage (the
//! `cryo.theme` key), the way the search-engine pick does: the settings
//! page writes it, `theme/theme.js` reads it in every document's head
//! and paints `data-theme` before the first frame, and storage is
//! origin-scoped, so no command channel is involved. What a document
//! cannot do is paint the window icon or the colour a view shows before
//! its first frame — for those the app keeps its own copy of the pick in
//! `theme.json` and hands this module's values to the window and to
//! every spawned view.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// The custom-property blocks, served at `/theme/tokens.css`.
pub(crate) const TOKENS: &str = include_str!("theme/tokens.css");

/// The head script that applies the stored pick, served at
/// `/theme/theme.js`.
pub(crate) const SCRIPT: &str = include_str!("theme/theme.js");

/// The crystal, with its five colours left as `{plate}`, `{core}`,
/// `{mid}`, `{hi}` and `{lit}` — the one copy of that geometry the shell
/// ships. Rendered per theme into the favicon (on its plate) and the
/// page art (plate left transparent).
const MARK: &str = include_str!("theme/mark.svg");

/// Path on the internal protocol serving [`json`]. Registered in
/// `pages::internal_page`; the settings page `fetch`es `/themes`.
pub const PATH: &str = "/themes";

/// Everything served under `/theme/`: the token blocks, the head script
/// and one favicon plus one page mark per theme. `None` for anything
/// else — including a slug no theme answers to, which 404s rather than
/// being served in the default's colours.
pub(crate) fn asset(path: &str) -> Option<&'static str> {
    match path.strip_prefix("/theme/")? {
        "tokens.css" => Some(TOKENS),
        "theme.js" => Some(SCRIPT),
        art => {
            let (kind, slug) = art.strip_suffix(".svg")?.split_once('-')?;
            let theme = Theme::from_slug(slug)?;
            match kind {
                "icon" => Some(theme.icon_svg()),
                "mark" => Some(theme.mark_svg()),
                _ => None,
            }
        }
    }
}

/// A shell theme — a value object. The whole palette hangs off it: the
/// token block a document paints with, the mark it shows, the window
/// icon the OS draws, and the colour a view fills before its first
/// frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    /// The shipped look: one blue-tinted dark ramp and an ice accent.
    Glacier,
    /// Paper and ink, with the same cold accent darkened for white.
    Light,
    /// The light structure over violet paper.
    Lilac,
}

/// Every theme the settings page offers, in display order — `DEFAULT`
/// is `ALL[0]`.
pub const ALL: &[Theme] = &[Theme::Glacier, Theme::Light, Theme::Lilac];

impl Theme {
    /// What a fresh profile, an unreadable pick and an incognito window
    /// all paint.
    pub const DEFAULT: Theme = Theme::Glacier;

    /// The hydration door (AGENTS §6): a stored or wire slug → a theme.
    /// `None` for anything else — callers fall back to `DEFAULT` rather
    /// than inventing a palette.
    pub fn from_slug(slug: &str) -> Option<Self> {
        ALL.iter().copied().find(|t| t.slug() == slug.trim())
    }

    /// The storage, wire and asset-path name.
    pub fn slug(&self) -> &'static str {
        match self {
            Self::Glacier => "glacier",
            Self::Light => "light",
            Self::Lilac => "lilac",
        }
    }

    /// What the settings page calls it.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Glacier => "Glacier",
            Self::Light => "Light",
            Self::Lilac => "Lilac",
        }
    }

    /// Resource id of this theme's icon inside the executable — the ids
    /// `assets/icon.rc` compiles, in this order. `1` is also the icon
    /// Explorer shows for `cryo.exe`, so the default theme owns it.
    #[cfg(windows)]
    pub fn icon_resource(&self) -> u16 {
        match self {
            Self::Glacier => 1,
            Self::Light => 2,
            Self::Lilac => 3,
        }
    }

    /// What a content view paints before its first document composites
    /// (RGBA) — the page canvas `--bg0`, so a fresh view never flashes a
    /// colour no theme uses.
    pub fn content_background(&self) -> (u8, u8, u8, u8) {
        art(*self).content
    }

    /// The same guard for the chrome strip: the toolbar's `--bg1`.
    pub fn chrome_background(&self) -> (u8, u8, u8, u8) {
        art(*self).chrome
    }

    /// The favicon every shell document wears, served at
    /// `/theme/icon-<slug>.svg`: the crystal on its plate.
    pub fn icon_svg(&self) -> &'static str {
        &art(*self).icon
    }

    /// The page art, served at `/theme/mark-<slug>.svg`: the same
    /// crystal with no plate behind it, which is what the new-tab hero
    /// and the error page draw through `--mark-url`.
    pub fn mark_svg(&self) -> &'static str {
        &art(*self).mark
    }

    /// Position in [`ALL`], which is how the built art is keyed.
    fn index(&self) -> usize {
        ALL.iter()
            .position(|t| t == self)
            .expect("every theme is in ALL")
    }
}

/// The theme list as the settings page consumes it: `[{slug, name}]`.
/// Built once from [`ALL`] so the wire cannot drift from the domain
/// list — a `OnceLock`, not a hand-kept second copy (§5 bans state
/// singletons; this is a derived constant).
pub fn json() -> &'static str {
    static JSON: OnceLock<String> = OnceLock::new();
    JSON.get_or_init(|| {
        let rows: Vec<serde_json::Value> = ALL
            .iter()
            .map(|t| serde_json::json!({ "slug": t.slug(), "name": t.name() }))
            .collect();
        serde_json::to_string(&rows).expect("theme rows are plain strings")
    })
}

/// Everything derived from a theme's tokens: the two documents built
/// from [`MARK`] and the two colours the app hands the engine.
struct Art {
    icon: String,
    mark: String,
    content: (u8, u8, u8, u8),
    chrome: (u8, u8, u8, u8),
}

/// The derived art for every theme, built once on first use. Derived
/// constants, not state (see [`json`]).
fn art(theme: Theme) -> &'static Art {
    static ART: OnceLock<Vec<Art>> = OnceLock::new();
    &ART.get_or_init(|| ALL.iter().map(|t| build(*t)).collect())[theme.index()]
}

fn build(theme: Theme) -> Art {
    let fill = |name: &str| token(theme, name);
    let paint = |plate: &str| {
        MARK.replace("{plate}", plate)
            .replace("{core}", fill("--mark-core"))
            .replace("{mid}", fill("--mark-mid"))
            .replace("{hi}", fill("--mark-hi"))
            .replace("{lit}", fill("--mark-lit"))
    };
    Art {
        icon: paint(fill("--mark-plate")),
        mark: paint("transparent"),
        content: opaque(fill("--bg0")),
        chrome: opaque(fill("--bg1")),
    }
}

/// A token's value out of `theme/tokens.css`. The stylesheet is the one
/// place a colour is written down (AGENTS §4), so the values the app
/// needs natively are read from it rather than typed again here.
///
/// Panics when a theme's block is missing the token: the sheet is
/// compiled in, so that is a bug in this crate, not a condition
/// (AGENTS §7), and `every_theme_defines_every_token` catches it in CI.
fn token(theme: Theme, name: &str) -> &'static str {
    let slug = theme.slug();
    let at = TOKENS
        .find(&format!("[data-theme=\"{slug}\"]"))
        .unwrap_or_else(|| panic!("tokens.css has no block for {slug}"));
    let block = &TOKENS[at..];
    let block = &block[..block.find('}').expect("a token block is closed")];
    let at = block
        .find(&format!("{name}:"))
        .unwrap_or_else(|| panic!("the {slug} block does not define {name}"));
    let value = &block[at + name.len() + 1..];
    value[..value.find(';').expect("a declaration ends in a semicolon")].trim()
}

/// An opaque RGBA from a `#rrggbb` token. Panics for anything else, for
/// the same reason [`token`] does.
fn opaque(hex: &str) -> (u8, u8, u8, u8) {
    let digits = hex.strip_prefix('#').filter(|d| d.len() == 6);
    let byte = |at: usize| {
        digits
            .and_then(|d| u8::from_str_radix(&d[at..at + 2], 16).ok())
            .unwrap_or_else(|| panic!("{hex} is not a #rrggbb colour"))
    };
    (byte(0), byte(2), byte(4), 255)
}

const CURRENT_VERSION: u32 = 1;

/// The stored pick, as it sits on disk. A DTO at the boundary: the slug
/// is text here and only becomes a [`Theme`] through
/// [`Theme::from_slug`] (AGENTS §6).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    version: u32,
    theme: String,
}

/// `theme.json` under the per-user data dir (`crate::data_dir`).
pub fn default_path() -> PathBuf {
    crate::data_dir().join("theme.json")
}

/// The app's copy of the pick, or `None` — a missing, stale-version or
/// corrupt file all mean "paint the default", never a launch failure.
/// The documents read the pick from localStorage; this file exists so
/// the window icon and a view's first frame do not have to wait for a
/// page to run.
pub fn load(path: &Path) -> Option<Theme> {
    let bytes = crate::read_file(path).ok()??;
    let file: ThemeFile = serde_json::from_slice(&bytes).ok()?;
    if file.version != CURRENT_VERSION {
        return None;
    }
    Theme::from_slug(&file.theme)
}

/// Remember the pick for the next launch. One tiny file, written
/// atomically through `crate::write_file_atomic`.
pub fn save(path: &Path, theme: Theme) -> Result<(), std::io::Error> {
    let file = ThemeFile {
        version: CURRENT_VERSION,
        theme: theme.slug().to_string(),
    };
    let bytes = serde_json::to_vec(&file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    crate::write_file_atomic(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every colour the shell paints is one of these names, so a theme
    /// that forgets one leaves an element painted with nothing at all.
    fn token_names(theme: Theme) -> Vec<&'static str> {
        let at = TOKENS
            .find(&format!("[data-theme=\"{}\"]", theme.slug()))
            .expect("the theme has a block");
        let block = &TOKENS[at..];
        let block = &block[..block.find('}').expect("the block is closed")];
        // A declaration is the only thing in the block that starts a
        // line with `--`; a comment mentioning a token does not.
        let mut names: Vec<&'static str> = block
            .match_indices("\n  --")
            .map(|(at, _)| {
                let rest = &block[at + 3..];
                &rest[..rest.find(':').expect("a declaration has a colon")]
            })
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    #[test]
    fn every_theme_defines_every_token() {
        let glacier = token_names(Theme::DEFAULT);
        assert!(glacier.len() > 40, "the palette lost its tokens");
        for theme in ALL {
            assert_eq!(
                token_names(*theme),
                glacier,
                "the {} block and the default block declare different tokens",
                theme.slug()
            );
            // Every value the app reads natively has to parse, not just
            // be present.
            assert_eq!(theme.content_background().3, 255);
            assert_eq!(theme.chrome_background().3, 255);
        }
    }

    /// A page sheet that declares its own token is a second source of
    /// truth for a colour, which is how the shell's ramps drifted apart
    /// before. A sheet that writes a literal colour is worse: it is a
    /// colour the light themes cannot reach.
    #[test]
    fn the_palette_lives_here_and_nowhere_else() {
        for (name, sheet) in [
            ("chrome.css", include_str!("chrome/chrome.css")),
            ("library.css", include_str!("chrome/library.css")),
            ("newtab.css", include_str!("newtab/newtab.css")),
            ("settings.css", include_str!("settings/settings.css")),
            ("error.css", include_str!("error/error.css")),
        ] {
            assert!(
                !sheet.contains(":root"),
                "{name} declares tokens; they belong in tokens.css"
            );
            for (at, _) in sheet.match_indices('#') {
                let rest = &sheet[at + 1..];
                let hex = rest.chars().take_while(|c| c.is_ascii_hexdigit()).count();
                // A colour is three, four, six or eight hex digits and
                // then something that is not part of a name — `#bar` is
                // an id, `#eaf9fd` was a colour.
                let ends = rest[hex..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '-' && c != '_');
                // Masks are the one place a colour is not a colour: the
                // gradient there is an alpha ramp, the same in every
                // theme, so `#000` stays written down.
                let line = sheet[..at].rsplit('\n').next().unwrap_or("");
                assert!(
                    !(matches!(hex, 3 | 4 | 6 | 8) && ends) || line.contains("mask"),
                    "{name} writes a literal colour at {:?}",
                    &rest[..hex]
                );
            }
            assert!(
                !sheet.contains("rgba(255, 255, 255") && !sheet.contains("rgba(255,255,255"),
                "{name} hardcodes a white overlay; spend --hair-rgb or --sheen-rgb"
            );
        }
        assert!(TOKENS.contains("--accent:"));
    }

    #[test]
    fn default_is_glacier_and_slugs_round_trip() {
        assert_eq!(Theme::DEFAULT.slug(), "glacier");
        assert_eq!(ALL[0], Theme::DEFAULT);
        for theme in ALL {
            assert_eq!(Theme::from_slug(theme.slug()), Some(*theme));
        }
        for bad in ["", "  ", "dark", "white", "glacier2"] {
            assert_eq!(Theme::from_slug(bad), None, "accepted {bad:?}");
        }
    }

    /// The head script validates the pick on its own — it runs before
    /// anything can ask the app — so its list has to be this one.
    #[test]
    fn the_head_script_knows_the_same_themes() {
        let at = SCRIPT
            .find("var THEMES = [")
            .expect("the script lists them");
        let list = &SCRIPT[at..];
        let list = &list[..list.find(']').expect("the list is closed")];
        let slugs: Vec<&str> = list
            .match_indices('"')
            .step_by(2)
            .map(|(at, _)| {
                let rest = &list[at + 1..];
                &rest[..rest.find('"').expect("a slug is quoted")]
            })
            .collect();
        assert_eq!(
            slugs,
            ALL.iter().map(|t| t.slug()).collect::<Vec<_>>(),
            "theme.js and ALL disagree about the themes"
        );
        assert!(SCRIPT.contains("\"cryo.theme\""));
    }

    #[test]
    fn json_mirrors_all() {
        let rows: serde_json::Value = serde_json::from_str(json()).expect("theme JSON parses");
        let rows = rows.as_array().expect("theme JSON is an array");
        assert_eq!(rows.len(), ALL.len(), "the wire list and ALL drifted");
        for (row, theme) in rows.iter().zip(ALL) {
            assert_eq!(row["slug"].as_str().unwrap(), theme.slug());
            assert_eq!(row["name"].as_str().unwrap(), theme.name());
        }
    }

    /// One geometry, two renderings: the favicon stands on the theme's
    /// plate, the page art has none. Both carry that theme's facets and
    /// no placeholder.
    #[test]
    fn each_theme_paints_the_one_crystal_in_its_own_colours() {
        for theme in ALL {
            for svg in [theme.icon_svg(), theme.mark_svg()] {
                assert!(!svg.contains('{'), "{} left a placeholder", theme.slug());
                assert!(svg.contains(token(*theme, "--mark-lit")));
                assert!(svg.contains("viewBox=\"0 0 96 96\""));
            }
            assert!(theme.icon_svg().contains(token(*theme, "--mark-plate")));
            assert!(theme.mark_svg().contains("fill=\"transparent\""));
            // A theme's art is its own.
            for other in ALL.iter().filter(|o| *o != theme) {
                assert_ne!(theme.mark_svg(), other.mark_svg());
            }
        }
    }

    /// The flash guard has to be the colour the page actually paints.
    #[test]
    fn the_backgrounds_come_from_the_canvas_tokens() {
        assert_eq!(Theme::Glacier.content_background(), (11, 14, 18, 255));
        assert_eq!(Theme::Glacier.chrome_background(), (17, 20, 26, 255));
        assert_eq!(Theme::Light.content_background(), (244, 246, 250, 255));
    }

    /// The ids are a contract with the resource script: an icon
    /// compiled under another number loads as the wrong theme's, or
    /// not at all.
    #[cfg(windows)]
    #[test]
    fn every_theme_has_its_own_icon_in_the_resource_script() {
        let rc = include_str!("../../assets/icon.rc");
        for shell_theme in ALL {
            let file = if *shell_theme == Theme::DEFAULT {
                "assets/icon.ico".to_string()
            } else {
                format!("assets/icon-{}.ico", shell_theme.slug())
            };
            assert!(
                rc.contains(&format!("{} ICON \"{file}\"", shell_theme.icon_resource())),
                "icon.rc does not compile {file} as {}",
                shell_theme.icon_resource()
            );
        }
        // Explorer shows the lowest id for the executable, so the
        // default theme has to own it.
        assert_eq!(Theme::DEFAULT.icon_resource(), 1);
    }

    fn temp(tag: &str) -> PathBuf {
        crate::test_dir(&format!("theme-{tag}")).join("theme.json")
    }

    #[test]
    fn missing_file_paints_the_default() {
        assert_eq!(load(&temp("missing")), None);
    }

    #[test]
    fn a_saved_pick_round_trips() {
        let path = temp("roundtrip");
        save(&path, Theme::Lilac).unwrap();
        assert_eq!(load(&path), Some(Theme::Lilac));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_pick_no_theme_answers_to_is_refused() {
        let path = temp("unknown");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for body in [
            r#"{"version":1,"theme":"solarized"}"#,
            r#"{"version":2,"theme":"lilac"}"#,
            "not json",
        ] {
            std::fs::write(&path, body).unwrap();
            assert_eq!(load(&path), None, "accepted {body}");
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
