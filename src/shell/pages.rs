//! Internal pages served over the engine's `cryo` protocol.
//!
//! Shell surfaces are real `http://cryo.localhost/...` documents, not
//! `NavigateToString` HTML: a `data:`-document runs no scripts and its IPC
//! messages never reach the host (wry drops unparseable Source URIs).

use crate::shell::{chrome, dial, error, search};

/// Origin that all internal pages share. `is_internal_url` keys off this —
/// keep the two in sync if the scheme registration ever changes.
pub(crate) const INTERNAL_ORIGIN: &str = "http://cryo.localhost";

pub const CHROME_URL: &str = "http://cryo.localhost/chrome";
pub const NEW_TAB_URL: &str = "http://cryo.localhost/newtab";
pub const SETTINGS_URL: &str = "http://cryo.localhost/settings";

pub(crate) fn download_folder_request(url: &str) -> Option<bool> {
    match url.strip_prefix(SETTINGS_URL)? {
        "/download-folder" => Some(false),
        "/download-folder/choose" => Some(true),
        _ => None,
    }
}

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
        // The engine list the settings page offers and the new-tab page
        // resolves its search box through.
        search::PATH => Some(search::json()),
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
  :root {
    color-scheme: dark;
    --bg1: #11141a; --bg2: #181d25; --line: #262d38;
    --tx: #e9edf2; --tx2: #98a1ae; --tx3: #67707d;
    --ice: #8fdcf0; --ice-deep: #2b7186; --ice-dim: #17455a;
  }
  body {
    min-height: 100vh; margin: 0; display: flex; align-items: flex-start;
    justify-content: center; color: var(--tx);
    /* A faint ice glow at the top gives the flat dark a horizon. */
    background:
      radial-gradient(900px 320px at 50% -80px, rgba(59, 142, 168, .14), transparent 70%),
      var(--bg1);
    font: 15px/1.6 system-ui, "Segoe UI", sans-serif;
    -webkit-font-smoothing: antialiased;
    -webkit-user-select: none; user-select: none;
  }
  main {
    width: min(520px, calc(100vw - 48px));
    /* Fixed drop from the top, not centreing: the dial tiles arrive over
       an async fetch, and a centred block would jump up the moment they
       land. ~15vh is where centreing put it anyway. */
    margin-top: 15vh; text-align: center;
  }
  svg { width: 72px; height: 72px; margin: 0 auto; display: block;
        filter: drop-shadow(0 6px 18px rgba(143, 220, 240, .18)); }
  .brand {
    margin-top: 20px; font-size: 34px; font-weight: 650; letter-spacing: .08em;
  }
  .tag { margin-top: 6px; color: var(--tx3); font-size: 14px; }
  form { margin-top: 32px; }
  input {
    width: 100%; box-sizing: border-box; padding: 14px 20px;
    border: 1px solid var(--line); border-radius: 999px;
    background: var(--bg2); color: var(--tx); font: inherit;
    transition: border-color .15s ease, box-shadow .15s ease;
    -webkit-user-select: text; user-select: text;
  }
  input::placeholder { color: var(--tx3); }
  input:focus {
    outline: none; border-color: var(--ice-deep);
    box-shadow: 0 0 0 4px rgba(143, 220, 240, .13), 0 8px 30px rgba(0,0,0,.35);
  }
  #dial {
    margin-top: 40px; display: flex; flex-wrap: wrap; gap: 12px;
    justify-content: center;
  }
  .tile {
    width: 96px; padding: 14px 6px 11px; border-radius: 12px;
    background: var(--bg2); border: 1px solid var(--line);
    color: var(--tx2); text-decoration: none; text-align: center;
    font-size: 12px; overflow: hidden;
    transition: border-color .15s ease, background .15s ease, transform .15s ease;
  }
  .tile:hover {
    border-color: var(--ice-deep); background: #1d232d; color: var(--tx);
    transform: translateY(-1px);
  }
  .tile .dot {
    width: 34px; height: 34px; margin: 0 auto 7px; border-radius: 9px;
    display: flex; align-items: center; justify-content: center;
    background: var(--ice-dim); color: var(--ice); font-size: 16px; font-weight: 600;
    box-shadow: inset 0 1px 0 rgba(255,255,255,.08);
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
    // A pasted Windows path becomes a file URL, like the Rust side's
    // local_path_url — otherwise the drive letter reads as a host.
    var asPath = s.replace(/\\/g, "/");
    if (/^[a-zA-Z]:[\\/]/.test(s)) return "file:///" + encodeURI(asPath);
    if (s.indexOf("\\\\") === 0) return "file://" + encodeURI(asPath.slice(2));
    if (s.indexOf(" ") === -1 && s.indexOf(".") !== -1) return "https://" + s;
    var e = engine();
    return e.template.replace("%s", encodeURIComponent(s));
  }

  // The search-engine pick is a slug or a custom URL template in the
  // shell origin's localStorage ("cryo.searchEngine", written by
  // /settings); the slug→engine map arrives over fetch("/engines").
  // Until it lands — or if it never does — the default engine answers.
  var engines = null;
  function engine() {
    var pick = localStorage.getItem("cryo.searchEngine") || "duckduckgo";
    var e = engines && engines[pick];
    if (e) return {name: e.name, template: e.prefix + "%s"};
    if (pick.indexOf("://") !== -1 && pick.indexOf("%s") !== -1) {
      return {name: pick.split("://")[1].split("/")[0] || "custom",
              template: pick};
    }
    return {name: "DuckDuckGo", template: "https://duckduckgo.com/?q=%s"};
  }

  var box = document.getElementById("q");
  document.getElementById("go").addEventListener("submit", function (e) {
    e.preventDefault();
    var url = destination(box.value);
    if (url) location.href = url;
  });
  box.focus();

  fetch("/engines").then(function (r) { return r.json(); }).then(function (list) {
    engines = {};
    list.forEach(function (e) { engines[e.slug] = e; });
    box.placeholder = "Search " + engine().name + " or type a URL";
  }).catch(function () {});

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

/// Settings: search preferences, download folder, and browser reference.
/// Search uses the shell origin's localStorage (see `search.rs`). Folder
/// requests use intercepted navigations, accepted only from a settings tab;
/// content views still have no general-purpose command channel.
pub const SETTINGS_HTML: &str = r##"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Settings</title><style>
  :root {
    color-scheme: dark;
    --bg0: #0b0e12; --bg1: #11141a; --bg2: #181d25; --bg3: #202631;
    --line: #262d38; --tx: #e9edf2; --tx2: #98a1ae; --tx3: #67707d;
    --ice: #8fdcf0; --ice-deep: #2b7186;
  }
  body {
    max-width: 660px; margin: 0 auto; padding: 48px 24px;
    background: var(--bg1); color: var(--tx);
    font: 14px/1.6 system-ui, "Segoe UI", sans-serif;
    -webkit-font-smoothing: antialiased;
  }
  h1 { font-size: 24px; font-weight: 650; margin-bottom: 4px; letter-spacing: -.01em; }
  h2 { font-size: 12px; font-weight: 700; text-transform: uppercase;
       letter-spacing: .1em; color: var(--tx3); margin: 32px 0 10px; }
  table {
    width: 100%; border-collapse: collapse;
    background: var(--bg2); border: 1px solid var(--line);
    border-radius: 14px; overflow: hidden;
  }
  td { padding: 12px 14px; border-bottom: 1px solid rgba(255,255,255,.05); vertical-align: top; }
  tr:last-child td { border-bottom: 0; }
  td.k { width: 220px; color: var(--ice); white-space: nowrap;
         font-family: ui-monospace, "Cascadia Mono", Consolas, monospace; font-size: 12px; }
  input[type="text"] {
    background: var(--bg0); color: var(--tx); border: 1px solid var(--line);
    border-radius: 9px; padding: 8px 11px; font: inherit;
    transition: border-color .12s ease, box-shadow .12s ease;
  }
  input[type="text"] {
    margin-top: 10px; width: 100%; box-sizing: border-box;
    font-family: ui-monospace, "Cascadia Mono", Consolas, monospace; font-size: 13px;
    -webkit-user-select: text; user-select: text;
  }
  input[type="text"]:focus {
    outline: none; border-color: var(--ice-deep);
    box-shadow: 0 0 0 3px rgba(143, 220, 240, .14);
  }
  input[type="text"]::placeholder { color: var(--tx3); }
  .note { color: var(--tx2); margin-top: 8px; }
  code { color: var(--ice); }
  /* Same custom-dropdown pattern the chrome library uses — a native
     <select> popup can't carry this theme. */
  .xdrop { position: relative; display: inline-block; }
  .xdrop-btn {
    display: flex; align-items: center; gap: 10px; min-width: 190px;
    padding: 8px 12px; border: 1px solid var(--line); border-radius: 10px;
    background: var(--bg0); color: var(--tx); font: inherit; cursor: pointer;
    transition: border-color .12s ease, box-shadow .12s ease;
  }
  .xdrop-btn:hover { border-color: #3a4250; }
  #download-change:focus-visible { outline: 2px solid var(--ice); outline-offset: 3px; }
  #download-change:disabled { opacity: .5; cursor: default; }
  .xdrop.open .xdrop-btn {
    border-color: var(--ice-deep);
    box-shadow: 0 0 0 3px rgba(143, 220, 240, .14);
  }
  .xdrop-btn .chev { margin-left: auto; color: var(--tx3); font-size: 10px;
                     transition: transform .15s ease; }
  .xdrop.open .chev { transform: rotate(180deg); }
  .xdrop-pop {
    display: none; position: absolute; top: calc(100% + 6px); left: 0; z-index: 5;
    min-width: 100%; padding: 4px; list-style: none;
    background: rgba(24, 29, 37, .97); border: 1px solid rgba(255,255,255,.09);
    border-radius: 10px; box-shadow: 0 16px 44px rgba(0,0,0,.55);
  }
  .xdrop.open .xdrop-pop { display: block; }
  .xdrop-item {
    display: block; width: 100%; padding: 8px 10px; border: 0; border-radius: 7px;
    background: transparent; color: var(--tx2); font: inherit; text-align: left;
    cursor: pointer; white-space: nowrap;
  }
  .xdrop-item:hover { background: rgba(255,255,255,.06); color: var(--tx); }
  .xdrop-item.on { color: var(--ice); }
  .xdrop-item.on::after { content: ' ✓'; }
</style></head>
<body>
  <h1>Settings</h1>
  <div class="note">cryo — a memory-obsessed browser shell.</div>

  <h2>Search</h2>
  <table>
    <tr><td class="k">Search engine</td><td>
      <div class="xdrop" id="engine-drop">
        <button class="xdrop-btn" id="engine" type="button"
                aria-haspopup="listbox" aria-label="Default search engine">
          <span id="engine-label">DuckDuckGo</span><span class="chev">▾</span>
        </button>
        <div class="xdrop-pop" id="engine-pop" role="listbox"></div>
      </div>
      <input id="engine-tpl" type="text" spellcheck="false" hidden
             placeholder="https://example.com/search?q=%s"
             aria-label="Custom search URL">
      <div class="note">Used by the address bar and the new-tab search box. Applies as soon as it is picked; for Custom, <code>%s</code> marks where the query goes.</div>
    </td></tr>
  </table>

  <h2>Downloads</h2>
  <table>
    <tr><td class="k">Download folder</td><td>
      <div id="download-path" style="overflow-wrap:anywhere">Loading…</div>
      <button class="xdrop-btn" id="download-change" type="button" aria-label="Change download folder" aria-describedby="download-path download-status" style="margin-top:10px" disabled>Change…</button>
      <div id="download-status" class="note" role="status" aria-live="polite"></div>
      <div class="note">New downloads are saved here. Your choice is remembered across restarts; downloads already in progress keep their original location.</div>
    </td></tr>
  </table>

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
    <tr><td class="k">Ctrl+H / Ctrl+Shift+O</td><td>History / library panel</td></tr>
    <tr><td class="k">Ctrl+Shift+N</td><td>Incognito window</td></tr>
    <tr><td class="k">Ctrl+Shift+Del</td><td>Clear browsing data</td></tr>
    <tr><td class="k">Ctrl+Shift+Q</td><td>Quit</td></tr>
  </table>
<script>
  // The pick is stored in this origin's localStorage — the same storage
  // chrome.html and the new-tab page read. It is either a preset slug
  // or a custom template: an http(s) URL holding %s where the encoded
  // query goes. `cryo.searchEngine.custom` remembers the last template
  // so toggling away and back doesn't lose it.
  var downloadChange = document.getElementById("download-change");
  window.__cryo_download_folder = function (path, error) {
    if (path) document.getElementById("download-path").textContent = path;
    document.getElementById("download-status").textContent = error || "";
    downloadChange.disabled = !path;
    if (!path) document.getElementById("download-path").textContent = "Folder unavailable";
  };
  downloadChange.onclick = function () {
    downloadChange.disabled = true;
    location.href = "/settings/download-folder/choose";
  };
  location.href = "/settings/download-folder";
  var KEY = "cryo.searchEngine";
  var CUSTOM_KEY = "cryo.searchEngine.custom";
  var drop = document.getElementById("engine-drop");
  var btn = document.getElementById("engine");
  var lbl = document.getElementById("engine-label");
  var pop = document.getElementById("engine-pop");
  var tpl = document.getElementById("engine-tpl");
  var value = "duckduckgo";
  function isTemplate(v) {
    v = v.trim();
    var m = /^https?:\/\/[^\s/]+/.exec(v);
    return !!m && v.indexOf("%s") !== -1 && !/\s/.test(v);
  }
  function showTpl() { tpl.hidden = value !== "custom"; }
  function paint() {
    [...pop.children].forEach(function (el) {
      el.classList.toggle("on", el.dataset.v === value);
    });
  }
  function pick(v, label, customTpl) {
    value = v;
    lbl.textContent = label;
    paint();
    showTpl();
    if (v !== "custom") {
      localStorage.setItem(KEY, v);
      return;
    }
    // Nothing is stored until the input holds a valid template — the
    // previous pick keeps working meanwhile.
    if (!tpl.value) tpl.value = customTpl || localStorage.getItem(CUSTOM_KEY) || "";
    if (isTemplate(tpl.value)) localStorage.setItem(KEY, tpl.value.trim());
    tpl.focus();
  }
  function addItem(v, label) {
    var item = document.createElement("button");
    item.type = "button";
    item.className = "xdrop-item";
    item.dataset.v = v;
    item.setAttribute("role", "option");
    item.textContent = label;
    item.onclick = function (e) { e.stopPropagation(); drop.classList.remove("open"); pick(v, label); };
    pop.appendChild(item);
  }
  btn.onclick = function (e) { e.stopPropagation(); drop.classList.toggle("open"); };
  drop.addEventListener("keydown", function (e) {
    if (e.key === "Escape") drop.classList.remove("open");
  });
  document.addEventListener("click", function () { drop.classList.remove("open"); });
  fetch("/engines").then(function (r) { return r.json(); }).then(function (list) {
    list.forEach(function (e) { addItem(e.slug, e.name); });
    addItem("custom", "Custom…");
    var stored = localStorage.getItem(KEY) || "duckduckgo";
    var hit = list.filter(function (e) { return e.slug === stored; })[0];
    if (hit) {
      pick(hit.slug, hit.name);
    } else {
      var t = isTemplate(stored) ? stored : (localStorage.getItem(CUSTOM_KEY) || "");
      if (t) tpl.value = t;
      pick("custom", "Custom…", t);
    }
  }).catch(function () {});
  tpl.addEventListener("input", function () {
    if (isTemplate(tpl.value)) {
      localStorage.setItem(KEY, tpl.value.trim());
      localStorage.setItem(CUSTOM_KEY, tpl.value.trim());
    }
  });
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_folder_requests_require_exact_settings_routes() {
        assert_eq!(
            download_folder_request(&format!("{SETTINGS_URL}/download-folder")),
            Some(false)
        );
        assert_eq!(
            download_folder_request(&format!("{SETTINGS_URL}/download-folder/choose")),
            Some(true)
        );
        for url in [
            "https://example.com/settings/download-folder/choose",
            "http://cryo.localhost.evil/settings/download-folder/choose",
            "http://cryo.localhost/settings/download-folder/choose?extra=1",
            SETTINGS_URL,
        ] {
            assert_eq!(download_folder_request(url), None);
        }
    }

    #[test]
    fn resolves_known_pages_only() {
        assert!(internal_page("/chrome").is_some());
        assert!(internal_page("/newtab").is_some());
        assert!(internal_page("/settings").is_some());
        assert!(internal_page(dial::PATH).is_some());
        assert!(internal_page(search::PATH).is_some());
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
