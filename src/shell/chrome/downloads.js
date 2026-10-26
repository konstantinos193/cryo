  // Download shelf: the ⬇ button appears once the first file lands;
  // the ring around the glyph is aggregate in-flight progress. The same
  // rows fill the library's Downloads pane — `dlRows` is the document's
  // copy of the shelf, kept current by every `__cryo_dl` push.
  const fmtBytes = (n) => {
    if (n < 1024) return n + ' B';
    if (n < 1048576) return Math.round(n / 1024) + ' KB';
    if (n < 1073741824) return (n / 1048576).toFixed(1) + ' MB';
    return (n / 1073741824).toFixed(2) + ' GB';
  };

  // The progress ring's circumference: 2π × r(9.5) — dash math below.
  const DL_CIRC = 59.7;
  let dlRows = [];
  let dlCloseTimer = null;

  // One row verb per state: progress pauses or dies, paused resumes,
  // done opens/drags/reveals, dead ones retry.
  const dlAct = (title, glyph, cmd) => {
    const b = document.createElement('button');
    b.className = 'act';
    b.title = title;
    b.setAttribute('aria-label', title);
    b.innerHTML = glyph;
    b.onclick = cmd;
    return b;
  };
  const DL_IC = {
    pause: '<svg width="15" height="15" viewBox="0 0 14 14" fill="currentColor"><rect x="3.5" y="2.5" width="2.6" height="9" rx="1"/><rect x="7.9" y="2.5" width="2.6" height="9" rx="1"/></svg>',
    play: '<svg width="15" height="15" viewBox="0 0 14 14" fill="currentColor"><path d="M4 2.5v9a1 1 0 0 0 1.52.85l6.8-4.5a1 1 0 0 0 0-1.7l-6.8-4.5A1 1 0 0 0 4 2.5z"/></svg>',
    folder: '<svg width="15" height="15" viewBox="0 0 14 14" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round"><path d="M1.5 3.5h4l1.5 1.8h5.5v5.9h-11z"/></svg>',
  };
  // A finished file drags out for real: press and move a few px and
  // the app takes over with a native OLE drag — drop on a folder to
  // copy, drop back on this strip to open the file in a tab.
  const armDlDrag = (el, key) => {
    el.addEventListener('pointerdown', (e) => {
      if (e.button !== 0 || e.target.closest('.act')) return;
      const x0 = e.clientX, y0 = e.clientY;
      const cleanup = () => {
        el.removeEventListener('pointermove', move);
        el.removeEventListener('pointerup', cleanup);
        el.removeEventListener('pointercancel', cleanup);
      };
      const move = (ev) => {
        if (Math.abs(ev.clientX - x0) + Math.abs(ev.clientY - y0) < 5) return;
        cleanup();
        ipc({cmd: 'download_drag', key});
      };
      el.addEventListener('pointermove', move);
      el.addEventListener('pointerup', cleanup);
      el.addEventListener('pointercancel', cleanup);
    });
  };

  // One download row — the shelf popup and the Downloads pane share it.
  // `prevState` maps row keys to their last state so a finish can flash.
  const dlRow = (d, prevState) => {
    const row = document.createElement('div');
    row.className = 'dl-row' + (d.state === 'done' ? ' done' : '');
    // progress → done between updates gets one green flash.
    if (prevState.get(d.key) === 'progress' && d.state === 'done') {
      row.classList.add('flash');
    }
    const top = document.createElement('div');
    top.className = 'top';
    const nm = document.createElement('span');
    nm.className = 'nm';
    nm.textContent = d.name;
    nm.title = d.path;
    top.appendChild(nm);
    if (d.state === 'progress') {
      top.appendChild(dlAct('Pause', DL_IC.pause, () => ipc({cmd: 'download_pause', key: d.key})));
      top.appendChild(dlAct('Cancel', '×', () => ipc({cmd: 'download_cancel', key: d.key})));
    } else if (d.state === 'paused') {
      top.appendChild(dlAct('Resume', DL_IC.play, () => ipc({cmd: 'download_resume', key: d.key})));
      top.appendChild(dlAct('Cancel', '×', () => ipc({cmd: 'download_cancel', key: d.key})));
    } else if (d.state === 'done') {
      top.appendChild(dlAct('Show in folder', DL_IC.folder, () => ipc({cmd: 'download_show', key: d.key})));
      // Click opens the file with its default app — the drag handler
      // is what a press-and-move becomes instead.
      row.onclick = (e) => {
        if (e.target.closest('.act')) return;
        ipc({cmd: 'download_open', key: d.key});
      };
      armDlDrag(row, d.key);
    } else {
      const b = document.createElement('button');
      b.className = 'act retry';
      b.textContent = 'Retry';
      b.onclick = () => ipc({cmd: 'download_retry', key: d.key});
      top.appendChild(b);
    }
    row.appendChild(top);
    const meta = document.createElement('div');
    meta.className = 'meta';
    const bytes = fmtBytes(d.received) + (d.total ? ' / ' + fmtBytes(d.total) : '');
    if (d.state === 'progress') {
      meta.textContent = (d.total ? Math.round(100 * d.received / d.total) + '% — ' : '') + bytes;
    } else if (d.state === 'paused') {
      meta.textContent = 'Paused — ' + bytes;
    } else if (d.state === 'done') {
      meta.textContent = fmtBytes(d.received);
    } else {
      meta.textContent = d.state === 'cancelled' ? 'Cancelled' : 'Interrupted';
    }
    row.appendChild(meta);
    return row;
  };

  // The Downloads pane: the same rows at page scale, repainted on every
  // progress push while it is open.
  const paintDownloads = () => {
    const head = node('div', 'lrow');
    const heading = node('div', 'main');
    heading.appendChild(node('div', 't',
      dlRows.length + (dlRows.length === 1 ? ' download' : ' downloads')));
    head.appendChild(heading);
    if (dlRows.some((d) => d.state !== 'progress' && d.state !== 'paused')) {
      head.appendChild(button('Clear finished', 'warn',
        () => ipc({cmd: 'downloads_clear'})));
    }
    libBody.appendChild(head);
    if (!dlRows.length) {
      libBody.appendChild(node('div', 'lempty',
        'Nothing yet — files you download land here and on the ⬇ shelf.'));
      return;
    }
    const list = node('div');
    for (const d of dlRows) list.appendChild(dlRow(d, new Map()));
    libBody.appendChild(list);
  };

  const renderDl = (dls, prev) => {
    const prevDls = prev.downloads || [];
    const wasProg = new Set(
      prevDls.filter((d) => d.state === 'progress').map((d) => d.key));
    const prevState = new Map(prevDls.map((d) => [d.key, d.state]));
    const nowProg = dls.filter((d) => d.state === 'progress');
    dlRows = dls;

    dlBtn.hidden = !dls.length;

    // A download that wasn't in flight last update just started: open
    // the shelf, the way Chrome opens its bubble.
    const started = nowProg.some((d) => !wasProg.has(d.key));
    if (started) {
      dlBtn.classList.remove('pop');
      void dlBtn.offsetWidth;   // restart the animation
      dlBtn.classList.add('pop');
      setDl(true);
      if (dlCloseTimer) { clearTimeout(dlCloseTimer); dlCloseTimer = null; }
    }
    // The last transfer settled — the shelf lingers a beat, then tucks.
    if (!nowProg.length && wasProg.size && dlOpen && !dlCloseTimer) {
      dlCloseTimer = setTimeout(() => {
        dlCloseTimer = null;
        if (dlOpen) setDl(false);
      }, 3000);
    }

    // Aggregate ring around the glyph: mean of known shares, or an
    // orbiting segment when nothing declares a size.
    let share = 0, parts = 0;
    for (const d of nowProg) {
      if (d.total) { share += d.received / d.total; parts++; }
    }
    const any = nowProg.length > 0;
    const ring = dlBtn.querySelector('.ring-prog');
    dlBtn.classList.toggle('busy', any);
    dlBtn.classList.toggle('ind', any && !parts);
    if (any && parts) {
      ring.style.strokeDashoffset = (DL_CIRC * (1 - share / parts)).toFixed(1);
    } else if (!any) {
      ring.style.strokeDashoffset = DL_CIRC;
    }

    dlPop.innerHTML = '';
    if (!dls.length) {
      const empty = document.createElement('div');
      empty.className = 'mi off';
      empty.textContent = 'No downloads';
      dlPop.appendChild(empty);
    }
    for (const d of dls) dlPop.appendChild(dlRow(d, prevState));
    const foot = document.createElement('div');
    foot.className = 'sep';
    dlPop.appendChild(foot);
    if (dls.some((d) => d.state !== 'progress' && d.state !== 'paused')) {
      const c = document.createElement('div');
      c.className = 'mi';
      c.textContent = 'Clear finished';
      c.onclick = () => ipc({cmd: 'downloads_clear'});
      dlPop.appendChild(c);
    }
    const all = document.createElement('div');
    all.className = 'mi';
    all.textContent = 'Show all downloads';
    all.onclick = () => openLib('downloads');
    dlPop.appendChild(all);
    // An open Downloads pane sees the same push — repaint it live.
    if (libOpen && libPane === 'downloads') {
      libBody.innerHTML = '';
      paintDownloads();
    }
  };
