//! Where assets live, checked instead of remembered.
//!
//! Stylesheets, scripts and documents belong in files under `src/`, pulled
//! into the crate by exactly one `include_str!`. An asset nothing includes
//! is dead weight that still reads like it ships (AGENTS §10); an asset two
//! modules include has two owners, and the second one will drift.
//!
//! Only includes above a file's `#[cfg(test)] mod tests` count as owning
//! one. Unit tests live in that one block at the bottom (AGENTS §10), and a
//! test reading a sibling's asset to assert something about it is not a
//! second owner.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// What counts as an asset: text a browser runs or renders, never Rust.
const ASSET_EXTENSIONS: [&str; 4] = ["js", "css", "html", "json"];

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("src is readable").flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn files_under_src() -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(&source_root(), &mut out);
    out
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.contains(&e))
}

fn shown(path: &Path) -> String {
    path.strip_prefix(source_root())
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/")
}

/// Each asset `include_str!`d by shipping code, mapped to the modules that
/// include it.
fn owners() -> BTreeMap<PathBuf, Vec<String>> {
    let mut owners: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for source in files_under_src() {
        if !has_extension(&source, &["rs"]) {
            continue;
        }
        let text = fs::read_to_string(&source).expect("a source file is UTF-8");
        let shipping = &text[..text.find("#[cfg(test)]").unwrap_or(text.len())];
        let module = shown(&source);
        let folder = source.parent().expect("a file has a parent").to_path_buf();
        for (at, _) in shipping.match_indices("include_str!(\"") {
            let rest = &shipping[at + "include_str!(\"".len()..];
            let arg = &rest[..rest.find('"').expect("the argument is a literal")];
            let resolved = folder.join(arg.replace('/', std::path::MAIN_SEPARATOR_STR));
            owners
                .entry(resolved.canonicalize().unwrap_or(resolved))
                .or_default()
                .push(module.clone());
        }
    }
    owners
}

#[test]
fn every_asset_in_src_has_exactly_one_owning_module() {
    let owners = owners();
    for asset in files_under_src() {
        if !has_extension(&asset, &ASSET_EXTENSIONS) {
            continue;
        }
        let key = asset.canonicalize().unwrap_or_else(|_| asset.clone());
        let holders = owners.get(&key).map(Vec::as_slice).unwrap_or_default();
        let name = shown(&asset);
        assert!(
            !holders.is_empty(),
            "src/{name} is in the tree but no module ships it — \
             wire it up or delete it"
        );
        assert_eq!(
            holders.len(),
            1,
            "src/{name} is included by {holders:?}; one asset, one owner"
        );
    }
}

/// Rust's whole vocabulary for talking to a document is the `__cryo_`
/// functions the scripts define. A call to one nothing defines fails
/// silently in a view nobody is watching, so the two sides are matched
/// here rather than discovered in use.
#[test]
fn every_cryo_function_the_app_calls_is_defined_by_a_script() {
    let mut defined = Vec::new();
    for script in files_under_src() {
        if !has_extension(&script, &["js"]) {
            continue;
        }
        let text = fs::read_to_string(&script).expect("a script is UTF-8");
        for name in names_after(&text, "__cryo_") {
            // Assignment, or `defineProperty` where the page must not be
            // able to swap the function out from under us.
            if text.contains(&format!("window.__cryo_{name} ="))
                || text.contains(&format!("'__cryo_{name}'"))
            {
                defined.push(name);
            }
        }
    }
    for source in files_under_src() {
        if !has_extension(&source, &["rs"]) {
            continue;
        }
        let text = fs::read_to_string(&source).expect("a source file is UTF-8");
        for name in names_after(&text, "window.__cryo_") {
            assert!(
                defined.contains(&name),
                "{} evals window.__cryo_{name}, which no script defines",
                shown(&source)
            );
        }
    }
    assert!(
        defined.len() > 5,
        "the scripts stopped naming their entry points"
    );
}

/// The shell's colours live in `theme/tokens.css`, one block per theme.
/// A script that mixes its own shade is a colour no theme can reach — a
/// chip mixed dark stays dark on a light page — so a script that needs
/// one spends a token: `hsl(<the site's hue> var(--tint-bg))`.
#[test]
fn no_shell_script_mixes_its_own_colour() {
    let shell = source_root().join("shell");
    let mut mixes = 0;
    for script in files_under_src() {
        if !script.starts_with(&shell) || !has_extension(&script, &["js"]) {
            continue;
        }
        let text = fs::read_to_string(&script).expect("a script is UTF-8");
        for (at, _) in text.match_indices("hsl(") {
            let rest = &text[at..];
            let call = &rest[..rest.find(')').map_or(rest.len(), |end| end + 1)];
            assert!(
                call.contains("var(--"),
                "{} mixes its own colour in {call}",
                shown(&script)
            );
            mixes += 1;
        }
    }
    assert!(mixes > 0, "the scripts stopped tinting anything");
}

/// The identifiers following each occurrence of `marker`.
fn names_after(text: &str, marker: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .match_indices(marker)
        .map(|(at, _)| {
            text[at + marker.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect()
        })
        .filter(|name: &String| !name.is_empty())
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The shell's documents are what a reader actually opens, so they keep the
/// shape the split settled on: a folder named for the page, holding the
/// document, its stylesheet and the script(s) behind it — never a `<style>`
/// or `<script>` body inline.
#[test]
fn every_shell_document_sits_in_its_own_folder_with_its_style_and_script() {
    let shell = source_root().join("shell");
    let mut documents = 0;
    for document in files_under_src() {
        if !document.starts_with(&shell) || !has_extension(&document, &["html"]) {
            continue;
        }
        documents += 1;
        let folder = document.parent().expect("a document has a folder");
        let page = document
            .file_stem()
            .expect("a document has a name")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            folder.file_name().expect("the folder has a name"),
            page.as_str(),
            "{} belongs in a folder named after it",
            shown(&document)
        );
        assert!(
            folder.join(format!("{page}.css")).exists(),
            "{page} has no {page}.css beside it",
        );
        // The chrome's script is a dozen feature files concatenated in one
        // order, so the count is what matters, not the name.
        let scripts = fs::read_dir(folder)
            .expect("the page folder is readable")
            .flatten()
            .filter(|e| has_extension(&e.path(), &["js"]))
            .count();
        assert!(scripts > 0, "{page} has no script beside it");

        let text = fs::read_to_string(&document).expect("a document is UTF-8");
        assert!(
            !text.contains("<style"),
            "{page} inlines a stylesheet; it belongs in {page}.css",
        );
        assert!(
            !text.contains("<script>"),
            "{page} inlines a script; it belongs beside it as a .js",
        );
    }
    assert!(documents >= 4, "the shell lost a document");
}
