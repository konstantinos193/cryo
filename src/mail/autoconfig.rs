//! Autoconfig: how the form fills its server boxes from an address.
//!
//! The lookup chain is the one every client that "just fills it in"
//! follows: the built-in provider table (`guess`) → the domain's own
//! `autoconfig.<domain>/mail/config-v1.1.xml` → Mozilla's hosted
//! database (`autoconfig.thunderbird.net`) → the near-universal
//! `imap.`/`smtp.` host pattern. Each source only produces a starting
//! point for the form — the worker's `Verify` is still what proves the
//! servers before anything is saved.
//!
//! Fetches run on the worker thread. The XML is a fixed flat schema,
//! so extraction is a bounded tag scan, not a parser dependency.

use super::account::{ServerSpec, Tls};
use super::guess::defaults_for;

/// Autoconfig payloads are a few kilobytes; a megabyte answer is not a
/// config file.
const MAX_XML: u64 = 64 * 1024;

/// A slow `autoconfig.` vhost must not hold the worker queue — the
/// whole lookup gets one modest budget.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// `(imap, smtp)` for the address, from whichever source answers
/// first. `None` leaves the form's server fields for the user.
pub(crate) fn autoconfig(email: &str) -> Option<(ServerSpec, ServerSpec)> {
    let domain = email.rsplit('@').next()?.trim().to_ascii_lowercase();
    if domain.is_empty() {
        return None;
    }
    defaults_for(email)
        .or_else(|| fetch_xml(&domain))
        .or_else(|| pattern_guess(&domain))
}

/// The domain's own config host, then the shared database.
fn fetch_xml(domain: &str) -> Option<(ServerSpec, ServerSpec)> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .new_agent();
    for url in [
        format!("https://autoconfig.{domain}/mail/config-v1.1.xml"),
        format!("https://autoconfig.thunderbird.net/v1.1/{domain}"),
    ] {
        let Ok(mut response) = agent.get(&url).call() else {
            continue;
        };
        let Ok(text) = response
            .body_mut()
            .with_config()
            .limit(MAX_XML)
            .read_to_string()
        else {
            continue;
        };
        if let Some(pair) = parse_config(&text) {
            return Some(pair);
        }
    }
    None
}

/// The schema is fixed and flat: an `incomingServer` and an
/// `outgoingServer` block, each holding hostname/port/socketType
/// tags. Case-sensitive is correct — the schema defines the casing.
fn parse_config(xml: &str) -> Option<(ServerSpec, ServerSpec)> {
    let imap = server_block(xml, "incomingServer", "imap")?;
    let smtp = server_block(xml, "outgoingServer", "smtp")?;
    Some((spec_from(imap)?, spec_from(smtp)?))
}

/// The `<…Server type="kind">` block's inner text — skips sibling
/// blocks of other types (an ispdb entry often lists imap AND pop3).
fn server_block<'a>(xml: &'a str, element: &str, kind: &str) -> Option<&'a str> {
    let open = format!("<{element}");
    let close = format!("</{element}>");
    let mut rest = xml;
    while let Some(at) = rest.find(&open) {
        let after = &rest[at + open.len()..];
        let tag_end = after.find('>')?;
        if after[..tag_end].contains(&format!("\"{kind}\"")) {
            let body = &after[tag_end + 1..];
            let end = body.find(&close)?;
            return Some(&body[..end]);
        }
        let end = after[tag_end + 1..].find(&close)? + tag_end + 1 + close.len();
        rest = &after[end..];
    }
    None
}

/// hostname + port + socketType → a validated spec. Plain sockets are
/// refused — a guess should never steer someone to a cleartext login.
fn spec_from(block: &str) -> Option<ServerSpec> {
    let tls = match tag(block, "socketType").map(str::trim) {
        Some("SSL") => Tls::Wrapper,
        Some("STARTTLS") => Tls::StartTls,
        _ => return None,
    };
    let port: u16 = tag(block, "port")?.trim().parse().ok()?;
    ServerSpec::new(tag(block, "hostname")?.trim(), port, tls).ok()
}

/// `<name>value</name>` — the tag has no attributes in this schema.
fn tag<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&close)? + start;
    Some(&block[start..end])
}

/// `imap.<domain>`/`smtp.<domain>` on the standard TLS ports — wrong
/// for some hosts, right for more, and always editable in the form.
fn pattern_guess(domain: &str) -> Option<(ServerSpec, ServerSpec)> {
    Some((
        ServerSpec::new(&format!("imap.{domain}"), 993, Tls::Wrapper).ok()?,
        ServerSpec::new(&format!("smtp.{domain}"), 465, Tls::Wrapper).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape ispdb actually serves.
    const ISPDB: &str = r#"<?xml version="1.0"?>
<clientConfig version="1.1">
  <emailProvider id="example">
    <domain>example.gr</domain>
    <incomingServer type="pop3">
      <hostname>pop.example.gr</hostname><port>995</port>
      <socketType>SSL</socketType>
    </incomingServer>
    <incomingServer type="imap">
      <hostname>imap.example.gr</hostname><port>143</port>
      <socketType>STARTTLS</socketType>
      <username>%EMAILADDRESS%</username>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.example.gr</hostname><port>587</port>
      <socketType>STARTTLS</socketType>
    </outgoingServer>
  </emailProvider>
</clientConfig>"#;

    #[test]
    fn an_ispdb_entry_yields_a_validated_pair() {
        let (imap, smtp) = parse_config(ISPDB).expect("the fixture parses");
        assert_eq!(imap.host(), "imap.example.gr");
        assert_eq!(imap.port(), 143);
        assert!(matches!(imap.tls(), Tls::StartTls));
        assert_eq!(smtp.host(), "smtp.example.gr");
        assert_eq!(smtp.port(), 587);
    }

    #[test]
    fn the_imap_block_wins_over_a_pop3_sibling() {
        let (imap, _) = parse_config(ISPDB).unwrap();
        assert_ne!(imap.host(), "pop.example.gr");
    }

    #[test]
    fn a_plain_socket_is_not_a_default_we_hand_out() {
        let xml = r#"<clientConfig><emailProvider>
            <incomingServer type="imap"><hostname>i.x</hostname>
            <port>143</port><socketType>plain</socketType></incomingServer>
            <outgoingServer type="smtp"><hostname>s.x</hostname>
            <port>25</port><socketType>plain</socketType></outgoingServer>
            </emailProvider></clientConfig>"#;
        assert!(parse_config(xml).is_none());
    }

    #[test]
    fn the_pattern_fills_something_for_any_domain() {
        let (imap, _) = pattern_guess("mail.example.org").unwrap();
        assert_eq!(imap.host(), "imap.mail.example.org");
    }
}
