//! Finding the font a page asked for.
//!
//! CSS names families; a machine has files. Nothing connects the two
//! except convention, and the conventions differ per platform — so this
//! module owns the whole of that guesswork and nothing above it has to
//! know that `font-family: Consolas` means `consola.ttf`.
//!
//! Three things happen here, in order:
//!
//! 1. **Substitution.** `Helvetica` is not on Windows, and every browser
//!    silently serves Arial instead. A page that asks for a font the
//!    machine lacks should still look close to intended, so the common
//!    substitutions are listed rather than left to the fallback.
//! 2. **Generic families.** `sans-serif` is not a font, it is a request
//!    for whatever the system's is. Each generic maps to an ordered list
//!    of real families to try.
//! 3. **Lookup.** The candidate file stems are tried in the platform's
//!    font directories, first by exact name and then by scanning — Linux
//!    nests fonts in per-family directories, so a flat guess is not
//!    enough there.
//!
//! What is deliberately not here: weight and style matching. Asking for
//! bold gets the regular file, so bold text is currently drawn in the
//! regular weight rather than synthesised or substituted. Doing it
//! properly means reading each file's `OS/2` table and picking among a
//! family's faces, which is the next step.

use std::path::{Path, PathBuf};

use super::outline_font::{FontSet, OutlineFont};

/// Families the web asks for that a machine may spell differently, or
/// not have at all.
///
/// The right-hand side is a list of file stems to try in order.
static SUBSTITUTIONS: &[(&str, &[&str])] = &[
    (
        "arial",
        &["arial", "Arial", "LiberationSans-Regular", "DejaVuSans"],
    ),
    (
        "helvetica",
        &["arial", "Helvetica", "LiberationSans-Regular", "DejaVuSans"],
    ),
    ("segoe ui", &["segoeui", "SegoeUI"]),
    ("system-ui", &["segoeui", "SFNSText", "DejaVuSans"]),
    ("tahoma", &["tahoma", "Tahoma", "DejaVuSans"]),
    ("verdana", &["verdana", "Verdana", "DejaVuSans"]),
    ("calibri", &["calibri", "Calibri", "Carlito-Regular"]),
    (
        "times new roman",
        &["times", "Times New Roman", "LiberationSerif-Regular"],
    ),
    (
        "times",
        &["times", "Times New Roman", "LiberationSerif-Regular"],
    ),
    ("georgia", &["georgia", "Georgia", "DejaVuSerif"]),
    ("garamond", &["GARA", "Garamond", "EBGaramond-Regular"]),
    (
        "courier new",
        &["cour", "Courier New", "LiberationMono-Regular"],
    ),
    (
        "courier",
        &["cour", "Courier New", "LiberationMono-Regular"],
    ),
    ("consolas", &["consola", "Consolas", "DejaVuSansMono"]),
    ("menlo", &["consola", "Menlo", "DejaVuSansMono"]),
    ("monaco", &["consola", "Monaco", "DejaVuSansMono"]),
    ("trebuchet ms", &["trebuc", "Trebuchet MS"]),
    ("comic sans ms", &["comic", "Comic Sans MS"]),
    ("impact", &["impact", "Impact"]),
    ("arial black", &["ariblk", "Arial Black"]),
];

/// The generic families CSS defines, each an ordered wish list.
static GENERICS: &[(&str, &[&str])] = &[
    (
        "sans-serif",
        &["segoeui", "arial", "DejaVuSans", "LiberationSans-Regular"],
    ),
    (
        "serif",
        &["times", "georgia", "DejaVuSerif", "LiberationSerif-Regular"],
    ),
    (
        "monospace",
        &[
            "consola",
            "cour",
            "DejaVuSansMono",
            "LiberationMono-Regular",
        ],
    ),
    ("cursive", &["comic", "Comic Sans MS", "DejaVuSans"]),
    ("fantasy", &["impact", "Impact", "DejaVuSans"]),
    ("ui-sans-serif", &["segoeui", "arial", "DejaVuSans"]),
    ("ui-serif", &["times", "DejaVuSerif"]),
    ("ui-monospace", &["consola", "DejaVuSansMono"]),
];

/// The families tried when a page's whole stack is unavailable.
const LAST_RESORT: &[&str] = &[
    "segoeui",
    "arial",
    "tahoma",
    "verdana",
    "DejaVuSans",
    "LiberationSans-Regular",
    "Helvetica",
];

/// Filename suffixes vendors use for a family's other faces.
///
/// There is no standard here and never was: Microsoft writes
/// `arialbd`, `segoeuib` and `consolab` for the same idea, and uses `z`
/// for bold-italic in some families and `bi` in others. Trying them all
/// and believing the file is cheaper than encoding a table per vendor.
const FACE_SUFFIXES: &[&str] = &[
    // Regular first, so a family always has one if it has anything.
    "",
    "bd",
    "b",
    "i",
    "it",
    "bi",
    "z",
    "-Bold",
    "-Italic",
    "-BoldItalic",
    "-Regular",
];

/// Extensions worth opening. `.ttc` is excluded on purpose: a collection
/// holds several fonts and the reader rejects it, so trying one would
/// only produce a confusing error.
const EXTENSIONS: &[&str] = &["ttf", "otf"];

/// Locates and loads fonts from the machine's font directories.
#[derive(Clone, Debug)]
pub struct SystemFonts {
    directories: Vec<PathBuf>,
}

impl Default for SystemFonts {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemFonts {
    /// Look in the platform's usual font directories.
    pub fn new() -> Self {
        Self {
            directories: font_directories(),
        }
    }

    /// Look only in the given directories — for tests, and for a build
    /// that ships its own fonts rather than using the system's.
    pub fn with_directories(directories: Vec<PathBuf>) -> Self {
        Self { directories }
    }

    pub fn directories(&self) -> &[PathBuf] {
        &self.directories
    }

    /// Load one CSS family, trying its substitutions in order.
    pub fn load_family(&self, family: &str) -> Option<OutlineFont> {
        for stem in candidates(family) {
            if let Some(font) = self.load_stem(stem, family) {
                return Some(font);
            }
        }
        None
    }

    /// Load the first family in `stack` that this machine has.
    pub fn load_stack(&self, stack: &[String]) -> Option<OutlineFont> {
        stack.iter().find_map(|family| self.load_family(family))
    }

    /// A font to fall back on when nothing a page asked for exists.
    pub fn load_default(&self) -> Option<OutlineFont> {
        LAST_RESORT
            .iter()
            .find_map(|stem| self.load_stem(stem, "sans-serif"))
    }

    /// Every face of the first last-resort family this machine has.
    ///
    /// A fallback of one regular face costs more than it looks: a page
    /// whose families are all unavailable then draws its headings, its
    /// labels and its body at the same weight, and the whole thing
    /// reads flat. The faces are what keep bold bold.
    pub fn load_default_faces(&self) -> Vec<OutlineFont> {
        LAST_RESORT
            .iter()
            .map(|stem| self.load_faces(stem))
            .find(|faces| !faces.is_empty())
            .unwrap_or_default()
    }

    /// Every face of a family this machine has: regular, bold, italic
    /// and bold italic, as far as they exist.
    ///
    /// The suffixes are vendor conventions and only decide *which files
    /// to open* — each file is then asked what it actually is, so a
    /// wrong guess costs a failed read rather than a mislabelled face.
    pub fn load_faces(&self, family: &str) -> Vec<OutlineFont> {
        let mut faces: Vec<OutlineFont> = Vec::new();
        for stem in candidates(family) {
            for suffix in FACE_SUFFIXES {
                let Some(font) = self.load_stem(&format!("{stem}{suffix}"), family) else {
                    continue;
                };
                // Two conventions can point at one file; keep each face
                // once, judged by what the file says it is.
                if faces.iter().any(|seen| seen.style() == font.style()) {
                    continue;
                }
                faces.push(font);
            }
            if !faces.is_empty() {
                // This stem is the family; later ones are substitutes.
                break;
            }
        }
        faces
    }

    /// Build a set covering every family in `families`, plus a default.
    ///
    /// Loading up front rather than on demand keeps the set immutable
    /// while a page is laid out and painted — no cache to borrow
    /// mutably from inside a render.
    pub fn load_set(&self, families: &[String]) -> FontSet {
        let mut set = FontSet::new();
        for family in families {
            for face in self.load_faces(family) {
                set.insert(family, face);
            }
        }
        let mut fallbacks = self.load_default_faces().into_iter();
        if let Some(first) = fallbacks.next() {
            set.set_fallback(first);
            for face in fallbacks {
                set.add_fallback(face);
            }
        }
        set
    }

    /// Try one file stem in every directory.
    fn load_stem(&self, stem: &str, family: &str) -> Option<OutlineFont> {
        for directory in &self.directories {
            for extension in EXTENSIONS {
                let path = directory.join(format!("{stem}.{extension}"));
                if let Ok(data) = std::fs::read(&path) {
                    if let Ok(font) = OutlineFont::from_bytes(data, family) {
                        return Some(font);
                    }
                }
            }
            // Linux keeps fonts in per-family subdirectories, so an
            // exact path is not enough there.
            if let Some(found) = scan_for(directory, stem, 2) {
                if let Ok(data) = std::fs::read(&found) {
                    if let Ok(font) = OutlineFont::from_bytes(data, family) {
                        return Some(font);
                    }
                }
            }
        }
        None
    }
}

/// The file stems worth trying for a CSS family name.
///
/// The name itself comes first — a machine that really does have
/// `Georgia.ttf` should use it — then the substitutions, then the
/// generic lists.
fn candidates(family: &str) -> Vec<&str> {
    let lowered = family.to_ascii_lowercase();
    let mut out = vec![family];
    for (name, stems) in SUBSTITUTIONS.iter().chain(GENERICS) {
        if *name == lowered {
            out.extend_from_slice(stems);
        }
    }
    out
}

/// Search `directory` for a file whose stem matches, up to `depth` deep.
fn scan_for(directory: &Path, stem: &str, depth: u8) -> Option<PathBuf> {
    let entries = std::fs::read_dir(directory).ok()?;
    let mut subdirectories = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            subdirectories.push(path);
            continue;
        }
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        if !EXTENSIONS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(extension))
        {
            continue;
        }
        let matches = path
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(|found| found.eq_ignore_ascii_case(stem));
        if matches {
            return Some(path);
        }
    }
    if depth == 0 {
        return None;
    }
    subdirectories
        .into_iter()
        .find_map(|path| scan_for(&path, stem, depth - 1))
}

/// Where this platform keeps fonts.
///
/// Directories that do not exist are dropped, so the list is always
/// usable and a caller never has to check.
pub fn font_directories() -> Vec<PathBuf> {
    let mut directories: Vec<PathBuf> = Vec::new();
    let environment = |name: &str| std::env::var_os(name).map(PathBuf::from);

    if cfg!(target_os = "windows") {
        if let Some(windows) = environment("WINDIR") {
            directories.push(windows.join("Fonts"));
        }
        directories.push(PathBuf::from("C:/Windows/Fonts"));
        if let Some(local) = environment("LOCALAPPDATA") {
            directories.push(local.join("Microsoft/Windows/Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        directories.push(PathBuf::from("/System/Library/Fonts"));
        directories.push(PathBuf::from("/System/Library/Fonts/Supplemental"));
        directories.push(PathBuf::from("/Library/Fonts"));
        if let Some(home) = environment("HOME") {
            directories.push(home.join("Library/Fonts"));
        }
    } else {
        directories.push(PathBuf::from("/usr/share/fonts"));
        directories.push(PathBuf::from("/usr/local/share/fonts"));
        if let Some(home) = environment("HOME") {
            directories.push(home.join(".local/share/fonts"));
            directories.push(home.join(".fonts"));
        }
    }

    directories.retain(|path| path.is_dir());
    directories.dedup();
    directories
}

#[cfg(test)]
mod tests {
    use super::super::outline_font::FaceRequest;
    use super::super::{GlyphSource, TextMeasure};
    use super::*;

    /// Whether this machine has any fonts at all. Tests that need a real
    /// one return early rather than failing on a bare container.
    fn has_fonts() -> bool {
        !font_directories().is_empty()
    }

    #[test]
    fn a_family_tries_its_own_name_first() {
        let tried = candidates("Georgia");
        assert_eq!(tried[0], "Georgia", "the literal name comes first");
    }

    #[test]
    fn substitutions_are_offered_after_the_literal_name() {
        let tried = candidates("Helvetica");
        assert!(tried.contains(&"arial"), "the usual stand-in: {tried:?}");
    }

    #[test]
    fn family_matching_ignores_case() {
        assert_eq!(candidates("ARIAL").len(), candidates("arial").len());
        assert!(candidates("Times New Roman").contains(&"times"));
    }

    #[test]
    fn generic_families_map_to_real_ones() {
        assert!(candidates("sans-serif").contains(&"arial"));
        assert!(candidates("serif").contains(&"times"));
        assert!(candidates("monospace").contains(&"consola"));
        assert!(candidates("cursive").contains(&"comic"));
    }

    #[test]
    fn an_unknown_family_offers_only_itself() {
        let tried = candidates("Nonexistent Typeface");
        assert_eq!(tried, ["Nonexistent Typeface"]);
    }

    #[test]
    fn every_substitution_and_generic_key_is_lowercase() {
        // Lookup folds the query once; a capitalised key would never
        // match and the entry would be silently dead.
        for (name, _) in SUBSTITUTIONS.iter().chain(GENERICS) {
            assert_eq!(*name, name.to_ascii_lowercase(), "{name} is not folded");
        }
    }

    #[test]
    fn no_substitution_list_is_empty() {
        for (name, stems) in SUBSTITUTIONS.iter().chain(GENERICS) {
            assert!(!stems.is_empty(), "{name} substitutes to nothing");
        }
    }

    #[test]
    fn the_reported_directories_all_exist() {
        for directory in font_directories() {
            assert!(directory.is_dir(), "{directory:?} was listed but is absent");
        }
    }

    #[test]
    fn an_empty_directory_list_finds_nothing() {
        let fonts = SystemFonts::with_directories(Vec::new());
        assert!(fonts.directories().is_empty());
        assert!(fonts.load_family("Arial").is_none());
        assert!(fonts.load_default().is_none());
        assert!(fonts.load_stack(&["Arial".to_owned()]).is_none());
    }

    #[test]
    fn a_directory_that_does_not_exist_is_harmless() {
        let fonts = SystemFonts::with_directories(vec![PathBuf::from("/no/such/place")]);
        assert!(fonts.load_family("Arial").is_none());
    }

    #[test]
    fn this_machine_has_a_default_font() {
        if !has_fonts() {
            return;
        }
        let fonts = SystemFonts::new();
        let font = fonts.load_default().expect("some usable font exists");
        assert!(font.file().glyph_count() > 50);
    }

    #[test]
    fn a_missing_first_directory_does_not_stop_font_lookup() {
        let fonts = SystemFonts::new();
        if fonts.load_default().is_none() {
            return;
        }
        let mut directories = vec![PathBuf::from("/no/such/cryo-font-directory")];
        directories.extend_from_slice(fonts.directories());
        assert!(SystemFonts::with_directories(directories)
            .load_default()
            .is_some());
    }

    #[test]
    fn a_generic_family_resolves_to_a_real_font() {
        if !has_fonts() {
            return;
        }
        let fonts = SystemFonts::new();
        for generic in ["sans-serif", "serif", "monospace"] {
            let font = fonts
                .load_family(generic)
                .unwrap_or_else(|| panic!("{generic} should resolve"));
            assert!(font.file().glyph_id('A').is_some(), "{generic} lacks 'A'");
        }
    }

    #[test]
    fn a_stack_takes_the_first_family_that_exists() {
        if !has_fonts() {
            return;
        }
        let fonts = SystemFonts::new();
        let stack = vec![
            "Absolutely Not Installed".to_owned(),
            "sans-serif".to_owned(),
        ];
        assert!(fonts.load_stack(&stack).is_some());
    }

    #[test]
    fn a_stack_of_nothing_that_exists_resolves_to_nothing() {
        let fonts = SystemFonts::new();
        let stack = vec!["Absolutely Not Installed".to_owned()];
        assert!(fonts.load_stack(&stack).is_none());
    }

    #[test]
    fn a_loaded_set_covers_the_families_it_was_given() {
        if !has_fonts() {
            return;
        }
        let fonts = SystemFonts::new();
        let set = fonts.load_set(&["serif".to_owned(), "monospace".to_owned()]);
        assert_eq!(set.len(), 2);
        assert!(set.resolve(&["serif".to_owned()]).is_some());
        // An unlisted family still gets the fallback.
        assert!(set.resolve(&["Nothing At All".to_owned()]).is_some());
    }

    #[test]
    fn a_monospace_font_really_is_monospaced() {
        if !has_fonts() {
            return;
        }
        let Some(font) = SystemFonts::new().load_family("monospace") else {
            return;
        };
        let narrow = font.advance_px('i', 32.0);
        let wide = font.advance_px('W', 32.0);
        assert!(
            (narrow - wide).abs() < 0.01,
            "monospace should give i ({narrow}) and W ({wide}) one width"
        );
    }

    #[test]
    fn a_serif_and_a_sans_font_are_different_files() {
        if !has_fonts() {
            return;
        }
        let fonts = SystemFonts::new();
        let (Some(serif), Some(sans)) =
            (fonts.load_family("serif"), fonts.load_family("sans-serif"))
        else {
            return;
        };
        // Different typefaces disagree about something: units per em,
        // ascent, or how wide a letter is.
        let differs = serif.file().metrics() != sans.file().metrics()
            || (serif.advance_px('W', 32.0) - sans.advance_px('W', 32.0)).abs() > 0.01;
        assert!(differs, "serif and sans-serif resolved to the same face");
    }

    #[test]
    fn scanning_finds_nothing_in_an_empty_place() {
        assert!(scan_for(Path::new("/no/such/place"), "arial", 2).is_none());
    }

    #[test]
    fn a_family_offers_more_than_one_face() {
        if !has_fonts() {
            return;
        }
        let faces = SystemFonts::new().load_faces("sans-serif");
        if faces.len() < 2 {
            // A machine with only a regular face is legitimate.
            return;
        }
        assert!(
            faces.iter().any(|face| face.style().weight >= 600),
            "a bold face should have been found: {:?}",
            faces.iter().map(|f| f.style()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn discovered_faces_are_distinct() {
        if !has_fonts() {
            return;
        }
        let faces = SystemFonts::new().load_faces("sans-serif");
        let mut styles: Vec<_> = faces.iter().map(|face| face.style()).collect();
        let before = styles.len();
        styles.sort_by_key(|style| (style.weight, style.italic));
        styles.dedup();
        assert_eq!(styles.len(), before, "the same face was loaded twice");
    }

    #[test]
    fn a_set_keeps_every_face_under_one_family() {
        if !has_fonts() {
            return;
        }
        let fonts = SystemFonts::new();
        let set = fonts.load_set(&["sans-serif".to_owned()]);
        assert_eq!(set.len(), 1, "one family");
        assert!(set.face_count() >= 1, "with at least its regular face");
    }

    #[test]
    fn asking_for_bold_gets_a_heavier_face_when_one_exists() {
        if !has_fonts() {
            return;
        }
        let fonts = SystemFonts::new();
        let set = fonts.load_set(&["sans-serif".to_owned()]);
        if set.face_count() < 2 {
            return;
        }
        let families = ["sans-serif".to_owned()];
        let regular = set
            .select(FaceRequest {
                families: &families,
                weight: 400,
                italic: false,
                sample: None,
            })
            .expect("a regular face");
        let bold = set
            .select(FaceRequest {
                families: &families,
                weight: 700,
                italic: false,
                sample: None,
            })
            .expect("a bold face");
        assert!(
            bold.style().weight > regular.style().weight,
            "bold {:?} should outweigh regular {:?}",
            bold.style(),
            regular.style()
        );
    }

    #[test]
    fn bold_text_is_wider_than_regular_text() {
        // The visible consequence, and what a page actually notices.
        if !has_fonts() {
            return;
        }
        let fonts = SystemFonts::new();
        let set = fonts.load_set(&["sans-serif".to_owned()]);
        if set.face_count() < 2 {
            return;
        }
        let families = ["sans-serif".to_owned()];
        let regular = set
            .select(FaceRequest {
                families: &families,
                weight: 400,
                italic: false,
                sample: None,
            })
            .expect("regular");
        let bold = set
            .select(FaceRequest {
                families: &families,
                weight: 700,
                italic: false,
                sample: None,
            })
            .expect("bold");
        assert!(
            bold.width("Hamburgefonstiv", &bold_style())
                > regular.width("Hamburgefonstiv", &bold_style()),
            "a bold face should set wider"
        );
    }

    fn bold_style() -> crate::engine::cryo_impl::css::computed::ComputedStyle {
        crate::engine::cryo_impl::css::computed::ComputedStyle {
            font_size: 32.0,
            ..crate::engine::cryo_impl::css::computed::ComputedStyle::INITIAL
        }
    }
}
