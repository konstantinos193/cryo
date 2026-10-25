//! In-memory engine for tests: records specs and calls so policy code can
//! be exercised without a WebView.

use std::cell::RefCell;
use std::rc::Rc;

use crate::engine::{ContentView, Cookie, DownloadId, Engine, EngineError, Rect, ViewSpec};

#[derive(Clone, Debug)]
pub enum FakeCall {
    Permission(
        crate::engine::PermissionId,
        crate::engine::PermissionDecision,
    ),
    LoadUrl(String),
    Reload,
    HardReload,
    GoBack,
    GoForward,
    Stop,
    SetVisible(bool),
    SetBounds(Rect),
    BringToFront,
    Focus,
    Eval(String),
    SetMuted(bool),
    SetZoomFactor(f64),
    CapturePng,
    SaveAs,
    ClearBrowsingData,
    Cookies(String),
    CancelDownload(DownloadId),
    PauseDownload(DownloadId),
    ResumeDownload(DownloadId),
}

#[derive(Default)]
pub struct FakeEngine {
    memory: RefCell<Option<crate::engine::accounting::MemorySnapshot>>,
    spawned: RefCell<Vec<String>>,
    scripted: RefCell<Vec<bool>>,
    calls: Rc<RefCell<Vec<FakeCall>>>,
}

impl FakeEngine {
    pub fn set_memory_snapshot(&self, snapshot: crate::engine::accounting::MemorySnapshot) {
        *self.memory.borrow_mut() = Some(snapshot);
    }

    /// Source of every spec spawned so far (`url:<addr>` or `none`), in
    /// order. A snapshot — interior mutability stays inside the fake.
    pub fn spawned(&self) -> Vec<String> {
        self.spawned.borrow().clone()
    }

    /// Every `ContentView` call recorded so far, in order.
    pub fn calls(&self) -> Vec<FakeCall> {
        self.calls.borrow().clone()
    }

    pub fn scripted(&self) -> Vec<bool> {
        self.scripted.borrow().clone()
    }
}

impl Engine for FakeEngine {
    fn measure_memory(
        &self,
        complete: crate::engine::accounting::MemoryCallback,
    ) -> Result<(), EngineError> {
        let snapshot = self
            .memory
            .borrow()
            .clone()
            .ok_or_else(|| EngineError::ReadStats("No memory snapshot configured".into()))?;
        complete(snapshot);
        Ok(())
    }

    fn spawn(&self, spec: ViewSpec) -> Result<Box<dyn ContentView>, EngineError> {
        self.scripted.borrow_mut().push(spec.page_script.is_some());
        let initial_url = spec.url;
        let label = match &initial_url {
            Some(u) => format!("url:{u}"),
            None => "none".to_string(),
        };
        self.spawned.borrow_mut().push(label);
        Ok(Box::new(FakeView {
            calls: self.calls.clone(),
            url: RefCell::new(initial_url),
            muted: RefCell::new(false),
            zoom: RefCell::new(1.0),
        }))
    }
}

struct FakeView {
    calls: Rc<RefCell<Vec<FakeCall>>>,
    url: RefCell<Option<String>>,
    muted: RefCell<bool>,
    zoom: RefCell<f64>,
}

impl ContentView for FakeView {
    fn answer_permission(
        &self,
        id: crate::engine::PermissionId,
        decision: crate::engine::PermissionDecision,
    ) -> Result<(), EngineError> {
        self.calls
            .borrow_mut()
            .push(FakeCall::Permission(id, decision));
        Ok(())
    }
    fn load_url(&self, url: &str) -> Result<(), EngineError> {
        *self.url.borrow_mut() = Some(url.to_string());
        self.calls.borrow_mut().push(FakeCall::LoadUrl(url.into()));
        Ok(())
    }

    fn reload(&self) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::Reload);
        Ok(())
    }

    fn hard_reload(&self) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::HardReload);
        Ok(())
    }

    fn go_back(&self) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::GoBack);
        Ok(())
    }

    fn go_forward(&self) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::GoForward);
        Ok(())
    }

    fn can_go_back(&self) -> Result<bool, EngineError> {
        Ok(false)
    }

    fn can_go_forward(&self) -> Result<bool, EngineError> {
        Ok(false)
    }

    fn stop(&self) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::Stop);
        Ok(())
    }

    fn set_visible(&self, visible: bool) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::SetVisible(visible));
        Ok(())
    }

    fn set_bounds(&self, bounds: Rect) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::SetBounds(bounds));
        Ok(())
    }

    fn bring_to_front(&self) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::BringToFront);
        Ok(())
    }

    fn focus(&self) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::Focus);
        Ok(())
    }

    fn eval(&self, js: &str) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::Eval(js.into()));
        Ok(())
    }

    fn url(&self) -> Option<String> {
        self.url.borrow().clone()
    }

    fn open_devtools(&self) {}

    fn muted(&self) -> Result<bool, EngineError> {
        Ok(*self.muted.borrow())
    }

    fn set_muted(&self, muted: bool) -> Result<(), EngineError> {
        *self.muted.borrow_mut() = muted;
        self.calls.borrow_mut().push(FakeCall::SetMuted(muted));
        Ok(())
    }

    fn zoom_factor(&self) -> Result<f64, EngineError> {
        Ok(*self.zoom.borrow())
    }

    fn set_zoom_factor(&self, factor: f64) -> Result<(), EngineError> {
        *self.zoom.borrow_mut() = factor;
        self.calls
            .borrow_mut()
            .push(FakeCall::SetZoomFactor(factor));
        Ok(())
    }

    fn capture_png(&self) -> Result<Vec<u8>, EngineError> {
        self.calls.borrow_mut().push(FakeCall::CapturePng);
        Ok(Vec::new())
    }

    fn save_as(&self) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::SaveAs);
        Ok(())
    }

    fn clear_browsing_data(&self) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::ClearBrowsingData);
        Ok(())
    }

    fn cookies(&self, uri: &str) -> Result<Vec<Cookie>, EngineError> {
        self.calls
            .borrow_mut()
            .push(FakeCall::Cookies(uri.to_string()));
        Ok(Vec::new())
    }

    fn cancel_download(&self, id: DownloadId) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::CancelDownload(id));
        Ok(())
    }

    fn pause_download(&self, id: DownloadId) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::PauseDownload(id));
        Ok(())
    }

    fn resume_download(&self, id: DownloadId) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::ResumeDownload(id));
        Ok(())
    }

    fn suspend(&self) -> Result<(), EngineError> {
        Ok(())
    }

    fn resume(&self) -> Result<(), EngineError> {
        Ok(())
    }

    fn is_suspended(&self) -> Result<bool, EngineError> {
        Ok(false)
    }

    fn playing_audio(&self) -> Option<bool> {
        Some(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_accounting_returns_configured_process_snapshot() {
        use crate::engine::accounting::{
            MemorySnapshot, MemoryStats, ProcessId, ProcessKind, ProcessMemory,
        };
        let engine = FakeEngine::default();
        assert!(engine
            .measure_memory(Box::new(|_| panic!("unexpected callback")))
            .is_err());
        let now = std::time::Instant::now();
        engine.set_memory_snapshot(MemorySnapshot::new(
            now,
            now,
            vec![ProcessMemory::new(
                ProcessId(7),
                ProcessKind::Renderer,
                Ok(MemoryStats::new(123, 456)),
            )],
        ));
        let (tx, rx) = std::sync::mpsc::channel();
        engine
            .measure_memory(Box::new(move |snapshot| tx.send(snapshot).unwrap()))
            .unwrap();
        let snapshot = rx.recv().unwrap();
        assert_eq!(snapshot.private_bytes(ProcessKind::Renderer), Some(123));
        assert_eq!(
            snapshot.processes()[0].stats().unwrap().working_set_bytes(),
            456
        );
    }

    #[test]
    fn spawn_records_source_and_view_records_calls() {
        let engine = FakeEngine::default();
        let view = engine.spawn(ViewSpec::url("about:blank")).unwrap();
        assert_eq!(engine.spawned().as_slice(), ["url:about:blank"]);
        assert_eq!(engine.scripted().as_slice(), [false]);

        view.load_url("https://example.com").unwrap();
        view.set_visible(true).unwrap();
        view.eval("1+1").unwrap();
        assert_eq!(view.url().as_deref(), Some("https://example.com"));
        assert_eq!(engine.calls().len(), 3);

        engine
            .spawn(ViewSpec {
                page_script: Some("void 0"),
                ..ViewSpec::url("https://example.test")
            })
            .unwrap();
        assert_eq!(engine.scripted().as_slice(), [false, true]);
    }
}
