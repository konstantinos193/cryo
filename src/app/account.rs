//! The Google session check behind the account button. The jar read
//! stays on this thread — the COM call pumps the loop like the capture
//! path does — while the HTTP round trip goes to a worker and lands
//! back as `AppEvent::GoogleSession` (AGENTS §9).

use crate::account;
use crate::shell::chrome::ChromeAccount;

use super::{post, App, AppEvent};

// Re-exported so `app::account` reads as this slice's whole vocabulary —
// `mod.rs` types the session field `account::Session`, beside
// `account::SessionCheck`.
pub(super) use crate::account::Session;

/// The one in-flight session check, and whether a second request folded
/// into it. A pair of bools would leave `!check && again` representable
/// but meaningless — the enum leaves only the real states (§8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SessionCheck {
    Idle,
    InFlight,
    /// A check is running and a second ask arrived meanwhile — one
    /// follow-up runs when it lands, however many asks folded in.
    InFlightDirty,
}

impl App {
    /// Re-ask who the profile is signed in as. One worker at a time: a
    /// request that lands while a check runs folds into a follow-up
    /// instead of stacking threads.
    pub(super) fn refresh_google_session(&mut self) {
        // Incognito windows don't ask: forgetting is their product, and
        // the account button stays a signed-out circle rather than
        // naming an identity its own jar borrowed.
        if self.incognito {
            return;
        }
        match self.google_check {
            SessionCheck::InFlight | SessionCheck::InFlightDirty => {
                self.google_check = SessionCheck::InFlightDirty;
                return;
            }
            SessionCheck::Idle => {}
        }
        let cookies = match self.chrome.cookies(account::JAR_URL) {
            Ok(c) => c,
            Err(e) => {
                crate::report!("account check: {e:?}");
                return;
            }
        };
        self.google_check = SessionCheck::InFlight;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let session = account::fetch(&cookies);
            post(&proxy, AppEvent::GoogleSession(session));
        });
    }

    /// The worker's answer: repaint the button when the session changed,
    /// and run the follow-up a busy check queued. A sign-out shrinks the
    /// list under the account pick, so the index is re-clamped here.
    pub(super) fn on_google_session(&mut self, session: Option<Session>) -> anyhow::Result<()> {
        let dirty = self.google_check == SessionCheck::InFlightDirty;
        self.google_check = SessionCheck::Idle;
        let changed = session != self.google;
        self.google = session;
        let len = self.google.as_ref().map(|s| s.accounts.len()).unwrap_or(0);
        self.active_account = self.active_account.min(len.saturating_sub(1));
        if dirty {
            self.refresh_google_session();
        }
        if changed {
            self.sync_chrome()
        } else {
            Ok(())
        }
    }

    /// A dropdown row picked: the window browses Google properties as
    /// that account from now on — the button wears its face, navigations
    /// carry its `authuser` — and the row's own Google home opens so the
    /// pick has somewhere to land.
    pub(super) fn account_open(&mut self, index: u64) -> anyhow::Result<()> {
        self.active_account = usize::try_from(index).unwrap_or(usize::MAX);
        let len = self.google.as_ref().map(|s| s.accounts.len()).unwrap_or(0);
        self.active_account = self.active_account.min(len.saturating_sub(1));
        self.spawn_tab(
            &format!(
                "https://myaccount.google.com/?authuser={}",
                self.active_account
            ),
            true,
        )?;
        self.sync_chrome()
    }

    /// Wipe the profile's browsing data. `clear_browsing_data` answers
    /// after the wipe, so the re-read sees the jar it leaves behind;
    /// the button flips to signed-out right away for feedback — and the
    /// follow-up check confirms it against what actually survived.
    pub(super) fn clear_data(&mut self) -> anyhow::Result<()> {
        let cleared = self
            .chrome
            .clear_browsing_data()
            .map_err(anyhow::Error::from);
        self.google = None;
        self.refresh_google_session();
        cleared.and_then(|()| self.sync_chrome())
    }

    /// `ChromeState`'s copy of the session: every live account in jar
    /// order — the dropdown lists them all, the button wears the first.
    pub(super) fn chrome_accounts(&self) -> Vec<ChromeAccount> {
        self.google
            .as_ref()
            .map(|s| {
                s.accounts
                    .iter()
                    .map(|a| ChromeAccount {
                        name: a.name.clone(),
                        email: a.email.clone(),
                        avatar: a.avatar.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}
