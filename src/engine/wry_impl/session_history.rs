//! Reading and walking a WebView2 view's back/forward list. WebView2's own
//! API stops at `GoBack`/`GoForward`; the list and the jump to an arbitrary
//! entry exist only over the DevTools protocol, so its wire format stops
//! here and the rest of the app sees `SessionHistory`.

use serde::Deserialize;
use wry::{WebView, WebViewExtWindows};

use crate::engine::{EngineError, SessionEntry, SessionEntryId, SessionHistory};

/// The list, read over `Page.getNavigationHistory`. The reply arrives on a
/// callback, so this waits through `wait_with_pump` like the cookie read.
pub(super) fn read(view: &WebView) -> Result<SessionHistory, EngineError> {
    let (tx, rx) = std::sync::mpsc::channel();
    let done = webview2_com::CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
        move |hr, json| {
            // A failed send means `wait_with_pump` already gave up —
            // nobody is left to signal.
            if tx.send(hr.map(|()| json)).is_err() {
                crate::report!("session history arrived with no receiver");
            }
            Ok(())
        },
    ));
    unsafe {
        view.webview().CallDevToolsProtocolMethod(
            &windows::core::HSTRING::from("Page.getNavigationHistory"),
            &windows::core::HSTRING::from("{}"),
            &done,
        )
    }
    .map_err(|e| EngineError::View(e.to_string()))?;
    let json = super::pump::wait_with_pump(rx)
        .map_err(|e| EngineError::View(e.to_string()))?
        .map_err(|e| EngineError::View(e.to_string()))?;
    parse(&json)
}

/// Jump straight to `entry`, however many steps away it is. Fire and
/// forget like `GoBack`: the navigation reports through the view's usual
/// events, so only a refused jump has anything to say here.
pub(super) fn go_to(view: &WebView, entry: SessionEntryId) -> Result<(), EngineError> {
    let done =
        webview2_com::CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|hr, _json| {
            if let Err(e) = hr {
                crate::report!("session history jump refused: {e}");
            }
            Ok(())
        }));
    let params = serde_json::json!({ "entryId": entry.get() }).to_string();
    unsafe {
        view.webview().CallDevToolsProtocolMethod(
            &windows::core::HSTRING::from("Page.navigateToHistoryEntry"),
            &windows::core::HSTRING::from(params),
            &done,
        )
    }
    .map_err(|e| EngineError::Navigate(e.to_string()))
}

/// `Page.getNavigationHistory`'s reply. A foreign format that grows fields,
/// so unknown ones are ignored rather than denied.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NavigationHistoryReply {
    current_index: usize,
    entries: Vec<NavigationEntry>,
}

#[derive(Deserialize)]
struct NavigationEntry {
    id: i64,
    url: String,
    title: String,
}

fn parse(json: &str) -> Result<SessionHistory, EngineError> {
    let reply: NavigationHistoryReply =
        serde_json::from_str(json).map_err(|e| EngineError::View(e.to_string()))?;
    let entries = reply
        .entries
        .into_iter()
        .map(|e| SessionEntry {
            id: SessionEntryId::from_raw(e.id),
            url: e.url,
            title: e.title,
        })
        .collect();
    SessionHistory::new(entries, reply.current_index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_devtools_reply_becomes_the_trail_it_describes() {
        // Trimmed from a real reply: `userTypedURL` and `transitionType`
        // are the fields this module ignores.
        let history = parse(
            r#"{"currentIndex":1,"entries":[
                {"id":7,"url":"https://a.test/","userTypedURL":"a.test","title":"A","transitionType":"typed"},
                {"id":9,"url":"https://b.test/","userTypedURL":"","title":"B","transitionType":"link"},
                {"id":12,"url":"https://c.test/","userTypedURL":"","title":"","transitionType":"link"}
            ]}"#,
        )
        .unwrap();
        let back: Vec<_> = history.behind().collect();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id.get(), 7);
        assert_eq!(back[0].title, "A");
        let forward: Vec<_> = history.ahead().collect();
        assert_eq!(forward.len(), 1);
        assert_eq!(forward[0].url, "https://c.test/");
    }

    #[test]
    fn a_malformed_reply_is_an_error_not_an_empty_list() {
        assert!(parse("{}").is_err());
        assert!(parse(r#"{"currentIndex":3,"entries":[]}"#).is_err());
    }
}
