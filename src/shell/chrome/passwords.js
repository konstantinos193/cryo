  const paintPasswords = () => {
    if (libData.locked) {
      libBody.appendChild(node('div', 'lnote warn',
        'The vault could not be opened: ' + libData.locked +
        ' — it is sealed to the Windows account that wrote it. ' +
        'Nothing will be overwritten.'));
    }
    if (!libData.passwords.length) {
      libBody.appendChild(node('div', 'lempty',
        'No saved passwords. Export them from your old browser and bring the CSV over in the Import pane.'));
    } else {
      const search = node('input', 'search');
      search.type = 'search';
      search.spellcheck = false;
      search.placeholder = 'Search passwords';
      search.setAttribute('aria-label', 'Search passwords');
      libBody.appendChild(search);
      const list = node('div');
      libBody.appendChild(list);
      const paintRows = () => {
        list.innerHTML = '';
        const needle = search.value.trim().toLowerCase();
        const rows = libData.passwords.filter((p) => !needle ||
          (p.origin + ' ' + p.username).toLowerCase().includes(needle));
        if (!rows.length) {
          list.appendChild(node('div', 'lempty', 'No matches.'));
          return;
        }
        for (const p of rows) {
          const key = rowKey(p.origin, p.username);
          const shown = revealed.get(key);
          const r = node('div', 'lrow');
          const main = node('div', 'main');
          main.append(node('div', 't', p.origin),
                      node('div', 'u', p.username || '(no username)'));
          r.appendChild(main);
          r.appendChild(node('div', 'dots', shown === undefined
            ? '•'.repeat(Math.min(p.length, 16)) : shown));
          if (shown === undefined) {
            r.appendChild(button('Reveal', '', () =>
              ipc({cmd: 'password_reveal', origin: p.origin, username: p.username})));
          } else {
            r.appendChild(button('Hide', '', () => { revealed.delete(key); paintRows(); }));
          }
          // One click either way: a revealed secret copies straight
          // from the document, a hidden one asks the app — which puts
          // up the same OS confirm as Reveal before it answers.
          r.appendChild(button('Copy', '', () => {
            if (shown !== undefined) {
              navigator.clipboard.writeText(shown).catch(() => {});
            } else {
              ipc({cmd: 'password_copy', origin: p.origin, username: p.username});
            }
          }));
          r.appendChild(button('Forget', 'warn', () =>
            ipc({cmd: 'password_remove', origin: p.origin, username: p.username})));
          list.appendChild(r);
        }
      };
      search.oninput = paintRows;
      paintRows();
      setTimeout(() => search.focus(), 0);
    }
    if (libData.more_passwords) {
      libBody.appendChild(node('div', 'lnote',
        'Showing the first ' + libData.passwords.length + ' of ' +
        (libData.passwords.length + libData.more_passwords) + ' saved.'));
    }
    libBody.appendChild(node('div', 'lnote',
      'Saved passwords are sealed with your Windows account, so the file is unreadable on any other. ' +
      'cryo does not offer to save passwords from forms yet — this list is what you import, and what Export writes.'));
  };
