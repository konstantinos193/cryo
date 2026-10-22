//! The app: single owner of event-loop state. Chrome commands, window
//! events and view callbacks all funnel through `handle_event` — nowhere
//! else does state change hands.

use std::collections::HashMap;
use std::rc::Rc;

use tao::event::{ElementState, Event, KeyEvent, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopProxy};
use tao::keyboard::{Key, ModifiersState};
use tao::window::Window;

use crate::engine::{ContentView, Engine, ViewSpec};
use crate::shell::chrome::{normalize_nav_input, ChromeCommand, ChromeState, ChromeTab};
use crate::shell::pages::{self, CHROME_URL, NEW_TAB_URL};
use crate::shell::{self};
use crate::tabs::{TabId, TabRegistry};

/// Domain URL recorded for a fresh tab. The view actually loads
/// `NEW_TAB_URL`; `about:blank` stays the tab's logical address so the
/// address bar starts empty and internal URLs never escape into it.
const DEFAULT_URL: &str = "about:blank";

/// Events that reach the loop from view callbacks (`AppEvent`) — chrome
/// IPC, title changes, navigations.
pub enum AppEvent {
    Chrome(ChromeCommand),
    TitleChanged(TabId, String),
    Navigated(TabId, String),
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
}

impl App {
    pub fn new(
        window: Rc<Window>,
        engine: Rc<dyn Engine>,
        proxy: EventLoopProxy<AppEvent>,
    ) -> anyhow::Result<Self> {
        let logical = window.inner_size().to_logical::<f64>(window.scale_factor());
        let logical_size = (logical.width, logical.height);
        let (chrome_rect, content_rect) = shell::layout(logical_size.0, logical_size.1);

        // Content spawns before chrome so the chrome bar sits above it in
        // the z-order of sibling container windows.
        let mut registry = TabRegistry::new();
        let first = registry.create(DEFAULT_URL.to_string());
        if let Some(tab) = registry.get_mut(first) {
            tab.set_title("New tab".to_string());
        }
        let first_view =
            Self::spawn_content_view(&*engine, &proxy, content_rect, first, NEW_TAB_URL)?;
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
                    tab.navigate(url);
                }
                if self.registry.active_id() == Some(id) {
                    self.sync_chrome()
                } else {
                    Ok(())
                }
            }
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
            ChromeCommand::CloseActive => match self.registry.active_id() {
                Some(id) => self.close_tab(id, control_flow),
                None => Ok(()),
            },
            ChromeCommand::NextTab => self.cycle(1),
            ChromeCommand::PrevTab => self.cycle(-1),
            ChromeCommand::Navigate { input } => {
                let url = normalize_nav_input(&input);
                self.navigate_active(&url)
            }
            ChromeCommand::Reload => self.reload_active(),
        }
    }

    fn on_key(&mut self, event: &KeyEvent, control_flow: &mut ControlFlow) {
        if event.state != ElementState::Pressed {
            return;
        }
        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        let result = match &event.logical_key {
            Key::Tab if ctrl => self.cycle(if shift { -1 } else { 1 }),
            Key::F5 => self.reload_active(),
            Key::F12 => self.devtools_active(),
            Key::Character(c) if ctrl => match c.to_lowercase().as_str() {
                "t" => self.new_tab(),
                "w" => match self.registry.active_id() {
                    Some(id) => self.close_tab(id, control_flow),
                    None => Ok(()),
                },
                "l" => self.focus_address_bar(),
                "r" => self.reload_active(),
                _ => Ok(()),
            },
            _ => Ok(()),
        };
        if let Err(e) = result {
            eprintln!("cryo: {e:?}");
        }
    }

    fn new_tab(&mut self) -> anyhow::Result<()> {
        let id = self.registry.create(DEFAULT_URL.to_string());
        if let Some(tab) = self.registry.get_mut(id) {
            tab.set_title("New tab".to_string());
        }
        let (_, content_rect) = shell::layout(self.logical_size.0, self.logical_size.1);
        let view =
            Self::spawn_content_view(&*self.engine, &self.proxy, content_rect, id, NEW_TAB_URL)?;
        self.views.insert(id, view);
        self.activate(id)
    }

    fn spawn_content_view(
        engine: &dyn Engine,
        proxy: &EventLoopProxy<AppEvent>,
        content_rect: crate::engine::Rect,
        id: TabId,
        url: &str,
    ) -> anyhow::Result<Box<dyn ContentView>> {
        let title_proxy = proxy.clone();
        let nav_proxy = proxy.clone();
        let view = engine.spawn(ViewSpec {
            bounds: content_rect,
            visible: true,
            on_title_change: Some(Box::new(move |t| {
                let _ = title_proxy.send_event(AppEvent::TitleChanged(id, t));
            })),
            on_navigate: Some(Box::new(move |uri| {
                let _ = nav_proxy.send_event(AppEvent::Navigated(id, uri.clone()));
                true
            })),
            ..ViewSpec::url(url)
        })?;
        Ok(view)
    }

    fn activate(&mut self, id: TabId) -> anyhow::Result<()> {
        let prev = self.registry.active_id();
        if prev == Some(id) {
            return Ok(());
        }
        if let Some(p) = prev.and_then(|p| self.views.get(&p)) {
            p.set_visible(false)?;
        }
        self.registry.set_active(id)?;
        if let Some(view) = self.views.get(&id) {
            let (_, content_rect) = shell::layout(self.logical_size.0, self.logical_size.1);
            view.set_visible(true)?;
            view.set_bounds(content_rect)?;
            view.focus()?;
        }
        self.sync_chrome()
    }

    fn close_tab(&mut self, id: TabId, control_flow: &mut ControlFlow) -> anyhow::Result<()> {
        self.views.remove(&id);
        match self.registry.close(id)? {
            Some(next) => self.activate(next),
            None => {
                *control_flow = ControlFlow::Exit;
                Ok(())
            }
        }
    }

    fn cycle(&mut self, offset: isize) -> anyhow::Result<()> {
        match self.registry.cycle_active(offset) {
            Some(id) => self.activate(id),
            None => Ok(()),
        }
    }

    fn navigate_active(&mut self, url: &str) -> anyhow::Result<()> {
        let Some(id) = self.registry.active_id() else {
            return Ok(());
        };
        if let Some(view) = self.views.get(&id) {
            // Navigating to about:blank renders a never-painting document in
            // WebView2 — substitute the internal new-tab page instead.
            if url == "about:blank" {
                view.load_url(NEW_TAB_URL)?;
            } else {
                view.load_url(url)?;
            }
        }
        if let Some(tab) = self.registry.get_mut(id) {
            tab.navigate(url.to_string());
        }
        self.sync_chrome()
    }

    fn reload_active(&mut self) -> anyhow::Result<()> {
        if let Some(id) = self.registry.active_id() {
            if let Some(view) = self.views.get(&id) {
                view.reload()?;
            }
        }
        Ok(())
    }

    fn devtools_active(&mut self) -> anyhow::Result<()> {
        if let Some(id) = self.registry.active_id() {
            if let Some(view) = self.views.get(&id) {
                view.open_devtools();
            }
        }
        Ok(())
    }

    fn focus_address_bar(&mut self) -> anyhow::Result<()> {
        self.chrome
            .eval("const u=document.getElementById('url');u.focus();u.select();")?;
        Ok(())
    }

    fn resize(&mut self, width_px: u32, height_px: u32) {
        let scale = self.window.scale_factor();
        let w = width_px as f64 / scale;
        let h = height_px as f64 / scale;
        self.logical_size = (w, h);
        let (chrome_rect, content_rect) = shell::layout(w, h);
        if let Err(e) = self.chrome.set_bounds(chrome_rect) {
            eprintln!("cryo: {e:?}");
        }
        for view in self.views.values() {
            if let Err(e) = view.set_bounds(content_rect) {
                eprintln!("cryo: {e:?}");
            }
        }
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
                })
                .collect(),
            active_url: self
                .registry
                .active()
                .map(|t| t.url().to_string())
                .filter(|u| u != "about:blank")
                .unwrap_or_default(),
        };
        let json = serde_json::to_string(&state)?;
        self.chrome.eval(&format!("window.__cryo_update({json})"))?;
        Ok(())
    }
}
