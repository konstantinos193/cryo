  // ===== The library panel: kept pages, saved passwords, import, export
  // Every list it shows is pushed by the app (`__cryo_library`); nothing
  // here reads storage, and a password only ever arrives one at a time
  // through `__cryo_secret`, after the app has asked the user.
  const libEl = document.getElementById('lib');
  const libBody = libEl.querySelector('.body');
  const libPanel = libEl.querySelector('.panel');
  const toast = document.getElementById('toast');
  let libPane = 'bookmarks';
  let libData = {pane: 'bookmarks', bookmarks: [], passwords: [], history: [],
                 sources: []};
  let libFilter = '';
  let bookmarkScope = 'all';
  let bookmarkSort = 'newest';
  // Secrets the user revealed in this sitting, keyed origin + username.
  // Cleared when the panel closes, so reopening hides them again.
  const revealed = new Map();
  const rowKey = (origin, username) => origin + '\n' + username;

  // Clicks inside the panel are the panel's; a click on the dimmed
  // backdrop lands on the document and dismisses, like the menu.
  libPanel.onclick = (e) => e.stopPropagation();
  for (const btn of libEl.querySelectorAll('.tab-btn[data-pane]')) {
    btn.onclick = () => openLib(btn.dataset.pane);
  }
  libEl.querySelector('.close').onclick = () => setLib(false);

  const node = (tag, cls, text) => {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    // Always textContent: every string here came from a web page's
    // title, a file someone else wrote, or a password.
    if (text !== undefined) n.textContent = text;
    return n;
  };

  const button = (label, cls, onclick) => {
    const b = node('button', 'lbtn' + (cls ? ' ' + cls : ''), label);
    b.onclick = onclick;
    return b;
  };

  // A real <select>'s popup renders with OS chrome and can't take the
  // theme — dropdowns are button + listbox. `options` is [[value, label]];
  // onpick gets the value. Escape or any panel click outside closes it.
  const XDROP_CHEVRON = '<svg viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M2.5 4.5 6 8l3.5-3.5"/></svg>';
  const xdrop = (options, current, onpick, label) => {
    const wrap = node('div', 'xdrop');
    const btn = node('button', 'xdrop-btn');
    btn.type = 'button';
    if (label) btn.setAttribute('aria-label', label);
    btn.setAttribute('aria-haspopup', 'listbox');
    const txt = node('span');
    btn.append(txt, node('span', 'xdrop-chev'));
    btn.lastChild.innerHTML = XDROP_CHEVRON;
    const pop = node('div', 'xdrop-pop');
    pop.setAttribute('role', 'listbox');
    let value = current;
    const paint = () => {
      txt.textContent = (options.find(([v]) => v === value) || [])[1] || value;
      [...pop.children].forEach((el, i) =>
        el.classList.toggle('on', options[i][0] === value));
    };
    const close = () => wrap.classList.remove('open');
    for (const [v, l] of options) {
      const item = node('button', 'xdrop-item', l);
      item.type = 'button';
      item.setAttribute('role', 'option');
      item.onclick = (e) => {
        e.stopPropagation();
        value = v; paint(); close(); onpick(v);
      };
      pop.appendChild(item);
    }
    btn.onclick = (e) => {
      e.stopPropagation();
      const was = wrap.classList.contains('open');
      libPanel.querySelectorAll('.xdrop.open').forEach((d) => d.classList.remove('open'));
      wrap.classList.toggle('open', !was);
    };
    wrap.onkeydown = (e) => { if (e.key === 'Escape') close(); };
    paint();
    wrap.append(btn, pop);
    return wrap;
  };
  // A panel click that lands outside an open dropdown tucks it — the
  // panel eats document clicks, so the dismissal lives here.
  libPanel.addEventListener('click', (e) => {
    libPanel.querySelectorAll('.xdrop.open').forEach((d) => {
      if (!d.contains(e.target)) d.classList.remove('open');
    });
  });

  const paintLib = () => {
    for (const btn of libEl.querySelectorAll('.tab-btn[data-pane]')) {
      btn.classList.toggle('on', btn.dataset.pane === libPane);
    }
    libBody.innerHTML = '';
    libBody.classList.toggle('bookmarks-body', libPane === 'bookmarks');
    if (libPane === 'bookmarks') paintBookmarks();
    else if (libPane === 'passwords') paintPasswords();
    else if (libPane === 'history') paintHistory();
    else if (libPane === 'import') paintImport();
    else paintExport();
  };

  window.__cryo_library = (data) => {
    libData = data;
    libPane = data.pane;
    paintLib();
  };

  // One password, for one click. The app has already asked the user.
  // A copy reply never touches the screen — it goes to the clipboard
  // and a toast says so; anything else is a reveal the pane shows.
  window.__cryo_secret = (reply) => {
    if (reply.copy) {
      navigator.clipboard.writeText(reply.secret).then(() =>
        window.__cryo_notice({ok: true, title: 'Password copied',
          lines: [reply.username
            ? reply.username + ' at ' + reply.origin : reply.origin]})
      ).catch(() => {});
      return;
    }
    revealed.set(rowKey(reply.origin, reply.username), reply.secret);
    if (libOpen) paintLib();
  };

  let toastTimer = null;
  window.__cryo_notice = (n) => {
    toast.innerHTML = '';
    toast.appendChild(node('div', 'tt', n.title));
    for (const line of n.lines || []) toast.appendChild(node('div', 'tl', line));
    toast.classList.toggle('bad', !n.ok);
    toast.classList.add('open');
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => toast.classList.remove('open'), 7000);
  };
  toast.onclick = () => toast.classList.remove('open');

  document.getElementById('star').onclick = () => ipc({cmd: 'bookmark_page'});
