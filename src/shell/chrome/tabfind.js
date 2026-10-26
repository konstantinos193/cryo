  // ===== Tab search: find one of forty open tabs by name
  // Freezing makes keeping tabs open cheap, which is exactly how a strip
  // stops being a way to find anything in it. This is the list the strip
  // cannot be. It owns no data: every row comes from the pushed state, in
  // registry order, so the results read in the same order as the strip.
  const tabfindEl = document.getElementById('tabfind');
  const tabfindPanel = tabfindEl.querySelector('.panel');
  const tabfindInput = tabfindEl.querySelector('input');
  const tabfindList = tabfindEl.querySelector('.results');
  const tabfindCount = tabfindEl.querySelector('.count');
  let tabfindOpen = false;
  // What is on screen, and which row Enter would take.
  let tabfindRows = [];
  let tabfindAt = 0;

  // Clicks inside the panel are the panel's; the dimmed backdrop lands on
  // the document and dismisses, like the library.
  tabfindPanel.onclick = (e) => e.stopPropagation();

  const setTabfind = (open) => {
    if (open === tabfindOpen) return;
    tabfindOpen = open;
    tabfindEl.classList.toggle('open', open);
    if (!open) {
      tabfindInput.value = '';
      tabfindList.innerHTML = '';
      tabfindRows = [];
    }
    reportOverlay();
  };

  const pickTab = (index) => {
    const tab = tabfindRows[index];
    if (!tab) return;
    // A frozen or discarded pick restores through the ordinary activate
    // path. This overlay is a selector; it owns no restore logic.
    ipc({cmd: 'activate', id: tab.id});
    setTabfind(false);
  };

  const paintTabfind = () => {
    const query = tabfindInput.value.trim().toLowerCase();
    const tabs = lastState.tabs || [];
    tabfindRows = query
      ? tabs.filter((t) => (t.title + '\n' + t.url).toLowerCase().includes(query))
      : tabs.slice();
    if (tabfindAt >= tabfindRows.length) {
      tabfindAt = Math.max(0, tabfindRows.length - 1);
    }
    tabfindList.innerHTML = '';
    tabfindRows.forEach((tab, index) => {
      const row = node('div', 'tf-row' + (index === tabfindAt ? ' at' : '') +
                              (tab.active ? ' live' : ''));
      row.setAttribute('role', 'option');
      row.setAttribute('aria-selected', index === tabfindAt ? 'true' : 'false');
      if (tab.favicon) {
        const img = document.createElement('img');
        img.className = 'fav';
        img.src = tab.favicon;
        img.alt = '';
        row.appendChild(img);
      } else {
        row.appendChild(node('span', 'fav blank'));
      }
      const copy = node('div', 'copy');
      copy.appendChild(node('div', 'nm', tab.title));
      copy.appendChild(node('div', 'ad', tab.url));
      row.appendChild(copy);
      // A frozen tab is listed exactly like a live one — that it has no
      // view is a fact about the surface, not a reason to hide the page.
      if (tab.state !== 'active') {
        row.appendChild(node('span', 'tf-state ' + tab.state,
          tab.state === 'frozen' ? '❄ Frozen' : 'Discarded'));
      } else if (tab.active) {
        row.appendChild(node('span', 'tf-state here', 'Current'));
      }
      row.onclick = () => pickTab(index);
      // Hover moves the highlight, so the mouse and the arrow keys never
      // disagree about what Enter would take.
      row.onmouseenter = () => {
        if (tabfindAt === index) return;
        tabfindAt = index;
        paintTabfind();
      };
      tabfindList.appendChild(row);
    });
    if (!tabs.length) {
      tabfindCount.textContent = '';
    } else if (!tabfindRows.length) {
      tabfindCount.textContent = 'No tab matches that';
    } else if (query) {
      tabfindCount.textContent = tabfindRows.length + ' of ' + tabs.length + ' tabs';
    } else {
      tabfindCount.textContent = tabs.length + (tabs.length === 1 ? ' tab' : ' tabs');
    }
    tabfindList.querySelector('.tf-row.at')?.scrollIntoView({block: 'nearest'});
  };

  // Wraps at both ends: a picker with forty rows is quicker to reach from
  // the bottom than from the top.
  const moveTabfind = (delta) => {
    if (!tabfindRows.length) return;
    tabfindAt = (tabfindAt + delta + tabfindRows.length) % tabfindRows.length;
    paintTabfind();
  };

  tabfindInput.oninput = () => {
    tabfindAt = 0;
    paintTabfind();
  };
  tabfindInput.onkeydown = (e) => {
    if (e.key === 'ArrowDown' || (e.key === 'Tab' && !e.shiftKey)) {
      e.preventDefault();
      moveTabfind(1);
    } else if (e.key === 'ArrowUp' || (e.key === 'Tab' && e.shiftKey)) {
      e.preventDefault();
      moveTabfind(-1);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      pickTab(tabfindAt);
    }
  };

  // Ctrl+Shift+A lands here. The app has already moved controller focus to
  // this view; DOM focus has to wait for the overlay to be displayed.
  window.__cryo_tabfind_toggle = () => {
    if (tabfindOpen) {
      setTabfind(false);
      return;
    }
    dismissOverlays();
    tabfindAt = 0;
    setTabfind(true);
    paintTabfind();
    setTimeout(() => tabfindInput.focus(), 0);
  };
