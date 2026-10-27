  // ===== "Add to Chrome" asked for an extension
  // One surface for the whole thing: the question, the download, and the
  // result land in the same card rather than a dialog that vanishes and a
  // toast that appears somewhere else.
  //
  // Consent cannot come from the page: a navigation to a crafted store
  // address would otherwise be a drive-by install. The app issues a token per
  // request and only an answer carrying it counts.
  const installPop = document.getElementById('install-pop');
  let installOpen = false;
  let installToken = null;
  let installDismissTimer = null;

  const refuseInstall = () => {
    if (installToken === null) return;
    ipc({cmd: 'extension_answer', token: installToken, install: false});
  };

  const setInstallOpen = (open) => {
    installOpen = open;
    installPop.classList.toggle('open', open);
    if (!open) {
      installPop.innerHTML = '';
      installToken = null;
    }
    reportOverlay();
  };

  const fmtMib = (bytes) => (bytes / 1048576).toFixed(1) + ' MB';

  window.__cryo_install_prompt = (data) => {
    clearTimeout(installDismissTimer);
    if (!data) {
      setInstallOpen(false);
      return;
    }
    installToken = data.token;
    const first = !installOpen;
    installPop.innerHTML = '';
    if (first) dismissOverlays();

    const head = node('div', 'ins-head');
    head.appendChild(node('div', 'ins-mark', '⬡'));
    const heading = node('div');
    heading.appendChild(node('div', 'ins-title',
      data.fetching ? 'Installing extension' : 'Install this extension?'));
    // The name is not known until the package is fetched, so the id stands in
    // rather than a guess at what it is called.
    heading.appendChild(node('div', 'ins-who', data.name || data.id));
    head.appendChild(heading);
    installPop.appendChild(head);

    if (data.fetching) {
      installPop.appendChild(node('div', 'ins-note', 'Fetching from the Chrome Web Store'));
      const track = node('div', 'ins-track');
      // Indeterminate until the first progress arrives: the store usually
      // sends a length, but a bar that claims 0% of nothing is a lie.
      track.classList.add('ind');
      track.appendChild(node('i'));
      installPop.appendChild(track);
      installPop.appendChild(node('div', 'ins-count', ''));
      return;
    }

    installPop.appendChild(node('div', 'ins-note',
      'cryo fetches it from the Chrome Web Store and checks that it is signed by ' +
      'the key its id comes from. It loads in tabs opened after that.'));
    const actions = node('div', 'ins-actions');
    const no = node('button', 'ins-btn', 'Not now');
    no.onclick = refuseInstall;
    const yes = node('button', 'ins-btn go', 'Install');
    yes.onclick = () => ipc({cmd: 'extension_answer', token: data.token, install: true});
    actions.append(no, yes);
    installPop.appendChild(actions);
    setInstallOpen(true);
  };

  // Bytes in. `total` is null when the store sent no length.
  window.__cryo_install_progress = (received, total) => {
    const track = installPop.querySelector('.ins-track');
    if (!track) return;
    const count = installPop.querySelector('.ins-count');
    if (total) {
      const share = Math.max(0, Math.min(1, received / total));
      track.classList.remove('ind');
      track.firstChild.style.width = (share * 100).toFixed(1) + '%';
      if (count) count.textContent = fmtMib(received) + ' of ' + fmtMib(total);
    } else if (count) {
      count.textContent = fmtMib(received);
    }
  };

  // The last state: what happened, in place, then the card puts itself away.
  window.__cryo_install_done = (result) => {
    if (!installOpen) return;
    installPop.innerHTML = '';
    const head = node('div', 'ins-head');
    head.appendChild(node('div', 'ins-mark' + (result.ok ? ' ok' : ' bad'),
      result.ok ? '✓' : '!'));
    const heading = node('div');
    // The app words it: installed, already installed, or not installed are
    // three different answers and only it knows which happened.
    heading.appendChild(node('div', 'ins-title',
      result.title || (result.ok ? 'Extension installed' : 'Not installed')));
    heading.appendChild(node('div', 'ins-who', result.text));
    head.appendChild(heading);
    installPop.appendChild(head);
    if (result.ok) {
      installPop.appendChild(node('div', 'ins-note',
        'Signature verified. It loads in tabs opened from now on.'));
    }
    // A failure is worth reading twice as long as a success.
    installDismissTimer = setTimeout(() => setInstallOpen(false), result.ok ? 2600 : 6000);
  };

  installPop.addEventListener('click', (e) => e.stopPropagation());