//! Passwords as CSV: the file the other browsers hand you when you ask
//! them to export, and the file they take back.
//!
//! This is deliberately the only password path cryo has. A browser's own
//! password database is encrypted against the signed-in account and is
//! nobody's interchange format; the export its settings page produces,
//! with the user standing there, is. So cryo reads that file and writes
//! one Chrome, Edge, Firefox and Safari can all read back.
//!
//! Every exporter names its columns differently, so the mapping below is
//! the whole trick: find the url, username, password and note columns by
//! name, then hand the rows to the vault, which decides what is valid
//! (AGENTS §6).

use crate::transfer::csv::{Table, Writer};
use crate::transfer::TransferError;
use crate::vault::{Credential, Draft};

/// Column names seen in the wild, best first.
///
/// Chrome/Edge: `name,url,username,password,note`.
/// Firefox: `url,username,password,httpRealm,…`.
/// Safari: `Title,URL,Username,Password,Notes,OTPAuth`.
/// Bitwarden: `…,name,notes,…,login_uri,login_username,login_password,…`.
/// KeePass: `Account,Login Name,Password,Web Site,Comments`.
const URL_COLUMNS: [&str; 9] = [
    "url",
    "login uri",
    "web site",
    "website",
    "login url",
    "origin",
    "hostname",
    "site",
    "urls",
];
const USERNAME_COLUMNS: [&str; 7] = [
    "username",
    "login username",
    "login name",
    "user name",
    "user",
    "email",
    "account",
];
const PASSWORD_COLUMNS: [&str; 3] = ["password", "login password", "pwd"];
const NOTE_COLUMNS: [&str; 4] = ["note", "notes", "comments", "comment"];
const TITLE_COLUMNS: [&str; 2] = ["name", "title"];

/// The header cryo writes — Chrome's own, so its importer accepts the
/// file without a word.
const EXPORT_HEADER: [&str; 5] = ["name", "url", "username", "password", "note"];

/// Read an exported CSV into vault drafts.
///
/// `stamped_at` is what rows get for `updated_at`: these files carry no
/// usable timestamp, and the caller stamps so nothing here reads a clock.
pub fn parse(text: &str, stamped_at: u64) -> Result<Vec<Draft>, TransferError> {
    let table = Table::parse(text);
    let password = table
        .column(&PASSWORD_COLUMNS)
        .ok_or_else(|| TransferError::Columns(found("no password column", &table)))?;
    let url = table.column(&URL_COLUMNS);
    let title = table.column(&TITLE_COLUMNS);
    // KeePass-style exports name the site and nothing else; the title is
    // then the only origin there is.
    if url.is_none() && title.is_none() {
        return Err(TransferError::Columns(found("no url column", &table)));
    }
    let username = table.column(&USERNAME_COLUMNS);
    let note = table.column(&NOTE_COLUMNS);

    let mut out = Vec::with_capacity(table.rows().len());
    for row in 0..table.rows().len() {
        let origin = match table.field(row, url) {
            "" => table.field(row, title),
            url => url,
        };
        out.push(Draft {
            origin: origin.to_string(),
            username: table.field(row, username).to_string(),
            secret: table.field(row, Some(password)).to_string(),
            note: table.field(row, note).to_string(),
            updated_at: stamped_at,
        });
    }
    Ok(out)
}

/// Render saved credentials as a Chrome-compatible CSV.
///
/// This writes every password in the clear — that is what an export is.
/// The confirmation the user clicks lives in the app layer; by the time
/// a call reaches here the decision has been made.
pub fn render(rows: &[&Credential]) -> String {
    let mut out = Writer::new(&EXPORT_HEADER);
    for c in rows {
        out.row(
            [
                site_name(c.origin()),
                c.origin().to_string(),
                c.username().to_string(),
                c.secret().expose().to_string(),
                c.note().to_string(),
            ]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .into_iter(),
        );
    }
    out.finish()
}

/// "no password column (file has: name, url, note)" — a refusal the
/// user can act on, since the fix is usually to export again from the
/// right screen. Capped: a Bitwarden header is eleven columns long.
fn found(why: &str, table: &Table) -> String {
    let names: Vec<&str> = table
        .header()
        .iter()
        .take(8)
        .map(std::string::String::as_str)
        .collect();
    if names.is_empty() {
        return format!("{why} (the file has no header row)");
    }
    format!("{why} (file has: {})", names.join(", "))
}

/// Chrome's `name` column: the bare host. Anything that is not an
/// http(s) origin (`android://…`) is its own name.
fn site_name(origin: &str) -> String {
    let Some(rest) = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
    else {
        return origin.to_string();
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .rsplit('@')
        .next()
        .unwrap_or(rest);
    host.split(':').next().unwrap_or(host).to_string()
}

/// Does this text look like a password export? A CSV whose header has a
/// password column is one — that is exactly the file we can read.
pub fn looks_like(text: &str) -> bool {
    let head = text.lines().next().unwrap_or_default();
    let table = Table::parse(head);
    table.column(&PASSWORD_COLUMNS).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::Credentials;

    #[test]
    fn reads_a_chrome_export() {
        let csv = "name,url,username,password,note\n\
                   example.com,https://example.com/,bob,hunter2,my note\n";
        let drafts = parse(csv, 500).unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].origin, "https://example.com/");
        assert_eq!(drafts[0].username, "bob");
        assert_eq!(drafts[0].secret, "hunter2");
        assert_eq!(drafts[0].note, "my note");
        assert_eq!(drafts[0].updated_at, 500);
    }

    #[test]
    fn reads_a_firefox_export() {
        let csv = "\"url\",\"username\",\"password\",\"httpRealm\",\"formActionOrigin\",\
                   \"guid\",\"timeCreated\",\"timeLastUsed\",\"timePasswordChanged\"\n\
                   \"https://a.test\",\"ann\",\"pw1\",\"\",\"https://a.test\",\"{guid}\",\
                   \"1600000000\",\"1600000001\",\"1600000002\"\n";
        let drafts = parse(csv, 1).unwrap();
        assert_eq!(drafts[0].origin, "https://a.test");
        assert_eq!(drafts[0].username, "ann");
        assert_eq!(drafts[0].secret, "pw1");
    }

    #[test]
    fn reads_a_safari_export() {
        let csv = "Title,URL,Username,Password,Notes,OTPAuth\n\
                   A Site,https://a.test,ann,pw1,note here,\n";
        let drafts = parse(csv, 1).unwrap();
        assert_eq!(drafts[0].origin, "https://a.test");
        assert_eq!(drafts[0].note, "note here");
    }

    #[test]
    fn reads_a_bitwarden_export() {
        let csv = "folder,favorite,type,name,notes,fields,reprompt,login_uri,\
                   login_username,login_password,login_totp\n\
                   ,,login,A Site,,,0,https://a.test,ann,pw1,\n";
        let drafts = parse(csv, 1).unwrap();
        assert_eq!(drafts[0].origin, "https://a.test");
        assert_eq!(drafts[0].username, "ann");
        assert_eq!(drafts[0].secret, "pw1");
    }

    #[test]
    fn reads_a_keepass_export_that_names_the_site_instead_of_a_url() {
        let csv = "\"Account\",\"Login Name\",\"Password\",\"Web Site\",\"Comments\"\n\
                   \"My bank\",\"bob\",\"pw\",\"https://bank.test\",\"\"\n";
        let drafts = parse(csv, 1).unwrap();
        assert_eq!(drafts[0].origin, "https://bank.test");
        assert_eq!(drafts[0].username, "bob");
    }

    #[test]
    fn an_empty_url_falls_back_to_the_title() {
        let csv = "name,url,username,password\nMy bank,,bob,pw\n";
        let drafts = parse(csv, 1).unwrap();
        assert_eq!(drafts[0].origin, "My bank");
    }

    #[test]
    fn a_file_without_a_password_column_is_refused() {
        let csv = "url,username\nhttps://a.test,bob\n";
        assert!(matches!(parse(csv, 1), Err(TransferError::Columns(_))));
    }

    #[test]
    fn a_file_with_only_passwords_and_no_site_is_refused() {
        let csv = "password,otp\npw,123\n";
        assert!(matches!(parse(csv, 1), Err(TransferError::Columns(_))));
    }

    #[test]
    fn a_header_only_file_imports_nothing_and_says_so_without_failing() {
        let drafts = parse("name,url,username,password,note\n", 1).unwrap();
        assert!(drafts.is_empty());
    }

    #[test]
    fn passwords_holding_commas_and_quotes_survive_the_csv() {
        let csv = "url,username,password\n\
                   https://a.test,bob,\"pw,with\"\"quotes\"\n";
        let drafts = parse(csv, 1).unwrap();
        assert_eq!(drafts[0].secret, "pw,with\"quotes");
    }

    /// Credentials only exist through the vault's own door, so tests
    /// build them the way the browser does.
    fn saved(origin: &str, user: &str, secret: &str, note: &str) -> Credentials {
        let mut vault = Credentials::default();
        vault
            .add(&Draft {
                origin: origin.into(),
                username: user.into(),
                secret: secret.into(),
                note: note.into(),
                updated_at: 1,
            })
            .unwrap();
        vault
    }

    #[test]
    fn export_writes_chromes_own_header() {
        let vault = saved("https://a.test/", "bob", "pw", "n");
        let csv = render(&vault.rows().iter().collect::<Vec<_>>());
        assert!(csv.starts_with("name,url,username,password,note\r\n"));
        assert!(csv.contains("a.test,https://a.test/,bob,pw,n"));
        assert!(looks_like(&csv));
    }

    #[test]
    fn site_name_is_the_bare_host() {
        assert_eq!(site_name("https://example.com/path?q=1"), "example.com");
        assert_eq!(site_name("http://user@example.com:8080/"), "example.com");
        assert_eq!(
            site_name("android://hash@com.app"),
            "android://hash@com.app"
        );
    }

    #[test]
    fn export_then_import_is_lossless() {
        let mut vault = Credentials::default();
        for (origin, user, secret, note) in [
            ("https://a.test/", "bob", "pw1", ""),
            (
                "https://b.test/",
                "ann",
                "pw,2 \"odd\"",
                "a note\nwith a line break",
            ),
            ("android://hash@com.example.app", "", "pw3", ""),
        ] {
            vault
                .add(&Draft {
                    origin: origin.into(),
                    username: user.into(),
                    secret: secret.into(),
                    note: note.into(),
                    updated_at: 9,
                })
                .unwrap();
        }
        let rows: Vec<&Credential> = vault.rows().iter().collect();
        let csv = render(&rows);

        let mut back = Credentials::default();
        let report = back.add_all(&parse(&csv, 9).unwrap());
        assert_eq!(report.added, 3);
        assert_eq!(report.rejected, 0);
        let before: Vec<_> = vault
            .rows()
            .iter()
            .map(|c| {
                (
                    c.origin().to_string(),
                    c.username().to_string(),
                    c.secret().expose().to_string(),
                    c.note().to_string(),
                )
            })
            .collect();
        let after: Vec<_> = back
            .rows()
            .iter()
            .map(|c| {
                (
                    c.origin().to_string(),
                    c.username().to_string(),
                    c.secret().expose().to_string(),
                    c.note().to_string(),
                )
            })
            .collect();
        assert_eq!(before, after);
    }

    #[test]
    fn looks_like_only_claims_password_files() {
        assert!(looks_like("name,url,username,password,note\n"));
        assert!(looks_like("\"url\",\"username\",\"password\"\n"));
        assert!(!looks_like("url,title,visits,last_visit\n"));
        assert!(!looks_like("<!DOCTYPE NETSCAPE-Bookmark-file-1>"));
        assert!(!looks_like(""));
    }
}
