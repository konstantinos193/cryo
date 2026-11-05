  // ===== The arrows' right-click menus: this tab's session history
  // The list belongs to the engine and is read fresh on every right-click,
  // so the popup opens when the reply lands rather than empty before it.
  // It owns no data past its own open state.
  const navPop = document.getElementById('nav-pop');
  navPop.tabIndex = -1;
  let navMenuOpen = false;
  // The direction on screen, and the one a reply is awaited for. A reply
  // nobody is waiting for (dismissed while it travelled) paints nothing.
  let navShown = null;
  let navAsked = null;
  // Every row in order, the history link included: what to do when picked.
  let navItems = [];
  let navAt = -1;

  const setNavMenu = (open) => {
    navAsked = null;
    if (open === navMenuOpen) return;
    navMenuOpen = open;
    navPop.classList.toggle('open', open);
    if (!open) {
      navPop.innerHTML = '';
      navShown = null;
      navItems = [];
      navAt = -1;
    }
    reportOverlay();
  };

  // One highlight, moved by the arrows and by hover alike. Moved by class
  // rather than repainted, or the row under the pointer would be replaced
  // between press and release and the click would land on nothing.
  const highlightNav = (index) => {
    navAt = index;
    navItems.forEach((item, i) => {
      item.el.classList.toggle('at', i === index);
      item.el.setAttribute('aria-selected', i === index ? 'true' : 'false');
    });
    navItems[index]?.el.scrollIntoView({block: 'nearest'});
  };

  const addNavItem = (el, pick) => {
    const index = navItems.length;
    navItems.push({el, pick});
    el.setAttribute('role', 'menuitem');
    el.onmouseenter = () => highlightNav(index);
    el.onclick = (e) => pick(e.ctrlKey, e.shiftKey);
    navPop.appendChild(el);
  };

  const navRow = (tab, row) => {
    const el = node('div', 'nav-row');
    if (row.favicon) {
      const img = document.createElement('img');
      img.className = 'fav';
      img.src = row.favicon;
      img.alt = '';
      el.appendChild(img);
    } else {
      el.appendChild(node('span', 'fav blank'));
    }
    el.appendChild(node('span', 'nm', row.title || row.url));
    el.title = row.url;
    // Ctrl and the middle button leave this tab where it is, the same way
    // they do on a history row; Ctrl+Shift keeps the new tab behind.
    const inNewTab = (shift) => {
      setNavMenu(false);
      ipc({cmd: 'history_open', url: row.url, ctrl: true, shift});
    };
    el.onauxclick = (e) => { if (e.button === 1) inNewTab(e.shiftKey); };
    // Without this the middle button starts the autoscroll cursor.
    el.onmousedown = (e) => { if (e.button === 1) e.preventDefault(); };
    addNavItem(el, (ctrl, shift) => {
      if (ctrl) { inNewTab(shift); return; }
      setNavMenu(false);
      ipc({cmd: 'nav_go', tab, entry: row.entry});
    });
  };

  // The app only answers while there is a live view to ask, and an empty
  // list (every entry was the blank starting document) opens nothing.
  window.__cryo_nav_menu = (menu) => {
    if (navAsked !== menu.forward || !menu.rows.length) return;
    navAsked = null;
    navPop.innerHTML = '';
    navItems = [];
    navAt = -1;
    menu.rows.forEach((row) => navRow(menu.tab, row));
    navPop.appendChild(node('div', 'sep'));
    addNavItem(node('div', 'nav-row full', 'Show full history'), () => {
      setNavMenu(false);
      openLib('history');
    });
    navShown = menu.forward;
    const button = menu.forward ? fwdBtn : backBtn;
    navPop.style.left = button.getBoundingClientRect().left + 'px';
    setNavMenu(true);
    navPop.focus();
  };

  const askNavMenu = (forward) => (e) => {
    e.preventDefault();
    e.stopPropagation();
    // A second right-click on the same arrow is how the menu goes away.
    const same = navShown === forward;
    dismissOverlays();
    if (same) return;
    navAsked = forward;
    ipc({cmd: 'nav_menu', forward});
  };
  backBtn.oncontextmenu = askNavMenu(false);
  fwdBtn.oncontextmenu = askNavMenu(true);

  // Clicks inside are the popup's; everywhere else the document dismisses.
  navPop.onclick = (e) => e.stopPropagation();
  navPop.oncontextmenu = (e) => {
    e.preventDefault();
    e.stopPropagation();
  };
  // A right-click elsewhere shuts it, as a left-click does.
  document.addEventListener('contextmenu', () => {
    if (navMenuOpen) dismissOverlays();
  });
  navPop.onkeydown = (e) => {
    const rows = navItems.length;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      highlightNav((navAt + 1) % rows);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      highlightNav(navAt < 0 ? rows - 1 : (navAt - 1 + rows) % rows);
    } else if (e.key === 'Enter' && navAt >= 0) {
      e.preventDefault();
      navItems[navAt].pick(e.ctrlKey, e.shiftKey);
    }
  };
