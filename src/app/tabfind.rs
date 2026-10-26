//! The tab-search overlay's app side. The overlay owns its open state and
//! does its own filtering — every tab it lists is already in the state
//! `sync_chrome` pushes — so the app's whole job is to hand it the keyboard.

use super::App;

impl App {
    /// Ctrl+Shift+A: raise the overlay, or shut it if it is already up.
    /// Controller focus moves to the chrome view first, because DOM focus
    /// alone leaves keystrokes with whichever view last held them — the
    /// same reason `focus_address_bar` does it. The document decides which
    /// way the toggle goes and reports the result through `Overlay`.
    pub(super) fn toggle_tab_search(&mut self) -> anyhow::Result<()> {
        self.chrome.focus()?;
        self.chrome_focused = true;
        self.chrome
            .eval("window.__cryo_tabfind_toggle && window.__cryo_tabfind_toggle()")?;
        Ok(())
    }
}
