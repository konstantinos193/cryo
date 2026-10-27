//! Deferred WebView2 permission requests. COM objects remain on the view
//! thread; only origin, kind and request identity travel to the app.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use webview2_com::Microsoft::Web::WebView2::Win32::*;
use windows::core::{Interface, PWSTR};
use wry::{WebView, WebViewExtWindows};

use crate::engine::{
    EngineError, PermissionDecision, PermissionHandler, PermissionId, PermissionKind,
    PermissionRequest,
};

type Deferred = (
    ICoreWebView2PermissionRequestedEventArgs,
    ICoreWebView2Deferral,
);

#[derive(Default)]
struct Pending {
    next: u64,
    requests: HashMap<PermissionId, Deferred>,
}

pub(super) struct Permissions {
    state: Rc<RefCell<Pending>>,
    _handler: ICoreWebView2PermissionRequestedEventHandler,
}

impl Permissions {
    pub(super) fn attach(
        view: &WebView,
        callback: Option<PermissionHandler>,
    ) -> Result<Self, EngineError> {
        let state = Rc::new(RefCell::new(Pending::default()));
        let pending = state.clone();
        let handler =
            webview2_com::PermissionRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                unsafe {
                    args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY)?;
                    // An engine too old to keep grants session-only cannot
                    // safely allow a capability on this permission seam.
                    let Ok(args3) = args.cast::<ICoreWebView2PermissionRequestedEventArgs3>()
                    else {
                        return Ok(());
                    };
                    args3.SetSavesInProfile(false)?;
                    let Some(callback) = &callback else {
                        return Ok(());
                    };
                    let mut state = pending.borrow_mut();
                    if state.requests.len() >= 16 {
                        return Ok(());
                    }
                    let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                    args.PermissionKind(&mut kind)?;
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    let origin = webview2_com::take_pwstr(uri);
                    state.next += 1;
                    let id = PermissionId(state.next);
                    let deferral = args.GetDeferral()?;
                    state.requests.insert(id, (args, deferral));
                    drop(state);
                    callback(PermissionRequest {
                        id,
                        origin,
                        kind: permission_kind(kind),
                    });
                }
                Ok(())
            }));
        let mut token = 0;
        unsafe { view.webview().add_PermissionRequested(&handler, &mut token) }
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
        Ok(Self {
            state,
            _handler: handler,
        })
    }

    pub(super) fn answer(
        &self,
        id: PermissionId,
        decision: PermissionDecision,
    ) -> Result<(), EngineError> {
        let Some((args, deferral)) = self.state.borrow_mut().requests.remove(&id) else {
            return Ok(());
        };
        unsafe {
            let result = args.SetState(if decision == PermissionDecision::Allow {
                COREWEBVIEW2_PERMISSION_STATE_ALLOW
            } else {
                COREWEBVIEW2_PERMISSION_STATE_DENY
            });
            let completed = deferral.Complete();
            result.and(completed)
        }
        .map_err(|e| EngineError::View(e.to_string()))
    }
}

impl Drop for Permissions {
    fn drop(&mut self) {
        let ids: Vec<_> = self.state.borrow().requests.keys().copied().collect();
        for id in ids {
            if let Err(e) = self.answer(id, PermissionDecision::Deny) {
                crate::report!("denying permission during teardown: {e}");
            }
        }
    }
}

/// Also how `permission_profile` names a stored grant in the log: a row
/// the reader has to act on is worth naming in our vocabulary, not in
/// WebView2's enum.
pub(super) fn permission_kind(kind: COREWEBVIEW2_PERMISSION_KIND) -> PermissionKind {
    match kind {
        COREWEBVIEW2_PERMISSION_KIND_MICROPHONE => PermissionKind::Microphone,
        COREWEBVIEW2_PERMISSION_KIND_CAMERA => PermissionKind::Camera,
        COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION => PermissionKind::Geolocation,
        COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS => PermissionKind::Notifications,
        COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ => PermissionKind::ClipboardRead,
        _ => PermissionKind::Other,
    }
}
