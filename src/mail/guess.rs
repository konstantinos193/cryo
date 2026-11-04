//! Provider guessing: the onboarding form asks for an address and a
//! password; for the providers everyone actually uses, the server
//! coordinates are knowable. A miss just means the form's advanced
//! fields stay open for the user to fill.

use super::account::{ServerSpec, Tls};

/// `(imap, smtp)` defaults for the address's domain, or `None` — the
/// form then shows its server fields. Hosts and ports only; the login
/// name convention (the address itself) lives in `Account::new`.
pub fn defaults_for(email: &str) -> Option<(ServerSpec, ServerSpec)> {
    let domain = email.rsplit('@').next()?.to_ascii_lowercase();
    let (imap_host, smtp_host) = match domain.as_str() {
        "gmail.com" | "googlemail.com" => ("imap.gmail.com", "smtp.gmail.com"),
        "outlook.com" | "hotmail.com" | "live.com" | "msn.com" | "hotmail.gr" => {
            ("outlook.office365.com", "smtp.office365.com")
        }
        "yahoo.com" | "yahoo.gr" | "ymail.com" => ("imap.mail.yahoo.com", "smtp.mail.yahoo.com"),
        "icloud.com" | "me.com" | "mac.com" => ("imap.mail.me.com", "smtp.mail.me.com"),
        "gmx.com" | "gmx.net" | "gmx.de" | "gmx.at" | "gmx.ch" => ("imap.gmx.com", "mail.gmx.com"),
        "zoho.com" | "zohomail.com" => ("imap.zoho.com", "smtp.zoho.com"),
        "aol.com" => ("imap.aol.com", "smtp.aol.com"),
        "proton.me" | "protonmail.com" | "pm.me" => return None, // needs the bridge
        "fastmail.com" | "fastmail.fm" => ("imap.fastmail.com", "smtp.fastmail.com"),
        _ => return None,
    };
    Some((
        ServerSpec::new(imap_host, 993, Tls::Wrapper).expect("a table entry is valid"),
        ServerSpec::new(smtp_host, 465, Tls::Wrapper).expect("a table entry is valid"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_big_providers_guess_whole_setups() {
        let (imap, smtp) = defaults_for("ann@gmail.com").unwrap();
        assert_eq!(imap.host(), "imap.gmail.com");
        assert_eq!(imap.port(), 993);
        assert_eq!(smtp.host(), "smtp.gmail.com");
        assert_eq!(smtp.port(), 465);
        assert!(defaults_for("ann@OUTLOOK.com").is_some());
    }

    #[test]
    fn unknown_and_bridge_providers_ask_for_servers() {
        assert!(defaults_for("ann@proton.me").is_none());
        assert!(defaults_for("ann@localhost").is_none());
        assert!(defaults_for("ann@").is_none());
        assert!(defaults_for("no-at-sign").is_none());
    }
}
