//! The app: single owner of event-loop state. Chrome commands, window
//! events and view callbacks all funnel through `handle_event` — nowhere
//! else does state change hands.

use std::collections::HashMap;
use std::rc::Rc;

use tao::event::{ElementState, Event, KeyEvent, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopProxy};
use tao::keyboard::{Key, ModifiersState};
use tao::window::{Fullscreen, Window};

use crate::engine::{
    Accelerator, AcceleratorHandler, AcceleratorKey, ContentView, Engine, LoadFailure,
    PermissionDecision, PermissionKind, ViewSpec,
};
use crate::shell::chrome::{
    canonize_nav_input, normalize_nav_input, ChromeCommand, ChromeState, ChromeTab,
};
use crate::shell::pages::{self, CHROME_URL, NEW_TAB_URL};
use crate::shell::{self, error};
use crate::tabs::{TabId, TabRegistry, TabSnapshot, TabState};
use crate::update;

/// Domain URL recorded for a fresh tab. The view actually loads
/// `NEW_TAB_URL`; `about:blank` stays the tab's logical address so the
/// address bar starts empty and internal URLs never escape into it.
const DEFAULT_URL: &str = "about:blank";

/// The address to hand the engine for a tab's domain URL. `about:blank`
/// is a never-painting document in WebView2 — the internal new-tab page
/// stands in for it everywhere a view (re)loads a tab's address.
fn content_url_for(domain_url: &str) -> &str {
    if domain_url == DEFAULT_URL {
        NEW_TAB_URL
    } else {
        domain_url
    }
}

/// Events that reach the loop from view callbacks (`AppEvent`) — chrome
/// IPC, title changes, navigations, keys the engine yielded to us.
pub enum AppEvent {
    Chrome(ChromeCommand),
    TitleChanged(TabId, String),
    Navigated(TabId, String),
    Accelerator(Accelerator),
    /// Page asked for a new window (target=_blank, ctrl/shift/middle-click,
    /// window.open) — we open a tab instead.
    OpenInNewTab(String),
    /// A page element entered/left DOM fullscreen (video fullscreen).
    PageFullscreen(TabId, bool),
    /// A navigation ended with no document, or the renderer died under one.
    LoadFailed(TabId, LoadFailure),
    /// A background update check finished — started at launch and by the
    /// menu's "Check for updates".
    UpdateChecked(update::Outcome),
}

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
    OpenFile,
    ViewSource,
    /// +1 zoom step in, -1 out, 0 reset to 100%.
    Zoom(i32),
    Screenshot,
    ClearBrowsingData,
    /// Freeze the active tab: snapshot it, drop its view, hand focus to
    /// a neighbour. Clicking it later restores it.
    FreezeActive,
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
        (true, false, false, Character('s')) => (KeyCommand::SaveAs, true),
        (true, false, false, Character('o')) => (KeyCommand::OpenFile, true),
        (true, false, false, Character('u')) => (KeyCommand::ViewSource, true),
        // '=' is the unshifted '+' key; Shift makes no difference to zoom.
        (true, false, _, Character('=' | '+')) => (KeyCommand::Zoom(1), true),
        (true, false, _, Character('-')) => (KeyCommand::Zoom(-1), true),
        (true, false, false, Character('0')) => (KeyCommand::Zoom(0), true),
        (true, false, true, Character('s')) => (KeyCommand::Screenshot, true),
        (true, false, true, Character('f')) => (KeyCommand::FreezeActive, true),
        (true, false, true, Delete) => (KeyCommand::ClearBrowsingData, true),
        _ => return None,
    };
    Some((cmd, steal))
}

/// "Restart to update now?" — shown when a *manual* check finds a staged
/// installer. `true` means apply immediately.
fn update_dialog(u: &update::Update) -> bool {
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

pub struct App {
    window: Rc<Window>,
    engine: Rc<dyn Engine>,
    proxy: EventLoopProxy<AppEvent>,
    registry: TabRegistry,
    views: HashMap<TabId, Box<dyn ContentView>>,
    chrome: Box<dyn ContentView>,
    logical_size: (f64, f64),
    modifiers: ModifiersState,
    /// Where keystrokes go right now: chrome bar or the active page.
    /// Approximate — a page click isn't reported back — but F6's cycle
    /// only needs our own focus calls to be consistent.
    chrome_focused: bool,
    /// The active view is in DOM fullscreen — it covers the chrome strip.
    page_fullscreen: bool,
    /// A chrome overlay (⋮ menu, omnibox dropdown) is open — the chrome
    /// view is stretched over the whole window so the popup can paint
    /// above the content views.
    overlay_open: bool,
    /// Downloaded, checksum-verified installer waiting for a restart.
    /// Surfaced as the ⋮ badge and the "Restart to update" menu item.
    pending_update: Option<update::Update>,
    /// A manual check was requested — the next `UpdateChecked` outcome
    /// gets a dialog instead of staying silent.
    update_prompt: bool,
}

impl App {
    pub fn new(
        window: Rc<Window>,
        engine: Rc<dyn Engine>,
        proxy: EventLoopProxy<AppEvent>,
        open_url: Option<String>,
    ) -> anyhow::Result<Self> {
        let logical = window.inner_size().to_logical::<f64>(window.scale_factor());
        let logical_size = (logical.width, logical.height);
        let (chrome_rect, content_rect) = shell::layout(logical_size.0, logical_size.1);

        // Content spawns before chrome so the chrome bar sits above it in
        // the z-order of sibling container windows.
        let mut registry = TabRegistry::new();
        // A spawned window can be handed an address to open (Ctrl+Shift+
        // Enter, shell args). Blank or absent means the new-tab page.
        let (domain, load) = match open_url.as_deref().map(normalize_nav_input) {
            Some(u) if u != DEFAULT_URL => (u.clone(), u),
            _ => (DEFAULT_URL.to_string(), NEW_TAB_URL.to_string()),
        };
        let first = registry.create(domain);
        if let Some(tab) = registry.get_mut(first) {
            tab.set_title("New tab".to_string());
        }
        let first_view =
            Self::spawn_content_view(&*engine, &proxy, content_rect, first, &load, true)?;
        let mut views = HashMap::new();
        views.insert(first, first_view);
        registry.set_active(first)?;

        // The engine's command transport runs inside the protocol handler,
        // which is Sync-bound — the proxy goes behind a Mutex (uncontended;
        // WebView2 calls it on the UI thread anyway).
        let ipc_proxy = std::sync::Mutex::new(proxy.clone());
        let chrome = engine.spawn(ViewSpec {
            bounds: chrome_rect,
            visible: true,
            // The view grows over the whole window while its menu is open;
            // transparent lets the content view show through the parts of
            // the document that don't paint anything.
            transparent: true,
            background: Some(shell::CHROME_BACKGROUND),
            on_accelerator: Some(Self::accelerator_forwarder(&proxy)),
            on_ipc: Some(Box::new(move |body| {
                if let Ok(cmd) = serde_json::from_str::<ChromeCommand>(&body) {
                    let _ = ipc_proxy
                        .lock()
                        .expect("ipc proxy lock")
                        .send_event(AppEvent::Chrome(cmd));
                }
            })),
            ..ViewSpec::url(CHROME_URL)
        })?;

        let app = Self {
            window,
            engine,
            proxy,
            registry,
            views,
            chrome,
            logical_size,
            modifiers: ModifiersState::empty(),
            chrome_focused: false,
            page_fullscreen: false,
            overlay_open: false,
            pending_update: None,
            update_prompt: false,
        };
        Ok(app)
    }

    pub fn handle_event(&mut self, event: Event<'_, AppEvent>, control_flow: &mut ControlFlow) {
        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,
                WindowEvent::Resized(size) => self.resize(size.width, size.height),
                WindowEvent::ModifiersChanged(m) => self.modifiers = m,
                WindowEvent::KeyboardInput { event, .. } => self.on_key(&event, control_flow),
                _ => {}
            },
            Event::UserEvent(e) => self.dispatch(e, control_flow),
            _ => {}
        }
    }

    fn dispatch(&mut self, event: AppEvent, control_flow: &mut ControlFlow) {
        let result = match event {
            AppEvent::Accelerator(acc) => match key_command(acc) {
                Some((cmd, _)) => self.run_key_command(cmd, control_flow),
                None => Ok(()),
            },
            AppEvent::Chrome(cmd) => self.on_chrome(cmd, control_flow),
            AppEvent::TitleChanged(id, title) => {
                if let Some(tab) = self.registry.get_mut(id) {
                    tab.set_title(title);
                }
                self.sync_chrome()
            }
            AppEvent::Navigated(id, url) => {
                // Internal pages are shell surfaces, not tab destinations —
                // never record them as a tab's address.
                if pages::is_internal_url(&url) {
                    return;
                }
                if let Some(tab) = self.registry.get_mut(id) {
                    // A view that already froze can still have events in
                    // flight from before the drop — they must not move the
                    // frozen tab's address away from its snapshot's.
                    if tab.state() == TabState::Active {
                        tab.navigate(url);
                    }
                }
                if self.registry.active_id() == Some(id) {
                    self.sync_chrome()
                } else {
                    Ok(())
                }
            }
            AppEvent::OpenInNewTab(url) => self.spawn_tab(&url, true),
            AppEvent::PageFullscreen(id, on) => self.page_fullscreen(id, on),
            AppEvent::LoadFailed(id, failure) => self.show_error(id, failure),
            AppEvent::UpdateChecked(outcome) => self.on_update(outcome, control_flow),
        };
        if let Err(e) = result {
            eprintln!("cryo: {e:?}");
        }
    }

    fn on_chrome(
        &mut self,
        cmd: ChromeCommand,
        control_flow: &mut ControlFlow,
    ) -> anyhow::Result<()> {
        match cmd {
            ChromeCommand::Ready => self.sync_chrome(),
            ChromeCommand::NewTab => self.new_tab(),
            ChromeCommand::Activate { id } => self.activate(TabId::from_raw(id)),
            ChromeCommand::Close { id } => self.close_tab(TabId::from_raw(id), control_flow),
            ChromeCommand::Navigate {
                input,
                ctrl,
                alt,
                shift,
            } => {
                let url = if ctrl && !alt && !shift {
                    canonize_nav_input(&input)
                } else {
                    normalize_nav_input(&input)
                };
                match (ctrl, alt, shift) {
                    // Ctrl+Shift+Enter: open the address in a new window.
                    (true, false, true) => Self::spawn_window(false, Some(&url)),
                    // Alt+Enter: new foreground tab; Alt+Shift+Enter: background.
                    (false, true, false) => self.spawn_tab(&url, true),
                    (false, true, true) => self.spawn_tab(&url, false),
                    _ => self.navigate_active(&url),
                }
            }
            ChromeCommand::Reload => self.reload_active(),
            ChromeCommand::Back => self.back_active(),
            ChromeCommand::Forward => self.forward_active(),
            // The WebView2 profile cookie jar is shared across views, so a
            // Google sign-in tab authenticates every tab — and persists.
            ChromeCommand::SignIn => self.spawn_tab("https://accounts.google.com", true),
            ChromeCommand::NewIncognito => Self::spawn_window(true, None),
            ChromeCommand::DevTools => self.devtools_active(),
            ChromeCommand::ClearData => self
                .chrome
                .clear_browsing_data()
                .map_err(|e| anyhow::anyhow!(e)),
            ChromeCommand::Settings => self.spawn_tab(pages::SETTINGS_URL, true),
            ChromeCommand::CheckUpdates => {
                self.request_update_check(true);
                Ok(())
            }
            ChromeCommand::UpdateApply => self.apply_update(control_flow),
            ChromeCommand::Freeze { id } => {
                match id
                    .map(TabId::from_raw)
                    .or_else(|| self.registry.active_id())
                {
                    Some(id) => self.freeze_tab(id),
                    None => Ok(()),
                }
            }
            ChromeCommand::Discard { id } => {
                match id
                    .map(TabId::from_raw)
                    .or_else(|| self.registry.active_id())
                {
                    Some(id) => self.discard_tab(id),
                    None => Ok(()),
                }
            }
            ChromeCommand::Overlay { open } => self.set_overlay_open(open),
        }
    }

    /// Chrome bounds for the current overlay state: the strip normally,
    /// the whole window while an overlay is open.
    fn chrome_rect(&self) -> crate::engine::Rect {
        let (w, h) = self.logical_size;
        if self.overlay_open {
            crate::engine::Rect {
                x: 0.0,
                y: 0.0,
                w,
                h,
            }
        } else {
            shell::layout(w, h).0
        }
    }

    /// Chrome overlays (the ⋮ menu, the omnibox dropdown) are part of the
    /// chrome document, but a view can't paint outside its own bounds —
    /// while one is open the chrome view is raised above the content
    /// views and stretched over the whole window. Its background is
    /// transparent, so only the strip and the popup paint; the page shows
    /// through and any click outside lands on the chrome document, which
    /// dismisses the overlay.
    fn set_overlay_open(&mut self, open: bool) -> anyhow::Result<()> {
        if self.overlay_open == open {
            return Ok(());
        }
        self.overlay_open = open;
        self.chrome.set_bounds(self.chrome_rect())?;
        if open {
            self.chrome.bring_to_front()?;
        }
        Ok(())
    }

    /// App-side actions that replace what's on screen must close overlays —
    /// the document can't see them, so it's synced through `__cryo_overlay`
    /// before the bounds come back down.
    fn dismiss_overlays(&mut self) -> anyhow::Result<()> {
        if !self.overlay_open {
            return Ok(());
        }
        self.chrome
            .eval("window.__cryo_overlay && window.__cryo_overlay(false)")?;
        self.set_overlay_open(false)
    }

    /// Fallback key path: fires only while no view holds focus — the
    /// accelerator handler on each view covers the normal case.
    fn on_key(&mut self, event: &KeyEvent, control_flow: &mut ControlFlow) {
        if event.state != ElementState::Pressed {
            return;
        }
        let Some(acc) = tao_accelerator(event, self.modifiers) else {
            return;
        };
        let result = match key_command(acc) {
            Some((cmd, _)) => self.run_key_command(cmd, control_flow),
            None => Ok(()),
        };
        if let Err(e) = result {
            eprintln!("cryo: {e:?}");
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
                *control_flow = ControlFlow::Exit;
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
            KeyCommand::SaveAs => self.save_active(),
            KeyCommand::OpenFile => self.open_file(),
            KeyCommand::ViewSource => self.view_source(),
            KeyCommand::Zoom(step) => self.zoom_active(step),
            KeyCommand::Screenshot => self.screenshot_active(),
            KeyCommand::ClearBrowsingData => self
                .chrome
                .clear_browsing_data()
                .map_err(|e| anyhow::anyhow!(e)),
            KeyCommand::FreezeActive => match self.registry.active_id() {
                Some(id) => self.freeze_tab(id),
                None => Ok(()),
            },
        }
    }

    /// One accelerator callback per view. Keys the browser owns are
    /// reported to the event loop and swallowed (`steal`); everything else
    /// returns `false` so the page and the engine see it normally.
    fn accelerator_forwarder(proxy: &EventLoopProxy<AppEvent>) -> AcceleratorHandler {
        let proxy = proxy.clone();
        Box::new(move |acc| match key_command(acc) {
            Some((_, steal)) => {
                let _ = proxy.send_event(AppEvent::Accelerator(acc));
                steal
            }
            None => false,
        })
    }

    /// One window per process: a "new window" key spawns a sibling cryo
    /// process. `--incognito` puts every view of that process on an
    /// InPrivate profile — incognito is a property of the window, not a
    /// tab, same as Chrome. A bare arg hands the child an address to open.
    fn spawn_window(incognito: bool, url: Option<&str>) -> anyhow::Result<()> {
        let exe = std::env::current_exe()?;
        let mut cmd = std::process::Command::new(exe);
        if incognito {
            cmd.arg("--incognito");
        }
        if let Some(url) = url {
            cmd.arg(url);
        }
        cmd.spawn()?;
        Ok(())
    }

    fn new_tab(&mut self) -> anyhow::Result<()> {
        let id = self.registry.create(DEFAULT_URL.to_string());
        if let Some(tab) = self.registry.get_mut(id) {
            tab.set_title("New tab".to_string());
        }
        self.spawn_view_for(id, NEW_TAB_URL)?;
        // A fresh tab means "type an address" — hand focus to the bar
        // like every browser does, not to the new-tab page.
        self.focus_address_bar()
    }

    /// Bring back the most recently closed tab — `activate` respawns the
    /// surface, so a tab that was closed while frozen comes back through
    /// the same restore path.
    fn reopen_closed(&mut self) -> anyhow::Result<()> {
        match self.registry.reopen_closed() {
            Some(id) => self.activate(id),
            None => Ok(()),
        }
    }

    /// Open `url` in a fresh tab — foreground for target=_blank /
    /// Alt+Enter, background for Alt+Shift+Enter.
    fn spawn_tab(&mut self, url: &str, foreground: bool) -> anyhow::Result<()> {
        let id = self.registry.create(url.to_string());
        let (_, content_rect) = shell::layout(self.logical_size.0, self.logical_size.1);
        let view = Self::spawn_content_view(
            &*self.engine,
            &self.proxy,
            content_rect,
            id,
            url,
            foreground,
        )?;
        self.views.insert(id, view);
        if foreground {
            self.activate(id)
        } else {
            self.sync_chrome()
        }
    }

    /// Spawn the engine surface for `id`, register it, activate it.
    fn spawn_view_for(&mut self, id: TabId, url: &str) -> anyhow::Result<()> {
        let (_, content_rect) = shell::layout(self.logical_size.0, self.logical_size.1);
        let view =
            Self::spawn_content_view(&*self.engine, &self.proxy, content_rect, id, url, true)?;
        self.views.insert(id, view);
        self.activate(id)
    }

    fn spawn_content_view(
        engine: &dyn Engine,
        proxy: &EventLoopProxy<AppEvent>,
        content_rect: crate::engine::Rect,
        id: TabId,
        url: &str,
        visible: bool,
    ) -> anyhow::Result<Box<dyn ContentView>> {
        let title_proxy = proxy.clone();
        let nav_proxy = proxy.clone();
        let nw_proxy = proxy.clone();
        let fs_proxy = proxy.clone();
        let fail_proxy = proxy.clone();
        let view = engine.spawn(ViewSpec {
            bounds: content_rect,
            visible,
            background: Some(shell::CONTENT_BACKGROUND),
            on_title_change: Some(Box::new(move |t| {
                let _ = title_proxy.send_event(AppEvent::TitleChanged(id, t));
            })),
            on_navigate: Some(Box::new(move |uri| {
                let _ = nav_proxy.send_event(AppEvent::Navigated(id, uri.clone()));
                true
            })),
            on_accelerator: Some(Self::accelerator_forwarder(proxy)),
            on_new_window: Some(Box::new(move |url| {
                let _ = nw_proxy.send_event(AppEvent::OpenInNewTab(url));
            })),
            on_fullscreen: Some(Box::new(move |on| {
                let _ = fs_proxy.send_event(AppEvent::PageFullscreen(id, on));
            })),
            on_load_failure: Some(Box::new(move |failure| {
                let _ = fail_proxy.send_event(AppEvent::LoadFailed(id, failure));
            })),
            // Pages may take the mic and camera — without a handler the
            // engine's default is a silent deny, which is why calls appear
            // to connect but transmit nothing. Everything else is denied.
            on_permission: Some(Box::new(|kind| match kind {
                PermissionKind::Microphone | PermissionKind::Camera => PermissionDecision::Allow,
                _ => PermissionDecision::Deny,
            })),
            ..ViewSpec::url(url)
        })?;
        Ok(view)
    }

    /// Freeze `id`: snapshot its surface state, destroy the view, leave a
    /// dimmed marker in the strip. Freezing the active tab hands focus to
    /// the same neighbour a close would; a lone tab can't freeze because
    /// nothing would be left to paint, so the command just does nothing.
    fn freeze_tab(&mut self, id: TabId) -> anyhow::Result<()> {
        if !self.leave_foreground(id)? {
            return Ok(());
        }
        // Only a live surface has anything worth snapshotting — a view
        // missing here means the tab is already frozen or discarded.
        let Some(view) = self.views.get(&id) else {
            return Ok(());
        };
        let zoom = view.zoom_factor().unwrap_or(1.0);
        let muted = view.muted().unwrap_or(false);
        let Some(tab) = self.registry.get_mut(id) else {
            return Ok(());
        };
        tab.freeze(TabSnapshot {
            url: tab.url().to_string(),
            frozen_at: std::time::Instant::now(),
            zoom,
            muted,
        })?;
        drop(self.views.remove(&id));
        self.sync_chrome()
    }

    /// Discard `id`: drop the surface keeping nothing but the address —
    /// freeze without a snapshot. Under memory pressure this is what the
    /// (Phase 2) enforcer will pick; the trigger here is manual.
    fn discard_tab(&mut self, id: TabId) -> anyhow::Result<()> {
        if !self.leave_foreground(id)? {
            return Ok(());
        }
        let Some(tab) = self.registry.get_mut(id) else {
            return Ok(());
        };
        tab.discard()?;
        drop(self.views.remove(&id));
        self.sync_chrome()
    }

    /// Move focus off `id` when it's the showing tab — freeze and discard
    /// leave the tab in the strip, so the neighbour a close would pick
    /// takes over first. Returns `false` when `id` is the only tab: with
    /// nothing left to paint, its view must stay alive.
    fn leave_foreground(&mut self, id: TabId) -> anyhow::Result<bool> {
        if self.registry.active_id() != Some(id) {
            return Ok(true);
        }
        let Some(next) = self.registry.focus_fallback(id) else {
            return Ok(false);
        };
        // A fullscreen page owns the whole window — leave fullscreen
        // before the switch or the strip stays hidden under it.
        if self.page_fullscreen {
            self.page_fullscreen(id, false)?;
        }
        self.activate(next)?;
        Ok(true)
    }

    /// Give a view-less tab — frozen, discarded, or one coming back from
    /// the closed stack — a fresh surface, then flip it back to live.
    /// Spawned invisible: `activate` owns the show/hide ordering.
    fn restore_view(&mut self, id: TabId) -> anyhow::Result<()> {
        let Some(tab) = self.registry.get(id) else {
            return Ok(());
        };
        let snap = tab.snapshot();
        let url = snap.map(|s| s.url.as_str()).unwrap_or_else(|| tab.url());
        let load = content_url_for(url).to_string();
        let zoom = snap.map(|s| s.zoom).unwrap_or(1.0);
        let muted = snap.map(|s| s.muted).unwrap_or(false);
        let (_, content_rect) = shell::layout(self.logical_size.0, self.logical_size.1);
        let view =
            Self::spawn_content_view(&*self.engine, &self.proxy, content_rect, id, &load, false)?;
        // Snapshot extras exist only for frozen tabs; discarded starts clean.
        if zoom != 1.0 {
            view.set_zoom_factor(zoom)?;
        }
        if muted {
            view.set_muted(true)?;
        }
        self.views.insert(id, view);
        // The surface exists — the domain may flip back to live. If spawn
        // had failed above we'd have bailed, leaving the tab frozen.
        if let Some(tab) = self.registry.get_mut(id) {
            if tab.state() != TabState::Active {
                tab.restore()?;
            }
        }
        Ok(())
    }

    fn activate(&mut self, id: TabId) -> anyhow::Result<()> {
        self.dismiss_overlays()?;
        let prev = self.registry.active_id();
        if prev == Some(id) {
            return Ok(());
        }
        // Frozen, discarded, and reopened tabs have no surface — clicking
        // them is what "restore" means.
        if !self.views.contains_key(&id) {
            self.restore_view(id)?;
        }
        self.registry.set_active(id)?;
        if let Some(view) = self.views.get(&id) {
            let (_, content_rect) = shell::layout(self.logical_size.0, self.logical_size.1);
            // Show before hiding: the incoming view becomes visible while
            // the outgoing one is still painted, so no frame ever shows
            // the bare window behind them. Both changes settle in this
            // same event turn, so z-order between the two never reaches
            // the screen.
            view.set_bounds(content_rect)?;
            view.set_visible(true)?;
            view.focus()?;
            self.chrome_focused = false;
        }
        if let Some(p) = prev.and_then(|p| self.views.get(&p)) {
            p.set_visible(false)?;
        }
        self.sync_chrome()
    }

    fn close_tab(&mut self, id: TabId, control_flow: &mut ControlFlow) -> anyhow::Result<()> {
        match self.registry.close(id)? {
            // Reveal the neighbour first, then destroy the dead view —
            // dropping it while it's still the only painted surface
            // leaves a one-frame hole where the window shows through.
            Some(next) => {
                self.activate(next)?;
                drop(self.views.remove(&id));
                self.sync_chrome()
            }
            None => {
                *control_flow = ControlFlow::Exit;
                Ok(())
            }
        }
    }

    fn cycle(&mut self, offset: isize) -> anyhow::Result<()> {
        match self.registry.cycle_target(offset) {
            Some(id) => self.activate(id),
            None => Ok(()),
        }
    }

    fn navigate_active(&mut self, url: &str) -> anyhow::Result<()> {
        self.dismiss_overlays()?;
        let Some(id) = self.registry.active_id() else {
            return Ok(());
        };
        if let Some(view) = self.views.get(&id) {
            view.load_url(content_url_for(url))?;
            // Hand keyboard focus to the page — a committed navigation in a
            // real browser moves focus out of the address bar.
            view.focus()?;
            self.chrome_focused = false;
        }
        if let Some(tab) = self.registry.get_mut(id) {
            tab.navigate(url.to_string());
        }
        self.sync_chrome()
    }

    /// The engine surface of the selected tab, if one is live.
    fn active_view(&self) -> Option<&dyn ContentView> {
        self.registry
            .active_id()
            .and_then(|id| self.views.get(&id).map(|v| &**v))
    }

    fn reload_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.reload()?;
        }
        Ok(())
    }

    fn hard_reload_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.hard_reload()?;
        }
        Ok(())
    }

    fn back_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.go_back()?;
        }
        Ok(())
    }

    fn forward_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.go_forward()?;
        }
        Ok(())
    }

    fn stop_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.stop()?;
        }
        Ok(())
    }

    fn move_active_tab(&mut self, delta: isize) -> anyhow::Result<()> {
        let Some(id) = self.registry.active_id() else {
            return Ok(());
        };
        let Some(pos) = self.registry.position_of(id) else {
            return Ok(());
        };
        self.registry
            .relocate(id, (pos as isize + delta).max(0) as usize)?;
        self.sync_chrome()
    }

    fn move_active_tab_to(&mut self, index: usize) -> anyhow::Result<()> {
        match self.registry.active_id() {
            Some(id) => {
                self.registry.relocate(id, index)?;
                self.sync_chrome()
            }
            None => Ok(()),
        }
    }

    fn duplicate_active(&mut self) -> anyhow::Result<()> {
        let Some(id) = self.registry.active_id() else {
            return Ok(());
        };
        let dup = self.registry.duplicate(id)?;
        let url = self.registry.get(dup).map(|t| t.url().to_string());
        let Some(url) = url else { return Ok(()) };
        let load = content_url_for(&url).to_string();
        self.spawn_view_for(dup, &load)
    }

    fn mute_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.set_muted(!view.muted()?)?;
        }
        Ok(())
    }

    /// F6 toggles between the address bar and the page; Ctrl+F6 jumps
    /// straight to the page.
    fn cycle_focus(&mut self) -> anyhow::Result<()> {
        if self.chrome_focused {
            self.focus_page()
        } else {
            self.focus_address_bar()
        }
    }

    fn focus_page(&mut self) -> anyhow::Result<()> {
        self.chrome_focused = false;
        if let Some(view) = self.active_view() {
            view.focus()?;
        }
        Ok(())
    }

    fn print_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.eval("window.print()")?;
        }
        Ok(())
    }

    fn save_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.save_as()?;
        }
        Ok(())
    }

    /// Ctrl+O: open a local file — the file dialog blocks the loop while
    /// it's up, same as any modal system dialog.
    fn open_file(&mut self) -> anyhow::Result<()> {
        let Some(path) = rfd::FileDialog::new().pick_file() else {
            return Ok(());
        };
        let url = format!("file:///{}", path.display().to_string().replace('\\', "/"));
        self.navigate_active(&url)
    }

    /// Ctrl+U: view-source opens in a new tab, same as Chrome.
    fn view_source(&mut self) -> anyhow::Result<()> {
        let url = self.registry.active().map(|t| t.url().to_string());
        match url {
            Some(u) if u.starts_with("http") => self.spawn_tab(&format!("view-source:{u}"), true),
            _ => Ok(()),
        }
    }

    /// Steps: in ×1.25, out ÷1.25, reset to 1.0 — clamped like Chrome's
    /// 25%–500% range.
    fn zoom_active(&mut self, step: i32) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            let current = view.zoom_factor()?;
            let next = match step {
                1 => current * 1.25,
                -1 => current / 1.25,
                _ => 1.0,
            }
            .clamp(0.25, 5.0);
            view.set_zoom_factor(next)?;
        }
        Ok(())
    }

    /// DOM fullscreen (YouTube etc.): the page element fills the *view*,
    /// and the view only covers the area under the chrome strip — so we
    /// hide chrome and stretch the view over the whole window. Chrome
    /// restores on exit.
    /// Paint the error page over the tab whose load failed.
    ///
    /// The tab keeps its own address, so reload and the address bar still
    /// point at the destination rather than at the error page — and because
    /// internal URLs are never recorded, the error page cannot become a
    /// tab's address and send the next failure back to itself.
    fn show_error(&mut self, id: TabId, failure: LoadFailure) -> anyhow::Result<()> {
        let Some(view) = self.views.get(&id) else {
            return Ok(());
        };
        let attempted = self
            .registry
            .get(id)
            .map(|tab| tab.url().to_string())
            .unwrap_or_default();
        view.load_url(&error::error_url(failure, &attempted))?;
        Ok(())
    }

    fn page_fullscreen(&mut self, id: TabId, on: bool) -> anyhow::Result<()> {
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
        if let Some(view) = self.views.get(&id) {
            let (w, h) = self.logical_size;
            let rect = if on {
                crate::engine::Rect {
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

    /// Ctrl+Shift+S: PNG of the visible page under Pictures\cryo.
    fn screenshot_active(&mut self) -> anyhow::Result<()> {
        let Some(view) = self.active_view() else {
            return Ok(());
        };
        let png = view.capture_png()?;
        if png.is_empty() {
            return Ok(());
        }
        let dir = std::env::var_os("USERPROFILE")
            .map(std::path::PathBuf::from)
            .map(|p| p.join("Pictures"))
            .filter(|p| p.is_dir())
            .unwrap_or_else(std::env::temp_dir)
            .join("cryo");
        std::fs::create_dir_all(&dir)?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = dir.join(format!("shot-{ts}.png"));
        std::fs::write(&path, png)?;
        eprintln!("cryo: screenshot saved to {}", path.display());
        Ok(())
    }

    fn toggle_fullscreen(&mut self) -> anyhow::Result<()> {
        let next = if self.window.fullscreen().is_some() {
            None
        } else {
            Some(Fullscreen::Borderless(None))
        };
        self.window.set_fullscreen(next);
        Ok(())
    }

    fn devtools_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.open_devtools();
        }
        Ok(())
    }

    fn focus_address_bar(&mut self) -> anyhow::Result<()> {
        // Move controller focus to the chrome view first — `u.focus()` in
        // JS sets DOM focus only, so without this the keystrokes keep going
        // to whichever webview last had it.
        self.chrome.focus()?;
        self.chrome
            .eval("const u=document.getElementById('url');u.focus();u.select();")?;
        self.chrome_focused = true;
        Ok(())
    }

    fn resize(&mut self, width_px: u32, height_px: u32) {
        let scale = self.window.scale_factor();
        let w = width_px as f64 / scale;
        let h = height_px as f64 / scale;
        self.logical_size = (w, h);
        let (_, content_rect) = shell::layout(w, h);
        if let Err(e) = self.chrome.set_bounds(self.chrome_rect()) {
            eprintln!("cryo: {e:?}");
        }
        // A DOM-fullscreen view keeps covering the whole window on resize.
        let active = self.registry.active_id();
        for (id, view) in &self.views {
            let rect = if self.page_fullscreen && Some(*id) == active {
                crate::engine::Rect {
                    x: 0.0,
                    y: 0.0,
                    w,
                    h,
                }
            } else {
                content_rect
            };
            if let Err(e) = view.set_bounds(rect) {
                eprintln!("cryo: {e:?}");
            }
        }
    }

    /// Ask the release feed for a newer build. `prompt` means the user
    /// asked (menu item) — the outcome gets a dialog; silent checks only
    /// surface the ⋮ badge once an installer is downloaded and verified.
    fn request_update_check(&mut self, prompt: bool) {
        self.update_prompt = prompt;
        let proxy = self.proxy.clone();
        update::spawn_check(move |outcome| {
            let _ = proxy.send_event(AppEvent::UpdateChecked(outcome));
        });
    }

    fn on_update(
        &mut self,
        outcome: update::Outcome,
        control_flow: &mut ControlFlow,
    ) -> anyhow::Result<()> {
        let prompt = std::mem::take(&mut self.update_prompt);
        match outcome {
            update::Outcome::Ready(u) => {
                let apply = prompt && update_dialog(&u);
                self.pending_update = Some(u);
                self.sync_chrome()?;
                if apply {
                    self.apply_update(control_flow)?;
                }
            }
            update::Outcome::UpToDate if prompt => {
                rfd::MessageDialog::new()
                    .set_title("cryo")
                    .set_description(format!(
                        "cryo is up to date (v{})",
                        env!("CARGO_PKG_VERSION")
                    ))
                    .set_level(rfd::MessageLevel::Info)
                    .show();
            }
            update::Outcome::UpToDate => {}
            update::Outcome::Failed(e) if prompt => {
                rfd::MessageDialog::new()
                    .set_title("Update check failed")
                    .set_description(e.to_string())
                    .set_level(rfd::MessageLevel::Error)
                    .show();
            }
            update::Outcome::Failed(e) => return Err(e.into()),
        }
        Ok(())
    }

    /// Launch the staged installer and leave. Inno runs fully silent and
    /// relaunches cryo itself (the `[Run]` entry in `cryo.iss`).
    fn apply_update(&mut self, control_flow: &mut ControlFlow) -> anyhow::Result<()> {
        let Some(u) = &self.pending_update else {
            return Ok(());
        };
        update::launch_installer(u)?;
        *control_flow = ControlFlow::Exit;
        Ok(())
    }

    fn sync_chrome(&self) -> anyhow::Result<()> {
        let state = ChromeState {
            tabs: self
                .registry
                .iter()
                .map(|t| ChromeTab {
                    id: t.id().get(),
                    title: t.title().to_string(),
                    active: self.registry.active_id() == Some(t.id()),
                    state: t.state().as_str(),
                })
                .collect(),
            active_url: self
                .registry
                .active()
                .map(|t| t.url().to_string())
                .filter(|u| u != "about:blank")
                .unwrap_or_default(),
            update_version: self.pending_update.as_ref().map(|u| u.version.to_string()),
        };
        let json = serde_json::to_string(&state)?;
        self.chrome.eval(&format!("window.__cryo_update({json})"))?;
        Ok(())
    }
}
