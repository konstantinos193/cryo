//! Accounts: who the client signs in as and where its servers live.
//!
//! `Account` is the entity — one per address, validated at construction
//! so no half-usable account exists (AGENTS §6). `AccountsFile` is the
//! versioned DTO `accounts.json` serializes; passwords never appear
//! here — they live in the vault under `imap://host` / `smtp://host`
//! origins.

use serde::{Deserialize, Serialize};

use super::MailError;

/// How a connection is secured. Plaintext is not offered: a mail
/// password over a bare socket is a credential leak, not a feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tls {
    /// TLS from the first byte (993, 465).
    Wrapper,
    /// STARTTLS upgrade after the greeting (143, 587).
    StartTls,
}

/// One server's coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerSpec {
    host: String,
    port: u16,
    tls: Tls,
}

impl ServerSpec {
    /// The door both hydration and the onboarding form come through.
    pub fn new(host: &str, port: u16, tls: Tls) -> Result<Self, MailError> {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() || host.chars().any(char::is_whitespace) {
            return Err(MailError::Invalid("a server needs a host name".into()));
        }
        if port == 0 {
            return Err(MailError::Invalid("a server needs a port".into()));
        }
        Ok(Self { host, port, tls })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn tls(&self) -> Tls {
        self.tls
    }

    /// The vault origin this server's password is keyed under —
    /// `imap://imap.example.com`, `smtp://smtp.example.com`. It can
    /// never collide with a web origin, so an autofill prompt will
    /// never offer it to a page.
    pub fn vault_origin(&self, scheme: &str) -> String {
        format!("{scheme}://{}", self.host)
    }
}

/// A signature is a few lines and maybe a logo; past this it is a page.
const MAX_SIGNATURE_BYTES: usize = 512 * 1024;

/// The page shrinks a picked picture to a small square before it gets
/// here, so a larger one didn't come from the page.
const MAX_PHOTO_BYTES: usize = 256 * 1024;

/// A configured account. `Clone` freely — it carries no secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    display: String,
    /// The address and the identity — two accounts never share one.
    email: String,
    /// IMAP login name; usually the address itself.
    username: String,
    imap: ServerSpec,
    smtp: ServerSpec,
    /// Sanitized HTML appended to what this account sends; may be empty.
    signature: String,
    /// The account's own picture as a `data:` URL; may be empty.
    photo: String,
}

impl Account {
    /// The one constructor — onboarding's fields and the stored row both
    /// land here, so a stored file can no more produce an invalid
    /// account than the form can.
    pub fn new(
        display: &str,
        email: &str,
        username: &str,
        imap: ServerSpec,
        smtp: ServerSpec,
    ) -> Result<Self, MailError> {
        let email = email.trim().to_ascii_lowercase();
        if !email.contains('@')
            || email.starts_with('@')
            || email.ends_with('@')
            || email.contains(char::is_whitespace)
        {
            return Err(MailError::Invalid("that is not an email address".into()));
        }
        let username = match username.trim() {
            "" => email.clone(),
            u => u.to_string(),
        };
        Ok(Self {
            display: display.trim().to_string(),
            email,
            username,
            imap,
            smtp,
            signature: String::new(),
            photo: String::new(),
        })
    }

    /// This account with the settings page's fields replaced — the
    /// signature comes back sanitized, so a stored one is always safe to
    /// show and to send.
    pub fn with_profile(
        mut self,
        display: &str,
        signature: &str,
        photo: &str,
    ) -> Result<Self, MailError> {
        if signature.len() > MAX_SIGNATURE_BYTES {
            return Err(MailError::Invalid(
                "that signature is too large — use smaller images".into(),
            ));
        }
        let is_picture = ["png", "jpeg", "webp"]
            .iter()
            .any(|kind| photo.starts_with(&format!("data:image/{kind};base64,")));
        if !photo.is_empty() && (!is_picture || photo.len() > MAX_PHOTO_BYTES) {
            return Err(MailError::Invalid("that picture can't be used".into()));
        }
        self.display = display.trim().to_string();
        self.signature = super::sanitize::clean_signature(signature);
        self.photo = photo.to_string();
        Ok(self)
    }

    /// What the "From" header and the SMTP envelope use — "Display
    /// <addr@host>" with the name dropped when none was set. `new`
    /// already refused a non-address; lettre's stricter grammar is the
    /// second gate and reports the address it rejected.
    pub fn mailbox(&self) -> Result<lettre::message::Mailbox, MailError> {
        let name = if self.display.is_empty() {
            None
        } else {
            Some(self.display.clone())
        };
        self.email
            .parse::<lettre::Address>()
            .map(|address| lettre::message::Mailbox::new(name, address))
            .map_err(|_| MailError::Invalid(format!("{} is not sendable", self.email)))
    }

    pub fn display(&self) -> &str {
        &self.display
    }

    pub fn email(&self) -> &str {
        &self.email
    }

    pub fn signature(&self) -> &str {
        &self.signature
    }

    pub fn photo(&self) -> &str {
        &self.photo
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn imap(&self) -> &ServerSpec {
        &self.imap
    }

    pub fn smtp(&self) -> &ServerSpec {
        &self.smtp
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerDto {
    host: String,
    port: u16,
    tls: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountDto {
    display: String,
    email: String,
    #[serde(default)]
    username: String,
    imap: ServerDto,
    smtp: ServerDto,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    signature: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    photo: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountsFile {
    version: u32,
    accounts: Vec<AccountDto>,
}

const CURRENT_VERSION: u32 = 1;

fn server_dto(s: &ServerSpec) -> ServerDto {
    ServerDto {
        host: s.host.clone(),
        port: s.port,
        tls: match s.tls {
            Tls::Wrapper => "tls".into(),
            Tls::StartTls => "starttls".into(),
        },
    }
}

fn dto_server(d: &ServerDto) -> Result<ServerSpec, MailError> {
    let tls = match d.tls.as_str() {
        "tls" => Tls::Wrapper,
        "starttls" => Tls::StartTls,
        other => return Err(MailError::Invalid(format!("unknown tls mode {other}"))),
    };
    ServerSpec::new(&d.host, d.port, tls)
}

/// Read `accounts.json`. A missing file is a profile with no accounts,
/// not an error; a file from another version is read best-effort row by
/// row, bad rows dropped — accounts cost a password each, so losing the
/// whole list to one typo'd row would be cruel.
pub fn load_accounts(path: &std::path::Path) -> Result<Vec<Account>, MailError> {
    let Some(bytes) = crate::read_file(path).map_err(|e| MailError::io("read", path, e))? else {
        return Ok(Vec::new());
    };
    let file: AccountsFile =
        serde_json::from_slice(&bytes).map_err(|e| MailError::io("parse", path, e))?;
    if file.version != CURRENT_VERSION {
        return Err(MailError::Version(file.version));
    }
    Ok(file
        .accounts
        .iter()
        .filter_map(|d| {
            let (imap, smtp) = (dto_server(&d.imap).ok()?, dto_server(&d.smtp).ok()?);
            let account = Account::new(&d.display, &d.email, &d.username, imap, smtp).ok()?;
            // A profile that no longer validates costs the profile, not
            // the account and the password behind it.
            Some(
                account
                    .clone()
                    .with_profile(&d.display, &d.signature, &d.photo)
                    .unwrap_or(account),
            )
        })
        .collect())
}

/// Write the account list atomically.
pub fn save_accounts(path: &std::path::Path, accounts: &[Account]) -> Result<(), MailError> {
    let file = AccountsFile {
        version: CURRENT_VERSION,
        accounts: accounts
            .iter()
            .map(|a| AccountDto {
                display: a.display.clone(),
                email: a.email.clone(),
                username: a.username.clone(),
                imap: server_dto(&a.imap),
                smtp: server_dto(&a.smtp),
                signature: a.signature.clone(),
                photo: a.photo.clone(),
            })
            .collect(),
    };
    let bytes = serde_json::to_vec_pretty(&file).map_err(|e| MailError::io("encode", path, e))?;
    crate::write_file_atomic(path, &bytes).map_err(|e| MailError::io("write", path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(host: &str) -> ServerSpec {
        ServerSpec::new(host, 993, Tls::Wrapper).unwrap()
    }

    #[test]
    fn construction_validates_what_the_form_could_send() {
        assert!(Account::new("Ann", "ann@a.test", "", spec("i.a.test"), spec("s.a.test")).is_ok());
        for bad in ["", "ann", "@a.test", "ann@", " ann @a.test"] {
            assert!(
                Account::new("", bad, "", spec("i.a.test"), spec("s.a.test")).is_err(),
                "{bad}"
            );
        }
        for bad in ["", " ", "i map.test", "\t"] {
            assert!(ServerSpec::new(bad, 993, Tls::Wrapper).is_err(), "{bad}");
        }
        assert!(ServerSpec::new("h", 0, Tls::Wrapper).is_err());
    }

    #[test]
    fn username_defaults_to_the_address() {
        let a = Account::new("", "Ann@A.test", " ", spec("i.a.test"), spec("s.a.test")).unwrap();
        assert_eq!(a.username(), "ann@a.test");
        assert_eq!(a.email(), "ann@a.test", "addresses are case-folded");
    }

    #[test]
    fn vault_origins_cannot_look_like_web_sites() {
        assert_eq!(
            spec("imap.a.test").vault_origin("imap"),
            "imap://imap.a.test"
        );
        assert!(crate::vault::SiteOrigin::parse("imap://imap.a.test").is_none());
    }

    #[test]
    fn a_profile_is_validated_and_its_signature_sanitized() {
        let a = Account::new("Ann", "ann@a.test", "", spec("i.a.test"), spec("s.a.test")).unwrap();
        let signed = a
            .clone()
            .with_profile(
                " Ann R ",
                "<p>Ann<script>x</script></p><img src=\"https://a.test/logo.png\">",
                "data:image/jpeg;base64,AAAA",
            )
            .unwrap();
        assert_eq!(signed.display(), "Ann R");
        assert!(!signed.signature().contains("script"));
        assert!(
            signed.signature().contains("https://a.test/logo.png"),
            "a signature's logo is the sender's own choice to send"
        );
        assert!(a
            .clone()
            .with_profile("", "", "javascript:alert(1)")
            .is_err());
        assert!(a
            .clone()
            .with_profile("", "", "data:text/html;base64,AA")
            .is_err());
        assert!(a
            .with_profile("", &"x".repeat(MAX_SIGNATURE_BYTES + 1), "")
            .is_err());
    }

    #[test]
    fn a_profile_survives_the_file_and_files_without_one_still_load() {
        let dir = crate::test_dir("mail-profile");
        let path = dir.join("accounts.json");
        let a = Account::new("Ann", "ann@a.test", "", spec("i.a.test"), spec("s.a.test"))
            .unwrap()
            .with_profile("Ann", "<p>Regards</p>", "data:image/png;base64,AA")
            .unwrap();
        save_accounts(&path, std::slice::from_ref(&a)).unwrap();
        let back = load_accounts(&path).unwrap();
        assert_eq!(back[0].signature(), "<p>Regards</p>");
        assert_eq!(back[0].photo(), "data:image/png;base64,AA");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn accounts_round_trip_and_drop_bad_rows() {
        let dir = crate::test_dir("mail-accounts");
        let path = dir.join("accounts.json");
        let a = Account::new("Ann", "ann@a.test", "", spec("i.a.test"), spec("s.a.test")).unwrap();
        save_accounts(&path, std::slice::from_ref(&a)).unwrap();
        assert_eq!(load_accounts(&path).unwrap(), vec![a]);
        let mut file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        file["accounts"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "display": "B", "email": "not-an-address",
                "imap": {"host": "h", "port": 1, "tls": "tls"},
                "smtp": {"host": "h", "port": 1, "tls": "tls"}
            }));
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert_eq!(load_accounts(&path).unwrap().len(), 1);
        assert!(load_accounts(&dir.join("missing.json")).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
