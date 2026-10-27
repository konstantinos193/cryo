
  // wry's window.ipc channel is dead on this backend: attaching
  // with_ipc_handler leaves the document's own scripts unexecuted.
  // Commands go over the internal protocol instead — POSTs to /cmd land
  // in the engine's page resolver and reach the app unchanged.
  const ipc = (obj) => fetch('/cmd', {method: 'POST', body: JSON.stringify(obj)});
  const tabsEl = document.getElementById('tabs');
  const urlEl = document.getElementById('url');
  // The default search engine pick lives in the shell origin's
  // localStorage (the settings page writes it): a preset slug or a
  // custom %s URL template. The app can't see the storage area, so
  // commands that consult it carry the current value — read fresh each
  // time, a change in /settings lands with the next keystroke.
  const enginePick = () => localStorage.getItem('cryo.searchEngine');
  ipc({cmd: 'ready', engine: enginePick()});

  const backBtn = document.getElementById('back');
  const fwdBtn = document.getElementById('fwd');
  const reloadBtn = document.getElementById('reload');
  const loadbar = document.getElementById('loadbar');
  document.getElementById('new-tab').onclick = () => ipc({cmd: 'new_tab'});
  backBtn.onclick = () => ipc({cmd: 'back'});
  fwdBtn.onclick = () => ipc({cmd: 'forward'});
  // One button, two jobs: while a page loads it's the stop control.
  reloadBtn.onclick = () =>
    ipc({cmd: reloadBtn.classList.contains('loading') ? 'stop' : 'reload'});
  const signinBtn = document.getElementById('signin');
  const acctPop = document.getElementById('acct-pop');
  const menuPop = document.getElementById('menu-pop');
  const omniPop = document.getElementById('omni-pop');
  const dlPop = document.getElementById('dl-pop');
  const passwordPop = document.getElementById('password-pop');
  const dlBtn = document.getElementById('dl');
  // Popups live in this document, but the view's bounds are only the
  // strip — a view can't paint outside them. While any overlay (menu,
  // omnibox, downloads) is open the app raises this view above the
  // content and stretches it over the whole window; the aggregate state
  // is reported so the bounds stay in sync. Clicks anywhere outside a
  // popup land on this document and dismiss.
  let menuOpen = false, omniOpen = false, dlOpen = false, libOpen = false,
      acctOpen = false, passwordOpen = false;
  const reportOverlay = () =>
    ipc({cmd: 'overlay', open: menuOpen || omniOpen || dlOpen || libOpen || acctOpen || passwordOpen || permissionOpen || tabfindOpen || installOpen});
  // Signed in, the avatar opens a dropdown of every account in the jar;
  // signed out, the same spot is still the sign-in pill.
  const setAcct = (open) => {
    if (open === acctOpen) return;
    acctOpen = open;
    acctPop.classList.toggle('open', open);
    if (open) {
      acctPop.style.right =
        (document.documentElement.clientWidth - signinBtn.getBoundingClientRect().right) + 'px';
      paintAcct();
    }
    reportOverlay();
  };
  const paintAcct = () => {
    acctPop.innerHTML = '';
    (lastState.accounts || []).forEach((a, i) => {
      const row = document.createElement('div');
      row.className = 'acct-row';
      const face = document.createElement('span');
      face.className = 'face';
      if (a.avatar) {
        const img = document.createElement('img');
        img.src = a.avatar;
        img.alt = '';
        face.appendChild(img);
      } else {
        face.textContent = Array.from(a.name.trim())[0]?.toUpperCase() || '?';
      }
      const copy = document.createElement('div');
      const nm = document.createElement('div');
      nm.className = 'nm';
      nm.textContent = a.name;
      const em = document.createElement('div');
      em.className = 'em';
      em.textContent = a.email;
      copy.append(nm, em);
      row.append(face, copy);
      if (i === (lastState.active_account || 0)) {
        const tick = document.createElement('span');
        tick.textContent = '✓';
        tick.style.cssText = 'margin-left:auto;color:var(--tx3);font-size:12px';
        row.appendChild(tick);
      }
      row.onclick = () => { setAcct(false); ipc({cmd: 'account_open', index: i}); };
      acctPop.appendChild(row);
    });
    const s = document.createElement('div');
    s.className = 'sep';
    acctPop.appendChild(s);
    const add = document.createElement('div');
    add.className = 'acct-add';
    add.textContent = 'Add another account';
    add.onclick = () => { setAcct(false); ipc({cmd: 'sign_in'}); };
    acctPop.appendChild(add);
  };
  signinBtn.onclick = (e) => {
    e.stopPropagation();
    if ((lastState.accounts || []).length) {
      setMenu(false); setOmni(false); setDl(false);
      setAcct(!acctOpen);
    } else {
      ipc({cmd: 'sign_in'});
    }
  };
  acctPop.onclick = (e) => e.stopPropagation();
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
  const setDl = (open) => {
    if (open === dlOpen) return;
    dlOpen = open;
    dlPop.classList.toggle('open', open);
    reportOverlay();
  };
  // The library panel: bookmarks, saved passwords, import and export.
  // It is a chrome overlay like the others — the app stretches this view
  // over the window while it is up — but it fills the window rather
  // than hanging off a button, and it asks the app for the pane's data
  // every time it opens so it never shows a stale list.
  const setLib = (open) => {
    if (open === libOpen) return;
    libOpen = open;
    libEl.classList.toggle('open', open);
    // A revealed password lives as long as the panel does.
    if (!open) revealed.clear();
    reportOverlay();
  };
  const openLib = (pane) => {
    setMenu(false); setOmni(false); setDl(false);
    libPane = pane;
    // Start the pane empty rather than showing the last one's rows for
    // the frame before the app's push lands.
    libData = {pane, bookmarks: [], passwords: [], history: [], sources: [],
               extensions: [], more_bookmarks: 0, more_passwords: 0,
               more_history: 0, locked: null, sources_ready: false,
               // Carried over, not reset: both answer "what can this window
               // do", which does not change between panes.
               extensions_supported: libData.extensions_supported,
               incognito: libData.incognito};
    libFilter = '';
    setLib(true);
    paintLib();
    ipc({cmd: 'library', pane});
  };
  // Every path that takes the screen away from an overlay shuts all of
  // them: Escape, a click on the document, a tab switch, the app replacing
  // the view. One list, so a new overlay joins it in one place.
  const dismissOverlays = () => {
    setMenu(false); setOmni(false); setDl(false); setLib(false); setAcct(false);
    setTabfind(false);
  };
  // Ctrl+Shift+O lands here.
  window.__cryo_library_toggle = (pane) => {
    if (libOpen && (!pane || pane === libPane)) { setLib(false); return; }
    openLib(pane || 'bookmarks');
  };
  // The app calls this when it replaces what's on screen (tab switch,
  // navigation, fullscreen) — the document can't see those actions.
  window.__cryo_overlay = (open) => {
    if (!open) {
      denyPermission();
      dismissOverlays();
      passwordOpen = false; passwordPop.classList.remove('open');
    }
  };

  const keyIcon = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="15" r="4"/><path d="m11 12 8-8m-3 3 2 2m-5 1 2 2"/></svg>';
  const closePassword = () => ipc({cmd: 'password_prompt_close'});
  window.__cryo_password_prompt = (data) => {
    passwordOpen = !!data;
    passwordPop.classList.toggle('open', passwordOpen);
    passwordPop.innerHTML = '';
    if (!data) return;
    dismissOverlays();
    const head = document.createElement('div');
    head.className = 'pw-head';
    const mark = document.createElement('span');
    mark.className = 'pw-mark'; mark.innerHTML = keyIcon;
    const copy = document.createElement('div'); copy.className = 'pw-copy';
    const title = document.createElement('div'); title.className = 'pw-title';
    title.textContent = data.mode === 'fill' ? 'Choose a saved login'
      : data.mode === 'update' ? 'Update saved password?' : 'Save this password?';
    const site = document.createElement('div'); site.className = 'pw-site';
    try { site.textContent = new URL(data.site).host; } catch (_) { site.textContent = data.site; }
    copy.append(title, site);
    const close = document.createElement('button'); close.className = 'pw-close';
    close.textContent = '×'; close.title = 'Not now'; close.onclick = closePassword;
    head.append(mark, copy, close); passwordPop.appendChild(head);
    if (data.mode === 'fill') {
      const list = document.createElement('div'); list.className = 'pw-accounts';
      for (const account of data.accounts || []) {
        const row = document.createElement('button'); row.className = 'pw-account';
        const dot = document.createElement('span'); dot.className = 'pw-userdot';
        dot.textContent = Array.from(account.username || '?')[0]?.toUpperCase() || '?';
        const words = document.createElement('span'); words.className = 'pw-account-copy';
        const name = document.createElement('div'); name.className = 'pw-account-name';
        name.textContent = account.username || 'Password only';
        const meta = document.createElement('div'); meta.className = 'pw-account-meta';
        meta.textContent = 'Fill password'; words.append(name, meta);
        const arrow = document.createElement('span'); arrow.className = 'pw-arrow'; arrow.textContent = '›';
        row.append(dot, words, arrow);
        row.onclick = () => ipc({cmd: 'password_fill', username: account.username});
        list.appendChild(row);
      }
      passwordPop.appendChild(list);
    } else {
      const summary = document.createElement('div'); summary.className = 'pw-summary';
      summary.append('For ');
      const who = document.createElement('strong'); who.textContent = data.username || 'this account';
      summary.append(who, data.mode === 'update'
        ? ', replace the password already kept in your sealed vault.'
        : ', keep this login in your sealed vault.');
      const actions = document.createElement('div'); actions.className = 'pw-actions';
      const later = document.createElement('button'); later.className = 'pw-later';
      later.textContent = 'Not now'; later.onclick = closePassword;
      const save = document.createElement('button'); save.className = 'pw-primary';
      save.textContent = data.mode === 'update' ? 'Update' : 'Save';
      save.onclick = () => ipc({cmd: 'password_save'});
      actions.append(later, save); passwordPop.append(summary, actions);
    }
  };
  passwordPop.onclick = (event) => event.stopPropagation();
  // Ctrl+J lands here — the engine's own downloads page is kept shut by
  // stealing the chord in key_command; this shelf is the replacement.
  window.__cryo_downloads = () => {
    setMenu(false); setOmni(false);
    setDl(!dlOpen);
  };
  // Last pushed state — menu guards (freeze/discard need a second tab to
  // hand focus to) read it at click time.
  let lastState = {tabs: []};
  const hasNeighbour = () => lastState.tabs.length > 1;
  const MENU = [
    {label: 'Freeze tab (Ctrl+Shift+F)', cmd: {cmd: 'freeze'}, ok: hasNeighbour},
    {label: 'Discard tab', cmd: {cmd: 'discard'}, ok: hasNeighbour},
    'sep',
    {label: 'Bookmark this page (Ctrl+D)', cmd: {cmd: 'bookmark_page'}},
    {label: 'Bookmarks (Ctrl+Shift+O)', lib: 'bookmarks'},
    {label: 'History (Ctrl+H)', lib: 'history'},
    {label: 'Saved passwords', lib: 'passwords'},
    {label: 'Import browser data…', lib: 'import'},
    {label: 'Export browser data…', lib: 'export'},
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
      // `lib` items open a library pane — they carry no `cmd` because
      // openLib posts the pane's `library` command itself.
      if (item.lib) {
        openLib(item.lib);
        return;
      }
      ipc(item.cmd);
    };
    menuPop.appendChild(el);
  }
  document.getElementById('menu').onclick = (e) => {
    e.stopPropagation();
    setDl(false); setOmni(false);
    setMenu(!menuOpen);
  };
  dlBtn.onclick = (e) => {
    e.stopPropagation();
    setMenu(false); setOmni(false);
    setDl(!dlOpen);
  };
  // Row buttons live inside the pop — clicks there must not bubble to
  // the document's dismiss-all. The menu needs it too: its `lib` items
  // open the library, and a click that reaches the document would shut
  // the panel in the same gesture that opened it.
  dlPop.onclick = (e) => e.stopPropagation();
  menuPop.onclick = (e) => e.stopPropagation();
  document.addEventListener('click', () => {
    if (permissionOpen) denyPermission();
    dismissOverlays();
    if (passwordOpen) closePassword();
  });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      if (permissionOpen) denyPermission();
      dismissOverlays();
      if (passwordOpen) closePassword();
    }
  });

  // The dial list feeds the bar below; omnibox suggestions are computed
  // app-side instead — history never reaches this document.
  let dialEntries = [];
