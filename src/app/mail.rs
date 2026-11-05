//! The mail slice: the /mail page's commands and the worker's reports.
//!
//! The page talks over `window.ipc` (a `PageMessage`); only a tab whose
//! address is exactly [`pages::MAIL_URL`] may send them — the internal
//! origin is shared with newtab and settings, so the URL check is what
//! keeps a web page, or even another shell page, out of the mailbox.
//! Everything the page sees arrives by `__cryo_mail` eval; nothing is
//! fetchable. RAM stays the product: the app's whole resident set is
//! the account list, folder names and statuses — listings and bodies
//! pass through from worker to page without the app keeping a copy.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use zeroize::Zeroize;

use crate::engine::PageMessage;
use crate::mail::{self, Account, Folder, MailError, ServerSpec, Tls};
use crate::shell::pages::{self, INTERNAL_ORIGIN, MAIL_URL, NEW_TAB_URL};
use crate::tabs::TabId;
use crate::vault::{Draft, Secret};

use super::{unix_now, App};

/// Re-sync every this often while the window runs — the `resource_tick`
/// heartbeat reads it. A mail client that only syncs while watched is
/// a webmail tab; one that syncs constantly is a battery bill.
const SYNC_EVERY: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// What the app keeps of mail — one per normal window, `None` in
/// incognito (an account there would leak the profile it shouldn't).
pub(super) struct Mail {
    worker: mail::Worker,
    /// `data_dir()/mail` — `accounts.json` lives inside; the cache root
    /// is the `FileStore`'s own concern.
    dir: PathBuf,
    accounts: Vec<Account>,
    /// Folder lists as syncs report them — the sidebar's data.
    folders: HashMap<String, Vec<Folder>>,
    /// Per-account status line — "", "syncing" or the last error.
    statuses: HashMap<String, String>,
    /// The folder the page shows — syncs fetch its listing, others get
    /// counts only.
    selected: Option<(String, String)>,
    /// The tab showing /mail — pushes and the "already open" check use
    /// it. Set on `hello`, so the last-opened mail tab wins.
    tab: Option<TabId>,
    /// The account awaiting its vault write between a successful
    /// `Verify` and the `Verified` outcome — the password rides here
    /// once, never in the page, never on disk.
    pending: Option<(Account, Secret)>,
    /// Files the compose drawer has picked — paths stay here, the page
    /// sees names and sizes, the worker reads them at send time.
    files: Vec<PathBuf>,
    last_sync: Instant,
}

impl Mail {
    pub fn new(dir: PathBuf, out: impl Fn(mail::Outcome) + Send + 'static) -> Self {
        let accounts_path = dir.join("accounts.json");
        let accounts = mail::load_accounts(&accounts_path).unwrap_or_else(|e| {
            crate::report!("mail accounts: {e}");
            Vec::new()
        });
        Self {
            worker: mail::spawn(mail::FileStore::new(dir.join("cache")), out),
            dir,
            accounts,
            folders: HashMap::new(),
            statuses: HashMap::new(),
            selected: None,
            tab: None,
            pending: None,
            files: Vec::new(),
            last_sync: Instant::now(),
        }
    }

    fn account(&self, email: &str) -> Option<Account> {
        self.accounts.iter().find(|a| a.email() == email).cloned()
    }

    fn accounts_path(&self) -> PathBuf {
        self.dir.join("accounts.json")
    }

    /// The state push — the sidebar's whole data.
    fn state_push(&self) -> mail::Push {
        mail::Push::State {
            incognito: false,
            accounts: self
                .accounts
                .iter()
                .map(|a| mail::PushAccount {
                    email: a.email().to_string(),
                    display: a.display().to_string(),
                    signature: a.signature().to_string(),
                    photo: a.photo().to_string(),
                    username: a.username().to_string(),
                    imap: format!("{}:{}", a.imap().host(), a.imap().port()),
                    smtp: format!("{}:{}", a.smtp().host(), a.smtp().port()),
                    folders: self
                        .folders
                        .get(a.email())
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .map(|f| mail::PushFolder {
                            name: f.name.clone(),
                            display: f.display.clone(),
                            path: f.path.clone(),
                            depth: f.depth,
                            selectable: f.selectable,
                            role: role_str(f.role),
                            unseen: f.unseen,
                            exists: f.exists,
                        })
                        .collect(),
                    status: self.statuses.get(a.email()).cloned().unwrap_or_default(),
                })
                .collect(),
        }
    }

    /// The home page's view of mail: one unread count per account,
    /// summed over its inbox — the folder column's data boiled down to
    /// what a tile can show.
    fn glance(&self) -> mail::Glance {
        let accounts: Vec<mail::GlanceAccount> = self
            .accounts
            .iter()
            .map(|a| mail::GlanceAccount {
                email: a.email().to_string(),
                display: a.display().to_string(),
                unseen: self
                    .folders
                    .get(a.email())
                    .map(|folders| {
                        folders
                            .iter()
                            .filter(|f| f.role == mail::Role::Inbox)
                            .filter_map(|f| f.unseen)
                            .sum()
                    })
                    .unwrap_or(0),
                status: self.statuses.get(a.email()).cloned().unwrap_or_default(),
            })
            .collect();
        mail::Glance {
            incognito: false,
            unseen: accounts.iter().map(|a| a.unseen).sum(),
            accounts,
        }
    }
}

fn role_str(role: mail::Role) -> &'static str {
    match role {
        mail::Role::Inbox => "inbox",
        mail::Role::Drafts => "drafts",
        mail::Role::Sent => "sent",
        mail::Role::Junk => "junk",
        mail::Role::Archive => "archive",
        mail::Role::Trash => "trash",
        mail::Role::Other => "other",
    }
}

fn tls_str(tls: Tls) -> &'static str {
    match tls {
        Tls::Wrapper => "tls",
        Tls::StartTls => "starttls",
    }
}

fn flag_str(atom: &str) -> &'static str {
    match atom {
        "\\Seen" => "seen",
        "\\Flagged" => "flagged",
        "\\Answered" => "answered",
        "\\Draft" => "draft",
        _ => "deleted",
    }
}

impl App {
    /// Open mail — focus the existing mail tab rather than stacking
    /// duplicates, like the apps this is shaped after.
    pub(super) fn open_mail(&mut self) -> anyhow::Result<()> {
        let existing = self
            .registry
            .iter()
            .map(|t| t.id())
            .find(|id| self.tab_shows(*id, MAIL_URL));
        match existing {
            Some(id) => self.activate(id),
            None => self.spawn_tab(MAIL_URL, true),
        }
    }

    /// A tab whose surface is showing the mail page — the engine's
    /// word, not the recorded address, since that is what gates the
    /// mailbox.
    pub(super) fn is_mail_tab(&self, id: TabId) -> bool {
        self.tab_shows(id, MAIL_URL)
    }

    /// The page → app command, already gated by `on_page_message`'s
    /// caller to a tab showing the mail page.
    pub(super) fn on_mail_command(&mut self, id: TabId, mut message: PageMessage) {
        // The internal origin is shared — a shell page is trusted only
        // for the URL its surface actually has loaded, which the caller
        // checked; the message must claim that origin too.
        let claimed = url::Url::parse(&message.source)
            .is_ok_and(|u| u.origin().ascii_serialization() == INTERNAL_ORIGIN);
        if !claimed || !self.is_mail_tab(id) {
            message.body.zeroize();
            return;
        }
        let command = serde_json::from_str::<mail::PageCommand>(&message.body);
        message.body.zeroize();
        let Ok(command) = command else {
            return;
        };
        if let Err(e) = self.mail_command(id, command) {
            crate::report!("mail command: {e:?}");
        }
    }

    fn mail_command(&mut self, id: TabId, command: mail::PageCommand) -> anyhow::Result<()> {
        use mail::PageCommand as C;
        match command {
            C::Hello {} => self.mail_hello(id),
            C::SelectFolder { account, folder } => {
                if let Some(mail) = self.mail_mut() {
                    mail.selected = Some((account.clone(), folder.clone()));
                }
                self.mail_worker(mail::Request::List { account, folder });
                self.mail_sync_selected();
                Ok(())
            }
            C::Open {
                account,
                folder,
                uid,
                remote,
            } => match self.mail_account(&account) {
                Some((account, secret)) => {
                    self.mail_worker(mail::Request::Open {
                        account,
                        secret,
                        folder,
                        uid,
                        remote,
                    });
                    Ok(())
                }
                None => Ok(()),
            },
            C::Sync {} => {
                self.mail_sync_all();
                Ok(())
            }
            C::SetFlag {
                account,
                folder,
                uid,
                flag,
                on,
            } => match self.mail_account(&account) {
                Some((account, secret)) => {
                    self.mail_worker(mail::Request::SetFlag {
                        account,
                        secret,
                        folder,
                        uid,
                        flag: flag.into(),
                        on,
                    });
                    Ok(())
                }
                None => Ok(()),
            },
            C::MoveTo {
                account,
                folder,
                uid,
                target,
            } => match self.mail_account(&account) {
                Some((account, secret)) => {
                    self.mail_worker(mail::Request::MoveTo {
                        account,
                        secret,
                        folder,
                        uid,
                        target,
                    });
                    Ok(())
                }
                None => Ok(()),
            },
            C::Delete {
                account,
                folder,
                uid,
            } => match self.mail_account(&account) {
                Some((account, secret)) => {
                    self.mail_worker(mail::Request::Delete {
                        account,
                        secret,
                        folder,
                        uid,
                    });
                    Ok(())
                }
                None => Ok(()),
            },
            command @ C::AddAccount { .. } => self.mail_add_account(command),
            C::RemoveAccount { email } => self.mail_remove_account(&email),
            C::SaveProfile {
                email,
                display,
                signature,
                photo,
            } => self.mail_save_profile(&email, &display, &signature, &photo),
            C::CleanSignature { html } => self.mail_push(&mail::Push::SignatureClean {
                html: mail::clean_signature(&html),
            }),
            C::Faces { addrs, lookup } => {
                self.mail_worker(mail::Request::Faces { addrs, lookup });
                Ok(())
            }
            C::SetFace { addr, photo } => {
                self.mail_worker(mail::Request::SetFace { addr, photo });
                Ok(())
            }
            C::Send {
                account,
                to,
                cc,
                bcc,
                subject,
                text,
                quote,
                sign,
                in_reply_to,
                references,
            } => match self.mail_account(&account) {
                Some((account, secret)) => {
                    let attachments = self.mail_take_files();
                    self.mail_worker(mail::Request::Send {
                        account,
                        secret,
                        draft: mail::Draft {
                            to,
                            cc,
                            bcc,
                            subject,
                            text,
                            quote,
                            sign,
                            in_reply_to,
                            references,
                            attachments,
                        },
                    });
                    Ok(())
                }
                None => Ok(()),
            },
            C::SaveDraft {
                account,
                to,
                cc,
                bcc,
                subject,
                text,
                quote,
                sign,
            } => match self.mail_account(&account) {
                Some((account, secret)) => {
                    let attachments = self.mail_take_files();
                    self.mail_worker(mail::Request::SaveDraft {
                        account,
                        secret,
                        draft: mail::Draft {
                            to,
                            cc,
                            bcc,
                            subject,
                            text,
                            quote,
                            sign,
                            attachments,
                            ..mail::Draft::default()
                        },
                    });
                    Ok(())
                }
                None => Ok(()),
            },
            C::AttachPick {} => {
                let picked = rfd::FileDialog::new().pick_files().unwrap_or_default();
                if let Some(mail) = self.mail_mut() {
                    mail.files.extend(picked);
                }
                self.mail_push_files()
            }
            C::AttachDrop { index } => {
                if let Some(mail) = self.mail_mut() {
                    if index < mail.files.len() {
                        mail.files.remove(index);
                    }
                }
                self.mail_push_files()
            }
            C::AttachClear {} => {
                if let Some(mail) = self.mail_mut() {
                    mail.files.clear();
                }
                self.mail_push_files()
            }
            C::SaveAttachment {
                account,
                folder,
                uid,
                index,
            } => match self.mail_account(&account) {
                Some((account, secret)) => {
                    self.mail_worker(mail::Request::Attachment {
                        account,
                        secret,
                        folder,
                        uid,
                        index,
                    });
                    Ok(())
                }
                None => Ok(()),
            },
            C::Guess { email } => {
                // The lookup may hit the network (autoconfig, ispdb) —
                // it rides the worker, never this thread.
                self.mail_worker(mail::Request::Guess { email });
                Ok(())
            }
        }
    }

    /// `hello`: the page is up — mark it, push everything, kick a sync.
    fn mail_hello(&mut self, id: TabId) -> anyhow::Result<()> {
        let empty = match self.mail.as_mut() {
            Some(mail) => {
                mail.tab = Some(id);
                mail.accounts.is_empty()
            }
            // Incognito has no Mail at all — the page still wants its
            // state, which is exactly "you can't have mail here".
            None => {
                return self.mail_push(&mail::Push::State {
                    incognito: true,
                    accounts: Vec::new(),
                })
            }
        };
        if empty {
            return self.mail_push(&mail::Push::State {
                incognito: false,
                accounts: Vec::new(),
            });
        }
        let push = self.mail.as_ref().map(|m| m.state_push());
        if let Some(push) = push {
            self.mail_push(&push)?;
        }
        self.mail_sync_all();
        Ok(())
    }

    /// Worker outcome → the page. Reports the app doesn't need first
    /// go straight through; the rest update state, then push.
    pub(super) fn on_mail(&mut self, outcome: mail::Outcome) -> anyhow::Result<()> {
        use mail::Outcome as O;
        match outcome {
            O::Verified { account } => self.mail_verified(account),
            O::Folders { email, folders } => {
                if let Some(mail) = self.mail_mut() {
                    mail.folders.insert(email.clone(), folders);
                    mail.statuses.insert(email, String::new());
                }
                let push = self.mail.as_ref().map(|m| m.state_push());
                if let Some(push) = push {
                    self.mail_push(&push)?;
                }
                Ok(())
            }
            O::Listing {
                email,
                folder,
                envelopes,
            } => {
                if self
                    .mail
                    .as_ref()
                    .and_then(|m| m.selected.clone())
                    .is_some_and(|s| s == (email.clone(), folder.clone()))
                {
                    self.mail_push(&mail::Push::List {
                        account: email,
                        folder,
                        envelopes: mail::list_entries(&envelopes),
                    })?;
                }
                Ok(())
            }
            O::Body(opened) => {
                // Marking read on open is eM Client behavior — the flag
                // write is the app's, not the fetch's (PEEK).
                if !opened.seen {
                    if let Some((account, secret)) = self.mail_account(&opened.email) {
                        self.mail_worker(mail::Request::SetFlag {
                            account,
                            secret,
                            folder: opened.folder.clone(),
                            uid: opened.uid,
                            flag: mail::Flag::Seen,
                            on: true,
                        });
                    }
                }
                let mail::Opened {
                    email,
                    folder,
                    uid,
                    body,
                    from,
                    to,
                    cc,
                    subject,
                    date,
                    message_id,
                    references,
                    seen,
                    flagged,
                } = *opened;
                self.mail_push(&mail::Push::Body(Box::new(mail::PushBody {
                    account: email,
                    folder,
                    uid,
                    from,
                    to,
                    cc,
                    subject,
                    date,
                    html: body.html,
                    text: body.text,
                    blocked_remote: body.blocked_remote,
                    remote_loaded: body.remote_loaded,
                    reply_to: body.reply_to,
                    unsubscribe: body.unsubscribe,
                    attachments: body
                        .attachments
                        .iter()
                        .map(|a| mail::PushAttachment {
                            index: a.index,
                            name: a.name.clone(),
                            mime: a.mime.clone(),
                            size: a.size,
                        })
                        .collect(),
                    message_id,
                    references,
                    seen,
                    flagged,
                })))
            }
            O::FlagDone {
                email,
                folder,
                uid,
                flag,
                on,
            } => self.mail_push(&mail::Push::Flag {
                account: email,
                folder,
                uid,
                flag: flag_str(flag),
                on,
            }),
            O::Guess { email: _, specs } => {
                let (imap, smtp) = specs.map(|(i, s)| (Some(i), Some(s))).unwrap_or_default();
                self.mail_push(&mail::Push::Guess {
                    imap_host: imap.as_ref().map(|s| s.host().to_string()),
                    imap_port: imap.as_ref().map(|s| s.port()),
                    imap_tls: imap.as_ref().map(|s| tls_str(s.tls()).to_string()),
                    smtp_host: smtp.as_ref().map(|s| s.host().to_string()),
                    smtp_port: smtp.as_ref().map(|s| s.port()),
                    smtp_tls: smtp.as_ref().map(|s| tls_str(s.tls()).to_string()),
                })
            }
            O::NewMail {
                email,
                count,
                from,
                subject,
            } => self.mail_notify(&email, count, &from, &subject),
            O::Done { op } => {
                if op == "sync" {
                    Ok(())
                } else if let Some((account, folder)) =
                    self.mail.as_ref().and_then(|m| m.selected.clone())
                {
                    // Move/delete/draft changed the listing — re-read
                    // the cached index so the page repaints.
                    self.mail_worker(mail::Request::List { account, folder });
                    Ok(())
                } else {
                    Ok(())
                }
            }
            O::Face { addr, src, chosen } => {
                self.mail_push(&mail::Push::Face { addr, src, chosen })
            }
            O::Sent => self.mail_push(&mail::Push::SendReply {
                error: String::new(),
            }),
            O::Attachment { name, mime, bytes } => {
                let _ = mime;
                let mut error = String::new();
                if let Some(path) = rfd::FileDialog::new().set_file_name(&name).save_file() {
                    if let Err(e) = std::fs::write(&path, &bytes) {
                        error = e.to_string();
                    }
                }
                self.mail_push(&mail::Push::AttachmentSaved { name, error })
            }
            O::Failed { op, email, error } => {
                if matches!(op, "login" | "smtp")
                    && self.mail.as_ref().is_some_and(|m| m.pending.is_some())
                {
                    // The onboarding verify failed — drop the pending
                    // secret; nothing was saved.
                    if let Some(mail) = self.mail.as_mut() {
                        mail.pending = None;
                    }
                    return self.mail_push(&mail::Push::AccountReply { error });
                }
                if matches!(op, "send") {
                    return self.mail_push(&mail::Push::SendReply { error });
                }
                if let Some(mail) = self.mail_mut() {
                    if !email.is_empty() {
                        mail.statuses.insert(email.clone(), error.clone());
                    }
                }
                self.mail_push(&mail::Push::Status {
                    account: email,
                    status: error.clone(),
                })?;
                self.mail_push(&mail::Push::Error {
                    op: op.to_string(),
                    detail: error,
                })
            }
        }
    }

    /// The onboarding verify came back good: seal the password into the
    /// vault under both server origins, write the account, sync it.
    fn mail_verified(&mut self, account: Account) -> anyhow::Result<()> {
        let is_pending = self.mail.as_ref().is_some_and(|m| {
            m.pending
                .as_ref()
                .is_some_and(|(a, _)| a.email() == account.email())
        });
        if !is_pending {
            return Ok(());
        }
        let Some((_, secret)) = self.mail.as_mut().and_then(|m| m.pending.take()) else {
            return Ok(());
        };
        for (scheme, spec) in [("imap", account.imap()), ("smtp", account.smtp())] {
            let draft = Draft {
                origin: spec.vault_origin(scheme),
                username: account.username().to_string(),
                secret: secret.expose().to_string(),
                note: "mail".into(),
                updated_at: unix_now(),
            };
            if let Err(e) = self.vault.add(&draft) {
                crate::report!("mail vault: {e}");
            }
        }
        let save = self.mail.as_mut().map(|mail| {
            mail.accounts.push(account.clone());
            (mail.accounts.clone(), mail.accounts_path())
        });
        if let Some((accounts, path)) = save {
            if let Err(e) = mail::save_accounts(&path, &accounts) {
                crate::report!("mail accounts: {e}");
            }
        }
        self.mail_push(&mail::Push::AccountReply {
            error: String::new(),
        })?;
        let push = self.mail.as_ref().map(|m| m.state_push());
        if let Some(push) = push {
            self.mail_push(&push)?;
        }
        if let Some((account, secret)) = self.mail_account(account.email()) {
            self.mail_worker(mail::Request::Sync {
                account,
                secret,
                selected: None,
            });
        }
        Ok(())
    }

    /// The onboarding form: validate the fields into an `Account`,
    /// stage the password, and have the worker prove both logins
    /// before anything is persisted.
    fn mail_add_account(&mut self, command: mail::PageCommand) -> anyhow::Result<()> {
        let mail::PageCommand::AddAccount {
            display,
            email,
            mut password,
            username,
            imap_host,
            imap_port,
            imap_tls,
            smtp_host,
            smtp_port,
            smtp_tls,
        } = command
        else {
            return Ok(());
        };
        // take() moves the heap buffer into the Secret — no plaintext
        // copy stays behind once the wire body is wiped.
        let secret = Secret::new(std::mem::take(&mut password));
        password.zeroize();
        let (display, email, username) = (display.as_str(), email.as_str(), username.as_str());
        let (imap_host, smtp_host) = (imap_host.as_str(), smtp_host.as_str());
        let tls = |s: &str| -> Result<Tls, MailError> {
            match s {
                "tls" => Ok(Tls::Wrapper),
                "starttls" => Ok(Tls::StartTls),
                other => Err(MailError::Invalid(format!("unknown security {other}"))),
            }
        };
        let built = (|| -> Result<Account, MailError> {
            let imap = ServerSpec::new(imap_host, imap_port, tls(&imap_tls)?)?;
            let smtp = ServerSpec::new(smtp_host, smtp_port, tls(&smtp_tls)?)?;
            Account::new(display, email, username, imap, smtp)
        })();
        let account = match built {
            Ok(account) => account,
            Err(e) => {
                return self.mail_push(&mail::Push::AccountReply {
                    error: e.to_string(),
                })
            }
        };
        if let Some(mail) = self.mail_mut() {
            mail.pending = Some((account.clone(), secret.clone()));
        }
        self.mail_worker(mail::Request::Verify { account, secret });
        Ok(())
    }

    fn mail_remove_account(&mut self, email: &str) -> anyhow::Result<()> {
        let Some((account, path, remaining)) = self.mail.as_mut().map(|mail| {
            let account = mail.account(email);
            mail.accounts.retain(|a| a.email() != email);
            mail.folders.remove(email);
            mail.statuses.remove(email);
            (account, mail.accounts_path(), mail.accounts.clone())
        }) else {
            return Ok(());
        };
        if let Err(e) = mail::save_accounts(&path, &remaining) {
            crate::report!("mail accounts: {e}");
        }
        if let Some(account) = account {
            for (scheme, spec) in [("imap", account.imap()), ("smtp", account.smtp())] {
                let origin = spec.vault_origin(scheme);
                if let Err(e) = self.vault.remove(&origin, account.username()) {
                    crate::report!("mail vault: {e}");
                }
            }
        }
        self.mail_worker(mail::Request::DropAccount {
            email: email.to_string(),
        });
        let push = self.mail.as_ref().map(|m| m.state_push());
        if let Some(push) = push {
            self.mail_push(&push)?;
        }
        Ok(())
    }

    /// The settings page's Save: the account's validated replacement,
    /// on disk, then a fresh state so the rail and compose repaint.
    fn mail_save_profile(
        &mut self,
        email: &str,
        display: &str,
        signature: &str,
        photo: &str,
    ) -> anyhow::Result<()> {
        let Some(mail) = self.mail.as_mut() else {
            return Ok(());
        };
        let Some(at) = mail.accounts.iter().position(|a| a.email() == email) else {
            return Ok(());
        };
        let saved = mail.accounts[at]
            .clone()
            .with_profile(display, signature, photo)
            .and_then(|account| {
                let mut accounts = mail.accounts.clone();
                accounts[at] = account;
                mail::save_accounts(&mail.accounts_path(), &accounts)?;
                mail.accounts = accounts;
                Ok(())
            });
        self.mail_push(&mail::Push::ProfileReply {
            error: saved.err().map(|e| e.to_string()).unwrap_or_default(),
        })?;
        let push = self.mail.as_ref().map(|m| m.state_push());
        if let Some(push) = push {
            self.mail_push(&push)?;
        }
        Ok(())
    }

    /// Queue one sync per account — the manual button and the tick.
    fn mail_sync_all(&mut self) {
        let Some((accounts, selected)) = self
            .mail
            .as_ref()
            .map(|m| (m.accounts.clone(), m.selected.clone()))
        else {
            return;
        };
        if let Some(mail) = self.mail.as_mut() {
            mail.last_sync = Instant::now();
            for account in &accounts {
                mail.statuses
                    .insert(account.email().to_string(), "syncing".into());
            }
        }
        for account in accounts {
            self.mail_push(&mail::Push::Status {
                account: account.email().to_string(),
                status: "syncing".into(),
            })
            .ok();
            let folder = selected
                .as_ref()
                .filter(|(a, _)| a == account.email())
                .map(|(_, f)| f.clone());
            match self.mail_secret(&account) {
                Some(secret) => self.mail_worker(mail::Request::Sync {
                    account,
                    secret,
                    selected: folder,
                }),
                None => {
                    self.mail_push(&mail::Push::Status {
                        account: account.email().to_string(),
                        status: "no stored password".into(),
                    })
                    .ok();
                }
            }
        }
    }

    /// Sync just the account a folder click selected — fresh rows are
    /// the point of clicking.
    fn mail_sync_selected(&mut self) {
        let Some((email, folder)) = self.mail.as_ref().and_then(|m| m.selected.clone()) else {
            return;
        };
        let Some(account) = self.mail.as_ref().and_then(|m| m.account(&email)) else {
            return;
        };
        if let Some(mail) = self.mail_mut() {
            mail.statuses.insert(email.clone(), "syncing".into());
        }
        self.mail_push(&mail::Push::Status {
            account: email.clone(),
            status: "syncing".into(),
        })
        .ok();
        if let Some(secret) = self.mail_secret(&account) {
            self.mail_worker(mail::Request::Sync {
                account,
                secret,
                selected: Some(folder),
            });
        }
    }

    /// The tick's share: a sync when the interval has passed and
    /// accounts exist. Called from `resource_tick`.
    pub(super) fn mail_tick(&mut self) {
        let due = self
            .mail
            .as_ref()
            .is_some_and(|m| !m.accounts.is_empty() && m.last_sync.elapsed() >= SYNC_EVERY);
        if due {
            self.mail_sync_all();
        }
    }

    /// The imap-side secret for an account — `None` if onboarding never
    /// sealed one (a hand-edited accounts.json, or a vault reset).
    fn mail_secret(&self, account: &Account) -> Option<Secret> {
        let origin = account.imap().vault_origin("imap");
        self.vault
            .credentials()
            .find(&origin, account.username())
            .map(|c| c.secret().clone())
    }

    fn mail_account(&self, email: &str) -> Option<(Account, Secret)> {
        let mail = self.mail.as_ref()?;
        let account = mail.account(email)?;
        Some((account.clone(), self.mail_secret(&account)?))
    }

    fn mail_worker(&self, request: mail::Request) {
        if let Some(mail) = &self.mail {
            mail.worker.post(request);
        }
    }

    fn mail_mut(&mut self) -> Option<&mut Mail> {
        self.mail.as_mut()
    }

    /// The picked files leave with the message — a second send starts
    /// from none.
    fn mail_take_files(&mut self) -> Vec<PathBuf> {
        self.mail_mut()
            .map(|m| std::mem::take(&mut m.files))
            .unwrap_or_default()
    }

    /// The compose drawer's file row — names and sizes, never paths.
    fn mail_push_files(&self) -> anyhow::Result<()> {
        let files = self
            .mail
            .as_ref()
            .map(|m| {
                m.files
                    .iter()
                    .map(|p| mail::PushFile {
                        name: p
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        size: std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.mail_push(&mail::Push::Attachments { files })
    }

    /// A real arrival: OS sound, taskbar flash and a toast, plus the
    /// in-app notice — every layer degrades on its own, so a missing
    /// toast registration never silences the rest.
    fn mail_notify(
        &mut self,
        email: &str,
        count: usize,
        from: &str,
        subject: &str,
    ) -> anyhow::Result<()> {
        let title = format!(
            "{count} new message{} — {email}",
            if count == 1 { "" } else { "s" }
        );
        let line = if from.is_empty() {
            subject.to_string()
        } else {
            format!("{from} — {subject}")
        };
        notify_sound();
        notify_flash(&self.window);
        notify_toast(&title, &line);
        self.notice(true, &title, vec![line])
    }

    /// One eval into the mail tab — best effort: a closed, frozen or
    /// navigated-away tab just drops it, the next `hello` repaints.
    fn mail_push(&self, push: &mail::Push) -> anyhow::Result<()> {
        // What the home page shows is a digest of these two, so they
        // are the moments it is told.
        if matches!(push, mail::Push::State { .. } | mail::Push::Status { .. }) {
            self.mail_glance_push_all();
        }
        let Some(mail) = &self.mail else {
            return Ok(());
        };
        let Some(tab) = mail.tab else {
            return Ok(());
        };
        if !self.is_mail_tab(tab) {
            return Ok(());
        }
        let Some(view) = self.view_of(tab) else {
            return Ok(());
        };
        let json = serde_json::to_string(push)?;
        view.eval(&format!("window.__cryo_mail && window.__cryo_mail({json})"))?;
        Ok(())
    }

    /// A tab showing the home page.
    pub(super) fn is_home_tab(&self, id: TabId) -> bool {
        self.tab_shows(id, NEW_TAB_URL)
    }

    /// The home page → app question. `true` when the body was the
    /// glance request and has been answered; `false` hands the message
    /// back to the caller's own parser untouched.
    pub(super) fn on_home_message(&mut self, id: TabId, message: &PageMessage) -> bool {
        if !pages::is_internal_url(&message.source) {
            return false;
        }
        let Ok(mail::GlanceCommand::MailGlance {}) =
            serde_json::from_str::<mail::GlanceCommand>(&message.body)
        else {
            return false;
        };
        if let Err(e) = self.mail_glance_push(id) {
            crate::report!("mail glance: {e:?}");
        }
        true
    }

    /// One eval into a home tab: `window.__cryo_mail_glance(json)`.
    fn mail_glance_push(&self, id: TabId) -> anyhow::Result<()> {
        if !self.is_home_tab(id) {
            return Ok(());
        }
        let Some(view) = self.view_of(id) else {
            return Ok(());
        };
        let glance = match &self.mail {
            Some(mail) => mail.glance(),
            None => mail::Glance {
                incognito: true,
                unseen: 0,
                accounts: Vec::new(),
            },
        };
        let json = serde_json::to_string(&glance)?;
        view.eval(&format!(
            "window.__cryo_mail_glance && window.__cryo_mail_glance({json})"
        ))?;
        Ok(())
    }

    /// Every home tab hears the change — the one in front and the ones
    /// behind it, so none shows a stale count when it is switched to.
    fn mail_glance_push_all(&self) {
        let homes: Vec<TabId> = self
            .registry
            .iter()
            .filter(|t| self.tab_shows(t.id(), NEW_TAB_URL))
            .map(|t| t.id())
            .collect();
        for id in homes {
            if let Err(e) = self.mail_glance_push(id) {
                crate::report!("mail glance: {e:?}");
            }
        }
    }
}

/// "MailBeep" is Windows' registered new-mail event — it plays under
/// the user's own sound scheme, silent machines stay silent.
#[cfg(target_os = "windows")]
fn notify_sound() {
    use windows::core::w;
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC};
    unsafe {
        let _ = PlaySoundW(w!("MailBeep"), None, SND_ALIAS | SND_ASYNC);
    }
}

#[cfg(not(target_os = "windows"))]
fn notify_sound() {}

/// Orange taskbar button until the window gets focus — the reliable
/// signal even when notifications are off or the window is minimized.
#[cfg(target_os = "windows")]
fn notify_flash(window: &tao::window::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        FlashWindowEx, FLASHWINFO, FLASHW_ALL, FLASHW_TIMERNOFG,
    };
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return;
    };
    let info = FLASHWINFO {
        cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
        hwnd: HWND(win32.hwnd.get() as _),
        dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
        uCount: 5,
        dwTimeout: 0,
    };
    unsafe {
        let _ = FlashWindowEx(&info);
    }
}

#[cfg(not(target_os = "windows"))]
fn notify_flash(_window: &tao::window::Window) {}

/// A Windows toast — needs an AUMID for an unpackaged app, which is
/// set lazily once. Failure is silent; sound/flash/notice still ran.
#[cfg(target_os = "windows")]
fn notify_toast(title: &str, line: &str) {
    use windows::core::{w, HSTRING};
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    static AUMID: std::sync::Once = std::sync::Once::new();
    AUMID.call_once(|| unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(w!("Cognition.Cryo"));
    });
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        esc(title),
        esc(line)
    );
    let attempt = || -> windows::core::Result<()> {
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(xml.as_str()))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from("Cognition.Cryo"))?
            .Show(&toast)
    };
    let _ = attempt();
}

#[cfg(not(target_os = "windows"))]
fn notify_toast(_title: &str, _line: &str) {}
