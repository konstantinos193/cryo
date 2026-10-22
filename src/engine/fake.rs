//! In-memory engine for tests: records specs and calls so policy code can
//! be exercised without a WebView.

use std::cell::RefCell;
use std::rc::Rc;

use crate::engine::{ContentView, Engine, EngineError, Rect, ViewSpec};

#[derive(Debug)]
pub enum FakeCall {
    LoadUrl(String),
    Reload,
    SetVisible(bool),
    SetBounds(Rect),
    Focus,
    Eval(String),
}

#[derive(Default)]
pub struct FakeEngine {
    pub spawned: RefCell<Vec<String>>,
    pub calls: Rc<RefCell<Vec<FakeCall>>>,
}

impl Engine for FakeEngine {
    fn spawn(&self, spec: ViewSpec) -> Result<Box<dyn ContentView>, EngineError> {
        let label = match spec.url {
            Some(u) => format!("url:{u}"),
            None => "none".to_string(),
        };
        self.spawned.borrow_mut().push(label);
        Ok(Box::new(FakeView {
            calls: self.calls.clone(),
            url: RefCell::new(None),
        }))
    }
}

struct FakeView {
    calls: Rc<RefCell<Vec<FakeCall>>>,
    url: RefCell<Option<String>>,
}

impl ContentView for FakeView {
    fn load_url(&self, url: &str) -> Result<(), EngineError> {
        *self.url.borrow_mut() = Some(url.to_string());
        self.calls.borrow_mut().push(FakeCall::LoadUrl(url.into()));
        Ok(())
    }

    fn reload(&self) -> Result<(), EngineError> {
        self.calls.borrow_mut().push(FakeCall::Reload);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_records_source_and_view_records_calls() {
        let engine = FakeEngine::default();
        let view = engine.spawn(ViewSpec::url("about:blank")).unwrap();
        assert_eq!(engine.spawned.borrow().as_slice(), ["url:about:blank"]);

        view.load_url("https://example.com").unwrap();
        view.set_visible(true).unwrap();
        view.eval("1+1").unwrap();
        assert_eq!(view.url().as_deref(), Some("https://example.com"));
        assert_eq!(engine.calls.borrow().len(), 3);
    }
}
