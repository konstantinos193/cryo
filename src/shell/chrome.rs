//! The chrome strip: a self-contained HTML/JS UI plus the wire types it
//! speaks with the app over IPC (`window.ipc.postMessage` in, one
//! `__cryo_update(state)` eval out).

use serde::{Deserialize, Serialize};

/// Commands the chrome UI can send. Wire format: JSON tagged by `cmd`.
#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ChromeCommand {
    /// Sent by the chrome document when it finishes parsing — the signal to
    /// push state. The page announces itself instead of the app racing the
    /// document's load.
    Ready,
    NewTab,
    Activate {
        id: u64,
    },
    Close {
        id: u64,
    },
    Navigate {
        input: String,
        /// Modifier state at Enter — decides where the navigation lands
        /// (current tab, new tab, new window). Wire fields, policy in app.
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        alt: bool,
        #[serde(default)]
        shift: bool,
    },
    Reload,
}

/// State pushed to the chrome UI after every change.
#[derive(Debug, Serialize)]
pub struct ChromeState {
    pub tabs: Vec<ChromeTab>,
    pub active_url: String,
}

#[derive(Debug, Serialize)]
pub struct ChromeTab {
    pub id: u64,
    pub title: String,
    pub active: bool,
}

/// Turn address-bar text into a URL: full URLs pass through, domain-looking
/// input gets `https://`, anything else becomes a search.
pub fn normalize_nav_input(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() {
        return "about:blank".to_string();
    }
    if s.contains("://")
        || s.starts_with("about:")
        || s.starts_with("data:")
        || s.starts_with("file:")
    {
        return s.to_string();
    }
    if !s.contains(' ') && s.contains('.') {
        return format!("https://{s}");
    }
    format!("https://duckduckgo.com/?q={}", percent_encode(s))
}

/// Chrome's Ctrl+Enter: a bare word becomes `www.<word>.com`; anything
/// already dotted or multi-word falls back to normal normalization.
pub fn canonize_nav_input(input: &str) -> String {
    let s = input.trim();
    if !s.is_empty() && !s.contains(' ') && !s.contains('.') && !s.contains("://") {
        return format!("https://www.{s}.com");
    }
    normalize_nav_input(s)
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub const CHROME_HTML: &str = r##"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  html, body { height: 100%; overflow: hidden; }
  body {
    display: flex; align-items: center; gap: 6px;
    background: #1b1d21; color: #d7dae0;
    font: 12px/1.4 system-ui, "Segoe UI", sans-serif;
    padding: 0 8px;
    -webkit-user-select: none; user-select: none;
  }
  #tabs { display: flex; gap: 4px; overflow: hidden; flex: 0 1 auto; }
  .tab {
    display: flex; align-items: center; gap: 6px;
    max-width: 180px; padding: 5px 8px;
    background: #26292f; border-radius: 7px 7px 0 0;
    cursor: default; white-space: nowrap;
  }
  .tab.active { background: #3a3f47; }
  .tab .title { overflow: hidden; text-overflow: ellipsis; }
  .tab .x { color: #8a8f98; padding: 0 3px; border-radius: 3px; }
  .tab .x:hover { background: #4a505a; color: #fff; }
  #new-tab {
    flex: 0 0 auto; width: 26px; height: 26px; border: 0; border-radius: 6px;
    background: transparent; color: #d7dae0; font-size: 16px; cursor: pointer;
  }
  #new-tab:hover { background: #2e323a; }
  #url {
    flex: 1 1 auto; min-width: 120px; height: 28px;
    background: #26292f; border: 1px solid #363a42; border-radius: 7px;
    color: #e8eaee; padding: 0 10px; font: inherit; outline: none;
    -webkit-user-select: text; user-select: text;
  }
  #url:focus { border-color: #5a8adf; }
</style>
</head>
<body>
  <div id="tabs"></div>
  <button id="new-tab" title="New tab (Ctrl+T)">+</button>
  <input id="url" type="text" spellcheck="false"
         placeholder="Search or enter address">
<script>
  // wry's window.ipc channel is dead on this backend: attaching
  // with_ipc_handler leaves the document's own scripts unexecuted.
  // Commands go over the internal protocol instead — POSTs to /cmd land
  // in the engine's page resolver and reach the app unchanged.
  const ipc = (obj) => fetch('/cmd', {method: 'POST', body: JSON.stringify(obj)});
  const tabsEl = document.getElementById('tabs');
  const urlEl = document.getElementById('url');
  ipc({cmd: 'ready'});

  document.getElementById('new-tab').onclick = () => ipc({cmd: 'new_tab'});

  urlEl.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      ipc({cmd: 'navigate', input: urlEl.value,
           ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey});
      urlEl.blur();
    } else if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === 'l') {
      // Edge's paste-and-go.
      navigator.clipboard.readText().then((t) => {
        if (t) { ipc({cmd: 'navigate', input: t}); urlEl.blur(); }
      }).catch(() => {});
      e.preventDefault();
    }
  });

  // Browser shortcuts are not handled here — the engine forwards every
  // accelerator to the app before the page sees it, for chrome and content
  // views alike.

  window.__cryo_update = (state) => {
    tabsEl.innerHTML = '';
    for (const t of state.tabs) {
      const el = document.createElement('div');
      el.className = 'tab' + (t.active ? ' active' : '');
      // Middle-click anywhere on the tab closes it.
      el.addEventListener('auxclick', (e) => {
        if (e.button === 1) ipc({cmd: 'close', id: t.id});
      });
      const title = document.createElement('span');
      title.className = 'title';
      title.textContent = t.title;
      title.onclick = () => ipc({cmd: 'activate', id: t.id});
      const x = document.createElement('span');
      x.className = 'x';
      x.textContent = '×';
      x.onclick = () => ipc({cmd: 'close', id: t.id});
      el.append(title, x);
      tabsEl.appendChild(el);
    }
    if (document.activeElement !== urlEl) urlEl.value = state.active_url;
  };
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_passthroughs_and_upgrades() {
        assert_eq!(normalize_nav_input(""), "about:blank");
        assert_eq!(normalize_nav_input("  "), "about:blank");
        assert_eq!(
            normalize_nav_input("https://a.b/c?d=e"),
            "https://a.b/c?d=e"
        );
        assert_eq!(normalize_nav_input("about:blank"), "about:blank");
        assert_eq!(normalize_nav_input("example.com"), "https://example.com");
        assert_eq!(
            normalize_nav_input("rust borrow checker"),
            "https://duckduckgo.com/?q=rust%20borrow%20checker"
        );
    }

    #[test]
    fn canonize_wraps_bare_words_only() {
        assert_eq!(canonize_nav_input("cnn"), "https://www.cnn.com");
        assert_eq!(
            canonize_nav_input("example.com"),
            "https://example.com"
        );
        assert_eq!(
            canonize_nav_input("two words"),
            "https://duckduckgo.com/?q=two%20words"
        );
    }

    #[test]
    fn command_wire_format_round_trips() {
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"navigate","input":"x"}"#).unwrap();
        assert!(matches!(
            cmd,
            ChromeCommand::Navigate {
                input,
                ctrl: false,
                alt: false,
                shift: false,
            } if input == "x"
        ));
        let cmd: ChromeCommand = serde_json::from_str(
            r#"{"cmd":"navigate","input":"x","alt":true}"#,
        )
        .unwrap();
        assert!(matches!(
            cmd,
            ChromeCommand::Navigate { alt: true, .. }
        ));
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"activate","id":3}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::Activate { id: 3 }));
        assert!(serde_json::from_str::<ChromeCommand>(r#"{"cmd":"bogus"}"#).is_err());
    }
}
