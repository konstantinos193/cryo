  // ===== Tasks pane: every tab, its state, and what the last memory
  // sample could honestly bill to it. Rows arrive on `__cryo_tasks`,
  // apart from the library's own push — a fresh sample lands without
  // the panel's other data changing.
  const paintTasks = () => {
    const tasks = taskData.tasks || [];
    const head = node('div', 'lrow');
    head.style.borderBottom = '0';
    const heading = node('div', 'main');
    heading.appendChild(node('div', 't',
      tasks.length + (tasks.length === 1 ? ' tab' : ' tabs')));
    head.appendChild(heading);
    if (taskData.measured) {
      const age = Math.max(0, Math.round(Date.now() / 1000 - taskData.sampled_at));
      head.appendChild(node('div', 'fold',
        'sampled ' + (age < 60 ? age + 's' : Math.floor(age / 60) + 'm') + ' ago'));
    }
    libBody.appendChild(head);
    if (!taskData.measured) {
      libBody.appendChild(node('div', 'lempty', 'Measuring…'));
      return;
    }
    const memText = (t) => {
      if (t.state !== 'active') return '—';
      if (t.bytes != null) return fmtBytes(t.bytes);
      return ({shared: 'shared', unattributed: '—', unreadable: '?'})[t.reason] || '—';
    };
    const stateChip = (t) =>
      t.state === 'active' ? null
        : node('span', 'chip ' + t.state, t.state);
    for (const t of tasks) {
      const r = node('div', 'lrow task');
      r.tabIndex = 0;
      r.title = t.url;
      if (t.favicon) {
        const img = node('img', 'site-fav');
        img.src = t.favicon;
        img.alt = '';
        img.draggable = false;
        r.appendChild(img);
      }
      const main = node('div', 'main');
      const title = node('div', 't');
      title.textContent = t.title || t.url;
      if (t.current) title.appendChild(node('span', 'you', ' · current'));
      main.append(title, node('div', 'u', t.url));
      r.appendChild(main);
      const chip = stateChip(t);
      if (chip) r.appendChild(chip);
      r.appendChild(node('div', 'fold mem', memText(t)));
      if (t.state === 'active') {
        r.appendChild(button('Freeze', '', (ev) => {
          ev.stopPropagation();
          ipc({cmd: 'tab_freeze', id: t.id});
        }));
        r.appendChild(button('Discard', 'warn', (ev) => {
          ev.stopPropagation();
          ipc({cmd: 'tab_discard', id: t.id});
        }));
      } else {
        r.appendChild(button('Discard', 'warn', (ev) => {
          ev.stopPropagation();
          ipc({cmd: 'tab_discard', id: t.id});
        }));
      }
      r.appendChild(button('✕', '', (ev) => {
        ev.stopPropagation();
        ipc({cmd: 'close', id: t.id});
      }));
      r.onclick = () => ipc({cmd: 'activate', id: t.id});
      r.onkeydown = (ev) => { if (ev.key === 'Enter') r.click(); };
      libBody.appendChild(r);
    }
    const pools = node('div', 'lnote',
      'Browser + GPU: ' + fmtBytes(taskData.overhead || 0) +
      ' · shared renderers: ' + fmtBytes(taskData.shared || 0) +
      (taskData.partial ? ' · a process read failed — figures are partial' : ''));
    libBody.appendChild(pools);
    // The runtime's own view, honestly labelled: processes whose link
    // to a view is unknown show their real bytes under the tabs they
    // cannot be billed to.
    const procs = taskData.processes || [];
    if (procs.length) {
      const sep = node('div', 'lrow');
      sep.style.borderBottom = '0';
      sep.appendChild(node('div', 'fold', procs.length + ' processes'));
      libBody.appendChild(sep);
      for (const p of procs) {
        const r = node('div', 'lrow task');
        const main = node('div', 'main');
        main.append(node('div', 't', p.kind + ' · pid ' + p.pid));
        r.appendChild(main);
        r.appendChild(node('div', 'fold mem', p.bytes != null ? fmtBytes(p.bytes) : '?'));
        libBody.appendChild(r);
      }
    }
  };
