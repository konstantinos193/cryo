//! The worker: every IMAP exchange, disk read and SMTP send lives on
//! this one thread (AGENTS §9 — blocking work off the event loop).
//!
//! RAM is the product, so the thread holds almost nothing standing:
//! sessions connect lazily per account, are reused while requests keep
//! coming, and the whole map is dropped after [`IDLE`] quiet — an idle
//! browser pays no socket, no TLS state and no mailbox selection for
//! its mail. One thread means one connection at a time even with
//! several accounts; eM Client's parallel sync is a RAM pattern this
//! project exists to avoid.
//!
//! Results reach the event loop through the `out` callback — the app
//! turns them into `window.__cryo_mail` pushes.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

use super::account::{Account, ServerSpec};
use super::client::{Client, Flag};
use super::store::{FileStore, MAX_KEPT};
use super::{Envelope, Folder, MailError};
use crate::vault::Secret;

/// Sessions older than this are dropped — the quiet-time price of a
/// mail client is zero.
const IDLE: Duration = Duration::from_secs(90);

/// Work the app queues. `account` carries coordinates and login name;
/// `secret` is fetched from the vault per request by the app — the
/// worker never stores one between requests.
pub enum Request {
    /// Onboarding: prove both logins work.
    Verify { account: Account, secret: Secret },
    /// Folder list + counts; envelopes only for INBOX and `selected`
    /// — the unread badges come free with `select`, the listing is
    /// what actually costs.
    Sync {
        account: Account,
        secret: Secret,
        selected: Option<String>,
    },
    /// Show a folder — the index on disk answers, no network.
    List { account: String, folder: String },
    /// Open a message — the raw cache answers, else a `BODY.PEEK[]`
    /// fetch that then fills it. `remote` lets this hydration keep
    /// remote images.
    Open {
        account: Account,
        secret: Secret,
        folder: String,
        uid: u32,
        remote: bool,
    },
    SetFlag {
        account: Account,
        secret: Secret,
        folder: String,
        uid: u32,
        flag: Flag,
        on: bool,
    },
    MoveTo {
        account: Account,
        secret: Secret,
        folder: String,
        uid: u32,
        target: String,
    },
    Delete {
        account: Account,
        secret: Secret,
        folder: String,
        uid: u32,
    },
    /// SMTP send, then the IMAP `APPEND` into Sent.
    Send {
        account: Account,
        secret: Secret,
        draft: super::send::Draft,
    },
    /// Compose's "Save draft" — an `APPEND` into Drafts, no SMTP.
    SaveDraft {
        account: Account,
        secret: Secret,
        draft: super::send::Draft,
    },
    /// Onboarding's autodetect — static table, the domain's
    /// `autoconfig.` host, then Mozilla's database. No session needed.
    Guess { email: String },
    /// Decode one attachment of a cached message — returns the bytes
    /// for the app's save dialog.
    Attachment {
        account: Account,
        secret: Secret,
        folder: String,
        uid: u32,
        index: usize,
    },
    /// Forget everything — the window is going away or the account
    /// was removed.
    DropAccount { email: String },
}

/// What the worker reports — everything the app turns into pushes.
pub enum Outcome {
    Verified {
        account: Account,
    },
    Folders {
        email: String,
        folders: Vec<Folder>,
    },
    Listing {
        email: String,
        folder: String,
        envelopes: Vec<Envelope>,
    },
    /// One message opened — boxed, since a body dwarfs every other
    /// report and the channel carries them all in one enum.
    Body(Box<Opened>),
    FlagDone {
        email: String,
        folder: String,
        uid: u32,
        flag: &'static str,
        on: bool,
    },
    /// Autodetect's answer — `None` means even the pattern guess found
    /// nothing usable.
    Guess {
        email: String,
        specs: Option<(ServerSpec, ServerSpec)>,
    },
    /// Unseen Inbox uids that weren't in the previous index — a sync
    /// found arrivals worth a notification.
    NewMail {
        email: String,
        count: usize,
        from: String,
        subject: String,
    },
    /// Move/delete finished — the app re-asks for the listing.
    Done {
        op: &'static str,
    },
    Sent,
    Attachment {
        name: String,
        mime: String,
        bytes: Vec<u8>,
    },
    Failed {
        op: &'static str,
        email: String,
        error: String,
    },
}

/// An opened message: its hydrated body plus the envelope the listing
/// already had for it, so the page needs no second lookup.
pub struct Opened {
    pub email: String,
    pub folder: String,
    pub uid: u32,
    pub body: super::message::Body,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub date: i64,
    pub message_id: String,
    /// The chain a reply's `References` should carry.
    pub references: String,
    pub seen: bool,
    pub flagged: bool,
}

/// The handle the app keeps: a `Sender` plus the join handle.
pub struct Worker {
    tx: Sender<Request>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    /// Queue one request. A dead worker swallows it — the next push
    /// will just show the state without its answer, and teardown races
    /// are the only way it happens.
    pub fn post(&self, request: Request) {
        let _ = self.tx.send(request);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Close our sender: the loop exits once the queue drains, and
        // the join makes that synchronous rather than leaving a thread
        // mid-IMAP at exit.
        drop(std::mem::replace(&mut self.tx, channel().0));
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Start the worker — `out` is the app's repost closure (it wraps the
/// event-loop proxy). Runs until every `Worker` handle is dropped.
pub fn spawn(store: FileStore, out: impl Fn(Outcome) + Send + 'static) -> Worker {
    let (tx, rx) = channel();
    let join = std::thread::Builder::new()
        .name("cryo-mail".into())
        .spawn(move || run(store, rx, &out))
        .expect("the mail worker thread spawns");
    Worker {
        tx,
        join: Some(join),
    }
}

/// Sessions keyed by account email — one open at a time, all dropped
/// on an idle timeout.
type Sessions = HashMap<String, Box<dyn Client>>;

fn session<'a>(
    sessions: &'a mut Sessions,
    account: &Account,
    secret: &Secret,
) -> Result<&'a mut dyn Client, MailError> {
    if !sessions.contains_key(account.email()) {
        let client = super::client::connect(account, secret)?;
        sessions.insert(account.email().to_string(), client);
    }
    Ok(sessions
        .get_mut(account.email())
        .expect("inserted above")
        .as_mut())
}

fn run(store: FileStore, rx: Receiver<Request>, out: &dyn Fn(Outcome)) {
    let mut sessions: Sessions = HashMap::new();
    loop {
        match rx.recv_timeout(IDLE) {
            Ok(request) => handle(&store, &mut sessions, request, out),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => sessions.clear(),
            // Every sender is gone — the window is closing.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Report a failure without losing which account it belonged to.
fn fail(out: &dyn Fn(Outcome), op: &'static str, email: &str, e: MailError) {
    out(Outcome::Failed {
        op,
        email: email.to_string(),
        error: e.to_string(),
    });
}

fn handle(store: &FileStore, sessions: &mut Sessions, request: Request, out: &dyn Fn(Outcome)) {
    match request {
        Request::Verify { account, secret } => {
            let smtp_ok = match smtp_alive(&account, &secret) {
                Ok(()) => true,
                Err(e) => {
                    fail(out, "smtp", account.email(), e);
                    false
                }
            };
            if !smtp_ok {
                return;
            }
            match super::client::connect(&account, &secret).and_then(|mut c| c.verify()) {
                Ok(()) => out(Outcome::Verified { account }),
                Err(e) => fail(out, "login", account.email(), e),
            }
        }
        Request::Sync {
            account,
            secret,
            selected,
        } => {
            let email = account.email().to_string();
            match sync(store, sessions, &account, &secret, selected.as_deref(), out) {
                Ok(()) => out(Outcome::Done { op: "sync" }),
                Err(e) => {
                    // A dead session is rebuilt next request — don't
                    // report the same error twice.
                    sessions.remove(&email);
                    fail(out, "sync", &email, e);
                }
            }
        }
        Request::Guess { email } => out(Outcome::Guess {
            specs: super::autoconfig::autoconfig(&email),
            email,
        }),
        Request::List { account, folder } => {
            if let Some(index) = store.peek_index(&account, &folder) {
                out(Outcome::Listing {
                    email: account,
                    folder,
                    envelopes: index.envelopes,
                });
            }
        }
        Request::Open {
            account,
            secret,
            folder,
            uid,
            remote,
        } => {
            let email = account.email().to_string();
            let raw = match store.load_raw(&email, &folder, uid) {
                Some(raw) => Ok(raw),
                None => session(sessions, &account, &secret)
                    .and_then(|c| select_for(c, &folder).and_then(|_| c.raw(uid)))
                    .and_then(|raw| store.save_raw(&email, &folder, uid, &raw).map(|_| raw)),
            };
            match raw {
                Ok(raw) => {
                    let (env, body) = envelope_and_body(store, &email, &folder, uid, &raw, remote);
                    let references = thread_chain(&env);
                    out(Outcome::Body(Box::new(Opened {
                        email,
                        folder,
                        uid,
                        body,
                        from: env.from,
                        to: env.to,
                        cc: env.cc,
                        subject: env.subject,
                        date: env.date,
                        message_id: env.message_id,
                        references,
                        seen: env.seen,
                        flagged: env.flagged,
                    })));
                }
                Err(e) => {
                    sessions.remove(&email);
                    fail(out, "open", &email, e);
                }
            }
        }
        Request::SetFlag {
            account,
            secret,
            folder,
            uid,
            flag,
            on,
        } => {
            let email = account.email().to_string();
            let done = session(sessions, &account, &secret)
                .and_then(|c| select_for(c, &folder).and_then(|_| c.set_flag(uid, flag, on)));
            match done {
                Ok(()) => {
                    remember_flag(store, &email, &folder, uid, flag, on);
                    out(Outcome::FlagDone {
                        email,
                        folder,
                        uid,
                        flag: flag.atom(),
                        on,
                    });
                }
                Err(e) => {
                    sessions.remove(&email);
                    fail(out, "flag", &email, e);
                }
            }
        }
        Request::MoveTo {
            account,
            secret,
            folder,
            uid,
            target,
        } => {
            let email = account.email().to_string();
            let done = session(sessions, &account, &secret)
                .and_then(|c| select_for(c, &folder).and_then(|_| c.moveto(uid, &target)));
            match done {
                Ok(()) => {
                    remove_from_index(store, &email, &folder, uid);
                    out(Outcome::Done { op: "move" });
                }
                Err(e) => {
                    sessions.remove(&email);
                    fail(out, "move", &email, e);
                }
            }
        }
        Request::Delete {
            account,
            secret,
            folder,
            uid,
        } => {
            let email = account.email().to_string();
            let done = session(sessions, &account, &secret)
                .and_then(|c| select_for(c, &folder).and_then(|_| c.delete(uid)));
            match done {
                Ok(()) => {
                    remove_from_index(store, &email, &folder, uid);
                    out(Outcome::Done { op: "delete" });
                }
                Err(e) => {
                    sessions.remove(&email);
                    fail(out, "delete", &email, e);
                }
            }
        }
        Request::Send {
            account,
            secret,
            draft,
        } => {
            let email = account.email().to_string();
            match super::send::send(&account, &secret, &draft) {
                Ok(raw) => {
                    // The SMTP server accepted it — now the Sent copy.
                    // Its failure is reported but doesn't undo the send.
                    if let Err(e) = session(sessions, &account, &secret).and_then(|c| {
                        let folder = sent_folder(c).unwrap_or_else(|| "Sent".to_string());
                        c.append(&folder, &raw)
                    }) {
                        fail(out, "sent-copy", &email, e);
                    }
                    out(Outcome::Sent);
                }
                Err(e) => fail(out, "send", &email, e),
            }
        }
        Request::SaveDraft {
            account,
            secret,
            draft,
        } => {
            let email = account.email().to_string();
            let done = super::send::build(&account, &draft).and_then(|message| {
                session(sessions, &account, &secret).and_then(|c| {
                    let folder = drafts_folder(c).unwrap_or_else(|| "Drafts".to_string());
                    c.append(&folder, &message.formatted())
                })
            });
            match done {
                Ok(()) => out(Outcome::Done { op: "draft" }),
                Err(e) => fail(out, "draft", &email, e),
            }
        }
        Request::Attachment {
            account,
            secret,
            folder,
            uid,
            index,
        } => {
            let email = account.email().to_string();
            let raw = match store.load_raw(&email, &folder, uid) {
                Some(raw) => Ok(raw),
                None => session(sessions, &account, &secret)
                    .and_then(|c| select_for(c, &folder).and_then(|_| c.raw(uid))),
            };
            match raw.and_then(|raw| {
                super::message::attachment_bytes(&raw, index).ok_or_else(|| {
                    MailError::Invalid(format!("no attachment {index} on uid {uid}"))
                })
            }) {
                Ok((name, mime, bytes)) => out(Outcome::Attachment { name, mime, bytes }),
                Err(e) => {
                    sessions.remove(&email);
                    fail(out, "attachment", &email, e);
                }
            }
        }
        Request::DropAccount { email } => {
            sessions.remove(&email);
            if let Err(e) = store.drop_account(&email) {
                crate::report!("mail cache drop: {e}");
            }
        }
    }
}

/// Select `folder` on an existing session before uid operations —
/// every verb needs its mailbox current.
fn select_for(
    client: &mut dyn Client,
    folder: &str,
) -> Result<super::client::Selection, MailError> {
    client.select(folder)
}

/// The SMTP half of onboarding — connect, authenticate, quit.
fn smtp_alive(account: &Account, secret: &Secret) -> Result<(), MailError> {
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::SmtpTransport;
    let spec = account.smtp();
    let creds = Credentials::new(account.username().to_string(), secret.expose().to_string());
    let builder = match spec.tls() {
        super::Tls::Wrapper => SmtpTransport::relay(spec.host()),
        super::Tls::StartTls => SmtpTransport::starttls_relay(spec.host()),
    }
    .map_err(|e| MailError::smtp("relay", e))?;
    let mailer = builder.credentials(creds).port(spec.port()).build();
    mailer
        .test_connection()
        .map_err(|e| MailError::smtp("connect", e))
        .and_then(|alive| {
            alive
                .then_some(())
                .ok_or_else(|| MailError::smtp("connect", "connection refused"))
        })
}

/// The folder marked `\Sent`, for the post-send APPEND — falls back to
/// the conventional name when no special flag exists.
fn sent_folder(client: &mut dyn Client) -> Option<String> {
    client
        .folders()
        .ok()?
        .into_iter()
        .find(|f| f.role == super::Role::Sent)
        .map(|f| f.name)
}

/// Same for `\Drafts`.
fn drafts_folder(client: &mut dyn Client) -> Option<String> {
    client
        .folders()
        .ok()?
        .into_iter()
        .find(|f| f.role == super::Role::Drafts)
        .map(|f| f.name)
}

/// The envelope fields a `Body` outcome carries — read back from the
/// index so the page header shows what the list showed.
fn envelope_and_body(
    store: &FileStore,
    email: &str,
    folder: &str,
    uid: u32,
    raw: &[u8],
    remote: bool,
) -> (Envelope, super::message::Body) {
    let env = store
        .peek_index(email, folder)
        .and_then(|i| i.envelopes.into_iter().find(|e| e.uid == uid))
        .unwrap_or_else(|| Envelope::placeholder(uid));
    (env, super::message::hydrate(raw, remote))
}

/// The `References` a reply to this message should carry: the chain it
/// came with plus its own id — RFC 5322's rule, built once here.
fn thread_chain(env: &Envelope) -> String {
    let mut ids: Vec<&str> = env.references.split_whitespace().collect();
    let own = env.message_id.trim();
    if !own.is_empty() {
        let wrapped = own.starts_with('<');
        ids.retain(|id| id.trim_matches(['<', '>']) != own.trim_matches(['<', '>']));
        if wrapped {
            ids.push(own);
        }
    }
    let mut out = ids
        .iter()
        .map(|id| {
            let bare = id.trim_matches(['<', '>']);
            format!("<{bare}>")
        })
        .collect::<Vec<_>>();
    if !own.is_empty() && !own.starts_with('<') {
        out.push(format!("<{own}>"));
    }
    out.join(" ")
}

/// Keep the cached listing in step after a flag write — the next
/// `peek_index` then shows the same truth the server holds.
fn remember_flag(store: &FileStore, email: &str, folder: &str, uid: u32, flag: Flag, on: bool) {
    let Some(mut index) = store.peek_index(email, folder) else {
        return;
    };
    let Some(env) = index.envelopes.iter_mut().find(|e| e.uid == uid) else {
        return;
    };
    match flag {
        Flag::Seen => env.seen = on,
        Flag::Flagged => env.flagged = on,
        Flag::Answered => env.answered = on,
        Flag::Draft => env.draft = on,
        Flag::Deleted => env.deleted = on,
    }
    if let Err(e) = store.save_index(email, folder, index.uid_validity, index.envelopes) {
        crate::report!("mail index write: {e}");
    }
}

/// A move or delete removes the row — the cached listing must not
/// resurrect it.
fn remove_from_index(store: &FileStore, email: &str, folder: &str, uid: u32) {
    let Some(mut index) = store.peek_index(email, folder) else {
        return;
    };
    index.envelopes.retain(|e| e.uid != uid);
    if let Err(e) = store.save_index(email, folder, index.uid_validity, index.envelopes) {
        crate::report!("mail index write: {e}");
    }
}

/// One account's sync round: folders, counts, and the two listings
/// that cost (INBOX always, plus the folder the user is looking at).
fn sync(
    store: &FileStore,
    sessions: &mut Sessions,
    account: &Account,
    secret: &Secret,
    selected: Option<&str>,
    out: &dyn Fn(Outcome),
) -> Result<(), MailError> {
    let email = account.email();
    let client = session(sessions, account, secret)?;
    let mut folders = client.folders()?;
    for folder in &mut folders {
        let sel = client.select(&folder.name)?;
        folder.exists = Some(sel.exists);
        folder.unseen = sel.unseen;
        if !folder.is_inbox() && Some(folder.name.as_str()) != selected {
            continue;
        }
        // The index before this sync — arrivals are the uids it lacks.
        // A generation change voids the comparison, not just the cache:
        // uids from a different generation name different messages.
        let old = store.peek_index(email, &folder.name);
        let same_generation = old
            .as_ref()
            .is_some_and(|i| i.uid_validity == sel.uid_validity);
        if old.is_some() && !same_generation {
            store.save_index(email, &folder.name, sel.uid_validity, Vec::new())?;
        }
        let envelopes = client.envelopes(MAX_KEPT as u32)?;
        // Unseen messages that weren't in the index a moment ago are
        // arrivals worth a noise — Inbox only, and only when a prior
        // index exists (the first sync after onboarding is not news).
        if folder.is_inbox() && same_generation {
            if let Some(old) = old {
                let known: std::collections::HashSet<u32> =
                    old.envelopes.iter().map(|e| e.uid).collect();
                let arrivals: Vec<&Envelope> = envelopes
                    .iter()
                    .filter(|e| !e.seen && !known.contains(&e.uid))
                    .collect();
                if let Some(&latest) = arrivals.first() {
                    out(Outcome::NewMail {
                        email: email.to_string(),
                        count: arrivals.len(),
                        from: latest.from.clone(),
                        subject: latest.subject.clone(),
                    });
                }
            }
        }
        store.save_index(email, &folder.name, sel.uid_validity, envelopes.clone())?;
        // Report the listing the page might be looking at — the app
        // filters by what it actually shows.
        if Some(folder.name.as_str()) == selected {
            out(Outcome::Listing {
                email: email.to_string(),
                folder: folder.name.clone(),
                envelopes,
            });
        }
    }
    out(Outcome::Folders {
        email: email.to_string(),
        folders,
    });
    Ok(())
}
