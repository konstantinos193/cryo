//! Saved-password form bridge and chrome prompt coordination.
//!
//! The page adapter reports focused login fields and submitted values;
//! this module validates the engine-observed origin before consulting the
//! vault. Websites never choose which origin is queried.

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::engine::PageMessage;
use crate::shell::chrome::PasswordAccount;
use crate::tabs::{TabId, ViewKey};
use crate::vault::{Draft, Saved, SiteOrigin};

use super::{unix_now, App};

pub(super) const PAGE_SCRIPT: &str = include_str!("autofill.js");
const MAX_MESSAGE_BYTES: usize = 16 * 1024;

pub(super) enum PasswordPrompt {
    Fill {
        view: ViewKey,
        origin: SiteOrigin,
        username_field: Option<String>,
        password_field: String,
    },
    Save {
        view: ViewKey,
        draft: Draft,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PageCommand {
    Focus {
        username_field: Option<String>,
        password_field: String,
    },
    Submit {
        username: String,
        password: String,
    },
}

#[derive(Serialize)]
struct FillReply<'a> {
    username_field: Option<&'a str>,
    password_field: &'a str,
    username: &'a str,
    password: &'a str,
}

#[derive(Serialize)]
struct Prompt<'a> {
    mode: &'a str,
    site: &'a str,
    accounts: Vec<PasswordAccount>,
    username: &'a str,
}

impl App {
    pub(super) fn on_page_message(
        &mut self,
        id: TabId,
        mut message: PageMessage,
    ) -> anyhow::Result<()> {
        if message.body.len() > MAX_MESSAGE_BYTES {
            message.body.zeroize();
            return Ok(());
        }
        let Some(origin) = self.message_origin(id, &message.source) else {
            message.body.zeroize();
            return Ok(());
        };
        let command = serde_json::from_str::<PageCommand>(&message.body);
        message.body.zeroize();
        let Ok(command) = command else {
            return Ok(());
        };
        match command {
            PageCommand::Focus {
                username_field,
                password_field,
            } => self.offer_passwords(id, origin, username_field, password_field),
            PageCommand::Submit { username, password } => {
                self.offer_password_save(id, origin, username, password)
            }
        }
    }

    pub(super) fn fill_password(&mut self, username: &str) -> anyhow::Result<()> {
        let Some(PasswordPrompt::Fill {
            view,
            origin,
            username_field,
            password_field,
        }) = self.password_prompt()
        else {
            return Ok(());
        };
        let tab = view.tab();
        if self.registry.active_id() != Some(tab)
            || self.registry.live(*view).is_none()
            || self.message_origin(tab, origin.as_str()).is_none()
        {
            return self.close_password_prompt();
        }
        let Some(credential) = self
            .vault
            .credentials()
            .for_page(origin)
            .into_iter()
            .find(|credential| credential.username() == username)
        else {
            return self.close_password_prompt();
        };
        let reply = FillReply {
            username_field: username_field.as_deref(),
            password_field,
            username: credential.username(),
            password: credential.secret().expose(),
        };
        let mut json = serde_json::to_string(&reply)?;
        let mut script =
            format!("window.__cryo_fill_password && window.__cryo_fill_password({json})");
        let result = self.view_of(tab).map_or(Ok(()), |view| view.eval(&script));
        script.zeroize();
        json.zeroize();
        result?;
        self.close_password_prompt()
    }

    pub(super) fn save_password(&mut self) -> anyhow::Result<()> {
        let prompt = self.take_password_prompt();
        self.chrome.eval("window.__cryo_password_prompt(null)")?;
        self.set_overlay_open(false)?;
        let Some(PasswordPrompt::Save { view, draft }) = prompt else {
            return Ok(());
        };
        if self.registry.live(view).is_none()
            || self.message_origin(view.tab(), &draft.origin).is_none()
        {
            return Ok(());
        }
        match self.vault.add(&draft) {
            Ok(Saved::Created) => self.notice(true, "Password saved", vec![draft.origin.clone()]),
            Ok(Saved::Updated) => self.notice(true, "Password updated", vec![draft.origin.clone()]),
            Ok(Saved::Unchanged) => Ok(()),
            Err(error) => self.notice(false, "Could not save password", vec![error.to_string()]),
        }
    }

    pub(super) fn close_password_prompt(&mut self) -> anyhow::Result<()> {
        self.take_password_prompt();
        self.chrome.eval("window.__cryo_password_prompt(null)")?;
        self.set_overlay_open(false)
    }
}

impl App {
    pub(super) fn password_prompt(&self) -> Option<&PasswordPrompt> {
        self.registry
            .active()
            .and_then(|t| t.resources().password_prompt.as_ref())
    }

    pub(super) fn take_password_prompt(&mut self) -> Option<PasswordPrompt> {
        let id = self.registry.active_id()?;
        self.registry
            .get_mut(id)?
            .resources_mut()
            .password_prompt
            .take()
    }

    fn set_password_prompt(&mut self, tab: TabId, prompt: PasswordPrompt) {
        if let Some(tab) = self.registry.get_mut(tab) {
            tab.resources_mut().password_prompt = Some(prompt);
        }
    }

    fn message_origin(&self, id: TabId, source: &str) -> Option<SiteOrigin> {
        let source = SiteOrigin::parse(source)?;
        let tab = self.registry.get(id)?;
        let current = SiteOrigin::parse(tab.url())?;
        let live_url = tab.view()?.url()?;
        let live_origin = SiteOrigin::parse(&live_url)?;
        (source == current && source == live_origin).then_some(source)
    }

    fn offer_passwords(
        &mut self,
        tab: TabId,
        origin: SiteOrigin,
        username_field: Option<String>,
        password_field: String,
    ) -> anyhow::Result<()> {
        if self.registry.active_id() != Some(tab) {
            return Ok(());
        }
        let accounts: Vec<PasswordAccount> = self
            .vault
            .credentials()
            .for_page(&origin)
            .into_iter()
            .map(|credential| PasswordAccount {
                username: credential.username().to_string(),
            })
            .collect();
        if accounts.is_empty() {
            return Ok(());
        }
        let prompt = Prompt {
            mode: "fill",
            site: origin.as_str(),
            accounts,
            username: "",
        };
        let json = serde_json::to_string(&prompt)?;
        let Some(view) = self.registry.get(tab).and_then(|t| t.view_key()) else {
            return Ok(());
        };
        self.set_password_prompt(
            tab,
            PasswordPrompt::Fill {
                view,
                origin,
                username_field,
                password_field,
            },
        );
        self.show_password_prompt(&json)
    }

    fn offer_password_save(
        &mut self,
        tab: TabId,
        origin: SiteOrigin,
        username: String,
        password: String,
    ) -> anyhow::Result<()> {
        if self.registry.active_id() != Some(tab) || password.is_empty() {
            return Ok(());
        }
        let username = username.trim().to_string();
        let existing = self
            .vault
            .credentials()
            .for_page(&origin)
            .into_iter()
            .find(|credential| credential.username() == username);
        if existing.is_some_and(|credential| credential.secret().expose() == password) {
            return Ok(());
        }
        let update = existing.is_some();
        let draft = Draft {
            origin: origin.as_str().to_string(),
            username,
            secret: password,
            note: String::new(),
            updated_at: unix_now(),
        };
        let prompt = Prompt {
            mode: if update { "update" } else { "save" },
            site: origin.as_str(),
            accounts: Vec::new(),
            username: &draft.username,
        };
        let json = serde_json::to_string(&prompt)?;
        let Some(view) = self.registry.get(tab).and_then(|t| t.view_key()) else {
            return Ok(());
        };
        self.set_password_prompt(tab, PasswordPrompt::Save { view, draft });
        self.show_password_prompt(&json)
    }

    fn show_password_prompt(&mut self, json: &str) -> anyhow::Result<()> {
        self.chrome
            .eval(&format!("window.__cryo_password_prompt({json})"))?;
        self.set_overlay_open(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_wire_rejects_unknown_fields() {
        let message =
            r#"{"kind":"focus","username_field":null,"password_field":"p","secret":"no"}"#;
        assert!(serde_json::from_str::<PageCommand>(message).is_err());
    }

    #[test]
    fn page_wire_accepts_focus_and_submit_messages() {
        let focus = r#"{"kind":"focus","username_field":"u","password_field":"p"}"#;
        let submit = r#"{"kind":"submit","username":"ann","password":"secret"}"#;
        assert!(matches!(
            serde_json::from_str::<PageCommand>(focus),
            Ok(PageCommand::Focus { .. })
        ));
        assert!(matches!(
            serde_json::from_str::<PageCommand>(submit),
            Ok(PageCommand::Submit { .. })
        ));
    }

    #[test]
    fn page_script_is_main_frame_guarded_and_does_not_fill_on_load() {
        assert!(PAGE_SCRIPT.contains("window.top !== window"));
        assert!(PAGE_SCRIPT.contains("focusin"));
        assert!(!PAGE_SCRIPT.contains("DOMContentLoaded"));
    }
}
