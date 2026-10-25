  // Omnibox dropdown: each keystroke asks the app, __cryo_suggest paints
  // the reply. Rows keep `input` — what gets sent back through navigate.
  let omniSel = -1;
  let omniRows = [];
  // What the user typed before arrows started browsing rows — cycling
  // back off the list (or Escape) restores it.
  let omniTyped = null;

  // Fallback glyphs for the verdict row — a magnifier for searches, a
  // globe for navigations. Sites without a remembered favicon get a
  // letter dot instead.
  const GLYPHS = {
    search: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"><circle cx="11" cy="11" r="7"/><path d="M16.3 16.3 21 21"/></svg>',
    go: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><circle cx="12" cy="12" r="9"/><path d="M3.5 12h17M12 3c2.6 2.8 4 5.6 4 9s-1.4 6.2-4 9c-2.6-2.8-4-5.6-4-9s1.4-6.2 4-9z"/></svg>',
    bookmark: '<svg viewBox="0 0 24 24" fill="currentColor"><path d="M12 3.6l2.5 5.2 5.7.8-4.1 4 1 5.7-5.1-2.8-5.1 2.8 1-5.7-4.1-4 5.7-.8z"/></svg>',
  };

  const go = (input, e) => {
    ipc({cmd: 'navigate', input,
         ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey,
         engine: enginePick()});
    urlEl.blur();
  };

  const paintSel = () => {
    [...omniPop.children].forEach((el, i) =>
      el.classList.toggle('sel', i === omniSel));
  };

  const renderOmni = () => {
    const q = urlEl.value.trim();
    omniSel = -1;
    omniTyped = null;
    if (!q) {
      omniRows = [];
      omniPop.innerHTML = '';
      setOmni(false);
      return;
    }
    // The old rows stay until the reply lands — clearing them here
    // would flash an empty popup on every keystroke.
    ipc({cmd: 'suggest', q, engine: enginePick()});
  };

  const omniIcon = (r) => {
    if (r.icon) {
      const img = document.createElement('img');
      img.className = 'ic'; img.src = r.icon; img.alt = '';
      return img;
    }
    const ic = document.createElement('span');
    if (GLYPHS[r.kind]) {
      ic.className = 'ic glyph';
      ic.innerHTML = GLYPHS[r.kind];
    } else {
      ic.className = 'ic dot';
      ic.textContent = (r.label[0] || '?').toUpperCase();
    }
    return ic;
  };

  window.__cryo_suggest = (reply) => {
    // Drop a stale reply — the user typed on or blurred meanwhile.
    if (document.activeElement !== urlEl || urlEl.value.trim() !== reply.q) {
      return;
    }
    omniSel = -1;
    omniRows = reply.rows.slice(0, 8);
    omniPop.innerHTML = '';
    for (const r of omniRows) {
      const el = document.createElement('div');
      el.className = 'mi omni-row';
      const lbl = document.createElement('span');
      lbl.className = 'lbl';
      lbl.textContent = r.label;
      el.append(omniIcon(r), lbl);
      // mousedown beats blur — preventDefault keeps the input focused so
      // the click lands instead of the popup closing under it.
      el.onmousedown = (e) => e.preventDefault();
      el.onclick = (e) => { e.stopPropagation(); go(r.input, e); };
      omniPop.appendChild(el);
    }
    if (!omniRows.length) { setOmni(false); return; }
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
      const n = omniRows.length;
      if (!n) return;
      e.preventDefault();
      if (omniSel === -1) omniTyped = urlEl.value;
      // Down cycles through the rows and off the end back to the typed
      // text; Up does the same in reverse — the box mirrors the row.
      if (e.key === 'ArrowDown') {
        omniSel = omniSel + 1 >= n ? -1 : omniSel + 1;
      } else {
        omniSel = omniSel === -1 ? n - 1 : omniSel - 1;
      }
      urlEl.value = omniSel === -1 ? omniTyped : omniRows[omniSel].input;
      urlEl.select();
      paintSel();
      return;
    }
    // Chrome's inline autocompletion: at the end of the input, → or End
    // accepts the top completable row — the typed text stays, the tail
    // arrives selected so typing overwrites it.
    if ((e.key === 'ArrowRight' || e.key === 'End') &&
        urlEl.selectionStart === urlEl.value.length &&
        urlEl.selectionEnd === urlEl.value.length) {
      const v = urlEl.value.toLowerCase();
      const row = omniRows.find((r) =>
        r.complete && r.complete.length > v.length &&
        r.complete.toLowerCase().startsWith(v));
      if (row) {
        e.preventDefault();
        urlEl.value = row.complete;
        urlEl.setSelectionRange(v.length, row.complete.length);
      }
      return;
    }
    if (e.key === 'Enter') {
      go(omniSel >= 0 ? omniRows[omniSel].input : urlEl.value, e);
    } else if (e.key === 'Escape' && omniTyped !== null) {
      // Arrow-browsing filled the box — give the typed text back before
      // the document handler tucks the popup.
      urlEl.value = omniTyped;
      omniTyped = null;
    } else if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === 'l') {
      // Edge's paste-and-go.
      navigator.clipboard.readText().then((t) => {
        if (t) {
          ipc({cmd: 'navigate', input: t, engine: enginePick()});
          urlEl.blur();
        }
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
    renderBar(lastState.bookmarks || []);
  }).catch(() => {});

  // One row, two sources: kept bookmarks once there are any, the pinned
  // dial list until then — a fresh profile still has somewhere to click.
  // Both speak the same two clicks: open here, or Ctrl/middle for a tab.
  const renderBar = (bookmarks) => {
    const bar = document.getElementById('dial');
    bar.innerHTML = '';
    const rows = bookmarks.length
      ? bookmarks.map((b) => ({name: b.title, url: b.url, id: b.id, icon: b.favicon}))
      : dialEntries.map((d) => ({name: d.name, url: d.url, id: null, icon: null}));
    for (const r of rows) {
      const el = document.createElement('button');
      el.className = 'dial';
      el.title = r.url + '\nClick: open here · Ctrl/middle-click: new tab' +
                 (r.id === null ? '' : ' · right-click: remove');
      // A kept page paints the site's icon; everything else keeps the
      // letter tile.
      let dot;
      if (r.icon) {
        dot = document.createElement('img');
        dot.src = r.icon;
        dot.alt = '';
        dot.draggable = false;
      } else {
        dot = document.createElement('span');
        dot.textContent = (r.name || r.url).charAt(0).toUpperCase();
      }
      dot.className = 'dot';
      const label = document.createElement('span');
      label.className = 'lbl';
      label.textContent = r.name || r.url;
      el.append(dot, label);
      // A bookmark goes through its own command so the app resolves the
      // id; a dial is just an address.
      el.onclick = (e) => r.id === null
        ? ipc({cmd: 'navigate', input: r.url, alt: e.ctrlKey, shift: e.shiftKey})
        : ipc({cmd: 'bookmark_open', id: r.id, ctrl: e.ctrlKey, shift: e.shiftKey});
      el.onauxclick = (e) => {
        if (e.button !== 1) return;
        if (r.id === null) ipc({cmd: 'navigate', input: r.url, alt: true});
        else ipc({cmd: 'bookmark_open', id: r.id, ctrl: true});
      };
      if (r.id !== null) {
        el.oncontextmenu = (e) => {
          e.preventDefault();
          ipc({cmd: 'bookmark_remove', id: r.id});
        };
      }
      bar.appendChild(el);
    }
    // The bar's own way into the panel, like every browser's "»".
    const all = document.createElement('button');
    all.className = 'dial all';
    all.title = 'Bookmarks, passwords, import and export (Ctrl+Shift+O)';
    all.textContent = bookmarks.length ? '»' : '★ »';
    all.onclick = () => openLib('bookmarks');
    bar.appendChild(all);
  };

  // Browser shortcuts are not handled here — the engine forwards every
  // accelerator to the app before the page sees it, for chrome and content
  // views alike.
