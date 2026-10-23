//! Internal pages served over the engine's `cryo` protocol.
//!
//! Shell surfaces are real `http://cryo.localhost/...` documents, not
//! `NavigateToString` HTML: a `data:`-document runs no scripts and its IPC
//! messages never reach the host (wry drops unparseable Source URIs).

use crate::shell::{chrome, dial, error};

/// Origin that all internal pages share. `is_internal_url` keys off this —
/// keep the two in sync if the scheme registration ever changes.
pub(crate) const INTERNAL_ORIGIN: &str = "http://cryo.localhost";

pub const CHROME_URL: &str = "http://cryo.localhost/chrome";
pub const NEW_TAB_URL: &str = "http://cryo.localhost/newtab";
pub const SETTINGS_URL: &str = "http://cryo.localhost/settings";

/// A URL that belongs to the shell, not to any web destination.
pub fn is_internal_url(url: &str) -> bool {
    url.starts_with(INTERNAL_ORIGIN)
}

/// Engine-facing resolver for the `cryo` scheme. wry hands us the request
/// path (`/chrome`), we hand back static HTML.
pub fn internal_page(path: &str) -> Option<&'static str> {
    match path {
        "/chrome" => Some(chrome::CHROME_HTML),
        "/newtab" => Some(NEW_TAB_HTML),
        "/settings" => Some(SETTINGS_HTML),
        // The dial list both shell surfaces fetch at load.
        dial::PATH => Some(dial::JSON),
        // The error page reads its parameters from the query string, which
        // never reaches this resolver — one document covers every failure.
        error::ERROR_PATH => Some(error::ERROR_HTML),
        _ => None,
    }
}

/// Home page: what the window opens on, and what every new tab shows. The
/// `<title>` matters — it is what the tab strip shows.
///
/// The address box navigates the view itself rather than talking to the
/// shell. Content views deliberately have no IPC handler, because the
/// internal protocol answers any origin: wiring `/cmd` into a content view
/// would let any website drive the browser.
pub const NEW_TAB_HTML: &str = r##"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>New tab</title><style>
  :root { color-scheme: dark; }
  body {
    min-height: 100vh; margin: 0; display: flex; align-items: flex-start;
    justify-content: center; background: #16181c; color: #d7dae0;
    font: 15px/1.6 system-ui, "Segoe UI", sans-serif;
    -webkit-user-select: none; user-select: none;
  }
  main {
    width: min(520px, calc(100vw - 48px));
    /* Fixed drop from the top, not centreing: the dial tiles arrive over
       an async fetch, and a centred block would jump up the moment they
       land. ~15vh is where centreing put it anyway. */
    margin-top: 15vh; text-align: center;
  }
  svg { width: 72px; height: 72px; margin: 0 auto; display: block; }
  .brand {
    margin-top: 20px; font-size: 34px; font-weight: 600; letter-spacing: .06em;
  }
  .tag { margin-top: 6px; color: #8a8f98; font-size: 14px; }
  form { margin-top: 32px; }
  input {
    width: 100%; box-sizing: border-box; padding: 13px 16px;
    border: 1px solid #272b33; border-radius: 10px;
    background: #1d2026; color: #e6e9ee; font: inherit;
    -webkit-user-select: text; user-select: text;
  }
  input::placeholder { color: #70767f; }
  input:focus {
    outline: none; border-color: #2a7089;
    box-shadow: 0 0 0 3px rgba(42, 112, 137, .25);
  }
  #dial {
    margin-top: 40px; display: flex; flex-wrap: wrap; gap: 12px;
    justify-content: center;
  }
  .tile {
    width: 96px; padding: 12px 6px 10px; border-radius: 10px;
    background: #1d2026; border: 1px solid #272b33;
    color: #c9cdd4; text-decoration: none; text-align: center;
    font-size: 12px; overflow: hidden;
  }
  .tile:hover { border-color: #2a7089; background: #22262d; }
  .tile .dot {
    width: 34px; height: 34px; margin: 0 auto 6px; border-radius: 8px;
    display: flex; align-items: center; justify-content: center;
    background: #17455a; color: #a2deee; font-size: 16px; font-weight: 600;
  }
</style></head>
<body>
  <main>
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" aria-hidden="true"><defs><clipPath id="cryoMark"><path d="M76.18 69.55 L52.25 85.44 L19.82 66.72 L19.82 29.28 L52.25 10.56 L76.18 26.45 L69.46 32.5 L52.25 28.65 L37.54 39.51 L37.54 56.49 L52.25 67.35 L69.46 63.5Z"/></clipPath></defs><path d="M76.18 69.55 L52.25 85.44 L19.82 66.72 L19.82 29.28 L52.25 10.56 L76.18 26.45 L69.46 32.5 L52.25 28.65 L37.54 39.51 L37.54 56.49 L52.25 67.35 L69.46 63.5Z" fill="#17455A"/><path d="M76.34 69.5 L52.15 85.58 L52.11 67.24Z" fill="#3B8EA8"/><path d="M76.35 69.59 L52.07 67.36 L69.58 63.38Z" fill="#3B8EA8"/><path d="M52.37 85.56 L19.65 66.69 L37.56 56.32Z" fill="#3B8EA8"/><path d="M52.3 85.6 L37.44 56.35 L52.4 67.28Z" fill="#A2DEEE"/><path d="M19.78 66.89 L19.76 29.12 L37.7 39.44Z" fill="#EAF9FD"/><path d="M19.7 66.85 L37.61 39.35 L37.71 56.55Z" fill="#A2DEEE"/><path d="M19.66 29.33 L52.36 10.43 L52.4 28.73Z" fill="#EAF9FD"/><path d="M19.65 29.25 L52.41 28.6 L37.57 39.68Z" fill="#A2DEEE"/><path d="M52.12 10.44 L76.34 26.5 L69.52 32.66Z" fill="#EAF9FD"/><path d="M52.18 10.4 L69.6 32.61 L52.11 28.76Z" fill="#3B8EA8"/><path d="M19.82 29.28 L52.25 10.56 L76.18 26.45" fill="none" stroke="#EAF9FD" stroke-width="2.44" clip-path="url(#cryoMark)"/></svg>
    <div class="brand">cryo</div>
    <div class="tag">Tabs freeze. Budgets hold.</div>
    <form id="go" autocomplete="off">
      <input id="q" type="text" spellcheck="false" autofocus
             placeholder="Search DuckDuckGo or type a URL"
             aria-label="Search or type a URL">
    </form>
    <div id="dial"></div>
  </main>
<script>
  // Mirrors `normalize_nav_input` in shell::chrome so this box and the
  // address bar read the same text the same way. Keep the two in step.
  function destination(raw) {
    var s = raw.trim();
    if (!s) return null;
    // `javascript://x%0Aalert(1)` passes a naive scheme test, and this page
    // runs on cryo's own origin.
    if (/^javascript:/i.test(s)) return null;
    if (s.indexOf("://") !== -1 || /^(about|data|file):/i.test(s)) return s;
    if (s.indexOf(" ") === -1 && s.indexOf(".") !== -1) return "https://" + s;
    return "https://duckduckgo.com/?q=" + encodeURIComponent(s);
  }

  var box = document.getElementById("q");
  document.getElementById("go").addEventListener("submit", function (e) {
    e.preventDefault();
    var url = destination(box.value);
    if (url) location.href = url;
  });
  box.focus();

  // Speed dial — the same list the chrome's dial bar reads. Plain links:
  // this page is a content view, so it deliberately has no command
  // channel, and a normal navigation is all a tile needs.
  fetch("/dials").then(function (r) { return r.json(); }).then(function (dials) {
    var wrap = document.getElementById("dial");
    dials.forEach(function (d) {
      var a = document.createElement("a");
      a.className = "tile";
      a.href = d.url;
      var dot = document.createElement("span");
      dot.className = "dot";
      dot.textContent = d.name.charAt(0);
      var label = document.createElement("div");
      label.textContent = d.name;
      a.append(dot, label);
      wrap.appendChild(a);
    });
  }).catch(function () {});
</script>
</body>
</html>
"##;

/// Settings: a read-only reference page — shortcuts, profile location,
/// and what the engine cannot do. Actions that mutate state live in the
/// chrome menu because internal pages in content tabs have no IPC (any
/// website could drive the browser otherwise — see `NEW_TAB_HTML`).
pub const SETTINGS_HTML: &str = r##"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Settings</title><style>
  :root { color-scheme: dark; }
  body {
    max-width: 640px; margin: 0 auto; padding: 40px 24px;
    background: #16181c; color: #d7dae0;
    font: 14px/1.6 system-ui, "Segoe UI", sans-serif;
  }
  h1 { font-size: 22px; font-weight: 600; margin-bottom: 4px; }
  h2 { font-size: 13px; text-transform: uppercase; letter-spacing: .08em;
       color: #8a8f98; margin: 28px 0 8px; }
  table { width: 100%; border-collapse: collapse; }
  td { padding: 6px 0; border-bottom: 1px solid #23262c; vertical-align: top; }
  td.k { width: 220px; color: #9ec2f5; white-space: nowrap;
         font-family: ui-monospace, Consolas, monospace; font-size: 13px; }
  .note { color: #8a8f98; }
</style></head>
<body>
  <h1>Settings</h1>
  <div class="note">cryo — a memory-obsessed browser shell.</div>

  <h2>Profile &amp; sign-in</h2>
  <table>
    <tr><td class="k">Google sign-in</td><td>Use the Sign in button in the toolbar. Cookies live in the shared profile, so every site sees the session.</td></tr>
    <tr><td class="k">Profile folder</td><td><code>cryo.exe.WebView2</code> next to the executable. Delete it (or Ctrl+Shift+Del) for a clean slate.</td></tr>
  </table>

  <h2>Permissions</h2>
  <table>
    <tr><td class="k">Microphone / camera</td><td>Granted automatically — WebView2 has no prompt UI. Everything else is denied.</td></tr>
    <tr><td class="k">Extensions</td><td>Not supported — WebView2 cannot run Chrome extensions.</td></tr>
  </table>

  <h2>Shortcuts</h2>
  <table>
    <tr><td class="k">Ctrl+T / Ctrl+W</td><td>New / close tab</td></tr>
    <tr><td class="k">Ctrl+Shift+T</td><td>Reopen closed tab</td></tr>
    <tr><td class="k">Ctrl+Tab / Ctrl+Shift+Tab</td><td>Next / previous tab</td></tr>
    <tr><td class="k">Ctrl+1..9</td><td>Jump to tab</td></tr>
    <tr><td class="k">Ctrl+L, F6</td><td>Address bar; F6 toggles bar ↔ page</td></tr>
    <tr><td class="k">F5 / Ctrl+Shift+R</td><td>Reload / hard reload</td></tr>
    <tr><td class="k">Alt+Left / Alt+Right</td><td>Back / forward</td></tr>
    <tr><td class="k">Ctrl+M</td><td>Mute tab</td></tr>
    <tr><td class="k">Ctrl+Shift+F</td><td>Freeze tab — drops the surface, keeps a snapshot; click the tab to restore. ⋮ menu also has Discard.</td></tr>
    <tr><td class="k">F11 / F12</td><td>Fullscreen / DevTools</td></tr>
    <tr><td class="k">Ctrl+Shift+N</td><td>Incognito window</td></tr>
    <tr><td class="k">Ctrl+Shift+Del</td><td>Clear browsing data</td></tr>
    <tr><td class="k">Ctrl+Shift+Q</td><td>Quit</td></tr>
  </table>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_pages_only() {
        assert!(internal_page("/chrome").is_some());
        assert!(internal_page("/newtab").is_some());
        assert!(internal_page("/settings").is_some());
        assert!(internal_page(dial::PATH).is_some());
        assert!(internal_page("/etc/passwd").is_none());
    }

    #[test]
    fn internal_urls_match_protocol_origin() {
        assert!(is_internal_url(NEW_TAB_URL));
        assert!(is_internal_url(CHROME_URL));
        assert!(!is_internal_url("https://example.com"));
        assert!(!is_internal_url("http://cryo.evil.com"));
    }
}
