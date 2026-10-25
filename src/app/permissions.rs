//! Per-origin permission consent. Only the foreground page can ask, only
//! the trusted chrome can answer, and grants last for this window only.

use super::App;
use crate::engine::{PermissionDecision, PermissionKind, PermissionRequest};
use crate::tabs::ViewKey;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct Grants(HashMap<(String, PermissionKind), PermissionDecision>);

#[derive(Serialize)]
pub(super) struct Prompt {
    pub token: u64,
    pub origin: String,
    pub capability: &'static str,
    #[serde(skip)]
    pub view: Option<ViewKey>,
    #[serde(skip)]
    request: Option<PermissionRequest>,
}

impl App {
    fn permission_prompt(&self) -> Option<&Prompt> {
        self.registry
            .active()
            .and_then(|tab| tab.resources().permission_prompt.as_ref())
    }

    fn take_permission_prompt(&mut self) -> Option<Prompt> {
        let id = self.registry.active_id()?;
        self.registry
            .get_mut(id)?
            .resources_mut()
            .permission_prompt
            .take()
    }

    pub(super) fn on_permission(
        &mut self,
        view: ViewKey,
        request: PermissionRequest,
    ) -> anyhow::Result<()> {
        let current = self.registry.live(view).map(|tab| tab.url());
        let origin = current.and_then(|current| {
            eligible_origin(
                &request,
                current,
                self.registry.active_id() == Some(view.tab()),
            )
        });
        let live_origin = self
            .view_of(view.tab())
            .and_then(|surface| surface.url())
            .and_then(|current| eligible_origin(&request, &current, true));
        let capability = match request.kind {
            PermissionKind::Microphone => "microphone",
            PermissionKind::Camera => "camera",
            PermissionKind::Geolocation => "location",
            PermissionKind::Notifications => "notifications",
            PermissionKind::ClipboardRead => "clipboard",
            _ => "unsupported capability",
        };
        let valid = origin.is_some() && origin == live_origin;
        if valid
            && matches!(
                request.kind,
                PermissionKind::Microphone | PermissionKind::Camera | PermissionKind::Other
            )
        {
            if let Some(tab) = self.registry.get_mut(view.tab()) {
                tab.resources_mut().automatic_freeze_blocked = true;
            }
        }
        let known = origin
            .as_ref()
            .and_then(|o| self.permission_grants.0.get(&(o.clone(), request.kind)))
            .copied();
        if !valid || known.is_some() || self.permission_prompt().is_some() {
            if let Some(surface) = self.view_of(view.tab()) {
                surface.answer_permission(
                    request.id,
                    if valid {
                        known.unwrap_or(PermissionDecision::Deny)
                    } else {
                        PermissionDecision::Deny
                    },
                )?;
            }
            return Ok(());
        }
        self.dismiss_overlays()?;
        self.permission_seq += 1;
        let prompt = Prompt {
            token: self.permission_seq,
            origin: origin.expect("valid requests have an origin"),
            capability,
            view: Some(view),
            request: Some(request),
        };
        let json = serde_json::to_string(&prompt)?;
        if let Some(tab) = self.registry.get_mut(view.tab()) {
            tab.resources_mut().permission_prompt = Some(prompt);
        }
        self.chrome
            .eval(&format!("window.__cryo_permission({json})"))?;
        self.set_overlay_open(true)
    }

    pub(super) fn answer_permission_prompt(
        &mut self,
        token: u64,
        allow: bool,
    ) -> anyhow::Result<()> {
        if self.permission_prompt().is_none_or(|p| p.token != token) {
            return Ok(());
        }
        let prompt = self
            .take_permission_prompt()
            .expect("matching permission prompt exists");
        if let (Some(view), Some(request)) = (prompt.view, prompt.request) {
            if let Some(tab) = self.registry.live(view) {
                let current = url::Url::parse(tab.url())
                    .ok()
                    .map(|u| u.origin().ascii_serialization());
                let live = tab
                    .view()
                    .and_then(|surface| surface.url())
                    .and_then(|url| url::Url::parse(&url).ok())
                    .map(|u| u.origin().ascii_serialization());
                let decision = if allow
                    && current.as_deref() == Some(&prompt.origin)
                    && live == current
                    && self.registry.active_id() == Some(view.tab())
                {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Deny
                };
                self.permission_grants
                    .0
                    .insert((prompt.origin, request.kind), decision);
                if let Some(surface) = tab.view() {
                    surface.answer_permission(request.id, decision)?;
                }
            }
        }
        self.chrome.eval("window.__cryo_permission(null)")?;
        self.set_overlay_open(false)
    }

    pub(super) fn cancel_permission_prompt(&mut self) -> anyhow::Result<()> {
        if let Some(token) = self.permission_prompt().map(|p| p.token) {
            self.answer_permission_prompt(token, false)?;
        }
        Ok(())
    }
}

fn eligible_origin(request: &PermissionRequest, current: &str, foreground: bool) -> Option<String> {
    if !foreground || request.kind == PermissionKind::Other {
        return None;
    }
    let source = url::Url::parse(&request.origin).ok()?;
    let current = url::Url::parse(current).ok()?;
    if source.scheme() != "https" || source.host().is_none() || source.origin() != current.origin()
    {
        return None;
    }
    Some(source.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::PermissionId;

    fn request(origin: &str, kind: PermissionKind) -> PermissionRequest {
        PermissionRequest {
            id: PermissionId(1),
            origin: origin.into(),
            kind,
        }
    }

    #[test]
    fn only_foreground_secure_matching_origins_can_prompt() {
        let camera = request("https://EXAMPLE.test:443/call", PermissionKind::Camera);
        assert_eq!(
            eligible_origin(&camera, "https://example.test/home", true).as_deref(),
            Some("https://example.test")
        );
        assert_eq!(
            eligible_origin(&camera, "https://example.test/home", false),
            None
        );
        for current in [
            "https://other.test",
            "http://example.test",
            "https://example.test:8443",
        ] {
            assert_eq!(eligible_origin(&camera, current, true), None);
        }
        assert_eq!(
            eligible_origin(
                &request("http://example.test", PermissionKind::Microphone),
                "http://example.test",
                true
            ),
            None
        );
        assert_eq!(
            eligible_origin(
                &request("https://example.test", PermissionKind::Other),
                "https://example.test",
                true
            ),
            None
        );
    }

    #[test]
    fn consent_is_scoped_to_origin_capability_and_window() {
        let mut grants = Grants::default();
        grants.0.insert(
            ("https://example.test".into(), PermissionKind::Camera),
            PermissionDecision::Allow,
        );
        assert_eq!(
            grants
                .0
                .get(&("https://example.test".into(), PermissionKind::Camera)),
            Some(&PermissionDecision::Allow)
        );
        assert!(!grants
            .0
            .contains_key(&("https://other.test".into(), PermissionKind::Camera)));
        assert!(!grants
            .0
            .contains_key(&("https://example.test".into(), PermissionKind::Microphone)));
        assert!(Grants::default().0.is_empty());
    }
}
