//! Remove legacy persisted grants before a view can navigate. Session
//! consent is owned by the app, never by WebView2's profile database.
//!
//! The reset gates external navigation, so every path through it has to
//! end with the gate open. A grant that refuses to clear is named in the
//! log and left where it is: a browser that answers no address at all is
//! a worse failure than one stale row the reader can act on. Views share
//! one profile, so a sibling view clearing the same row first is one of
//! the ordinary ways a clear fails.

use crate::engine::EngineError;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use windows::core::{Interface, HSTRING, PWSTR};
use wry::{WebView, WebViewExtWindows};

#[derive(Default)]
struct State {
    pending: Option<String>,
    gate: Arc<AtomicBool>,
}

pub(super) struct ProfileConsent(Rc<RefCell<State>>);

impl ProfileConsent {
    pub(super) fn reset(view: &WebView, gate: Arc<AtomicBool>) -> Result<Self, EngineError> {
        let state = Rc::new(RefCell::new(State {
            gate,
            ..State::default()
        }));
        let webview = view.webview();
        let profile: ICoreWebView2Profile4 = unsafe {
            webview
                .cast::<ICoreWebView2_13>()
                .and_then(|wv| wv.Profile())
                .and_then(|profile| profile.cast())
        }
        .map_err(|e| EngineError::Spawn(format!("permission profile: {e}")))?;
        let ready = state.clone();
        let clearing = profile.clone();
        let callback = webview2_com::GetNonDefaultPermissionSettingsCompletedHandler::create(
            Box::new(move |status, settings| {
                let settings = match status {
                    Ok(()) => settings,
                    Err(e) => {
                        crate::report!("reading stored permission settings: {e}");
                        None
                    }
                };
                // Nothing left to wait for — no rows, or none whose removal
                // even started — so the gate opens here rather than on an
                // answer that is never coming.
                if clear(&clearing, settings.as_ref(), &ready, &webview) == 0 {
                    resume(&webview, open(&ready));
                }
                Ok(())
            }),
        );
        unsafe { profile.GetNonDefaultPermissionSettings(&callback) }
            .map_err(|e| EngineError::Spawn(format!("resetting permission profile: {e}")))?;
        Ok(Self(state))
    }

    pub(super) fn navigate(&self, view: &WebView, url: &str) -> Result<(), EngineError> {
        // Trusted static chrome can paint while permissions are reset.
        // External navigation remains gated until all legacy grants clear.
        if super::protocol::internal_url(url) {
            return view
                .load_url(url)
                .map_err(|e| EngineError::Navigate(e.to_string()));
        }
        let mut state = self.0.borrow_mut();
        if state.gate.load(Ordering::Acquire) {
            drop(state);
            view.load_url(url)
                .map_err(|e| EngineError::Navigate(e.to_string()))
        } else {
            state.pending = Some(url.to_string());
            Ok(())
        }
    }
}

/// What a row reports back to once the profile has answered it. Every row
/// settles exactly once — that is the whole invariant this type exists to
/// hold, because a row that forgets to settle shuts the browser's address
/// bar for the lifetime of the view.
#[derive(Clone)]
struct Countdown {
    remaining: Rc<Cell<u32>>,
    state: Rc<RefCell<State>>,
    view: ICoreWebView2,
}

impl Countdown {
    fn settle(&self) {
        self.remaining.set(self.remaining.get().saturating_sub(1));
        if self.remaining.get() == 0 {
            resume(&self.view, open(&self.state));
        }
    }
}

/// Ask the profile to forget every stored grant, and return how many
/// answers are still outstanding. Failure is per row and never aborts the
/// sweep: the rows that can go still go, and the ones that cannot still
/// settle.
fn clear(
    profile: &ICoreWebView2Profile4,
    settings: Option<&ICoreWebView2PermissionSettingCollectionView>,
    state: &Rc<RefCell<State>>,
    view: &ICoreWebView2,
) -> u32 {
    let Some(settings) = settings else { return 0 };
    let mut count = 0;
    if let Err(e) = unsafe { settings.Count(&mut count) } {
        crate::report!("counting stored permission grants: {e}");
        return 0;
    }
    if count == 0 {
        return 0;
    }
    let countdown = Countdown {
        remaining: Rc::new(Cell::new(count)),
        state: state.clone(),
        view: view.clone(),
    };
    for index in 0..count {
        match clear_one(profile, settings, index, &countdown) {
            // An answer is coming; that handler settles the row.
            Ok(true) => {}
            Ok(false) => countdown.settle(),
            Err(e) => {
                crate::report!("clearing a stored permission grant: {e}");
                countdown.settle();
            }
        }
    }
    countdown.remaining.get()
}

/// Start one row's removal, reporting whether an answer is on its way.
/// From the moment `SetPermissionState` accepts the row, that row's
/// completion handler owns its place in the countdown.
fn clear_one(
    profile: &ICoreWebView2Profile4,
    settings: &ICoreWebView2PermissionSettingCollectionView,
    index: u32,
    countdown: &Countdown,
) -> windows::core::Result<bool> {
    let setting = unsafe { settings.GetValueAtIndex(index)? };
    let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
    let mut origin = PWSTR::null();
    unsafe {
        setting.PermissionKind(&mut kind)?;
        setting.PermissionOrigin(&mut origin)?;
    }
    let origin = webview2_com::take_pwstr(origin);
    // Only web origins are ours to revoke. An extension's grants came
    // with an extension the user installed on purpose and are declared in
    // its manifest, and WebView2 refuses `SetPermissionState` on a
    // `chrome-extension://` origin regardless — asking only produces a
    // failure every launch that no reader can act on.
    if !is_web_origin(&origin) {
        return Ok(false);
    }
    let origin = HSTRING::from(origin.as_str());
    // Named in full: the one thing a reader needs from a failure here is
    // which grant is still sitting in the profile.
    let row = format!(
        "{:?} for {origin}",
        super::permissions::permission_kind(kind)
    );
    let countdown = countdown.clone();
    let done = webview2_com::SetPermissionStateCompletedHandler::create(Box::new(move |status| {
        if let Err(e) = status {
            crate::report!("clearing the stored {row} grant: {e}");
        }
        countdown.settle();
        Ok(())
    }));
    unsafe {
        profile.SetPermissionState(kind, &origin, COREWEBVIEW2_PERMISSION_STATE_DEFAULT, &done)?;
    }
    Ok(true)
}

/// A grant the app's own consent seam is responsible for. Anything else
/// belongs to whoever installed it.
fn is_web_origin(origin: &str) -> bool {
    origin.starts_with("https://") || origin.starts_with("http://")
}

/// Open the gate and hand back the navigation it was holding. Answers
/// after the first find the gate already open and release nothing.
fn open(state: &RefCell<State>) -> Option<String> {
    let mut state = state.borrow_mut();
    if state.gate.swap(true, Ordering::AcqRel) {
        return None;
    }
    state.pending.take()
}

/// Run the navigation the gate had been holding. A failure leaves the
/// view on the page it was already showing, which the reader is told.
fn resume(view: &ICoreWebView2, url: Option<String>) {
    let Some(url) = url else { return };
    if let Err(e) = unsafe { view.Navigate(&HSTRING::from(url.as_str())) } {
        crate::report!("navigating to {url} after the permission reset: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting_on(url: Option<&str>) -> Rc<RefCell<State>> {
        Rc::new(RefCell::new(State {
            pending: url.map(str::to_owned),
            gate: Arc::new(AtomicBool::new(false)),
        }))
    }

    #[test]
    fn opening_the_gate_releases_the_navigation_that_waited_on_it() {
        let state = waiting_on(Some("https://example.com"));
        assert_eq!(open(&state), Some("https://example.com".into()));
        assert!(state.borrow().gate.load(Ordering::Acquire));
    }

    #[test]
    fn a_profile_with_nothing_to_release_still_opens_the_gate() {
        let state = waiting_on(None);
        assert_eq!(open(&state), None);
        assert!(state.borrow().gate.load(Ordering::Acquire));
    }

    #[test]
    fn only_web_origins_are_ours_to_revoke() {
        assert!(is_web_origin("https://example.com"));
        assert!(is_web_origin("http://example.com"));
        // The row that shut the address bar for a whole release: WebView2
        // will not clear it, so cryo must not wait on an answer to it.
        assert!(!is_web_origin(
            "chrome-extension://capkjengjmegfalafolcgldkgmdpefjd/"
        ));
    }

    #[test]
    fn a_second_answer_opens_nothing_and_navigates_nowhere() {
        let state = waiting_on(Some("https://example.com"));
        assert!(open(&state).is_some());
        // A later row settling must not replay the navigation the first
        // one already released.
        assert_eq!(open(&state), None);
        assert!(state.borrow().gate.load(Ordering::Acquire));
    }
}
