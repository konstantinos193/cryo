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
    Back,
    Forward,
    /// Open Google sign-in in a new tab — the profile cookie jar is shared,
    /// so signing in once applies to every site and survives restarts.
    SignIn,
    NewIncognito,
    DevTools,
    ClearData,
    /// Open the internal settings page in a new tab.
    Settings,
    /// Menu's "Check for updates" — result surfaces in a dialog.
    CheckUpdates,
    /// "Restart to update" — runs the staged installer and exits.
    UpdateApply,
    /// Freeze a tab: snapshot kept, surface dropped. `id` absent means
    /// the active tab — the ⋮ menu doesn't know tab ids, the strip's
    /// per-tab buttons do.
    Freeze {
        #[serde(default)]
        id: Option<u64>,
    },
    /// Discard a tab: the harsher drop — only the address survives.
    Discard {
        #[serde(default)]
        id: Option<u64>,
    },
    /// A chrome overlay (⋮ menu, omnibox dropdown) opened or closed —
    /// while open the app grows this view over the whole window so the
    /// popup can paint above the content views (a view can't draw
    /// outside its own bounds).
    Overlay {
        open: bool,
    },
}

/// State pushed to the chrome UI after every change.
#[derive(Debug, Serialize)]
pub struct ChromeState {
    pub tabs: Vec<ChromeTab>,
    pub active_url: String,
    /// A downloaded, verified installer is staged — the menu shows
    /// "Restart to update" and the ⋮ button gets a badge.
    pub update_version: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChromeTab {
    pub id: u64,
    pub title: String,
    pub active: bool,
    /// "active" | "frozen" | "discarded" — the strip dims and badges by it.
    pub state: &'static str,
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
    color: #d7dae0;
    font: 12px/1.4 system-ui, "Segoe UI", sans-serif;
    -webkit-user-select: none; user-select: none;
  }
  /* The visible strip. The view is transparent and grows over the whole
     window while the menu is open — everything outside #bar and the popup
     must stay unpainted so the page shows through. */
  #bar {
    position: fixed; top: 0; left: 0; right: 0; height: 44px;
    display: flex; align-items: center; gap: 6px;
    background: #1b1d21; padding: 0 8px;
  }
  /* The speed-dial row — offsets must match BAR_HEIGHT / DIAL_HEIGHT in
     shell::mod. Painted like #bar: the view is transparent, so an
     unpainted row would let the page show through. */
  #dial {
    position: fixed; top: 44px; left: 0; right: 0; height: 32px;
    display: flex; align-items: center; gap: 2px;
    background: #1b1d21; padding: 0 8px; overflow: hidden;
    border-top: 1px solid #23262c;
  }
  .dial {
    display: flex; align-items: center; gap: 6px; flex: 0 0 auto;
    height: 24px; padding: 0 8px 0 4px; border: 0; border-radius: 6px;
    background: transparent; color: #c9cdd4; font: inherit; cursor: pointer;
    white-space: nowrap;
  }
  .dial:hover { background: #2e323a; }
  .dial .dot {
    display: flex; align-items: center; justify-content: center;
    width: 18px; height: 18px; border-radius: 5px;
    background: #17455a; color: #a2deee; font-size: 11px; font-weight: 600;
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
  /* The per-tab freeze button — sits where a favicon would. */
  .tab .fz {
    color: #7fb6c9; padding: 0 3px; border-radius: 3px; cursor: default;
  }
  .tab .fz:hover { background: #4a505a; color: #cfeeff; }
  /* A frozen tab shows the same flake as a static state marker. */
  .tab .fzmark { color: #7fb6c9; padding: 0 3px; }
  .tab.frozen .title { color: #9db4c0; font-style: italic; }
  .tab.discarded .title { color: #6b717b; }
  .nav, #new-tab {
    flex: 0 0 auto; width: 26px; height: 26px; border: 0; border-radius: 6px;
    background: transparent; color: #d7dae0; font-size: 16px; cursor: pointer;
  }
  .nav:hover, #new-tab:hover { background: #2e323a; }
  .nav { font-size: 14px; }
  #url {
    flex: 1 1 auto; min-width: 120px; height: 28px;
    background: #26292f; border: 1px solid #363a42; border-radius: 7px;
    color: #e8eaee; padding: 0 10px; font: inherit; outline: none;
    -webkit-user-select: text; user-select: text;
  }
  #url:focus { border-color: #5a8adf; }
  #signin {
    flex: 0 0 auto; height: 26px; padding: 0 10px;
    border: 1px solid #3d434d; border-radius: 13px;
    background: transparent; color: #9ec2f5; font: inherit; cursor: pointer;
  }
  #signin:hover { background: #2a2f38; }
  #menu-pop {
    display: none; position: fixed; top: 40px; right: 8px; z-index: 10;
    min-width: 220px; padding: 6px 0;
    background: #26292f; border: 1px solid #3a3f47; border-radius: 8px;
    box-shadow: 0 6px 20px rgba(0,0,0,.45);
  }
  #menu-pop.open, #omni-pop.open { display: block; }
  /* The omnibox dropdown anchors under the toolbar (top = BAR_HEIGHT)
     and positions itself over the dial bar like every browser's does. */
  #omni-pop {
    display: none; position: fixed; top: 44px; z-index: 10;
    padding: 6px 0;
    background: #26292f; border: 1px solid #3a3f47; border-radius: 8px;
    box-shadow: 0 6px 20px rgba(0,0,0,.45);
  }
  .mi { padding: 7px 14px; cursor: default; white-space: nowrap; }
  .mi:hover, .mi.sel { background: #3a3f47; }
  .mi.off { color: #6b717b; }
  .mi.off:hover { background: transparent; }
  .mi.upd { color: #4ec96f; }
  #menu { position: relative; }
  #menu.has-update::after {
    content: ''; position: absolute; top: 4px; right: 4px;
    width: 7px; height: 7px; border-radius: 50%;
    background: #4ec96f;
  }
  .omni-row { overflow: hidden; text-overflow: ellipsis; }
  .sep { height: 1px; background: #3a3f47; margin: 5px 0; }
</style>
</head>
<body>
  <div id="bar">
    <button class="nav" id="back" title="Back (Alt+Left)">←</button>
    <button class="nav" id="fwd" title="Forward (Alt+Right)">→</button>
    <button class="nav" id="reload" title="Reload (F5)">⟳</button>
    <div id="tabs"></div>
    <button id="new-tab" title="New tab (Ctrl+T)">+</button>
    <input id="url" type="text" spellcheck="false"
           placeholder="Search or enter address">
    <button id="signin" title="Sign in with your Google account">Sign in</button>
    <button class="nav" id="menu" title="Menu">⋮</button>
  </div>
  <div id="dial"></div>
  <div id="menu-pop"></div>
  <div id="omni-pop"></div>
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
  document.getElementById('back').onclick = () => ipc({cmd: 'back'});
  document.getElementById('fwd').onclick = () => ipc({cmd: 'forward'});
  document.getElementById('reload').onclick = () => ipc({cmd: 'reload'});
  document.getElementById('signin').onclick = () => ipc({cmd: 'sign_in'});

  const menuPop = document.getElementById('menu-pop');
  const omniPop = document.getElementById('omni-pop');
  // Popups live in this document, but the view's bounds are only the
  // strip — a view can't paint outside them. While any overlay (menu,
  // omnibox) is open the app raises this view above the content and
  // stretches it over the whole window; the aggregate state is reported
  // so the bounds stay in sync. Clicks anywhere outside a popup land on
  // this document and dismiss.
  let menuOpen = false, omniOpen = false;
  const reportOverlay = () =>
    ipc({cmd: 'overlay', open: menuOpen || omniOpen});
  const setMenu = (open) => {
    if (open === menuOpen) return;
    menuOpen = open;
    menuPop.classList.toggle('open', open);
    reportOverlay();
  };
  const setOmni = (open) => {
    if (open === omniOpen) return;
    omniOpen = open;
    omniPop.classList.toggle('open', open);
    reportOverlay();
  };
  // The app calls this when it replaces what's on screen (tab switch,
  // navigation, fullscreen) — the document can't see those actions.
  window.__cryo_overlay = (open) => {
    if (!open) { setMenu(false); setOmni(false); }
  };
  // Last pushed state — menu guards (freeze/discard need a second tab to
  // hand focus to) read it at click time.
  let lastState = {tabs: []};
  const hasNeighbour = () => lastState.tabs.length > 1;
  const MENU = [
    {label: 'Freeze tab (Ctrl+Shift+F)', cmd: {cmd: 'freeze'}, ok: hasNeighbour},
    {label: 'Discard tab', cmd: {cmd: 'discard'}, ok: hasNeighbour},
    'sep',
    {label: 'New incognito window (Ctrl+Shift+N)', cmd: {cmd: 'new_incognito'}},
    {label: 'DevTools (F12)', cmd: {cmd: 'devtools'}},
    {label: 'Clear browsing data (Ctrl+Shift+Del)', cmd: {cmd: 'clear_data'}},
    'sep',
    {label: 'Settings', cmd: {cmd: 'settings'}},
    {label: 'Extensions — not supported (needs Chromium)', off: true},
    'sep',
    {label: 'Check for updates', cmd: {cmd: 'check_updates'}},
  ];
  // The update item lives outside MENU: it only exists while a verified
  // installer is staged, and its label carries the version. __cryo_update
  // creates/refreshes it.
  let updateItem = document.createElement('div');
  updateItem.className = 'mi upd';
  updateItem.onclick = () => { setMenu(false); ipc({cmd: 'update_apply'}); };
  for (const item of MENU) {
    if (item === 'sep') {
      const s = document.createElement('div');
      s.className = 'sep';
      menuPop.appendChild(s);
      continue;
    }
    const el = document.createElement('div');
    el.className = 'mi' + (item.off ? ' off' : '');
    el.textContent = item.label;
    if (!item.off) el.onclick = () => {
      setMenu(false);
      // Items can refuse at click time when the state forbids them —
      // the app would no-op anyway, but a dead click shouldn't look live.
      if (item.ok && !item.ok()) return;
      ipc(item.cmd);
    };
    menuPop.appendChild(el);
  }
  document.getElementById('menu').onclick = (e) => {
    e.stopPropagation();
    setMenu(!menuOpen);
  };
  document.addEventListener('click', () => { setMenu(false); setOmni(false); });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') { setMenu(false); setOmni(false); }
  });

  // The dial list feeds the bar below and the omnibox suggestions.
  let dialEntries = [];

  // Omnibox dropdown: a "search/go to" row plus dial matches — the two
  // sources the shell has until history exists (persist lands Phase 1+).
  let omniSel = -1;
  let omniRows = [];

  const go = (input, e) => {
    ipc({cmd: 'navigate', input,
         ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey});
    urlEl.blur();
  };

  const paintSel = () => {
    [...omniPop.children].forEach((el, i) =>
      el.classList.toggle('sel', i === omniSel));
  };

  const renderOmni = () => {
    const q = urlEl.value.trim();
    omniSel = -1;
    omniRows = [];
    omniPop.innerHTML = '';
    if (!q) { setOmni(false); return; }
    const isUrl = q.indexOf(' ') === -1 &&
                  (q.indexOf('.') !== -1 || q.indexOf('://') !== -1);
    const rows = [{label: (isUrl ? 'Go to ' : 'Search DuckDuckGo for ') + q,
                   input: q}];
    const ql = q.toLowerCase();
    for (const d of dialEntries) {
      if (d.name.toLowerCase().indexOf(ql) !== -1 ||
          d.url.toLowerCase().indexOf(ql) !== -1) {
        rows.push({label: d.name + ' — ' + d.url, input: d.url});
      }
    }
    for (const r of rows.slice(0, 8)) {
      const el = document.createElement('div');
      el.className = 'mi omni-row';
      el.textContent = r.label;
      // mousedown beats blur — preventDefault keeps the input focused so
      // the click lands instead of the popup closing under it.
      el.onmousedown = (e) => e.preventDefault();
      el.onclick = (e) => { e.stopPropagation(); go(r.input, e); };
      omniPop.appendChild(el);
      omniRows.push(r);
    }
    const rect = urlEl.getBoundingClientRect();
    omniPop.style.left = rect.left + 'px';
    omniPop.style.width = rect.width + 'px';
    setOmni(true);
  };

  urlEl.addEventListener('input', renderOmni);
  urlEl.addEventListener('focus', renderOmni);
  urlEl.addEventListener('blur', () => setOmni(false));

  urlEl.addEventListener('keydown', (e) => {
    if ((e.key === 'ArrowDown' || e.key === 'ArrowUp') && omniOpen) {
      e.preventDefault();
      const n = omniRows.length;
      omniSel = e.key === 'ArrowDown' ? (omniSel + 1) % n
                                    : (omniSel - 1 + n) % n;
      paintSel();
      return;
    }
    if (e.key === 'Enter') {
      go(omniSel >= 0 ? omniRows[omniSel].input : urlEl.value, e);
    } else if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === 'l') {
      // Edge's paste-and-go.
      navigator.clipboard.readText().then((t) => {
        if (t) { ipc({cmd: 'navigate', input: t}); urlEl.blur(); }
      }).catch(() => {});
      e.preventDefault();
    }
  });

  // Dial bar: the same pinned list the new-tab tiles render. A click
  // navigates the active tab — favorites-bar behaviour; Ctrl or middle
  // click opens a new tab and Ctrl+Shift a background one, expressed
  // through the navigate command's modifier fields (policy stays in app).
  fetch('/dials').then(r => r.json()).then(dials => {
    dialEntries = dials;
    const bar = document.getElementById('dial');
    for (const d of dials) {
      const el = document.createElement('button');
      el.className = 'dial';
      el.title = d.url + '\nClick: open here · Ctrl/middle-click: new tab';
      const dot = document.createElement('span');
      dot.className = 'dot';
      dot.textContent = d.name.charAt(0);
      const label = document.createElement('span');
      label.textContent = d.name;
      el.append(dot, label);
      el.onclick = (e) => ipc({cmd: 'navigate', input: d.url,
                               alt: e.ctrlKey, shift: e.shiftKey});
      el.onauxclick = (e) => {
        if (e.button === 1) ipc({cmd: 'navigate', input: d.url, alt: true});
      };
      bar.appendChild(el);
    }
  }).catch(() => {});

  // Browser shortcuts are not handled here — the engine forwards every
  // accelerator to the app before the page sees it, for chrome and content
  // views alike.

  window.__cryo_update = (state) => {
    lastState = state;
    tabsEl.innerHTML = '';
    for (const t of state.tabs) {
      const el = document.createElement('div');
      el.className = 'tab' + (t.active ? ' active' : '') +
                     (t.state !== 'active' ? ' ' + t.state : '');
      el.title = t.state === 'frozen' ? 'Frozen — click to restore'
               : t.state === 'discarded' ? 'Discarded — click to reload' : '';
      // Middle-click anywhere on the tab closes it.
      el.addEventListener('auxclick', (e) => {
        if (e.button === 1) ipc({cmd: 'close', id: t.id});
      });
      // Live tabs get a freeze button in the favicon slot; a frozen tab
      // shows the same flake as a state marker (clicking the tab restores).
      if (t.state === 'active') {
        const fz = document.createElement('span');
        fz.className = 'fz';
        fz.textContent = '❄';
        fz.title = 'Freeze tab (Ctrl+Shift+F)';
        fz.onclick = () => ipc({cmd: 'freeze', id: t.id});
        el.appendChild(fz);
      } else if (t.state === 'frozen') {
        const mark = document.createElement('span');
        mark.className = 'fzmark';
        mark.textContent = '❄';
        el.appendChild(mark);
      }
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
    const menuBtn = document.getElementById('menu');
    menuBtn.classList.toggle('has-update', !!state.update_version);
    if (state.update_version) {
      updateItem.textContent = 'Restart to update — v' + state.update_version;
      menuPop.prepend(updateItem);
    } else {
      updateItem.remove();
    }
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
        assert_eq!(canonize_nav_input("example.com"), "https://example.com");
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
        let cmd: ChromeCommand =
            serde_json::from_str(r#"{"cmd":"navigate","input":"x","alt":true}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::Navigate { alt: true, .. }));
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"activate","id":3}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::Activate { id: 3 }));
        // Freeze/discard carry an optional id — absent means active tab.
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"freeze","id":3}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::Freeze { id: Some(3) }));
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"freeze"}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::Freeze { id: None }));
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"discard","id":2}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::Discard { id: Some(2) }));
        assert!(serde_json::from_str::<ChromeCommand>(r#"{"cmd":"bogus"}"#).is_err());
    }
}
