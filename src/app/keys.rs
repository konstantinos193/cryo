//! Keyboard input: tao key events and engine accelerators normalize into
//! one `Accelerator` vocabulary, one `KeyCommand` mapping, one dispatch.

use tao::event::{ElementState, KeyEvent};
use tao::event_loop::{ControlFlow, EventLoopProxy};
use tao::keyboard::{Key, ModifiersState};

use crate::engine::{Accelerator, AcceleratorHandler, AcceleratorKey};

use super::{post, App, AppEvent, DEFAULT_URL};

/// What a keypress should do. One mapping serves both input paths —
/// `AcceleratorKeyPressed` while a view has focus, and tao's
/// `KeyboardInput` when it doesn't.
#[derive(Clone, Copy)]
enum KeyCommand {
    NewTab,
    NewWindow,
    NewIncognitoWindow,
    ReopenClosed,
    CloseActive,
    CloseWindow,
    NextTab,
    PrevTab,
    /// 0-based index; `usize::MAX` selects the last tab (Ctrl+9).
    SelectTab(usize),
    /// Shift the active tab `delta` positions in the strip.
    MoveTab(isize),
    /// Absolute target index; `usize::MAX` lands it last.
    MoveTabTo(usize),
    DuplicateTab,
    MuteTab,
    Quit,
    GoHome,
    /// F6 toggles between the address bar and the page.
    CycleFocus,
    FocusPage,
    FocusAddressBar,
    Reload,
    HardReload,
    Back,
    Forward,
    Stop,
    ToggleFullscreen,
    DevTools,
    Print,
    SaveAs,
    /// Ctrl+J: toggle the download shelf. Must be stolen — otherwise the
    /// engine opens its own built-in downloads UI over the page.
    Downloads,
    OpenFile,
    ViewSource,
    /// +1 zoom step in, -1 out, 0 reset to 100%.
    Zoom(i32),
    Screenshot,
    ClearBrowsingData,
    /// Freeze the active tab: snapshot it, drop its view, hand focus to
    /// a neighbour. Clicking it later restores it.
    FreezeActive,
    /// Ctrl+D: keep the active page on the bookmarks bar, or drop it
    /// again if it is already kept.
    BookmarkPage,
    /// Ctrl+Shift+O: the library panel — kept pages, saved passwords,
    /// import and export.
    Library,
    /// Ctrl+H: the library panel's history pane. History is private —
    /// never a fetchable page — so it lives in the chrome overlay.
    History,
}

/// Map a key to the browser command it triggers, plus whether the key is
/// swallowed (`steal = true`) or also passed through to the page. `None`
/// means cryo doesn't own the key — WebView2 and the page keep it.
fn key_command(acc: Accelerator) -> Option<(KeyCommand, bool)> {
    use AcceleratorKey::*;
    let Accelerator {
        key,
        ctrl,
        alt,
        shift,
    } = acc;
    let (cmd, steal) = match (ctrl, alt, shift, key) {
        (true, false, false, Character('t')) => (KeyCommand::NewTab, true),
        (true, false, false, Character('n')) => (KeyCommand::NewWindow, true),
        (true, false, true, Character('n')) => (KeyCommand::NewIncognitoWindow, true),
        (true, false, true, Character('t')) => (KeyCommand::ReopenClosed, true),
        (true, false, false, Character('w')) | (true, false, false, F(4)) => {
            (KeyCommand::CloseActive, true)
        }
        (true, false, true, Character('w')) => (KeyCommand::CloseWindow, true),
        (true, false, false, Tab) | (true, false, false, PageDown) => (KeyCommand::NextTab, true),
        (true, false, true, Tab) | (true, false, false, PageUp) => (KeyCommand::PrevTab, true),
        (true, false, true, PageUp) => (KeyCommand::MoveTab(-1), true),
        (true, false, true, PageDown) => (KeyCommand::MoveTab(1), true),
        (true, false, true, Home) => (KeyCommand::MoveTabTo(0), true),
        (true, false, true, End) => (KeyCommand::MoveTabTo(usize::MAX), true),
        (true, false, true, Character('k')) => (KeyCommand::DuplicateTab, true),
        (true, false, false, Character('m')) => (KeyCommand::MuteTab, true),
        (true, false, true, Character('q')) => (KeyCommand::Quit, true),
        (false, true, false, Home) => (KeyCommand::GoHome, true),
        (false, false, false, F(6)) => (KeyCommand::CycleFocus, true),
        (true, false, false, F(6)) => (KeyCommand::FocusPage, true),
        (true, false, false, Character(d @ '1'..='8')) => {
            (KeyCommand::SelectTab((d as usize) - ('1' as usize)), true)
        }
        (true, false, false, Character('9')) => (KeyCommand::SelectTab(usize::MAX), true),
        (true, false, false, Character('l' | 'k' | 'e')) | (false, true, false, Character('d')) => {
            (KeyCommand::FocusAddressBar, true)
        }
        (false, false, false, F(5)) | (true, false, false, Character('r')) => {
            (KeyCommand::Reload, true)
        }
        (true, false, false, F(5))
        | (true, false, true, Character('r'))
        | (false, false, true, F(5)) => (KeyCommand::HardReload, true),
        (false, true, false, Left) => (KeyCommand::Back, true),
        (false, true, false, Right) => (KeyCommand::Forward, true),
        // Esc still reaches the page — it dismisses dialogs and menus —
        // while the view also stops loading.
        (false, false, false, Escape) => (KeyCommand::Stop, false),
        (false, false, false, F(11)) => (KeyCommand::ToggleFullscreen, true),
        (false, false, false, F(12)) | (true, false, true, Character('i' | 'j' | 'c')) => {
            (KeyCommand::DevTools, true)
        }
        (true, false, false, Character('p')) => (KeyCommand::Print, true),
        (true, false, false, Character('j')) => (KeyCommand::Downloads, true),
        (true, false, false, Character('s')) => (KeyCommand::SaveAs, true),
        (true, false, false, Character('o')) => (KeyCommand::OpenFile, true),
        (true, false, false, Character('u')) => (KeyCommand::ViewSource, true),
        // '=' is the unshifted '+' key; Shift makes no difference to zoom.
        (true, false, _, Character('=' | '+')) => (KeyCommand::Zoom(1), true),
        (true, false, _, Character('-')) => (KeyCommand::Zoom(-1), true),
        (true, false, false, Character('0')) => (KeyCommand::Zoom(0), true),
        (true, false, true, Character('s')) => (KeyCommand::Screenshot, true),
        (true, false, true, Character('f')) => (KeyCommand::FreezeActive, true),
        (true, false, false, Character('d')) => (KeyCommand::BookmarkPage, true),
        (true, false, true, Character('o')) => (KeyCommand::Library, true),
        (true, false, false, Character('h')) => (KeyCommand::History, true),
        (true, false, true, Delete) => (KeyCommand::ClearBrowsingData, true),
        _ => return None,
    };
    Some((cmd, steal))
}

/// tao delivers `KeyEvent`s in its own key type — normalize into our
/// `Accelerator` so both input paths share `key_command`.
fn tao_accelerator(event: &KeyEvent, modifiers: ModifiersState) -> Option<Accelerator> {
    let key = match &event.logical_key {
        Key::Character(c) => {
            let lower = c.to_lowercase();
            let mut chars = lower.chars();
            match (chars.next(), chars.next()) {
                (Some(c @ ('a'..='z' | '0'..='9' | '=' | '+' | '-')), None) => {
                    AcceleratorKey::Character(c)
                }
                _ => return None,
            }
        }
        Key::Tab => AcceleratorKey::Tab,
        Key::ArrowLeft => AcceleratorKey::Left,
        Key::ArrowRight => AcceleratorKey::Right,
        Key::PageUp => AcceleratorKey::PageUp,
        Key::PageDown => AcceleratorKey::PageDown,
        Key::Home => AcceleratorKey::Home,
        Key::End => AcceleratorKey::End,
        Key::Delete => AcceleratorKey::Delete,
        Key::Escape => AcceleratorKey::Escape,
        Key::F4 => AcceleratorKey::F(4),
        Key::F5 => AcceleratorKey::F(5),
        Key::F6 => AcceleratorKey::F(6),
        Key::F11 => AcceleratorKey::F(11),
        Key::F12 => AcceleratorKey::F(12),
        _ => return None,
    };
    Some(Accelerator {
        key,
        ctrl: modifiers.control_key(),
        alt: modifiers.alt_key(),
        shift: modifiers.shift_key(),
    })
}

impl App {
    /// Fallback key path: fires only while no view holds focus — the
    /// accelerator handler on each view covers the normal case.
    pub(super) fn on_key(&mut self, event: &KeyEvent, control_flow: &mut ControlFlow) {
        if event.state != ElementState::Pressed {
            return;
        }
        let Some(acc) = tao_accelerator(event, self.modifiers) else {
            return;
        };
        if let Err(e) = self.on_accelerator(acc, control_flow) {
            crate::report!("{e:?}");
        }
    }

    /// An accelerator a view forwarded — the keys the browser owns were
    /// already filtered out by `accelerator_forwarder`, so a miss here
    /// just means the view asked first and the mapping shrugged.
    pub(super) fn on_accelerator(
        &mut self,
        acc: Accelerator,
        control_flow: &mut ControlFlow,
    ) -> anyhow::Result<()> {
        if self.password_prompt().is_some()
            && acc.key == AcceleratorKey::Escape
            && !acc.ctrl
            && !acc.alt
            && !acc.shift
        {
            return self.close_password_prompt();
        }
        match key_command(acc) {
            Some((cmd, _)) => self.run_key_command(cmd, control_flow),
            None => Ok(()),
        }
    }

    fn run_key_command(
        &mut self,
        cmd: KeyCommand,
        control_flow: &mut ControlFlow,
    ) -> anyhow::Result<()> {
        match cmd {
            KeyCommand::NewTab => self.new_tab(),
            KeyCommand::NewWindow => Self::spawn_window(false, None),
            KeyCommand::NewIncognitoWindow => Self::spawn_window(true, None),
            KeyCommand::ReopenClosed => self.reopen_closed(),
            KeyCommand::CloseActive => match self.registry.active_id() {
                Some(id) => self.close_tab(id, control_flow),
                None => Ok(()),
            },
            KeyCommand::NextTab => self.cycle(1),
            KeyCommand::PrevTab => self.cycle(-1),
            KeyCommand::SelectTab(index) => match self.registry.id_at(index) {
                Some(id) => self.activate(id),
                None => Ok(()),
            },
            KeyCommand::MoveTab(delta) => self.move_active_tab(delta),
            KeyCommand::MoveTabTo(index) => self.move_active_tab_to(index),
            KeyCommand::DuplicateTab => self.duplicate_active(),
            KeyCommand::MuteTab => self.mute_active(),
            KeyCommand::Quit | KeyCommand::CloseWindow => {
                self.request_exit(control_flow);
                Ok(())
            }
            KeyCommand::GoHome => self.navigate_active(DEFAULT_URL),
            KeyCommand::CycleFocus => self.cycle_focus(),
            KeyCommand::FocusPage => self.focus_page(),
            KeyCommand::FocusAddressBar => self.focus_address_bar(),
            KeyCommand::Reload => self.reload_active(),
            KeyCommand::HardReload => self.hard_reload_active(),
            KeyCommand::Back => self.back_active(),
            KeyCommand::Forward => self.forward_active(),
            KeyCommand::Stop => self.stop_active(),
            KeyCommand::ToggleFullscreen => self.toggle_fullscreen(),
            KeyCommand::DevTools => self.devtools_active(),
            KeyCommand::Print => self.print_active(),
            KeyCommand::Downloads => self.toggle_downloads(),
            KeyCommand::SaveAs => self.save_active(),
            KeyCommand::OpenFile => self.open_file(),
            KeyCommand::ViewSource => self.view_source(),
            KeyCommand::Zoom(step) => self.zoom_active(step),
            KeyCommand::Screenshot => self.screenshot_active(),
            KeyCommand::ClearBrowsingData => self.clear_data(),
            KeyCommand::BookmarkPage => self.bookmark_active(),
            KeyCommand::Library => self.toggle_library(),
            KeyCommand::History => self.toggle_history(),
            KeyCommand::FreezeActive => match self.registry.active_id() {
                Some(id) => self.freeze_tab(id),
                None => Ok(()),
            },
        }
    }

    /// One accelerator callback per view. Keys the browser owns are
    /// reported to the event loop and swallowed (`steal`); everything else
    /// returns `false` so the page and the engine see it normally.
    pub(super) fn accelerator_forwarder(
        proxy: &EventLoopProxy<AppEvent>,
        view: Option<crate::tabs::ViewKey>,
    ) -> AcceleratorHandler {
        let proxy = proxy.clone();
        Box::new(move |acc| match key_command(acc) {
            Some((_, steal)) => {
                let event = view.map_or(AppEvent::Accelerator(acc), |key| {
                    AppEvent::View(key, super::ViewEvent::Accelerator(acc))
                });
                post(&proxy, event);
                steal
            }
            None => false,
        })
    }
}
