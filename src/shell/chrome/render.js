  // Shelf-only pushes: progress ticks repaint the shelf without paying
  // for a whole-state sync. lastState stays in step so the next full
  // update's started/settled diffing still sees the truth.
  window.__cryo_dl = (rows) => {
    renderDl(rows || [], lastState);
    lastState.downloads = rows || [];
  };

  window.__cryo_update = (state) => {
    const prev = lastState;
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
      // A loading tab spins in the icon slot; once settled the site
      // icon leads the tab in every state — the strip badges the page,
      // not the surface's liveness.
      if (t.loading) {
        const spin = document.createElement('span');
        spin.className = 'spin';
        el.appendChild(spin);
      } else if (t.favicon) {
        const img = document.createElement('img');
        img.className = 'fav';
        img.src = t.favicon;
        img.alt = '';
        el.appendChild(img);
      }
      // Live tabs get a freeze button; a frozen tab shows the same flake
      // as a state marker (clicking the tab restores).
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
    // Loading state: the active tab's flag swaps reload for ✕ and runs
    // the indeterminate bar; background tabs only spin their own icon.
    const active = state.tabs.find((t) => t.active);
    const loading = !!(active && active.loading);
    reloadBtn.classList.toggle('loading', loading);
    reloadBtn.title = loading ? 'Stop loading (Esc)' : 'Reload (F5)';
    loadbar.classList.toggle('on', loading);
    // The arrows grey out where history has nowhere to step.
    backBtn.disabled = !state.can_back;
    fwdBtn.disabled = !state.can_fwd;
    const menuBtn = document.getElementById('menu');
    menuBtn.classList.toggle('has-update', !!state.update_version);
    if (state.update_version) {
      updateItem.textContent = 'Restart to update — v' + state.update_version;
      menuPop.prepend(updateItem);
    } else {
      updateItem.remove();
    }
    renderDl(state.downloads || [], prev);
    // The bar shows kept pages once there are any, the dial list until
    // then; the ★ fills in while this page is one of them.
    renderBar(state.bookmarks || []);
    const star = document.getElementById('star');
    star.classList.toggle('on', !!state.starred);
    star.title = state.starred
      ? 'Remove bookmark (Ctrl+D)' : 'Bookmark this page (Ctrl+D)';
    // The account button mirrors the profile's Google session: the
    // picked account's picture (or an initial) when signed in — the
    // button's click lists them all — the pill when not.
    const accounts = state.accounts || [];
    const account = accounts[state.active_account || 0] || accounts[0];
    if (account) {
      const key = account.name + '|' + (account.avatar || '');
      if (signinBtn.dataset.k !== key) {
        signinBtn.dataset.k = key;
        signinBtn.innerHTML = '';
        if (account.avatar) {
          const img = document.createElement('img');
          img.className = 'ava';
          img.src = account.avatar;
          img.alt = '';
          signinBtn.appendChild(img);
        } else {
          const dot = document.createElement('span');
          dot.className = 'initial';
          dot.textContent =
            Array.from(account.name.trim())[0]?.toUpperCase() || '?';
          signinBtn.appendChild(dot);
        }
      }
      signinBtn.classList.add('signed');
      signinBtn.title = account.email
        ? 'Google account — ' + account.email + ' (click to switch)'
        : 'Google account — ' + account.name;
    } else {
      signinBtn.classList.remove('signed');
      delete signinBtn.dataset.k;
      signinBtn.textContent = 'Sign in';
      signinBtn.title = 'Sign in with your Google account';
    }
  };
