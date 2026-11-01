//! WebView2's native Find session, options and callback lifetimes.

use std::rc::Rc;

use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2Environment15, ICoreWebView2Find,
    ICoreWebView2FindActiveMatchIndexChangedEventHandler,
    ICoreWebView2FindMatchCountChangedEventHandler, ICoreWebView2_28,
};
use windows::core::{Interface, HSTRING};
use wry::{WebView, WebViewExtWindows};

use crate::engine::{EngineError, FindHandler, FindMatches};

pub(super) struct Session {
    native: ICoreWebView2Find,
    count_token: i64,
    index_token: i64,
    _count: ICoreWebView2FindMatchCountChangedEventHandler,
    _index: ICoreWebView2FindActiveMatchIndexChangedEventHandler,
}

impl Session {
    pub(super) fn supported(view: &WebView) -> bool {
        view.webview().cast::<ICoreWebView2_28>().is_ok()
            && view
                .environment()
                .cast::<ICoreWebView2Environment15>()
                .is_ok()
    }

    pub(super) fn start(
        view: &WebView,
        query: &str,
        match_case: bool,
        report: FindHandler,
    ) -> Result<Self, EngineError> {
        unsafe { Self::start_native(view, query, match_case, report) }.map_err(failure)
    }

    pub(super) fn next(&self, backwards: bool) -> Result<(), EngineError> {
        unsafe {
            if backwards {
                self.native.FindPrevious()
            } else {
                self.native.FindNext()
            }
        }
        .map_err(failure)
    }

    unsafe fn start_native(
        view: &WebView,
        query: &str,
        match_case: bool,
        report: FindHandler,
    ) -> windows::core::Result<Self> {
        let native = view.webview().cast::<ICoreWebView2_28>()?.Find()?;
        let options = view
            .environment()
            .cast::<ICoreWebView2Environment15>()?
            .CreateFindOptions()?;
        options.SetFindTerm(&HSTRING::from(query))?;
        options.SetIsCaseSensitive(match_case)?;
        options.SetShouldHighlightAllMatches(true)?;
        options.SetSuppressDefaultFindDialog(true)?;
        let report: Rc<dyn Fn(Result<FindMatches, EngineError>)> = Rc::from(report);
        let count_report = report.clone();
        let count =
            webview2_com::FindMatchCountChangedEventHandler::create(Box::new(move |sender, _| {
                if let Some(sender) = sender {
                    count_report(matches(&sender).map_err(failure));
                }
                Ok(())
            }));
        let index_report = report.clone();
        let index = webview2_com::FindActiveMatchIndexChangedEventHandler::create(Box::new(
            move |sender, _| {
                if let Some(sender) = sender {
                    index_report(matches(&sender).map_err(failure));
                }
                Ok(())
            },
        ));
        let mut session = Self {
            native,
            count_token: 0,
            index_token: 0,
            _count: count,
            _index: index,
        };
        session
            .native
            .add_MatchCountChanged(&session._count, &mut session.count_token)?;
        session
            .native
            .add_ActiveMatchIndexChanged(&session._index, &mut session.index_token)?;
        let started = session.native.clone();
        let completed = webview2_com::FindStartCompletedHandler::create(Box::new(move |hr| {
            let result = hr.and_then(|()| matches(&started)).map_err(failure);
            report(result);
            Ok(())
        }));
        session.native.Start(&options, &completed)?;
        Ok(session)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            for result in [
                self.native.remove_MatchCountChanged(self.count_token),
                self.native.remove_ActiveMatchIndexChanged(self.index_token),
                self.native.Stop(),
            ] {
                if let Err(error) = result {
                    crate::report!("closing find session: {error}");
                }
            }
        }
    }
}

unsafe fn matches(native: &ICoreWebView2Find) -> windows::core::Result<FindMatches> {
    let (mut active, mut total) = (0, 0);
    native.ActiveMatchIndex(&mut active)?;
    native.MatchCount(&mut total)?;
    Ok(FindMatches {
        active: active.max(0) as u32,
        total: total.max(0) as u32,
    })
}

fn failure(error: windows::core::Error) -> EngineError {
    EngineError::Find(error.to_string())
}
