//! Per-origin permission consent. Only the foreground page can ask, only
//! the trusted chrome can answer. An explicit Allow or Block is
//! remembered for the origin in `permissions.json` — the next request
//! from that site is answered without asking, like Chrome — while a
//! dismissed prompt (Escape, a click elsewhere, a tab switch) denies that
//! one request and asks again next time. Incognito keeps grants in
//! memory only. The site-info popup is where a remembered decision is
//! reset.

use super::App;
use crate::engine::{PermissionDecision, PermissionKind, PermissionRequest};
use crate::shell::chrome::ChromeSiteGrant;
use crate::tabs::ViewKey;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Remembered decisions, keyed by origin and capability, plus the file
/// they persist to — `None` keeps them session-only.
#[derive(Default)]
pub(super) struct Grants {
    decisions: HashMap<(String, PermissionKind), PermissionDecision>,
    path: Option<PathBuf>,
}

const CURRENT_VERSION: u32 = 1;

/// The wire shape of `permissions.json` — a DTO at the boundary, never
/// the domain's own map (AGENTS §6).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantsFile {
    version: u32,
    grants: Vec<GrantRow>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantRow {
    origin: String,
    capability: String,
    allowed: bool,
}

impl Grants {
    /// Grants remembered at `path`, or none when the file is missing,
    /// stale or corrupt — a profile that cannot be read asks again,
    /// which is never a launch failure. A row naming a capability this
    /// build does not know is dropped, not kept blind.
    pub(super) fn open(path: PathBuf) -> Self {
        let decisions = load(&path).unwrap_or_default();
        Self {
            decisions,
            path: Some(path),
        }
    }

    pub(super) fn get(&self, origin: &str, kind: PermissionKind) -> Option<PermissionDecision> {
        self.decisions.get(&(origin.to_string(), kind)).copied()
    }

    /// Remember `decision` for `origin` and write it out. A write that
    /// fails is reported and the decision still holds for this session.
    pub(super) fn remember(
        &mut self,
        origin: String,
        kind: PermissionKind,
        decision: PermissionDecision,
    ) {
        self.decisions.insert((origin, kind), decision);
        self.save();
    }

    /// Forget one remembered decision — the site asks again next time.
    pub(super) fn forget(&mut self, origin: &str, kind: PermissionKind) {
        if self.decisions.remove(&(origin.to_string(), kind)).is_some() {
            self.save();
        }
    }

    /// What is remembered for `origin`, in a stable order for the popup.
    pub(super) fn for_origin(&self, origin: &str) -> Vec<ChromeSiteGrant> {
        let mut rows: Vec<_> = self
            .decisions
            .iter()
            .filter(|((o, _), _)| o == origin)
            .map(|((_, kind), decision)| ChromeSiteGrant {
                capability: capability_name(*kind),
                allowed: *decision == PermissionDecision::Allow,
            })
            .collect();
        rows.sort_by_key(|r| r.capability);
        rows
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Err(e) = save(path, &self.decisions) {
            crate::report!("saving remembered permissions: {e}");
        }
    }
}

fn load(path: &Path) -> Option<HashMap<(String, PermissionKind), PermissionDecision>> {
    let bytes = crate::read_file(path).ok()??;
    let file: GrantsFile = serde_json::from_slice(&bytes).ok()?;
    if file.version != CURRENT_VERSION {
        return None;
    }
    Some(
        file.grants
            .into_iter()
            .filter_map(|row| {
                let kind = capability_kind(&row.capability)?;
                let decision = if row.allowed {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Deny
                };
                Some(((row.origin, kind), decision))
            })
            .collect(),
    )
}

fn save(
    path: &Path,
    decisions: &HashMap<(String, PermissionKind), PermissionDecision>,
) -> Result<(), std::io::Error> {
    let mut grants: Vec<_> = decisions
        .iter()
        .map(|((origin, kind), decision)| GrantRow {
            origin: origin.clone(),
            capability: capability_name(*kind).to_string(),
            allowed: *decision == PermissionDecision::Allow,
        })
        .collect();
    // Sorted so the file diffs cleanly and a re-save of the same set is
    // byte-identical.
    grants.sort_by(|a, b| (&a.origin, &a.capability).cmp(&(&b.origin, &b.capability)));
    let file = GrantsFile {
        version: CURRENT_VERSION,
        grants,
    };
    let bytes = serde_json::to_vec_pretty(&file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    crate::write_file_atomic(path, &bytes)
}

/// How a capability is named on the wire and in the prompt — one word
/// per kind, the same word in the file, the popup and the reset command.
pub(super) fn capability_name(kind: PermissionKind) -> &'static str {
    match kind {
        PermissionKind::Microphone => "microphone",
        PermissionKind::Camera => "camera",
        PermissionKind::Geolocation => "location",
        PermissionKind::Notifications => "notifications",
        PermissionKind::ClipboardRead => "clipboard",
        _ => "unsupported capability",
    }
}

/// The inverse of `capability_name` for names that reach the app from
/// the file or the popup; an unknown or unsupported name is `None`.
pub(super) fn capability_kind(name: &str) -> Option<PermissionKind> {
    Some(match name {
        "microphone" => PermissionKind::Microphone,
        "camera" => PermissionKind::Camera,
        "location" => PermissionKind::Geolocation,
        "notifications" => PermissionKind::Notifications,
        "clipboard" => PermissionKind::ClipboardRead,
        _ => return None,
    })
}

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

/// How a prompt closes. The two buttons are remembered; everything that
/// merely takes the prompt off screen denies this one request only.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Answer {
    Allow,
    Block,
    Dismiss,
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
        let capability = capability_name(request.kind);
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
            .and_then(|o| self.permission_grants.get(o, request.kind));
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
        answer: Answer,
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
                // An Allow only stands while the page that asked is still
                // the one on screen; a mismatch denies this request and
                // remembers nothing — a race must never become a stored
                // block.
                let still_there = current.as_deref() == Some(&prompt.origin)
                    && live == current
                    && self.registry.active_id() == Some(view.tab());
                let decision = match answer {
                    Answer::Allow if still_there => PermissionDecision::Allow,
                    _ => PermissionDecision::Deny,
                };
                let remember = match answer {
                    Answer::Allow => still_there,
                    Answer::Block => true,
                    Answer::Dismiss => false,
                };
                if remember {
                    self.permission_grants
                        .remember(prompt.origin, request.kind, decision);
                }
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
            self.answer_permission_prompt(token, Answer::Dismiss)?;
        }
        Ok(())
    }

    /// The site-info popup's reset: forget one remembered decision for
    /// `origin`, then repaint the popup so the row is gone.
    pub(super) fn reset_permission(
        &mut self,
        origin: &str,
        capability: &str,
    ) -> anyhow::Result<()> {
        if let Some(kind) = capability_kind(capability) {
            self.permission_grants.forget(origin, kind);
        }
        self.push_site_info()
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
    fn consent_is_scoped_to_origin_and_capability() {
        let mut grants = Grants::default();
        grants.remember(
            "https://example.test".into(),
            PermissionKind::Camera,
            PermissionDecision::Allow,
        );
        assert_eq!(
            grants.get("https://example.test", PermissionKind::Camera),
            Some(PermissionDecision::Allow)
        );
        assert_eq!(
            grants.get("https://other.test", PermissionKind::Camera),
            None
        );
        assert_eq!(
            grants.get("https://example.test", PermissionKind::Microphone),
            None
        );
        grants.forget("https://example.test", PermissionKind::Camera);
        assert_eq!(
            grants.get("https://example.test", PermissionKind::Camera),
            None
        );
    }

    fn temp(tag: &str) -> PathBuf {
        crate::test_dir(&format!("permissions-{tag}")).join("permissions.json")
    }

    #[test]
    fn remembered_decisions_survive_a_reopen() {
        let path = temp("roundtrip");
        let mut grants = Grants::open(path.clone());
        grants.remember(
            "https://meet.test".into(),
            PermissionKind::Camera,
            PermissionDecision::Allow,
        );
        grants.remember(
            "https://meet.test".into(),
            PermissionKind::Microphone,
            PermissionDecision::Deny,
        );
        let reopened = Grants::open(path.clone());
        assert_eq!(
            reopened.get("https://meet.test", PermissionKind::Camera),
            Some(PermissionDecision::Allow)
        );
        assert_eq!(
            reopened.get("https://meet.test", PermissionKind::Microphone),
            Some(PermissionDecision::Deny)
        );
        let rows = reopened.for_origin("https://meet.test");
        assert_eq!(
            rows.iter()
                .map(|r| (r.capability, r.allowed))
                .collect::<Vec<_>>(),
            vec![("camera", true), ("microphone", false)]
        );
        assert!(reopened.for_origin("https://other.test").is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_stale_or_corrupt_file_asks_again() {
        let path = temp("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json").unwrap();
        assert!(Grants::open(path.clone())
            .get("https://meet.test", PermissionKind::Camera)
            .is_none());
        std::fs::write(
            &path,
            r#"{"version":99,"grants":[{"origin":"https://meet.test","capability":"camera","allowed":true}]}"#,
        )
        .unwrap();
        assert!(Grants::open(path.clone())
            .get("https://meet.test", PermissionKind::Camera)
            .is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn capability_names_round_trip() {
        for kind in [
            PermissionKind::Microphone,
            PermissionKind::Camera,
            PermissionKind::Geolocation,
            PermissionKind::Notifications,
            PermissionKind::ClipboardRead,
        ] {
            assert_eq!(capability_kind(capability_name(kind)), Some(kind));
        }
        assert_eq!(capability_kind("unsupported capability"), None);
    }
}
