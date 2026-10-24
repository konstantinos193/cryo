//! Self-update wiring: background checks against the release feed, the
//! "restart to update" dialog, and launching the staged installer.

use tao::event_loop::ControlFlow;

use crate::update::{self, Outcome, Update};

use super::{post, App, AppEvent};

/// "Restart to update now?" — shown when a *manual* check finds a staged
/// installer. `true` means apply immediately.
fn update_dialog(u: &Update) -> bool {
    rfd::MessageDialog::new()
        .set_title("Update ready")
        .set_description(format!(
            "cryo v{} is downloaded and verified. Restart to update?",
            u.version
        ))
        .set_buttons(rfd::MessageButtons::YesNo)
        .set_level(rfd::MessageLevel::Info)
        .show()
        == rfd::MessageDialogResult::Yes
}

impl App {
    /// Ask the release feed for a newer build. `prompt` means the user
    /// asked (menu item) — the outcome gets a dialog; silent checks only
    /// surface the ⋮ badge once an installer is downloaded and verified.
    pub(super) fn request_update_check(&mut self, prompt: bool) {
        self.update_prompt = prompt;
        let proxy = self.proxy.clone();
        update::spawn_check(move |outcome| {
            post(&proxy, AppEvent::UpdateChecked(outcome));
        });
    }

    pub(super) fn on_update(
        &mut self,
        outcome: Outcome,
        control_flow: &mut ControlFlow,
    ) -> anyhow::Result<()> {
        let prompt = std::mem::take(&mut self.update_prompt);
        match outcome {
            Outcome::Ready(u) => {
                let apply = prompt && update_dialog(&u);
                self.pending_update = Some(u);
                self.sync_chrome()?;
                if apply {
                    self.apply_update(control_flow)?;
                }
            }
            Outcome::UpToDate if prompt => {
                rfd::MessageDialog::new()
                    .set_title("cryo")
                    .set_description(format!(
                        "cryo is up to date (v{})",
                        env!("CARGO_PKG_VERSION")
                    ))
                    .set_level(rfd::MessageLevel::Info)
                    .show();
            }
            Outcome::UpToDate => {}
            Outcome::Failed(e) if prompt => {
                rfd::MessageDialog::new()
                    .set_title("Update check failed")
                    .set_description(e.to_string())
                    .set_level(rfd::MessageLevel::Error)
                    .show();
            }
            Outcome::Failed(e) => return Err(e.into()),
        }
        Ok(())
    }

    /// Launch the staged installer and leave. Inno runs fully silent and
    /// relaunches cryo itself (the `[Run]` entry in `cryo.iss`).
    pub(super) fn apply_update(&mut self, control_flow: &mut ControlFlow) -> anyhow::Result<()> {
        let Some(u) = &self.pending_update else {
            return Ok(());
        };
        update::launch_installer(u)?;
        self.request_exit(control_flow);
        Ok(())
    }
}
