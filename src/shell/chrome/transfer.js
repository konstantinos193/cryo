  const paintImport = () => {
    if (libData.incognito) {
      libBody.appendChild(node('div', 'lnote warn',
        'This is an incognito window: it keeps nothing after it closes. Import from a normal window.'));
    }
    libBody.appendChild(node('div', 'lhead', 'From a file'));
    const pick = node('div', 'lrow');
    pick.appendChild(node('div', 'main',
      'Bookmarks (.html), passwords (.csv) or a cryo history export (.csv). cryo reads the file to see which it is.'));
    const pickBtn = button('Choose a file…', '', () => ipc({cmd: 'import_file'}));
    pickBtn.disabled = !!libData.incognito;
    pick.appendChild(pickBtn);
    libBody.appendChild(pick);

    libBody.appendChild(node('div', 'lhead', 'Installed browsers'));
    if (!libData.sources_ready) {
      libBody.appendChild(node('div', 'lempty', 'Looking for installed browsers…'));
    } else if (!libData.sources.length) {
      libBody.appendChild(node('div', 'lempty',
        'No Chrome, Edge, Firefox, Brave, Vivaldi or Opera profile found on this machine.'));
    }
    for (const s of libData.sources) {
      const r = node('div', 'lrow');
      const main = node('div', 'main');
      main.append(node('div', 't', s.browser + ' · ' + s.profile),
                  node('div', 'u', s.count + (s.count === 1 ? ' bookmark' : ' bookmarks')));
      r.appendChild(main);
      const go = button('Import bookmarks', '', () =>
        ipc({cmd: 'import_profile', index: s.index}));
      go.disabled = !!libData.incognito;
      r.appendChild(go);
      libBody.appendChild(r);
    }
    libBody.appendChild(node('div', 'lnote',
      'Bookmarks are read straight out of the other browser’s profile — Firefox included. ' +
      'Passwords are not: they sit in that browser’s own encrypted store, locked to its account. ' +
      'Export them from its settings — Edge: edge://settings/passwords, Chrome: chrome://password-manager/passwords — and choose that CSV above.'));
  };

  const paintExport = () => {
    if (libData.incognito) {
      libBody.appendChild(node('div', 'lnote warn',
        'An incognito window has nothing stored to export.'));
    }
    const kinds = [
      ['bookmarks', 'Bookmarks', 'A Netscape bookmark file (.html) — every browser imports it.', ''],
      ['passwords', 'Passwords', 'Chrome’s CSV layout. Written in plain text: anyone who opens the file can read every password.', 'warn'],
      ['history', 'History', 'cryo’s own CSV (url, title, visits, last visit), so a profile can move machines.', ''],
    ];
    for (const [what, title, blurb, cls] of kinds) {
      const r = node('div', 'lrow');
      const main = node('div', 'main');
      main.append(node('div', 't', title), node('div', 'u', blurb));
      r.appendChild(main);
      const go = button('Export…', cls, () => ipc({cmd: 'export', what}));
      go.disabled = !!libData.incognito;
      r.appendChild(go);
      libBody.appendChild(r);
    }
  };
