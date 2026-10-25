  const paintBookmarks = () => {
    const shell = node('div', 'bookmark-shell');
    const folders = node('aside', 'bookmark-folders');
    folders.appendChild(node('h3', '', 'Browse'));
    const main = node('section', 'bookmark-main');
    const top = node('div', 'bookmark-top');
    const titlebar = node('div', 'bookmark-titlebar');
    const heading = node('div');
    heading.append(node('h2', '', 'Bookmarks'), node('div', 'bookmark-subtitle',
      libData.bookmarks.length + (libData.bookmarks.length === 1 ? ' saved page' : ' saved pages')));
    const importBtn = button('Import', 'bookmark-import', () => openLib('import'));
    titlebar.append(heading, importBtn);

    const searchRow = node('div', 'bookmark-search-row');
    const searchWrap = node('div', 'bookmark-search-wrap');
    searchWrap.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="11" cy="11" r="7"/><path d="m20 20-4-4"/></svg>';
    const search = node('input', 'bookmark-search');
    search.type = 'search';
    search.spellcheck = false;
    search.placeholder = 'Search title, website, or folder';
    search.setAttribute('aria-label', 'Search bookmarks');
    search.value = libFilter;
    const clear = node('button', 'search-clear' + (libFilter ? ' show' : ''), '×');
    clear.title = 'Clear search';
    clear.onclick = () => {
      libFilter = ''; search.value = ''; clear.classList.remove('show'); paintRows(); search.focus();
    };
    searchWrap.append(search, clear);
    const sort = xdrop(
      [['newest', 'Newest'], ['az', 'A–Z'], ['site', 'Website']],
      bookmarkSort, (v) => { bookmarkSort = v; paintRows(); }, 'Sort bookmarks');
    searchRow.append(searchWrap, sort);
    top.append(titlebar, searchRow);
    const list = node('div', 'bookmark-results');
    main.append(top, list);
    shell.append(folders, main);
    libBody.appendChild(shell);

    const folderCounts = new Map();
    for (const b of libData.bookmarks) {
      const key = b.folder || '';
      folderCounts.set(key, (folderCounts.get(key) || 0) + 1);
    }
    if (bookmarkScope !== 'all' && !folderCounts.has(bookmarkScope)) bookmarkScope = 'all';
    const folderIcon = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M3.5 6.5h6l2 2h9v9.5a2 2 0 0 1-2 2h-15z"/></svg>';
    const scopeButton = (key, label, count, icon) => {
      const b = node('button', 'folder-btn' + (bookmarkScope === key ? ' on' : ''));
      b.title = label;
      b.innerHTML = icon || folderIcon;
      b.append(node('span', 'folder-name', label), node('span', 'folder-count', String(count)));
      b.onclick = () => { bookmarkScope = key; paintLib(); };
      folders.appendChild(b);
    };
    scopeButton('all', 'All bookmarks', libData.bookmarks.length,
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M6 3.5h12v17l-6-4-6 4z"/></svg>');
    if (folderCounts.has('')) scopeButton('', 'Bookmarks bar', folderCounts.get(''));
    [...folderCounts.entries()].filter(([name]) => name).sort((a, b) => a[0].localeCompare(b[0]))
      .forEach(([name, count]) => scopeButton(name, name, count));

    const paintRows = () => {
      list.innerHTML = '';
      const needle = libFilter.trim().toLowerCase();
      const rows = libData.bookmarks.filter((b) =>
        (bookmarkScope === 'all' || b.folder === bookmarkScope) && (!needle ||
        (b.title + ' ' + b.url + ' ' + b.folder).toLowerCase().includes(needle)));
      const host = (url) => {
        try { return new URL(url).hostname.replace(/^www\./, '') || url; } catch (_) { return url; }
      };
      rows.sort((a, b) => bookmarkSort === 'az' ? a.title.localeCompare(b.title)
        : bookmarkSort === 'site' ? host(a.url).localeCompare(host(b.url))
        : (b.added_at || 0) - (a.added_at || 0));
      if (!rows.length) {
        const empty = node('div', 'bookmark-empty');
        const copy = node('div');
        copy.append(node('div', 'bookmark-empty-mark', needle ? '⌕' : '☆'),
          node('h3', '', libData.bookmarks.length ? 'No bookmarks found' : 'Your library is ready'),
          node('p', '', libData.bookmarks.length
            ? 'Try another search or choose a different folder.'
            : 'Save any page with Ctrl+D, or import bookmarks from another browser.'));
        empty.appendChild(copy); list.appendChild(empty);
        return;
      }
      list.appendChild(node('div', 'bookmark-summary', rows.length +
        (rows.length === 1 ? ' result' : ' results')));
      for (const b of rows) {
        const r = node('div', 'bookmark-card');
        r.tabIndex = 0; r.title = b.url;
        const site = host(b.url);
        const mark = node('div', 'site-mark');
        const hue = [...site].reduce((n, c) => n + c.charCodeAt(0), 0) % 360;
        mark.style.background = 'hsl(' + hue + ' 28% 25%)';
        // The site's icon on the tile when one has arrived; the letter
        // stays for pages never visited (or visited before v2).
        if (b.favicon) {
          const img = node('img', 'site-fav');
          img.src = b.favicon;
          img.alt = '';
          img.draggable = false;
          mark.appendChild(img);
        } else {
          mark.textContent = site[0] || b.title[0] || '?';
        }
        const copy = node('div', 'bookmark-copy');
        const meta = node('div', 'bookmark-meta');
        meta.appendChild(node('span', 'bookmark-host', site));
        if (b.folder) meta.appendChild(node('span', 'bookmark-folder-pill', b.folder));
        copy.append(node('div', 'bookmark-name', b.title), meta);
        const actions = node('div', 'bookmark-actions');
        const newTab = node('button', 'icon-btn');
        newTab.title = 'Open in background tab';
        newTab.setAttribute('aria-label', 'Open ' + b.title + ' in background tab');
        newTab.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M14 4h6v6M20 4l-9 9"/><path d="M18 13v6a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h6"/></svg>';
        newTab.onclick = (e) => {
          e.stopPropagation(); ipc({cmd: 'bookmark_open', id: b.id, ctrl: true, shift: true});
        };
        const remove = node('button', 'icon-btn danger');
        remove.title = 'Remove bookmark'; remove.setAttribute('aria-label', 'Remove ' + b.title);
        remove.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M4 7h16M9 7V4h6v3m3 0-1 13H7L6 7m4 4v5m4-5v5"/></svg>';
        remove.onclick = (e) => {
          e.stopPropagation(); actions.innerHTML = '';
          const check = node('div', 'delete-check', 'Remove?');
          const no = node('button', 'no', 'Cancel');
          no.onclick = (ev) => { ev.stopPropagation(); paintRows(); };
          const yes = node('button', 'yes', 'Remove');
          yes.onclick = (ev) => { ev.stopPropagation(); ipc({cmd: 'bookmark_remove', id: b.id}); };
          check.append(no, yes); actions.appendChild(check);
        };
        actions.append(newTab, remove);
        r.append(mark, copy, actions);
        r.onclick = (e) => ipc({cmd: 'bookmark_open', id: b.id, ctrl: e.ctrlKey, shift: e.shiftKey});
        r.onkeydown = (e) => { if (e.key === 'Enter') r.click(); };
        list.appendChild(r);
      }
      if (libData.more_bookmarks) {
        list.appendChild(node('div', 'lnote',
          'Showing the first ' + libData.bookmarks.length + ' of ' +
          (libData.bookmarks.length + libData.more_bookmarks) + ' kept.'));
      }
    };
    search.oninput = () => {
      libFilter = search.value; clear.classList.toggle('show', !!libFilter); paintRows();
    };
    search.onkeydown = (e) => {
      if (e.key === 'Enter') list.querySelector('.bookmark-card')?.click();
      if (e.key === 'ArrowDown') { e.preventDefault(); list.querySelector('.bookmark-card')?.focus(); }
    };
    paintRows();
    setTimeout(() => search.focus(), 0);
  };
