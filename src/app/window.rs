//! Window appearance: the theme it wears, fullscreen layout, and
//! sibling-process window creation.
use super::App;
use crate::engine::Rect;
use crate::shell;
use crate::shell::theme::{self, Theme};
use crate::tabs::TabId;
use tao::window::Fullscreen;

impl App {
    /// The settings page picked a theme. The documents have already
    /// restyled themselves out of storage, so what is left is the part
    /// no page can reach: the window and taskbar icons, the colour the
    /// next view paints before its first frame, and remembering the
    /// pick for the next launch.
    ///
    /// Only a settings tab may ask, the same guard the download folder
    /// has: a web page that guessed the route must not be able to
    /// restyle the browser.
    pub(super) fn theme_setting(&mut self, id: TabId, picked: Theme) -> anyhow::Result<()> {
        if !self.tab_shows(id, crate::shell::pages::SETTINGS_URL) {
            return Ok(());
        }
        self.theme = picked;
        self.apply_theme_icons();
        // A window with no file to write (incognito) still restyles; it
        // just leaves nothing behind.
        if let Some(path) = &self.theme_state {
            if let Err(e) = theme::save(path, picked) {
                crate::report!("remember theme: {e}");
            }
        }
        Ok(())
    }

    /// Dress the window in the current theme's icon. A theme whose icon
    /// will not load is reported and the old one stays — an icon is not
    /// worth failing a theme change over.
    pub(super) fn apply_theme_icons(&self) {
        #[cfg(windows)]
        match crate::window_icons(self.theme) {
            Ok((title_bar, taskbar)) => {
                use tao::platform::windows::WindowExtWindows;
                self.window.set_window_icon(Some(title_bar));
                self.window.set_taskbar_icon(Some(taskbar));
            }
            Err(e) => crate::report!("theme icon: {e}"),
        }
    }

    /// DOM fullscreen (YouTube etc.): the page element fills the *view*,
    /// and the view only covers the area under the chrome strip — so we
    /// hide chrome and stretch the view over the whole window. Chrome
    /// restores on exit.
    pub(super) fn page_fullscreen(&mut self, id: TabId, on: bool) -> anyhow::Result<()> {
        if self.registry.active_id() != Some(id) {
            return Ok(());
        }
        if on {
            self.dismiss_overlays()?;
        }
        self.page_fullscreen = on;
        // Video fullscreen in a real browser is OS fullscreen — the window
        // goes borderless over the taskbar too, not just over the strip.
        self.window.set_fullscreen(if on {
            Some(Fullscreen::Borderless(None))
        } else {
            None
        });
        self.chrome.set_visible(!on)?;
        if let Some(view) = self.view_of(id) {
            let (w, h) = self.logical_size;
            let rect = if on {
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w,
                    h,
                }
            } else {
                shell::layout(w, h).1
            };
            view.set_bounds(rect)?;
        }
        Ok(())
    }

    /// One window per process: a "new window" key spawns a sibling cryo
    /// process. `--incognito` puts every view of that process on an
    /// InPrivate profile — incognito is a property of the window, not a
    /// tab, same as Chrome. A bare arg hands the child an address to open.
    pub(super) fn spawn_window(incognito: bool, url: Option<&str>) -> anyhow::Result<()> {
        let exe = std::env::current_exe()?;
        let mut cmd = std::process::Command::new(exe);
        // A spawned window keeps its address: the flag stops the child
        // from handing it to us over the single-instance pipe.
        cmd.arg("--new-window");
        if incognito {
            cmd.arg("--incognito");
        }
        if let Some(url) = url {
            cmd.arg(url);
        }
        cmd.spawn()?;
        Ok(())
    }
}
