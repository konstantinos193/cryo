//! Reading a Mozilla-family profile: Firefox, Floorp, Zen and
//! LibreWolf keep bookmarks inside `places.sqlite` — but that database
//! is the browser's private storage and is locked while it runs, so it
//! is not what cryo reads (the module doc spells out the rule).
//!
//! What Firefox itself writes, unlocked and refreshed on every change,
//! is `bookmarkbackups/bookmarks-*.jsonlz4`: the same tree an HTML
//! export carries, LZ4-block compressed behind a `mozLz40` header.
//! That sanctioned file is what this module reads — a profile yields
//! its newest backup, and picking one by hand works the same as a
//! Chromium `Bookmarks` file.
//!
//! `profiles.ini` supplies display names the way Chromium's `Local
//! State` does; folder names stand in where it is missing.
//!
//! The JSON and the lz4 payload are the foreign wire format, so the
//! decoding stays at this boundary and the drafts that leave are the
//! domain's (AGENTS §6).

use std::path::{Path, PathBuf};

use crate::bookmarks::Draft;
use crate::transfer::{Profile, TransferError};

/// Deepest folder nesting followed. Matches the bookmark registry's own
/// bound; deeper bookmarks still arrive, filed at the cut-off path.
const MAX_DEPTH: usize = 16;

/// `jsonlz4` magic: 8 bytes, then a u32 size, then the LZ4 stream.
const MOZLZ4_MAGIC: &[u8; 8] = b"mozLz40\0";

/// The Mozilla-family browsers worth looking for, and where their
/// profile directories live under `%APPDATA%` (`Root::Roaming`). A
/// profile is a directory holding `bookmarkbackups/`; where the vendor
/// nests them under `Profiles/` that level is walked instead.
const FAMILY: [&str; 4] = [r"Mozilla\Firefox", r"Floorp", r"Zen", r"LibreWolf"];

/// What a vendor's profile folder is called on screen.
fn browser_name(suffix: &str) -> &'static str {
    match suffix {
        r"Mozilla\Firefox" => "Mozilla Firefox",
        "Floorp" => "Floorp",
        "Zen" => "Zen",
        "LibreWolf" => "LibreWolf",
        _ => "Firefox",
    }
}

/// Every readable Mozilla profile on this machine, in `FAMILY` order.
pub fn profiles() -> Vec<Profile> {
    let Some(roaming) = std::env::var_os("APPDATA").map(PathBuf::from) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for suffix in FAMILY {
        out.extend(profiles_in(&roaming.join(suffix), browser_name(suffix)));
    }
    out
}

/// The profiles under one vendor's folder. Split out from `profiles`
/// so a fabricated tree can be tested without an installed browser.
fn profiles_in(vendor_dir: &Path, browser: &str) -> Vec<Profile> {
    let names = display_names(&vendor_dir.join("profiles.ini"));
    // Profiles sit either directly under the vendor folder (Floorp,
    // LibreWolf) or one level down in `Profiles/` (Firefox, Zen).
    let mut roots = vec![vendor_dir.to_path_buf()];
    let nested = vendor_dir.join("Profiles");
    if nested.is_dir() {
        roots.insert(0, nested);
    }

    let mut out = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.join("bookmarkbackups").is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let Some(backup) = newest_backup(&dir) else {
                continue;
            };
            let folder = dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            // A backup that cannot be read is not offered: a source the
            // user picks must be one that works.
            let Ok(count) = read_file(&backup).map(|d| d.len()) else {
                continue;
            };
            out.push(Profile {
                browser: browser.to_string(),
                name: names
                    .iter()
                    .find(|(k, _)| *k == folder)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| folder_name(&folder)),
                bookmarks: backup,
                count,
            });
        }
    }
    out
}

/// The newest `bookmarks-*.jsonlz4` in a profile — newest wins because
/// Firefox rotates backups and the freshest tree is the truthful one.
/// Modification time decides; the name is only a tiebreak.
fn newest_backup(profile_dir: &Path) -> Option<PathBuf> {
    let dir = profile_dir.join("bookmarkbackups");
    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let name = path.file_name()?.to_string_lossy();
        if !(name.starts_with("bookmarks-") && name.ends_with(".jsonlz4")) {
            continue;
        }
        let modified = entry.metadata().and_then(|m| m.modified()).ok();
        let better = match (&best, modified) {
            (None, m) => Some(m.unwrap_or(std::time::SystemTime::UNIX_EPOCH)),
            (Some((_, t)), Some(m)) if m > *t => Some(m),
            _ => None,
        };
        if let Some(m) = better {
            best = Some((path, m));
        }
    }
    best.map(|(p, _)| p)
}

/// Profile display names out of `profiles.ini` (`Path=` → `Name=`).
/// Missing or unreadable means folder names stand on their own.
fn display_names(ini: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(ini) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let (mut name, mut path) = (None::<String>, None::<String>);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if let (Some(n), Some(p)) = (name.take(), path.take()) {
                out.push((folder_of(&p), n));
            }
        } else if let Some(v) = line.strip_prefix("Name=") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Path=") {
            path = Some(v.trim().to_string());
        }
    }
    if let (Some(n), Some(p)) = (name, path) {
        out.push((folder_of(&p), n));
    }
    out
}

/// `Profiles/abc123.default-release` → `abc123.default-release` — the
/// key `profiles.ini` is matched on is the directory name alone.
fn folder_of(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// `abc123.default-release` → `default-release` for the pane; a folder
/// with no hash prefix shows as it is.
fn folder_name(folder: &str) -> String {
    folder
        .split_once('.')
        .map(|(_, rest)| rest)
        .filter(|rest| !rest.is_empty())
        .unwrap_or(folder)
        .to_string()
}

/// Read one backup file (`.jsonlz4` or a plain JSON copy) into drafts.
pub fn read_file(path: &Path) -> Result<Vec<Draft>, TransferError> {
    let bytes = std::fs::read(path).map_err(|source| TransferError::Read {
        path: path.display().to_string(),
        source,
    })?;
    parse_bytes(&bytes)
}

/// Parse a backup's bytes: decompress when the `mozLz40` magic is
/// there, parse as JSON otherwise — a user may hand over an
/// uncompressed copy and it is the same format underneath.
pub fn parse_bytes(bytes: &[u8]) -> Result<Vec<Draft>, TransferError> {
    let json = if looks_like(bytes) {
        decompress(bytes)?
    } else {
        bytes.to_vec()
    };
    parse(&json)
}

/// Parse the JSON tree a backup carries. The root node is `places`;
/// its container children hold `root` tags naming the toolbar, the
/// menu and unfiled — those become cryo's bar and named folders.
pub fn parse(bytes: &[u8]) -> Result<Vec<Draft>, TransferError> {
    let json: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| TransferError::Parse(e.to_string()))?;
    let mut out = Vec::new();
    // Deterministic order, bar first — same race rule as chromium.
    let mut roots: Vec<&serde_json::Value> = json
        .get("children")
        .and_then(|c| c.as_array())
        .map(|c| c.iter().collect())
        .unwrap_or_default();
    roots.sort_by_key(|n| match n.get("root").and_then(|r| r.as_str()) {
        Some("toolbarFolder") => 0,
        Some("bookmarksMenuFolder") => 1,
        Some("unfiledBookmarksFolder") => 2,
        _ => 3,
    });
    if roots.is_empty() {
        // A bare subtree is still a tree — the root tag is absent on a
        // hand-trimmed file.
        walk(&json, "", 0, &mut out);
        return Ok(out);
    }
    for node in roots {
        let folder = match node.get("root").and_then(|r| r.as_str()) {
            Some("toolbarFolder") => String::new(),
            Some("bookmarksMenuFolder") => "Bookmarks menu".to_string(),
            Some("unfiledBookmarksFolder") => "Other bookmarks".to_string(),
            _ => node
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string(),
        };
        walk(node, &folder, 0, &mut out);
    }
    Ok(out)
}

/// Depth-first over a container's children, carrying the folder path.
/// `type` strings are the same ones a JSON export carries:
/// `text/x-moz-place` is a bookmark, `…-container` a folder.
fn walk(node: &serde_json::Value, folder: &str, depth: usize, out: &mut Vec<Draft>) {
    let Some(children) = node.get("children").and_then(|c| c.as_array()) else {
        return;
    };
    for child in children {
        let kind = child.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match kind {
            "text/x-moz-place" => {
                let Some(url) = child.get("uri").and_then(|u| u.as_str()) else {
                    continue;
                };
                out.push(Draft {
                    title: child
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                    url: url.to_string(),
                    folder: folder.to_string(),
                    added_at: added_at(child),
                    favicon: None,
                });
            }
            "text/x-moz-place-container" => {
                if depth >= MAX_DEPTH {
                    walk(child, folder, depth, out);
                    continue;
                }
                let name = child.get("title").and_then(|t| t.as_str()).unwrap_or("");
                let nested = if folder.is_empty() {
                    name.to_string()
                } else {
                    format!("{folder}/{name}")
                };
                walk(child, &nested, depth + 1, out);
            }
            // Separators and smart folders hold no url.
            _ => {}
        }
    }
}

/// `dateAdded`: microseconds since the Unix epoch in a backup JSON.
/// Anything else reads as undated rather than as "now".
fn added_at(node: &serde_json::Value) -> u64 {
    node.get("dateAdded")
        .and_then(|d| d.as_u64())
        .map(|us| us / 1_000_000)
        .unwrap_or(0)
}

/// Is this a `mozLz40` payload? Used both to decide whether to
/// decompress and to sniff a file the user picked by hand.
pub fn looks_like(bytes: &[u8]) -> bool {
    bytes.starts_with(MOZLZ4_MAGIC)
}

/// Sniff a hand-picked *text* file: an uncompressed Firefox JSON
/// export announces itself by its node `type` strings.
pub fn looks_like_json(text: &str) -> bool {
    let head = text
        .get(..text.len().min(4096))
        .unwrap_or(text)
        .replace(char::is_whitespace, "");
    head.starts_with('{') && head.contains("\"text/x-moz-place")
}

/// `mozLz40` → JSON bytes: 8-byte magic, u32 uncompressed size
/// (little-endian — informational; the stream's own bounds rule),
/// then one LZ4 block stream.
fn decompress(bytes: &[u8]) -> Result<Vec<u8>, TransferError> {
    let bad = || TransferError::Parse("corrupt jsonlz4 backup".into());
    let stream = bytes.get(MOZLZ4_MAGIC.len() + 4..).ok_or_else(bad)?;
    lz4_block(stream)
}

/// One LZ4 block stream: token, literal run, back-reference, repeat.
/// Implemented here because pulling in a crate for sixty lines of
/// decompression is the worse trade — the format has not moved in a
/// decade.
fn lz4_block(input: &[u8]) -> Result<Vec<u8>, TransferError> {
    let bad = || TransferError::Parse("corrupt lz4 stream".into());
    let mut out = Vec::new();
    let mut i = 0;
    while i < input.len() {
        let token = input[i];
        i += 1;
        // Literal run: high nibble, 15 means "keep reading length bytes".
        let mut lit_len = usize::from(token >> 4);
        if lit_len == 15 {
            lit_len += read_len(input, &mut i)?;
        }
        let lits = input.get(i..i + lit_len).ok_or_else(bad)?;
        out.extend_from_slice(lits);
        i += lit_len;
        // A block ends after its last literal run: no match follows.
        if i >= input.len() {
            break;
        }
        // Back-reference: 2-byte offset (little-endian), then match
        // length in the low nibble plus the base of 4.
        let off = usize::from(u16::from_le_bytes(
            input.get(i..i + 2).ok_or_else(bad)?.try_into().unwrap(),
        ));
        i += 2;
        if off == 0 || off > out.len() {
            return Err(bad());
        }
        let mut match_len = usize::from(token & 0x0f);
        if match_len == 15 {
            match_len += read_len(input, &mut i)?;
        }
        match_len += 4;
        // Overlapping copies are legal — copy byte by byte.
        for _ in 0..match_len {
            let b = out[out.len() - off];
            out.push(b);
        }
    }
    Ok(out)
}

/// The 0xff-terminated length bytes that follow a saturated nibble.
fn read_len(input: &[u8], i: &mut usize) -> Result<usize, TransferError> {
    let bad = || TransferError::Parse("corrupt lz4 stream".into());
    let mut len = 0;
    loop {
        let b = *input.get(*i).ok_or_else(bad)?;
        *i += 1;
        len += usize::from(b);
        if b != 0xff {
            return Ok(len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed to the shape a real `bookmarks-*.jsonlz4` carries once
    /// decompressed: the `places` root, its tagged containers, a
    /// nested folder and a separator to skip.
    const FIREFOX_TREE: &str = r#"{
      "guid": "root________",
      "type": "text/x-moz-place-container",
      "children": [
        {
          "guid": "menu________", "root": "bookmarksMenuFolder",
          "type": "text/x-moz-place-container",
          "title": "Bookmarks Menu",
          "children": [
            {
              "guid": "a", "type": "text/x-moz-place",
              "title": "Menu page", "uri": "https://menu.test/",
              "dateAdded": 1664870400000000
            }
          ]
        },
        {
          "guid": "toolbar_____", "root": "toolbarFolder",
          "type": "text/x-moz-place-container",
          "title": "Bookmarks Toolbar",
          "children": [
            {
              "guid": "b", "type": "text/x-moz-place",
              "title": "On the bar", "uri": "https://bar.test/",
              "dateAdded": 1664870400000000
            },
            {
              "guid": "c", "type": "text/x-moz-place-container",
              "title": "News",
              "children": [
                {
                  "guid": "d", "type": "text/x-moz-place",
                  "title": "Deep", "uri": "https://deep.test/"
                }
              ]
            },
            { "guid": "sep", "type": "text/x-moz-separator" }
          ]
        },
        {
          "guid": "unfiled_____", "root": "unfiledBookmarksFolder",
          "type": "text/x-moz-place-container",
          "title": "Other Bookmarks",
          "children": [
            {
              "guid": "e", "type": "text/x-moz-place",
              "title": "Elsewhere", "uri": "https://other.test/",
              "dateAdded": 0
            }
          ]
        }
      ]
    }"#;

    /// The simplest honest lz4 stream: one literal run, no matches.
    /// Lengths past 15 ride in the 0xff-terminated bytes after the
    /// token, same as the real format.
    fn lz4_of(text: &str) -> Vec<u8> {
        let b = text.as_bytes();
        let mut out = Vec::new();
        if b.len() < 15 {
            out.push((b.len() as u8) << 4);
        } else {
            out.push(0xf0);
            let mut rest = b.len() - 15;
            while rest >= 0xff {
                out.push(0xff);
                rest -= 0xff;
            }
            out.push(rest as u8);
        }
        out.extend_from_slice(b);
        out
    }

    /// `mozLz40` + u32 size + stream.
    fn jsonlz4_of(text: &str) -> Vec<u8> {
        let mut out = MOZLZ4_MAGIC.to_vec();
        out.extend_from_slice(&(text.len() as u32).to_le_bytes());
        out.extend(lz4_of(text));
        out
    }

    #[test]
    fn reads_a_firefox_bookmark_tree() {
        let drafts = parse(FIREFOX_TREE.as_bytes()).unwrap();
        assert_eq!(drafts.len(), 4);
        assert_eq!(drafts[0].url, "https://bar.test/");
        assert_eq!(drafts[0].folder, "", "the toolbar becomes cryo's bar");
        assert_eq!(drafts[1].folder, "News");
        assert_eq!(drafts[1].url, "https://deep.test/");
        assert_eq!(drafts[2].folder, "Bookmarks menu");
        assert_eq!(drafts[3].folder, "Other bookmarks");
    }

    #[test]
    fn firefox_timestamps_become_unix_seconds() {
        let drafts = parse(FIREFOX_TREE.as_bytes()).unwrap();
        // 1664870400000000µs == 2022-10-04T00:00:00Z.
        assert_eq!(drafts[0].added_at, 1_664_870_400);
        assert_eq!(drafts[3].added_at, 0, "a zero date is undated");
    }

    #[test]
    fn the_toolbar_is_read_before_the_other_roots() {
        let json = r#"{"type":"text/x-moz-place-container","children":[
            {"root":"unfiledBookmarksFolder","type":"text/x-moz-place-container","children":[
              {"type":"text/x-moz-place","uri":"https://u.test"}]},
            {"root":"toolbarFolder","type":"text/x-moz-place-container","children":[
              {"type":"text/x-moz-place","uri":"https://t.test"}]}
        ]}"#;
        let drafts = parse(json.as_bytes()).unwrap();
        assert_eq!(drafts[0].url, "https://t.test");
        assert_eq!(drafts[1].url, "https://u.test");
    }

    #[test]
    fn decompresses_a_jsonlz4_backup() {
        let bytes = jsonlz4_of(FIREFOX_TREE);
        let drafts = parse_bytes(&bytes).unwrap();
        assert_eq!(drafts.len(), 4);
    }

    #[test]
    fn lz4_literals_round_trip_long_runs() {
        let text = "x".repeat(300);
        assert_eq!(lz4_block(&lz4_of(&text)).unwrap(), text.as_bytes());
    }

    #[test]
    fn lz4_back_references_decode() {
        // token 0x22: 2 literals, match len 2+4=6 at offset 2 → "abababab"
        let stream = [0x22, b'a', b'b', 0x02, 0x00];
        assert_eq!(lz4_block(&stream).unwrap(), b"abababab");
    }

    #[test]
    fn a_corrupt_lz4_stream_is_refused_not_panicked() {
        assert!(lz4_block(&[0x50, b'a']).is_err());
        // match offset pointing before the output start
        assert!(lz4_block(&[0x11, b'a', 0x05, 0x00]).is_err());
        assert!(parse_bytes(b"mozLz40").is_err());
    }

    #[test]
    fn sniffs_binary_and_json_backups_only() {
        assert!(looks_like(&jsonlz4_of("{}")));
        assert!(!looks_like(b"{\"children\":[]}"));
        assert!(looks_like_json(FIREFOX_TREE));
        assert!(!looks_like_json("<!DOCTYPE NETSCAPE-Bookmark-file-1>"));
        assert!(!looks_like_json(r#"{"roots":{"bookmark_bar":{}}}"#));
        assert!(!looks_like_json(""));
    }

    #[test]
    fn discovery_finds_profiles_with_a_readable_backup() {
        let root = std::env::temp_dir().join(format!("cryo-ff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let profiles = root.join("Profiles");
        for profile in ["abc.default-release", "xyz.work"] {
            let backups = profiles.join(profile).join("bookmarkbackups");
            std::fs::create_dir_all(&backups).unwrap();
            std::fs::write(
                backups.join("bookmarks-2026-10-01_1_abc.jsonlz4"),
                jsonlz4_of(FIREFOX_TREE),
            )
            .unwrap();
        }
        // A profile dir with no backups folder is not a source.
        std::fs::create_dir_all(profiles.join("empty")).unwrap();
        std::fs::write(
            root.join("profiles.ini"),
            "[Profile0]\nName=Default User\nPath=Profiles/abc.default-release\n",
        )
        .unwrap();

        let found = profiles_in(&root, "Mozilla Firefox");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "Default User", "profiles.ini names this one");
        assert_eq!(
            found[1].name, "work",
            "no ini entry, so the folder minus its hash"
        );
        assert_eq!(found[0].browser, "Mozilla Firefox");
        assert_eq!(found[0].count, 4);
        assert!(found[0].bookmarks.to_string_lossy().ends_with(".jsonlz4"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovery_uses_the_newest_backup() {
        let root = std::env::temp_dir().join(format!("cryo-ff-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let backups = root.join("p.default").join("bookmarkbackups");
        std::fs::create_dir_all(&backups).unwrap();
        let old = backups.join("bookmarks-2026-01-01_1_a.jsonlz4");
        let new = backups.join("bookmarks-2026-10-01_1_b.jsonlz4");
        std::fs::write(&old, jsonlz4_of(r#"{"children":[]}"#)).unwrap();
        std::fs::write(&new, jsonlz4_of(FIREFOX_TREE)).unwrap();
        // Make the ordering explicit: `new` carries the later mtime.
        let old_time = std::time::SystemTime::UNIX_EPOCH;
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(old_time)
            .unwrap();

        let found = newest_backup(&root.join("p.default")).unwrap();
        assert_eq!(found, new);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovery_of_a_vendor_that_is_not_installed_finds_nothing() {
        let missing = std::env::temp_dir().join("cryo-no-such-mozilla-at-all");
        assert!(profiles_in(&missing, "Zen").is_empty());
    }
}
