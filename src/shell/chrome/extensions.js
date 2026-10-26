  // ===== The extensions pane: unpacked folders the engine loads
  // The app owns the catalogue and the folder picker — this pane names a
  // slug and lets the app resolve it. Nothing here touches a path.
  const paintExtensions = () => {
    const rows = libData.extensions || [];
    const head = node('div', 'lrow');
    head.style.borderBottom = '0';
    const heading = node('div', 'main');
    heading.appendChild(node('div', 't',
      rows.length + (rows.length === 1 ? ' extension' : ' extensions')));
    head.appendChild(heading);
    if (libData.extensions_supported) {
      head.appendChild(button('Add unpacked folder', '', () => ipc({cmd: 'extension_add'})));
    }
    libBody.appendChild(head);

    if (!libData.extensions_supported) {
      libBody.appendChild(node('div', 'lnote warn', libData.incognito
        ? 'This is an incognito window: it loads no extensions. Open a normal window to manage them.'
        : 'Extensions load through WebView2, so this platform cannot run them. The pane stays here rather than pretending none are installed.'));
      return;
    }

    // Said once, up front, because it is the surprising part and not a bug:
    // loading happens when a tab's view is built.
    libBody.appendChild(node('div', 'lnote',
      'An extension starts working in tabs opened after it is added. A folder you add is copied in, ' +
      'and editing or moving that copy makes WebView2 drop the extension.'));

    // The store's own "Add to Chrome" cannot reach cryo — WebView2 carries no
    // endpoint for it — so the address goes in here instead and cryo fetches
    // the package itself.
    const store = node('div', 'lrow');
    const storeMain = node('div', 'main');
    storeMain.appendChild(node('div', 't', 'Add from the Chrome Web Store'));
    storeMain.appendChild(node('div', 'u',
      "Paste the store page address, or the extension id out of it. The package's signature is " +
      'checked against the key its id comes from before anything is installed.'));
    const field = node('input', 'search');
    field.type = 'text';
    field.spellcheck = false;
    field.placeholder = 'https://chromewebstore.google.com/detail/…  or an extension id';
    field.setAttribute('aria-label', 'Chrome Web Store address or extension id');
    const submit = () => {
      const input = field.value.trim();
      if (!input) return;
      ipc({cmd: 'extension_install', input});
      field.value = '';
    };
    field.onkeydown = (e) => { if (e.key === 'Enter') submit(); };
    storeMain.appendChild(field);
    store.appendChild(storeMain);
    store.appendChild(button('Fetch', '', submit));
    libBody.appendChild(store);

    if (!rows.length) {
      libBody.appendChild(node('div', 'lempty',
        'Nothing installed. Unzip an extension, then add the folder that holds its manifest.json.'));
      return;
    }

    const list = node('div');
    for (const ext of rows) {
      const row = node('div', 'lrow');
      const main = node('div', 'main');
      main.appendChild(node('div', 't', ext.name));
      main.appendChild(node('div', 'u', 'Version ' + ext.version +
        ' — Manifest V' + ext.manifest_version));
      // The folder, so it can be found and left alone.
      main.appendChild(node('div', 'u path', ext.dir));
      row.appendChild(main);
      row.appendChild(button('Remove', 'warn',
        () => ipc({cmd: 'extension_remove', slug: ext.slug})));
      list.appendChild(row);
    }
    libBody.appendChild(list);
  };
