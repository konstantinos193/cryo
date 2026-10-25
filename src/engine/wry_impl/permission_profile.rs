//! Remove legacy persisted grants before a view can navigate. Session
//! consent is owned by the app, never by WebView2's profile database.

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
                status
                    .inspect_err(|e| crate::report!("reading stored permission settings: {e}"))?;
                let Some(settings) = settings else {
                    return activate(&ready, &webview);
                };
                let mut count = 0;
                unsafe { settings.Count(&mut count)? };
                if count == 0 {
                    return activate(&ready, &webview);
                }
                let remaining = Rc::new(Cell::new(count));
                for index in 0..count {
                    let setting = unsafe { settings.GetValueAtIndex(index)? };
                    let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                    let mut origin = PWSTR::null();
                    unsafe {
                        setting.PermissionKind(&mut kind)?;
                        setting.PermissionOrigin(&mut origin)?;
                    }
                    let origin = HSTRING::from(webview2_com::take_pwstr(origin));
                    let remaining = remaining.clone();
                    let ready = ready.clone();
                    let webview = webview.clone();
                    let done = webview2_com::SetPermissionStateCompletedHandler::create(Box::new(
                        move |status| {
                            status.inspect_err(|e| {
                                crate::report!("clearing stored permission grant: {e}")
                            })?;
                            remaining.set(remaining.get() - 1);
                            if remaining.get() == 0 {
                                activate(&ready, &webview)?;
                            }
                            Ok(())
                        },
                    ));
                    unsafe {
                        clearing.SetPermissionState(
                            kind,
                            &origin,
                            COREWEBVIEW2_PERMISSION_STATE_DEFAULT,
                            &done,
                        )?;
                    }
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

fn activate(state: &RefCell<State>, view: &ICoreWebView2) -> windows::core::Result<()> {
    let url = {
        let mut state = state.borrow_mut();
        state.gate.store(true, Ordering::Release);
        state.pending.take()
    };
    if let Some(url) = url {
        unsafe {
            view.Navigate(&HSTRING::from(url))?;
        }
    }
    Ok(())
}
