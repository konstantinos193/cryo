//! Import and export: the one place cryo speaks another browser's
//! formats.
//!
//! The rule that keeps this from sprawling: foreign bytes are turned
//! into *drafts* here and nothing else. Every draft then goes through
//! the owning context's one constructor — `Bookmarks::add_all`,
//! `Credentials::add_all`, `History::merge` — so an imported row is held
//! to exactly the same invariants as one the browser made itself
//! (AGENTS §6). Nothing in this module keeps state, reads a clock or
//! knows about views.
//!
//! What each format is for:
//!
//! - `netscape` — bookmarks as HTML. Every browser exports and imports
//!   it, so it is cryo's bookmark interchange both ways.
//! - `chromium` — the plaintext `Bookmarks` file inside an installed
//!   Edge/Chrome/Brave/Vivaldi/Opera profile, read in place.
//! - `firefox` — the `bookmarks-*.jsonlz4` backup Mozilla-family
//!   profiles write on every change, read in place.
//! - `passwords` — the CSV a browser writes when the user asks it to
//!   export passwords, and the CSV it reads back.
//! - `history` — cryo's own CSV, so a profile's memory can move
//!   machines.
//!
//! Deliberately absent: reading another browser's live password or
//! history database. Those are that browser's private storage, locked to
//! its account; the sanctioned export is the path, and it is the one
//! cryo takes.
//!
//! The work here is file-sized, so the app runs it on a worker thread
//! and posts the `Bundle` back to the event loop (AGENTS §9). Everything
//! that crosses that boundary is plain data.

pub mod chromium;
mod csv;
pub mod firefox;
pub mod history;
pub mod netscape;
pub mod passwords;

use std::path::{Path, PathBuf};

use crate::bookmarks;
use crate::persist;
use crate::vault;

pub use chromium::Profile;

/// Every readable browser profile on this machine — the Chromium
/// family first, then the Mozilla one.
pub fn profiles() -> Vec<Profile> {
    let mut out = chromium::profiles();
    out.extend(firefox::profiles());
    out
}

/// Largest file read. A bookmark export is kilobytes and a big password
/// CSV is hundreds; anything past this is not one of these formats, and
/// reading it would hand a browser's worth of memory to a text parser.
const MAX_FILE: u64 = 64 * 1024 * 1024;

/// Where an import comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A file the user picked. What it holds is sniffed from its
    /// content, not its extension — browsers disagree about those.
    File(PathBuf),
    /// A file inside an installed profile, as found by
    /// `chromium::profiles` or `firefox::profiles`. The whole profile
    /// travels along so the report names it the way discovery did.
    Profile(Profile),
}

impl Source {
    pub fn path(&self) -> &Path {
        match self {
            Source::File(p) => p,
            Source::Profile(p) => &p.bookmarks,
        }
    }
}

/// Which kind of data a source turned out to hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Payload {
    #[default]
    Bookmarks,
    Passwords,
    History,
}

impl Payload {
    /// What to call it in the report the user reads.
    pub fn as_str(self) -> &'static str {
        match self {
            Payload::Bookmarks => "bookmarks",
            Payload::Passwords => "passwords",
            Payload::History => "history",
        }
    }
}

/// What one import holds, on its way to the contexts that own it.
/// Exactly one of the three lists is non-empty — a file is one format.
#[derive(Debug, Default)]
pub struct Bundle {
    pub payload: Payload,
    pub bookmarks: Vec<bookmarks::Draft>,
    pub credentials: Vec<vault::Draft>,
    pub visits: Vec<persist::Entry>,
    /// Rows the format itself could not use (no url at all). Rows the
    /// *domain* refuses are counted by the merge reports instead.
    pub skipped: usize,
    /// What to show the user as the origin of this import.
    pub source: String,
}

impl Bundle {
    /// Rows this bundle offers, whatever kind they are.
    pub fn len(&self) -> usize {
        self.bookmarks.len() + self.credentials.len() + self.visits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum TransferError {
    #[error("reading {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("writing {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is {bytes} bytes — too big to be an export file")]
    TooBig { path: String, bytes: u64 },
    #[error("{0} is not a bookmark, password or history export")]
    Unrecognized(String),
    /// A CSV whose header does not carry the columns the format needs.
    #[error("that CSV has no column cryo can use: {0}")]
    Columns(String),
    #[error("parsing the file: {0}")]
    Parse(String),
}

mod import;
#[cfg(test)]
use import::decode;
pub use import::{read, write};

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cryo-xfer-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("picked.file")
    }

    fn written(tag: &str, text: &str) -> PathBuf {
        let path = temp(tag);
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn sniffs_a_netscape_bookmark_file() {
        let path = written(
            "netscape",
            "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n<DL><p>\
             <DT><A HREF=\"https://a.test\">A</A>\n</DL><p>",
        );
        let bundle = read(&Source::File(path.clone()), 1).unwrap();
        assert_eq!(bundle.payload, Payload::Bookmarks);
        assert_eq!(bundle.bookmarks.len(), 1);
        assert_eq!(bundle.source, "picked.file", "the report names the file");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn sniffs_a_chromium_bookmarks_file() {
        let path = written(
            "chromium",
            r#"{"roots":{"bookmark_bar":{"type":"folder","children":[
                {"type":"url","name":"A","url":"https://a.test"}]}}}"#,
        );
        let bundle = read(&Source::File(path.clone()), 1).unwrap();
        assert_eq!(bundle.payload, Payload::Bookmarks);
        assert_eq!(bundle.bookmarks.len(), 1);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn sniffs_a_password_csv() {
        let path = written(
            "pw",
            "name,url,username,password,note\nA,https://a.test,bob,pw,\n",
        );
        let bundle = read(&Source::File(path.clone()), 77).unwrap();
        assert_eq!(bundle.payload, Payload::Passwords);
        assert_eq!(bundle.credentials.len(), 1);
        assert_eq!(bundle.credentials[0].updated_at, 77, "the caller stamps");
        assert!(bundle.bookmarks.is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn sniffs_a_history_csv() {
        let path = written(
            "hist",
            "url,title,visits,last_visit\nhttps://a.test,A,3,1970-01-01T00:00:05Z\n",
        );
        let bundle = read(&Source::File(path.clone()), 1).unwrap();
        assert_eq!(bundle.payload, Payload::History);
        assert_eq!(bundle.visits.len(), 1);
        assert_eq!(bundle.visits[0].uses, 3);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_password_csv_is_never_read_as_history() {
        // Both sniffs could claim a file with url + password + a date.
        let path = written(
            "both",
            "url,username,password,last_visit\nhttps://a.test,bob,pw,5\n",
        );
        let bundle = read(&Source::File(path.clone()), 1).unwrap();
        assert_eq!(bundle.payload, Payload::Passwords);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_unrecognized_file_says_so_and_names_itself() {
        let path = written("junk", "just some prose about bookmarks and passwords");
        match read(&Source::File(path.clone()), 1) {
            Err(TransferError::Unrecognized(name)) => assert_eq!(name, "picked.file"),
            other => panic!("expected Unrecognized, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_file_reports_its_path() {
        let path = std::env::temp_dir().join("cryo-no-such-import-file");
        match read(&Source::File(path), 1) {
            Err(TransferError::Read { path, .. }) => assert!(path.contains("cryo-no-such")),
            other => panic!("expected a read error, got {other:?}"),
        }
    }

    #[test]
    fn a_file_too_big_to_be_an_export_is_refused_unread() {
        let path = temp("big");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_FILE + 1).unwrap();
        drop(file);
        match read(&Source::File(path.clone()), 1) {
            Err(TransferError::TooBig { bytes, .. }) => assert_eq!(bytes, MAX_FILE + 1),
            other => panic!("expected TooBig, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_utf16_csv_from_excel_still_reads() {
        let csv = "url,username,password\nhttps://a.test,bob,pw\n";
        let mut bytes = vec![0xff, 0xfe];
        for unit in csv.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let path = temp("utf16");
        std::fs::write(&path, &bytes).unwrap();
        let bundle = read(&Source::File(path.clone()), 1).unwrap();
        assert_eq!(bundle.payload, Payload::Passwords);
        assert_eq!(bundle.credentials[0].origin, "https://a.test");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn decode_handles_both_byte_orders_and_broken_utf8() {
        assert_eq!(decode(b"plain"), "plain");
        assert_eq!(decode(&[0xff, 0xfe, b'h', 0, b'i', 0]), "hi");
        assert_eq!(decode(&[0xfe, 0xff, 0, b'h', 0, b'i']), "hi");
        assert_eq!(decode(&[b'a', 0xff, b'b']), "a\u{fffd}b");
    }

    #[test]
    fn a_profile_source_is_read_as_bookmarks_and_labelled_by_its_profile() {
        let base = std::env::temp_dir().join(format!("cryo-xfer-prof-{}", std::process::id()));
        let dir = base.join("Edge").join("User Data").join("Default");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Bookmarks");
        std::fs::write(
            &path,
            r#"{"roots":{"bookmark_bar":{"type":"folder","children":[
                {"type":"url","name":"A","url":"https://a.test"}]}}}"#,
        )
        .unwrap();
        let profile = Profile {
            browser: "Microsoft Edge".to_string(),
            name: "Default".to_string(),
            bookmarks: path,
            count: 1,
        };
        let bundle = read(&Source::Profile(profile), 1).unwrap();
        assert_eq!(bundle.payload, Payload::Bookmarks);
        assert_eq!(bundle.source, "Microsoft Edge · Default");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn write_creates_the_folder_and_names_what_it_could_not_write() {
        let dir = std::env::temp_dir().join(format!("cryo-xfer-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("export.html");
        write(&path, "hello").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        // A directory is not a file: the error carries the path.
        match write(&dir, "nope") {
            Err(TransferError::Write { path, .. }) => assert!(path.contains("cryo-xfer-out")),
            other => panic!("expected a write error, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundle_counts_rows_of_whatever_kind_it_holds() {
        let mut bundle = Bundle::default();
        assert!(bundle.is_empty());
        bundle
            .visits
            .push(persist::Entry::new("https://a", None, 1, 1).unwrap());
        assert_eq!(bundle.len(), 1);
        assert!(!bundle.is_empty());
    }

    #[test]
    fn payload_names_itself_for_the_report() {
        assert_eq!(Payload::Bookmarks.as_str(), "bookmarks");
        assert_eq!(Payload::Passwords.as_str(), "passwords");
        assert_eq!(Payload::History.as_str(), "history");
    }
}
