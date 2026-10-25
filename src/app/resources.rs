//! Per-tab app resources. Lifecycle invalidation is invoked by the tab
//! aggregate for freeze, discard and close; engine teardown has no policy.

use super::{autofill, downloads, permissions};
use crate::engine::DownloadState;
use crate::tabs::SurfaceResources;

#[derive(Default)]
pub(super) struct TabResources {
    pub downloads: Vec<downloads::Entry>,
    pub password_prompt: Option<autofill::PasswordPrompt>,
    pub permission_prompt: Option<permissions::Prompt>,
}

impl SurfaceResources for TabResources {
    fn drop_surface(&mut self) {
        self.password_prompt = None;
        self.permission_prompt = None;
        for row in &mut self.downloads {
            if matches!(row.state, DownloadState::InProgress | DownloadState::Paused) {
                row.state = DownloadState::Interrupted;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{fake::FakeEngine, ContentView, DownloadId, Engine, ViewSpec};
    use crate::tabs::TabRegistry;

    #[test]
    fn discarding_interrupts_transfers_and_drops_password_cards_in_the_aggregate() {
        let engine = FakeEngine::default();
        let mut tabs = TabRegistry::<Box<dyn ContentView>, TabResources>::new();
        let id = tabs
            .create_with("https://a.test".into(), |_| {
                engine.spawn(ViewSpec::url("https://a.test"))
            })
            .unwrap();
        let tab = tabs.get_mut(id).unwrap();
        let view = tab.view_key().unwrap();
        for (key, state) in [(1, DownloadState::Paused), (2, DownloadState::Done)] {
            tab.resources_mut().downloads.push(downloads::Entry {
                key,
                view,
                engine_id: DownloadId(key),
                name: "test.txt".into(),
                path: "test.txt".into(),
                uri: "https://a.test/test.txt".into(),
                received: 1,
                total: Some(2),
                state,
            });
        }
        tab.resources_mut().password_prompt = Some(autofill::PasswordPrompt::Fill {
            view,
            origin: crate::vault::SiteOrigin::parse("https://a.test").unwrap(),
            username_field: None,
            password_field: "password".into(),
        });
        tab.discard().unwrap();
        assert!(tab.resources().password_prompt.is_none());
        assert_eq!(
            tab.resources().downloads[0].state,
            DownloadState::Interrupted
        );
        assert_eq!(tab.resources().downloads[1].state, DownloadState::Done);
        assert!(tabs.live(view).is_none());
    }
}
