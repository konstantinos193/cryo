//! Import and export, as the user experiences them: pick a file or a
//! profile, wait, read what happened.
//!
//! Reading a file is file-sized work, so it happens on a worker thread
//! and comes back as `AppEvent::Imported` (AGENTS §9). Merging is the
//! event loop's job, because that is where the stores live — the worker
//! only ever produces drafts.
//!
//! Export runs on the loop: the system dialog has already blocked it,
//! and rendering what is in memory is a few milliseconds.
//!
//! Nothing here phrases an invariant of its own. Every row goes through
//! `Bookmarks::add_all`, `Credentials::add_all` or `History::merge`, and
//! the counts those return are what the user is shown.

use std::path::{Path, PathBuf};

use crate::transfer::{self, Bundle, Payload, Source, TransferError};

use super::library::confirm;
use super::{post, unix_now, App, AppEvent};

impl App {
    /// "Import from a file…": one picker for all three formats, because
    /// the file says what it is (`transfer::read` sniffs it) and asking
    /// the user to classify their own export is a quiz, not a feature.
    pub(super) fn import_file(&mut self) -> anyhow::Result<()> {
        if self.refuse_in_incognito()? {
            return Ok(());
        }
        let picked = rfd::FileDialog::new()
            .set_title("Import bookmarks, passwords or history")
            .add_filter(
                "Export files (*.html, *.csv, *.json, *.jsonlz4)",
                &["html", "htm", "csv", "json", "jsonlz4"],
            )
            .add_filter("All files", &["*"])
            .pick_file();
        let Some(path) = picked else {
            return Ok(());
        };
        self.start_import(Source::File(path))
    }

    /// Import from a browser profile the app found. The index is into
    /// the pushed source list — the document never names a path.
    pub(super) fn import_profile(&mut self, index: usize) -> anyhow::Result<()> {
        if self.refuse_in_incognito()? {
            return Ok(());
        }
        let Some(profile) = self.import_sources.as_ref().and_then(|s| s.get(index)) else {
            return Ok(());
        };
        let source = Source::Profile(profile.clone());
        self.start_import(source)
    }

    /// Hand the reading to a worker and tell the user it started. A big
    /// profile takes long enough that silence would read as a dead
    /// click.
    fn start_import(&mut self, source: Source) -> anyhow::Result<()> {
        let proxy = self.proxy.clone();
        // The caller stamps: rows from a format with no timestamps get
        // this one, and `transfer` keeps no clock of its own.
        let stamped_at = unix_now();
        let what = source
            .path()
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        std::thread::spawn(move || {
            let outcome = transfer::read(&source, stamped_at);
            post(&proxy, AppEvent::Imported(outcome.map_err(Box::new)));
        });
        self.notice(true, "Importing…", vec![format!("Reading {what}")])
    }

    /// Look for installed Chromium- and Mozilla-family profiles. One
    /// directory walk and a couple of small reads per browser — off the
    /// loop, like every other disk read.
    pub(super) fn find_import_sources(&mut self) {
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            post(&proxy, AppEvent::ImportSources(transfer::profiles()));
        });
    }

    /// The worker found profiles — keep them (the import command indexes
    /// this list) and repaint the pane.
    pub(super) fn on_import_sources(
        &mut self,
        sources: Vec<transfer::Profile>,
    ) -> anyhow::Result<()> {
        self.import_sources = Some(sources);
        self.push_library()
    }

    /// A worker finished reading. Merging happens here, where the stores
    /// are, and the report is whatever the contexts say they did.
    pub(super) fn on_imported(
        &mut self,
        outcome: Result<Bundle, Box<TransferError>>,
    ) -> anyhow::Result<()> {
        let bundle = match outcome {
            Ok(bundle) => bundle,
            Err(e) => {
                return self.notice(false, "Import failed", vec![e.to_string()]);
            }
        };
        if bundle.is_empty() {
            return self.notice(
                false,
                "Nothing to import",
                vec![format!(
                    "{} held no {} cryo could read.",
                    bundle.source,
                    bundle.payload.as_str()
                )],
            );
        }
        let (title, mut lines) = match bundle.payload {
            Payload::Bookmarks => {
                let report = self.bookmarks.add_all(&bundle.bookmarks)?;
                let mut lines = vec![plural(report.added, "bookmark", "kept")];
                if report.duplicates > 0 {
                    lines.push(format!("{} already kept", report.duplicates));
                }
                if report.rejected > 0 {
                    lines.push(format!("{} refused — not a web address", report.rejected));
                }
                if report.overflowed > 0 {
                    lines.push(format!("{} dropped — the bar is full", report.overflowed));
                }
                ("Bookmarks imported", lines)
            }
            Payload::Passwords => {
                let report = self.vault.add_all(&bundle.credentials)?;
                let mut lines = vec![plural(report.added, "password", "saved")];
                if report.updated > 0 {
                    lines.push(format!("{} updated", report.updated));
                }
                if report.duplicates > 0 {
                    lines.push(format!("{} unchanged", report.duplicates));
                }
                if report.rejected > 0 {
                    lines.push(format!(
                        "{} refused — no site to sign in to",
                        report.rejected
                    ));
                }
                if report.overflowed > 0 {
                    lines.push(format!("{} dropped — the vault is full", report.overflowed));
                }
                ("Passwords imported", lines)
            }
            Payload::History => {
                let Some(recorder) = self.recorder.as_mut() else {
                    return self.notice(
                        false,
                        "This window keeps no history",
                        vec!["Import history from a normal window.".into()],
                    );
                };
                let report = recorder.import(bundle.visits)?;
                let mut lines = vec![plural(report.added, "address", "remembered")];
                if report.merged > 0 {
                    lines.push(format!("{} already known", report.merged));
                }
                if report.overflowed > 0 {
                    lines.push(format!(
                        "{} dropped — history is full at its bound",
                        report.overflowed
                    ));
                }
                ("History imported", lines)
            }
        };
        if bundle.skipped > 0 {
            lines.push(format!("{} rows had nothing to read", bundle.skipped));
        }
        lines.push(format!("from {}", bundle.source));
        self.sync_chrome()?;
        self.push_library()?;
        self.notice(true, title, lines)
    }

    /// Write what this profile keeps out to a file the user picks.
    ///
    /// Each kind gets the format other browsers read: bookmarks as a
    /// Netscape file, passwords as Chrome's CSV, history as cryo's own.
    pub(super) fn export(&mut self, what: &str) -> anyhow::Result<()> {
        match what {
            "bookmarks" => self.export_bookmarks(),
            "passwords" => self.export_passwords(),
            "history" => self.export_history(),
            _ => Ok(()),
        }
    }

    fn export_bookmarks(&mut self) -> anyhow::Result<()> {
        let marks = self.bookmarks.marks();
        if marks.is_empty() {
            return self.notice(
                false,
                "No bookmarks to export",
                vec!["Keep a page with ★ first.".into()],
            );
        }
        let count = marks.len();
        let rows: Vec<&crate::bookmarks::Bookmark> = marks.rows().iter().collect();
        let text = transfer::netscape::render(&rows, unix_now());
        let Some(path) = save_as("cryo-bookmarks.html", "Bookmark file", &["html"]) else {
            return Ok(());
        };
        self.write_export(&path, &text, plural(count, "bookmark", "written"))
    }

    fn export_passwords(&mut self) -> anyhow::Result<()> {
        let creds = self.vault.credentials();
        if creds.is_empty() {
            return self.notice(
                false,
                "No passwords to export",
                vec!["This profile has none saved.".into()],
            );
        }
        // The file is plaintext by definition — every browser's password
        // export is. The user agrees to that here, not in a tooltip.
        if !confirm(
            "Export passwords",
            "The file cryo writes holds every saved password in plain text. \
             Anyone who opens it can read them.\n\nExport anyway?",
        ) {
            return Ok(());
        }
        let count = creds.len();
        let rows: Vec<&crate::vault::Credential> = creds.rows().iter().collect();
        let text = transfer::passwords::render(&rows);
        let Some(path) = save_as("cryo-passwords.csv", "CSV file", &["csv"]) else {
            return Ok(());
        };
        self.write_export(&path, &text, plural(count, "password", "written"))
    }

    fn export_history(&mut self) -> anyhow::Result<()> {
        let Some(recorder) = self.recorder.as_ref() else {
            return self.notice(
                false,
                "This window keeps no history",
                vec!["An incognito window has none to export.".into()],
            );
        };
        if recorder.entries().is_empty() {
            return self.notice(
                false,
                "No history to export",
                vec!["Nothing visited yet.".into()],
            );
        }
        let count = recorder.entries().len();
        let text = transfer::history::render(recorder.entries());
        let Some(path) = save_as("cryo-history.csv", "CSV file", &["csv"]) else {
            return Ok(());
        };
        self.write_export(&path, &text, plural(count, "address", "written"))
    }

    /// One write path, so a failure always names the file and a success
    /// always says where it went.
    fn write_export(&mut self, path: &Path, text: &str, headline: String) -> anyhow::Result<()> {
        match transfer::write(path, text) {
            Ok(()) => self.notice(
                true,
                "Exported",
                vec![headline, format!("to {}", path.display())],
            ),
            Err(e) => self.notice(false, "Export failed", vec![e.to_string()]),
        }
    }

    /// Incognito keeps nothing past the window, so an import into it
    /// would quietly evaporate. Says so instead.
    fn refuse_in_incognito(&mut self) -> anyhow::Result<bool> {
        if !self.incognito {
            return Ok(false);
        }
        self.notice(
            false,
            "An incognito window keeps nothing",
            vec!["Import from a normal window instead.".into()],
        )?;
        Ok(true)
    }
}

/// The save dialog, with a name the user can recognise a week later.
fn save_as(name: &str, kind: &str, extensions: &[&str]) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Export")
        .set_file_name(name)
        .add_filter(kind, extensions)
        .save_file()
}

/// "1 bookmark kept" / "12 bookmarks kept" — the headline of a report.
fn plural(count: usize, noun: &str, verb: &str) -> String {
    if count == 1 {
        format!("1 {noun} {verb}")
    } else {
        format!("{count} {noun}s {verb}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_headlines_read_like_sentences() {
        assert_eq!(plural(0, "bookmark", "kept"), "0 bookmarks kept");
        assert_eq!(plural(1, "bookmark", "kept"), "1 bookmark kept");
        assert_eq!(plural(2, "password", "saved"), "2 passwords saved");
    }
}
