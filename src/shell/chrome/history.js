  const paintHistory = () => {
    if (libData.incognito) {
      libBody.appendChild(node('div', 'lnote warn',
        'This is an incognito window: it keeps nothing — there is no history to show.'));
      return;
    }
    const entries = libData.history || [];
    const head = node('div', 'lrow');
    head.style.borderBottom = '0';
    const heading = node('div', 'main');
    heading.appendChild(node('div', 't',
      entries.length + (entries.length === 1 ? ' remembered address' : ' remembered addresses')));
    head.appendChild(heading);
    if (entries.length) {
      head.appendChild(button('Clear history', 'warn', () => ipc({cmd: 'history_clear'})));
    }
    libBody.appendChild(head);
    if (!entries.length) {
      libBody.appendChild(node('div', 'lempty',
        'Nothing yet — addresses you visit or commit in the address bar land here.'));
      return;
    }
    const search = node('input', 'search');
    search.type = 'search';
    search.spellcheck = false;
    search.placeholder = 'Search history';
    search.setAttribute('aria-label', 'Search history');
    libBody.appendChild(search);
    const list = node('div');
    libBody.appendChild(list);
    const paintRows = () => {
      list.innerHTML = '';
      const needle = search.value.trim().toLowerCase();
      const rows = entries.filter((e) => !needle ||
        (e.text + ' ' + (e.title || '')).toLowerCase().includes(needle));
      if (!rows.length) {
        list.appendChild(node('div', 'lempty', 'No matches.'));
        return;
      }
      let lastDay = null;
      for (const e of rows) {
        const when = new Date(e.last_seen * 1000);
        const day = when.toLocaleDateString(undefined,
          {weekday: 'long', year: 'numeric', month: 'long', day: 'numeric'});
        if (day !== lastDay) {
          lastDay = day;
          list.appendChild(node('div', 'lhead', day));
        }
        const r = node('div', 'lrow');
        r.tabIndex = 0;
        r.title = e.text;
        if (e.favicon) {
          const img = node('img', 'site-fav');
          img.src = e.favicon;
          img.alt = '';
          img.draggable = false;
          img.style.width = '16px'; img.style.height = '16px';
          r.appendChild(img);
        }
        const main = node('div', 'main');
        main.append(node('div', 't', e.title || e.text), node('div', 'u', e.text));
        r.appendChild(main);
        r.appendChild(node('div', 'fold', when.toLocaleTimeString(undefined,
          {hour: '2-digit', minute: '2-digit'}) + (e.uses > 1 ? ' · ' + e.uses + ' visits' : '')));
        const forget = button('Forget', 'warn', (ev) => {
          ev.stopPropagation();
          ipc({cmd: 'history_remove', text: e.text});
        });
        r.appendChild(forget);
        r.onclick = (ev) =>
          ipc({cmd: 'history_open', url: e.text, ctrl: ev.ctrlKey, shift: ev.shiftKey});
        r.onkeydown = (ev) => { if (ev.key === 'Enter') r.click(); };
        list.appendChild(r);
      }
      if (libData.more_history) {
        list.appendChild(node('div', 'lnote',
          'Showing the first ' + entries.length + ' of ' +
          (entries.length + libData.more_history) + ' remembered.'));
      }
    };
    search.oninput = paintRows;
    paintRows();
    setTimeout(() => search.focus(), 0);
  };
