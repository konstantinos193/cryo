//! Reading a Chromium-family profile: Edge, Chrome, Brave, Vivaldi,
//! Opera and plain Chromium all keep their bookmarks in the same place,
//! in the same JSON file, in the clear.
//!
//! That file is the whole of what cryo reads from an installed browser.
//! History and passwords live in databases that belong to that browser
//! and, for passwords, are encrypted against the signed-in account —
//! those come in through the exports the browser's own settings page
//! produces, with the user standing there (`passwords`, `history`).
//!
//! The JSON here is the foreign wire format, so the DTOs stay at this
//! boundary and the drafts that leave are the domain's (AGENTS §6).

use std::path::{Path, PathBuf};

use crate::bookmarks::Draft;
use crate::transfer::TransferError;

/// Seconds between 1601-01-01 (the Windows/Chromium epoch) and
/// 1970-01-01. Chromium stores `date_added` as microseconds since the
/// earlier one, as a decimal string.
const CHROMIUM_EPOCH_OFFSET: u64 = 11_644_473_600;

/// Deepest folder nesting followed. Matches the bookmark registry's own
/// bound; deeper bookmarks still arrive, filed at the cut-off path.
const MAX_DEPTH: usize = 16;

/// The browsers worth looking for, and where their user data lives.
/// `User Data` holds one directory per profile; Opera's profile folder
/// is the leaf itself.
const FAMILY: [(&str, Root, &str); 7] = [
    ("Microsoft Edge", Root::Local, r"Microsoft\Edge\User Data"),
    ("Google Chrome", Root::Local, r"Google\Chrome\User Data"),
    (
        "Brave",
        Root::Local,
        r"BraveSoftware\Brave-Browser\User Data",
    ),
    ("Vivaldi", Root::Local, r"Vivaldi\User Data"),
    ("Chromium", Root::Local, r"Chromium\User Data"),
    ("Opera", Root::Roaming, r"Opera Software\Opera Stable"),
    ("Opera GX", Root::Roaming, r"Opera Software\Opera GX Stable"),
];

/// Which per-user folder a browser keeps its profiles under.
#[derive(Clone, Copy)]
enum Root {
    /// `%LOCALAPPDATA%`
    Local,
    /// `%APPDATA%`
    Roaming,
}

impl Root {
    fn path(self) -> Option<PathBuf> {
        let var = match self {
            Root::Local => "LOCALAPPDATA",
            Root::Roaming => "APPDATA",
        };
        std::env::var_os(var).map(PathBuf::from)
    }
}

/// One installed profile cryo can read bookmarks from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    /// "Microsoft Edge", "Google Chrome", …
    pub browser: String,
    /// The profile's own name — its display name where the browser
    /// records one, otherwise its folder ("Default", "Profile 1").
    pub name: String,
    /// The `Bookmarks` file itself. Kept whole so the importer never
    /// has to rebuild a path.
    pub bookmarks: PathBuf,
    /// How many bookmarks it holds, so the user can tell two profiles
    /// apart before importing either.
    pub count: usize,
}

/// Every readable profile on this machine, in `FAMILY` order.
///
/// Touches the disk (one directory listing and two small reads per
/// browser), so callers run it off the event loop (AGENTS §9).
pub fn profiles() -> Vec<Profile> {
    let mut out = Vec::new();
    for (browser, root, suffix) in FAMILY {
        let Some(base) = root.path() else { continue };
        out.extend(profiles_in(&base.join(suffix), browser));
    }
    out
}

/// The profiles under one browser's user-data folder. Split out from
/// `profiles` so a fabricated tree can be tested without the registry
/// or an installed browser.
fn profiles_in(user_data: &Path, browser: &str) -> Vec<Profile> {
    let names = display_names(&user_data.join("Local State"));
    // Opera keeps `Bookmarks` directly in its profile folder; the rest
    // keep one directory per profile under `User Data`.
    let mut candidates: Vec<PathBuf> = vec![user_data.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(user_data) {
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        candidates.extend(dirs);
    }

    let mut out = Vec::new();
    for dir in candidates {
        let file = dir.join("Bookmarks");
        if !file.is_file() {
            continue;
        }
        let folder = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        // A profile whose file cannot be read is not offered: a source
        // the user picks must be one that works.
        let Ok(count) = read_file(&file).map(|d| d.len()) else {
            continue;
        };
        out.push(Profile {
            browser: browser.to_string(),
            name: names
                .iter()
                .find(|(k, _)| *k == folder)
                .map(|(_, v)| v.clone())
                .unwrap_or(folder),
            bookmarks: file,
            count,
        });
    }
    out
}

/// Profile display names out of `Local State`
/// (`profile.info_cache.<folder>.name`). Missing or unreadable means
/// the folder names stand on their own.
fn display_names(local_state: &Path) -> Vec<(String, String)> {
    let Ok(bytes) = std::fs::read(local_state) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    let Some(cache) = json
        .get("profile")
        .and_then(|p| p.get("info_cache"))
        .and_then(|c| c.as_object())
    else {
        return Vec::new();
    };
    cache
        .iter()
        .filter_map(|(folder, info)| {
            let name = info.get("name")?.as_str()?.trim();
            (!name.is_empty()).then(|| (folder.clone(), name.to_string()))
        })
        .collect()
}

/// Read one `Bookmarks` file into drafts.
pub fn read_file(path: &Path) -> Result<Vec<Draft>, TransferError> {
    let bytes = std::fs::read(path).map_err(|source| TransferError::Read {
        path: path.display().to_string(),
        source,
    })?;
    parse(&bytes)
}

/// Parse the bytes of a `Bookmarks` file.
pub fn parse(bytes: &[u8]) -> Result<Vec<Draft>, TransferError> {
    let json: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| TransferError::Parse(e.to_string()))?;
    let roots = json
        .get("roots")
        .and_then(|r| r.as_object())
        .ok_or_else(|| TransferError::Parse("no bookmark roots in the file".into()))?;

    let mut out = Vec::new();
    // Deterministic order, bar first: the bar is what the user sees, so
    // it should win any duplicate race against a copy filed elsewhere.
    let mut keys: Vec<&String> = roots.keys().collect();
    keys.sort_by_key(|k| match k.as_str() {
        "bookmark_bar" => 0,
        "other" => 1,
        "synced" => 2,
        _ => 3,
    });
    for key in keys {
        let Some(node) = roots.get(key) else { continue };
        let folder = match key.as_str() {
            // Chromium's toolbar becomes cryo's bar — the empty path.
            "bookmark_bar" => String::new(),
            "other" => "Other bookmarks".to_string(),
            "synced" => "Mobile bookmarks".to_string(),
            _ => node
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or(key)
                .to_string(),
        };
        walk(node, &folder, 0, &mut out);
    }
    Ok(out)
}

/// Depth-first over one root's children, carrying the folder path.
fn walk(node: &serde_json::Value, folder: &str, depth: usize, out: &mut Vec<Draft>) {
    let Some(children) = node.get("children").and_then(|c| c.as_array()) else {
        return;
    };
    for child in children {
        let kind = child.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let name = child.get("name").and_then(|n| n.as_str()).unwrap_or("");
        match kind {
            "url" => {
                let Some(url) = child.get("url").and_then(|u| u.as_str()) else {
                    continue;
                };
                out.push(Draft {
                    title: name.to_string(),
                    url: url.to_string(),
                    folder: folder.to_string(),
                    added_at: added_at(child),
                    favicon: None,
                });
            }
            "folder" => {
                if depth >= MAX_DEPTH {
                    // Deeper levels collapse into the deepest path kept
                    // rather than being dropped.
                    walk(child, folder, depth, out);
                    continue;
                }
                let nested = if folder.is_empty() {
                    name.to_string()
                } else {
                    format!("{folder}/{name}")
                };
                walk(child, &nested, depth + 1, out);
            }
            // Newer files carry other node types; they hold no url.
            _ => {}
        }
    }
}

/// `date_added`: microseconds since 1601, as a string. Anything else
/// reads as undated rather than as "now".
fn added_at(node: &serde_json::Value) -> u64 {
    let raw = match node.get("date_added") {
        Some(serde_json::Value::String(s)) => s.parse::<u64>().unwrap_or(0),
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
        _ => 0,
    };
    (raw / 1_000_000).saturating_sub(CHROMIUM_EPOCH_OFFSET)
}

/// Does this look like a Chromium `Bookmarks` file? Used to sniff a
/// file the user picked by hand.
pub fn looks_like(text: &str) -> bool {
    let head = text
        .get(..text.len().min(4096))
        .unwrap_or(text)
        .replace(char::is_whitespace, "");
    head.starts_with('{') && head.contains("\"roots\":")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed to the shape that matters, with Edge's real field spread.
    const EDGE_BOOKMARKS: &str = r#"{
      "checksum": "0bb3…",
      "roots": {
        "bookmark_bar": {
          "children": [
            {
              "date_added": "13309344000000000",
              "guid": "a", "id": "6",
              "name": "On the bar",
              "type": "url",
              "url": "https://bar.test/"
            },
            {
              "children": [
                {
                  "date_added": "13309344000000000",
                  "id": "8", "name": "Deep", "type": "url",
                  "url": "https://deep.test/"
                }
              ],
              "date_added": "13309344000000000",
              "id": "7", "name": "News", "type": "folder"
            }
          ],
          "date_added": "13309344000000000",
          "name": "Bookmarks bar",
          "type": "folder"
        },
        "other": {
          "children": [
            {
              "date_added": "0", "id": "9", "name": "Elsewhere",
              "type": "url", "url": "https://other.test/"
            }
          ],
          "name": "Other bookmarks",
          "type": "folder"
        },
        "synced": { "children": [], "name": "Mobile bookmarks", "type": "folder" }
      },
      "version": 1
    }"#;

    #[test]
    fn reads_an_edge_bookmarks_file() {
        let drafts = parse(EDGE_BOOKMARKS.as_bytes()).unwrap();
        assert_eq!(drafts.len(), 3);
        assert_eq!(drafts[0].title, "On the bar");
        assert_eq!(drafts[0].folder, "", "the toolbar becomes cryo's bar");
        assert_eq!(drafts[1].folder, "News");
        assert_eq!(drafts[1].url, "https://deep.test/");
        assert_eq!(drafts[2].folder, "Other bookmarks");
    }

    #[test]
    fn chromium_timestamps_become_unix_seconds() {
        let drafts = parse(EDGE_BOOKMARKS.as_bytes()).unwrap();
        // 13309344000000000µs since 1601 == 2022-10-04T00:00:00Z.
        assert_eq!(drafts[0].added_at, 1_664_870_400);
        // A zero date is undated, not 1601.
        assert_eq!(drafts[2].added_at, 0);
    }

    #[test]
    fn the_bar_is_read_before_the_other_roots() {
        let json = r#"{"roots":{
            "synced":{"type":"folder","name":"Mobile bookmarks","children":[
              {"type":"url","name":"m","url":"https://m.test"}]},
            "other":{"type":"folder","name":"Other bookmarks","children":[
              {"type":"url","name":"o","url":"https://o.test"}]},
            "bookmark_bar":{"type":"folder","name":"Bookmarks bar","children":[
              {"type":"url","name":"b","url":"https://b.test"}]}
        }}"#;
        let drafts = parse(json.as_bytes()).unwrap();
        assert_eq!(drafts[0].folder, "");
        assert_eq!(drafts[1].folder, "Other bookmarks");
        assert_eq!(drafts[2].folder, "Mobile bookmarks");
    }

    #[test]
    fn an_unknown_root_keeps_its_own_name() {
        let json = r#"{"roots":{"experiment":{"type":"folder","name":"Experiment",
            "children":[{"type":"url","name":"x","url":"https://x.test"}]}}}"#;
        assert_eq!(parse(json.as_bytes()).unwrap()[0].folder, "Experiment");
    }

    #[test]
    fn nodes_without_a_url_are_skipped_not_fatal() {
        let json = r#"{"roots":{"bookmark_bar":{"type":"folder","children":[
            {"type":"url","name":"no url here"},
            {"type":"something-new","name":"?"},
            {"type":"url","name":"fine","url":"https://fine.test"}
        ]}}}"#;
        let drafts = parse(json.as_bytes()).unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].url, "https://fine.test");
    }

    #[test]
    fn runaway_nesting_is_bounded_but_keeps_the_bookmarks() {
        let mut json = String::from(r#"{"roots":{"bookmark_bar":{"type":"folder","#);
        let depth = MAX_DEPTH + 10;
        for i in 0..depth {
            json.push_str(&format!(r#""children":[{{"type":"folder","name":"d{i}","#));
        }
        json.push_str(r#""children":[{"type":"url","name":"deep","url":"https://deep.test"}]}"#);
        for _ in 0..depth - 1 {
            json.push_str("]}");
        }
        json.push_str("]}}}");
        let drafts = parse(json.as_bytes()).unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].folder.split('/').count(), MAX_DEPTH);
    }

    #[test]
    fn a_file_that_is_not_a_bookmarks_file_is_refused() {
        assert!(matches!(
            parse(b"not json at all"),
            Err(TransferError::Parse(_))
        ));
        assert!(matches!(
            parse(br#"{"version":1}"#),
            Err(TransferError::Parse(_))
        ));
    }

    #[test]
    fn an_empty_profile_reads_as_no_bookmarks() {
        let json = r#"{"roots":{"bookmark_bar":{"type":"folder","children":[]}}}"#;
        assert!(parse(json.as_bytes()).unwrap().is_empty());
    }

    #[test]
    fn looks_like_claims_bookmarks_json_only() {
        assert!(looks_like(EDGE_BOOKMARKS));
        assert!(!looks_like("<!DOCTYPE NETSCAPE-Bookmark-file-1>"));
        assert!(!looks_like("url,username,password"));
        assert!(!looks_like(r#"{"version":1,"rows":[]}"#));
        assert!(!looks_like(""));
    }

    #[test]
    fn discovery_finds_every_profile_that_has_a_readable_file() {
        let root = std::env::temp_dir().join(format!("cryo-prof-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for profile in ["Default", "Profile 1"] {
            std::fs::create_dir_all(root.join(profile)).unwrap();
            std::fs::write(root.join(profile).join("Bookmarks"), EDGE_BOOKMARKS).unwrap();
        }
        // A profile folder with no bookmarks file is not a source.
        std::fs::create_dir_all(root.join("Guest Profile")).unwrap();
        // Neither is one whose file is unreadable.
        std::fs::create_dir_all(root.join("Broken")).unwrap();
        std::fs::write(root.join("Broken").join("Bookmarks"), "{]").unwrap();
        std::fs::write(
            root.join("Local State"),
            r#"{"profile":{"info_cache":{"Profile 1":{"name":"Work"}}}}"#,
        )
        .unwrap();

        let found = profiles_in(&root, "Microsoft Edge");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "Default", "no display name, so the folder");
        assert_eq!(found[1].name, "Work", "Local State names this one");
        assert_eq!(found[0].browser, "Microsoft Edge");
        assert_eq!(found[0].count, 3);
        assert!(found[0].bookmarks.ends_with("Default/Bookmarks"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovery_finds_an_opera_style_profile_at_the_root() {
        let root = std::env::temp_dir().join(format!("cryo-opera-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Bookmarks"), EDGE_BOOKMARKS).unwrap();
        let found = profiles_in(&root, "Opera");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].browser, "Opera");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovery_of_a_browser_that_is_not_installed_finds_nothing() {
        let missing = std::env::temp_dir().join("cryo-no-such-browser-at-all");
        assert!(profiles_in(&missing, "Chromium").is_empty());
    }

    #[test]
    fn read_file_reports_the_path_it_could_not_read() {
        let missing = std::env::temp_dir().join("cryo-no-such-bookmarks-file");
        match read_file(&missing) {
            Err(TransferError::Read { path, .. }) => assert!(path.contains("cryo-no-such")),
            other => panic!("expected a read error, got {other:?}"),
        }
    }
}
