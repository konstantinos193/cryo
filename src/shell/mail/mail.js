(() => {
  'use strict';

  // ── The wire ──────────────────────────────────────────────
  // One channel: window.ipc (the content page's PageMessage path).
  // The app answers through window.__cryo_mail — eval, never fetch.
  const ipc = (obj) => {
    try { window.ipc.postMessage(JSON.stringify(obj)); } catch (_) {}
  };

  const $ = (id) => document.getElementById(id);
  const el = (tag, cls, text) => {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  };
  const SVG = 'http://www.w3.org/2000/svg';
  // A glyph from the sprite the document carries — the script draws
  // nothing of its own.
  const icon = (name, cls) => {
    const svg = document.createElementNS(SVG, 'svg');
    svg.setAttribute('class', 'ic' + (cls ? ' ' + cls : ''));
    svg.setAttribute('aria-hidden', 'true');
    const use = document.createElementNS(SVG, 'use');
    use.setAttribute('href', '#i-' + name);
    svg.appendChild(use);
    return svg;
  };
  const iconButton = (name, title, cls, onclick) => {
    const b = el('button', cls);
    b.type = 'button';
    b.title = title;
    b.setAttribute('aria-label', title);
    b.appendChild(icon(name));
    b.onclick = (e) => { e.stopPropagation(); onclick(e); };
    return b;
  };

  // ── Client state ──────────────────────────────────────────
  // Whatever the page holds lives in the DOM too — a frozen tab gives
  // it all back; nothing here is precious.
  let accounts = [];            // PushState accounts
  let statusByEmail = {};       // email -> "syncing" | "" | error text
  let sel = null;               // {account, folder}
  let list = [];                // ListEntry rows of sel
  let loading = false;          // a listing is on its way for sel
  let cursor = null;            // thread key the list highlights
  let bodies = {};              // uid -> the Body push, for the open thread
  let expanded = new Set();     // uids shown in full in the open thread
  let focus = null;             // uid the toolbar acts on
  let view = 'all';             // 'all' | 'unread' | 'starred'
  let filter = '';              // the column's filter text, lower-cased
  let files = [];               // compose's picked files (names, sizes)
  let sendSeq = 0;              // compose generation — stale replies drop
  let lastSynced = 0;           // ms epoch of the last idle status

  const findAccount = (email) => accounts.find((a) => a.email === email);
  const findFolder = (email, name) =>
    ((findAccount(email) || {}).folders || []).find((f) => f.name === name);
  const folderByRole = (email, role) =>
    ((findAccount(email) || {}).folders || []).find((f) => f.role === role);
  const ownAddresses = () => new Set(accounts.map((a) => a.email.toLowerCase()));

  const ROLE_ORDER = {inbox: 0, drafts: 1, sent: 2, junk: 3, archive: 4, trash: 5, other: 6};
  const ROLE_ICON = {inbox: 'inbox', drafts: 'drafts', sent: 'sent', junk: 'junk', archive: 'archive', trash: 'trash', other: 'folder'};
  const MAX_AUTO_OPEN = 8;      // bodies fetched when a thread opens

  // Senders whose remote images load without asking — the reader's
  // choice, kept in this origin's storage beside the theme pick.
  const TRUST = 'cryo.mail.remote';
  const trusted = () => {
    try { return new Set(JSON.parse(localStorage.getItem(TRUST) || '[]')); } catch (_) { return new Set(); }
  };
  const trust = (addr) => {
    const set = trusted();
    set.add(addr.toLowerCase());
    try { localStorage.setItem(TRUST, JSON.stringify([...set])); } catch (_) {}
  };

  // ── Addresses, dates, sizes ───────────────────────────────
  // "Ann Rivers <ann@example.com>" → a name and an address; a bare
  // address is its own name.
  const splitAddr = (s) => {
    const m = /^\s*"?([^"<]*?)"?\s*<([^>]+)>\s*$/.exec(s || '');
    if (m) return {name: m[1].trim() || m[2].trim(), addr: m[2].trim()};
    const t = (s || '').trim();
    return {name: t, addr: t};
  };
  const addrList = (s) => (s || '').split(',').map((p) => p.trim()).filter(Boolean).map(splitAddr);
  const firstAddr = (s) => addrList(s)[0] || {name: '', addr: ''};
  const names = (s) => addrList(s).map((a) => a.name).join(', ');
  const initial = (name) => {
    const c = (name || '').replace(/^[^\p{L}\p{N}]+/u, '').charAt(0);
    return c ? c.toUpperCase() : '?';
  };
  // A stable hue per sender, kept in the cold half of the wheel so the
  // list reads as one set. How light that hue is worn belongs to the
  // theme (`--tint-*`), not here.
  const tone = (key) => {
    let h = 0;
    for (let i = 0; i < key.length; i++) h = (h * 31 + key.charCodeAt(i)) % 1021;
    const hue = 168 + (h % 92);
    return {bg: `hsl(${hue} var(--tint-bg))`, fg: `hsl(${hue} var(--tint-fg))`};
  };
  const avatar = (who, cls) => {
    const a = el('span', 'avatar' + (cls ? ' ' + cls : ''), initial(who.name));
    const t = tone(who.addr.toLowerCase());
    a.style.background = t.bg;
    a.style.color = t.fg;
    return a;
  };

  const fmtDate = (ts) => {
    if (!ts) return '';
    const d = new Date(ts * 1000);
    const now = new Date();
    if (d.toDateString() === now.toDateString()) {
      return d.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'});
    }
    if (d.getFullYear() === now.getFullYear()) {
      return d.toLocaleDateString([], {day: 'numeric', month: 'short'});
    }
    return d.toLocaleDateString([], {day: 'numeric', month: 'short', year: 'numeric'});
  };
  const fmtFull = (ts) => ts
    ? new Date(ts * 1000).toLocaleString([], {
        weekday: 'short', day: 'numeric', month: 'short', year: 'numeric',
        hour: '2-digit', minute: '2-digit'})
    : '';
  const fmtClock = (ms) => new Date(ms).toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'});

  const fmtSize = (n) => {
    if (n < 1024) return n + ' B';
    if (n < 1024 * 1024) return Math.round(n / 1024) + ' KB';
    return (n / 1024 / 1024).toFixed(1) + ' MB';
  };

  const toast = (text, bad) => {
    const t = $('toast');
    $('toast-text').textContent = text;
    $('toast-glyph').setAttribute('href', bad ? '#i-warn' : '#i-check');
    t.classList.toggle('bad', !!bad);
    t.hidden = false;
    clearTimeout(t._h);
    t._h = setTimeout(() => { t.hidden = true; }, bad ? 6500 : 3600);
  };

  // ── Rail: accounts + folders ──────────────────────────────
  const paintFolders = () => {
    const host = $('accounts');
    host.textContent = '';
    for (const a of accounts) {
      const box = el('div', 'acct');
      const head = el('div', 'acct-head');
      const who = splitAddr(a.display ? `${a.display} <${a.email}>` : a.email);
      head.appendChild(avatar(who, 'sm'));
      const text = el('div', 'who');
      text.appendChild(el('b', '', a.display || a.email));
      if (a.display) text.appendChild(el('span', 'dim', a.email));
      head.appendChild(text);
      const dot = el('span', 'dot');
      const st = statusByEmail[a.email] || '';
      if (st === 'syncing') dot.classList.add('busy');
      else if (st) dot.classList.add('bad');
      dot.title = st === 'syncing' ? 'Syncing…' : (st || 'Connected');
      head.appendChild(dot);
      head.appendChild(iconButton('x', `Remove ${a.email}`, 'x', () => {
        if (confirm(`Remove ${a.email} and its cached mail?`)) {
          ipc({kind: 'remove_account', email: a.email});
        }
      }));
      box.appendChild(head);
      const folders = [...(a.folders || [])].sort(
        (x, y) => (ROLE_ORDER[x.role] ?? 6) - (ROLE_ORDER[y.role] ?? 6) || x.display.localeCompare(y.display));
      for (const f of folders) {
        const row = el('button', 'fold');
        row.type = 'button';
        const current = sel && sel.account === a.email && sel.folder === f.name;
        if (current) {
          row.classList.add('sel');
          row.setAttribute('aria-current', 'true');
        }
        row.appendChild(icon(ROLE_ICON[f.role] || 'folder'));
        row.appendChild(el('span', 'name', f.display));
        if (f.unseen) row.appendChild(el('span', 'n', String(f.unseen)));
        row.onclick = () => selectFolder(a.email, f.name);
        box.appendChild(row);
      }
      host.appendChild(box);
    }
    paintStatus();
  };

  const paintStatus = () => {
    const line = $('status');
    const states = accounts.map((a) => statusByEmail[a.email] || '');
    const busy = states.includes('syncing');
    const bad = states.find((s) => s && s !== 'syncing');
    $('sync-btn').classList.toggle('busy', busy);
    line.classList.toggle('bad', !!bad && !busy);
    line.textContent = busy ? 'Syncing…' : bad ? bad : lastSynced ? `Synced ${fmtClock(lastSynced)}` : '';
    line.title = line.textContent;
  };

  const selectFolder = (account, folder) => {
    sel = {account, folder};
    closeThread();
    list = [];
    loading = true;
    paintFolders();
    paintHead();
    paintList();
    paintBody();
    ipc({kind: 'select_folder', account, folder});
  };

  // ── Threads ───────────────────────────────────────────────
  // The listing arrives threaded (every row names its conversation);
  // this is the grouping, newest conversation first, newest message
  // first inside each.
  const threads = () => {
    const map = new Map();
    for (const m of list) {
      let t = map.get(m.thread);
      if (!t) { t = {key: m.thread, msgs: []}; map.set(m.thread, t); }
      t.msgs.push(m);
    }
    const out = [...map.values()];
    for (const t of out) {
      t.msgs.sort((a, b) => (b.date || 0) - (a.date || 0) || b.uid - a.uid);
      t.latest = t.msgs[0];
      t.unread = t.msgs.some((m) => !m.seen);
      t.flagged = t.msgs.some((m) => m.flagged);
      t.draft = t.msgs.some((m) => m.draft);
    }
    out.sort((a, b) => (b.latest.date || 0) - (a.latest.date || 0) || b.latest.uid - a.latest.uid);
    return out;
  };
  const threadOf = (key) => threads().find((t) => t.key === key) || null;

  const matches = (m) =>
    (m.from || '').toLowerCase().includes(filter) || (m.subject || '').toLowerCase().includes(filter);
  const visible = () => threads().filter((t) =>
    (view === 'all' || (view === 'unread' && t.unread) || (view === 'starred' && t.flagged)) &&
    (!filter || t.msgs.some(matches)));

  // ── Message column ────────────────────────────────────────
  const paintHead = () => {
    const f = sel && findFolder(sel.account, sel.folder);
    $('folder-name').textContent = f ? f.display : (sel ? sel.folder : 'Mail');
    const count = $('folder-count');
    if (!f) { count.textContent = ''; return; }
    if (f.unseen) count.textContent = `${f.unseen} unread`;
    else if (f.exists != null) count.textContent = `${f.exists} message${f.exists === 1 ? '' : 's'}`;
    else count.textContent = '';
    for (const chip of document.querySelectorAll('#views .chip')) {
      chip.classList.toggle('sel', chip.dataset.view === view);
      chip.setAttribute('aria-pressed', chip.dataset.view === view ? 'true' : 'false');
    }
  };

  const emptyNote = (title, text) => {
    const box = el('div', 'empty');
    box.appendChild(el('b', '', title));
    box.appendChild(document.createTextNode(text));
    return box;
  };

  // "Bob, Ann, Carol" — who spoke in the thread, newest first, the
  // reader's own name dropped unless they were the only one.
  const participants = (t) => {
    const own = ownAddresses();
    const seen = new Set();
    const out = [];
    for (const m of t.msgs) {
      const a = firstAddr(m.from);
      const key = a.addr.toLowerCase();
      if (!key || seen.has(key)) continue;
      seen.add(key);
      out.push(own.has(key) ? 'me' : (a.name.split(' ')[0] || a.addr));
    }
    const others = out.filter((n) => n !== 'me');
    const shown = (others.length ? others : out).slice(0, 3);
    return shown.join(', ') + ((others.length || out.length) > 3 ? '…' : '');
  };

  const paintList = () => {
    const host = $('list');
    host.textContent = '';
    if (!sel) {
      host.appendChild(emptyNote('No folder open', 'Pick one from the rail.'));
      return;
    }
    if (loading && !list.length) {
      for (let i = 0; i < 6; i++) {
        const row = el('div', 'skel');
        row.appendChild(el('span', 'a'));
        row.appendChild(el('span', 'b'));
        row.appendChild(el('span', 'c'));
        host.appendChild(row);
      }
      return;
    }
    const rows = visible();
    if (!rows.length) {
      host.appendChild(filter
        ? emptyNote('No matches', `Nothing here mentions “${$('filter').value.trim()}”.`)
        : view === 'unread' ? emptyNote('All read', 'Nothing unread in this folder.')
        : view === 'starred' ? emptyNote('No stars', 'Star a message to find it here.')
        : emptyNote('Nothing here yet', 'This folder is empty.'));
      return;
    }
    for (const t of rows) {
      const m = t.latest;
      const row = el('div', 'msg' + (t.unread ? ' unread' : '') + (t.draft ? ' draft' : '') +
        (cursor === t.key ? ' sel' : ''));
      row.setAttribute('role', 'option');
      row.setAttribute('aria-selected', cursor === t.key ? 'true' : 'false');
      row.tabIndex = -1;
      row.dataset.thread = t.key;
      row.appendChild(avatar(firstAddr(m.from)));
      const top = el('div', 'msg-top');
      top.appendChild(el('span', 'who', t.msgs.length > 1 ? participants(t) : (firstAddr(m.from).name || '(no sender)')));
      if (t.msgs.length > 1) top.appendChild(el('span', 'cnt', String(t.msgs.length)));
      top.appendChild(el('span', 'date', fmtDate(m.date)));
      row.appendChild(top);
      const sub = el('div', 'msg-sub');
      if (t.flagged) sub.appendChild(icon('star', 'star'));
      if (m.answered) sub.appendChild(icon('reply'));
      sub.appendChild(el('span', 'subj', m.subject || '(no subject)'));
      row.appendChild(sub);
      const acts = el('span', 'acts');
      acts.appendChild(iconButton('star', t.flagged ? 'Unstar' : 'Star', t.flagged ? 'on' : '', () =>
        setFlagOn(m, 'flagged', !t.flagged)));
      if (folderByRole(sel.account, 'archive') && sel.folder !== folderByRole(sel.account, 'archive').name) {
        acts.appendChild(iconButton('archive', 'Archive', '', () => moveThread(t, 'archive')));
      }
      acts.appendChild(iconButton('trash', 'Delete', '', () => deleteThread(t)));
      row.appendChild(acts);
      row.onclick = () => openThread(t);
      host.appendChild(row);
    }
  };

  // ── Opening ───────────────────────────────────────────────
  const openMsg = (m) => {
    const remote = trusted().has(firstAddr(m.from).addr.toLowerCase());
    ipc({kind: 'open', account: sel.account, folder: sel.folder, uid: m.uid, remote});
  };

  const openThread = (t) => {
    cursor = t.key;
    bodies = {};
    expanded = new Set();
    focus = t.latest.uid;
    // The newest message and whatever is unread open in full; the
    // rest wait, collapsed, for a click.
    let fetched = 0;
    for (const m of t.msgs) {
      if ((m === t.latest || !m.seen) && fetched < MAX_AUTO_OPEN) {
        expanded.add(m.uid);
        fetched++;
        if (m.draft) continue;
        openMsg(m);
      }
    }
    paintList();
    const row = $('list').querySelector(`.msg[data-thread="${CSS.escape(t.key)}"]`);
    if (row) row.scrollIntoView({block: 'nearest'});
    paintBody();
    if (t.latest.draft) openDraft(t.latest);
  };

  const closeThread = () => {
    cursor = null;
    bodies = {};
    expanded = new Set();
    focus = null;
  };

  // j/k and the arrows: one conversation along the visible list from
  // wherever the cursor is, or the top when nothing is.
  const moveCursor = (delta) => {
    const rows = visible();
    if (!rows.length) return;
    let i = rows.findIndex((t) => t.key === cursor);
    i = i < 0 ? (delta > 0 ? 0 : rows.length - 1) : Math.max(0, Math.min(rows.length - 1, i + delta));
    openThread(rows[i]);
  };

  // ── Reading pane ──────────────────────────────────────────
  const TOOLS = ['reply-btn', 'reply-all-btn', 'fwd-btn', 'read-btn', 'flag-btn', 'move-sel', 'del-btn', 'archive-btn', 'junk-btn'];
  const focused = () => (focus != null && list.find((m) => m.uid === focus)) || null;
  const focusedBody = () => (focus != null && bodies[focus]) || null;

  const remoteNotice = (body, from) => {
    const box = el('div', 'notice');
    box.appendChild(icon('shield'));
    box.appendChild(el('span', 'grow',
      `${body.blocked_remote} remote image${body.blocked_remote > 1 ? 's' : ''} blocked — the sender can't tell you opened this.`));
    const load = el('button', 'small');
    load.type = 'button';
    load.textContent = 'Load images';
    load.onclick = () => ipc({kind: 'open', account: sel.account, folder: sel.folder, uid: body.uid, remote: true});
    box.appendChild(load);
    const always = el('button', 'small ghost');
    always.type = 'button';
    always.textContent = `Always from ${from.addr}`;
    always.onclick = () => {
      trust(from.addr);
      ipc({kind: 'open', account: sel.account, folder: sel.folder, uid: body.uid, remote: true});
    };
    box.appendChild(always);
    return box;
  };

  const attachmentChips = (body) => {
    const host = el('div', 'atts');
    for (const a of body.attachments || []) {
      const chip = el('span', 'att');
      chip.appendChild(icon('clip'));
      chip.appendChild(el('span', 'name', a.name));
      chip.appendChild(el('span', 'size', fmtSize(a.size)));
      chip.appendChild(iconButton('download', `Save ${a.name}`, '', () => ipc({
        kind: 'save_attachment', account: sel.account, folder: sel.folder, uid: body.uid, index: a.index
      })));
      host.appendChild(chip);
    }
    return host;
  };

  // One message of the open thread: its header always, its body when
  // expanded. The header toggles; the toolbar follows the focus.
  const messageCard = (m, single) => {
    const card = el('article', 'm' + (expanded.has(m.uid) ? ' open' : '') + (m.uid === focus ? ' focus' : '') + (m.seen ? '' : ' unread'));
    card.dataset.uid = m.uid;
    const body = bodies[m.uid];
    const from = firstAddr(m.from);
    const head = el('header', 'm-head');
    head.appendChild(avatar(from, 'md'));
    const who = el('div', 'm-who');
    const line = el('div', 'm-from');
    line.appendChild(el('b', '', from.name || '(no sender)'));
    if (from.addr && from.addr !== from.name) line.appendChild(el('span', 'dim', ` <${from.addr}>`));
    who.appendChild(line);
    const meta = el('div', 'm-line dim');
    if (expanded.has(m.uid)) {
      meta.appendChild(document.createTextNode('to ' + (names(m.to) || '(undisclosed)')));
      if (m.cc) meta.appendChild(document.createTextNode(' · cc ' + names(m.cc)));
      if (body && body.reply_to) meta.appendChild(document.createTextNode(' · replies to ' + names(body.reply_to)));
      if (body && body.unsubscribe) {
        meta.appendChild(document.createTextNode(' · '));
        const un = el('a', 'unsub', 'Unsubscribe');
        un.href = body.unsubscribe;
        un.target = '_blank';
        un.rel = 'noopener noreferrer';
        un.onclick = (e) => e.stopPropagation();
        meta.appendChild(un);
      }
    } else {
      meta.textContent = body ? (body.text || '').replace(/\s+/g, ' ').trim().slice(0, 120) : (m.subject || '');
    }
    who.appendChild(meta);
    head.appendChild(who);
    const right = el('div', 'm-right');
    if (m.flagged) right.appendChild(icon('star', 'star'));
    right.appendChild(el('time', 'dim', expanded.has(m.uid) ? fmtFull(m.date) : fmtDate(m.date)));
    head.appendChild(right);
    head.onclick = () => {
      focus = m.uid;
      if (single) { paintBody(); return; }
      if (expanded.has(m.uid)) expanded.delete(m.uid);
      else {
        expanded.add(m.uid);
        if (!bodies[m.uid] && !m.draft) openMsg(m);
      }
      paintBody();
    };
    card.appendChild(head);
    if (!expanded.has(m.uid)) return card;
    const section = el('div', 'm-body');
    if (!body) {
      for (let i = 0; i < 3; i++) section.appendChild(el('span', 'line'));
      section.classList.add('skel-text');
    } else {
      if (body.blocked_remote && !body.remote_loaded) section.appendChild(remoteNotice(body, from));
      if ((body.attachments || []).length) section.appendChild(attachmentChips(body));
      const content = el('div', 'r-body');
      if (body.html) {
        // Sanitized Rust-side: scripts, layout weapons and — unless
        // asked — remote content never got this far. Verbatim
        // injection is the design.
        content.innerHTML = body.html;
      } else {
        content.appendChild(el('pre', '', body.text || '(empty message)'));
      }
      section.appendChild(content);
    }
    section.onclick = () => { if (focus !== m.uid) { focus = m.uid; paintBody(); } };
    card.appendChild(section);
    return card;
  };

  const paintBody = () => {
    const t = cursor && threadOf(cursor);
    document.body.classList.toggle('reading', !!t);
    const m = focused();
    TOOLS.forEach((b) => { $(b).disabled = !t || !m; });
    if (!t) {
      $('reader').hidden = true;
      $('blank').hidden = false;
      $('flag-btn').setAttribute('aria-pressed', 'false');
      return;
    }
    $('blank').hidden = true;
    $('reader').hidden = false;
    const subject = $('r-subject');
    subject.textContent = t.latest.subject || '(no subject)';
    const host = $('r-thread');
    host.textContent = '';
    // Oldest first, the way a conversation reads; the newest is open
    // and the pane lands on it.
    const chrono = [...t.msgs].reverse();
    for (const msg of chrono) host.appendChild(messageCard(msg, chrono.length === 1));
    const open = host.querySelector(`.m[data-uid="${focus}"]`);
    if (open && chrono.length > 1) open.scrollIntoView({block: 'nearest'});
    if (!m) return;
    const b = focusedBody();
    $('reply-btn').disabled = $('reply-all-btn').disabled = $('fwd-btn').disabled = !b;
    $('read-btn').title = t.unread ? 'Mark read (u)' : 'Mark unread (u)';
    $('read-glyph').setAttribute('href', t.unread ? '#i-read' : '#i-unread');
    $('flag-btn').setAttribute('aria-pressed', m.flagged ? 'true' : 'false');
    $('flag-btn').title = m.flagged ? 'Unstar (s)' : 'Star (s)';
    const archive = folderByRole(sel.account, 'archive');
    const junk = folderByRole(sel.account, 'junk');
    $('archive-btn').hidden = !archive || sel.folder === archive.name;
    $('junk-btn').hidden = !junk;
    $('junk-btn').title = sel.folder === (junk || {}).name ? 'Not junk — move to inbox (!)' : 'Junk (!)';
    const move = $('move-sel');
    move.textContent = '';
    move.appendChild(el('option', '', 'Move to…'));
    const acct = findAccount(sel.account);
    for (const f of (acct ? acct.folders : [])) {
      if (f.name === sel.folder) continue;
      const opt = el('option', '', f.display);
      opt.value = f.name;
      move.appendChild(opt);
    }
    move.selectedIndex = 0;
  };

  const closeReader = () => {
    closeThread();
    paintList();
    paintBody();
  };

  // ── Actions on messages and threads ───────────────────────
  const setFlagOn = (m, flag, on) => ipc({
    kind: 'set_flag', account: sel.account, folder: sel.folder, uid: m.uid, flag, on
  });
  const currentThread = () => cursor && threadOf(cursor);
  const moveMsgs = (msgs, target) => {
    for (const m of msgs) ipc({kind: 'move_to', account: sel.account, folder: sel.folder, uid: m.uid, target});
  };
  const moveThread = (t, role) => {
    const target = role === 'inbox' ? folderByRole(sel.account, 'inbox') : folderByRole(sel.account, role);
    if (!target) { toast(`This account has no ${role} folder.`, true); return; }
    moveMsgs(t.msgs, target.name);
    if (t.key === cursor) closeReader();
  };
  const deleteThread = (t) => {
    for (const m of t.msgs) ipc({kind: 'delete', account: sel.account, folder: sel.folder, uid: m.uid});
    if (t.key === cursor) closeReader();
  };
  const toggleThreadRead = () => {
    const t = currentThread();
    if (!t) return;
    const on = t.unread;
    for (const m of t.msgs) if (m.seen !== on) setFlagOn(m, 'seen', on);
  };

  // ── Compose ───────────────────────────────────────────────
  const quote = (prefix, text) =>
    '\n\n' + prefix + '\n' + String(text || '').split('\n').map((l) => '> ' + l).join('\n');

  const paintFiles = () => {
    const host = $('c-files');
    host.textContent = '';
    host.hidden = !files.length;
    files.forEach((f, i) => {
      const chip = el('span', 'att');
      chip.appendChild(icon('clip'));
      chip.appendChild(el('span', 'name', f.name));
      chip.appendChild(el('span', 'size', fmtSize(f.size)));
      chip.appendChild(iconButton('x', `Remove ${f.name}`, '', () => ipc({kind: 'attach_drop', index: i})));
      host.appendChild(chip);
    });
  };

  const openCompose = (opts) => {
    $('compose-title').textContent = opts.title || 'New message';
    $('c-to').value = opts.to || '';
    $('c-cc').value = opts.cc || '';
    $('c-bcc').value = '';
    $('c-bcc-row').hidden = true;
    $('c-bcc-show').hidden = false;
    $('c-subject').value = opts.subject || '';
    $('c-body').value = opts.text || '';
    $('c-error').hidden = true;
    $('c-send').disabled = false;
    const from = $('c-from');
    from.textContent = '';
    for (const a of accounts) from.appendChild(el('option', '', a.email));
    from.value = opts.account || (accounts[0] && accounts[0].email) || '';
    $('compose').hidden = false;
    $('compose')._meta = {in_reply_to: opts.in_reply_to || '', references: opts.references || ''};
    files = [];
    paintFiles();
    paintCount();
    const body = $('c-body');
    if (opts.to) {
      body.focus();
      body.setSelectionRange(0, 0);
    } else {
      $('c-to').focus();
    }
  };

  const composeDirty = () =>
    $('c-to').value.trim() || $('c-subject').value.trim() || $('c-body').value.trim() || files.length;

  const closeCompose = (force) => {
    if (!force && composeDirty() && !confirm('Discard this message?')) return;
    $('compose').hidden = true;
    if (files.length) ipc({kind: 'attach_clear'});
    files = [];
  };

  const paintCount = () => {
    const n = $('c-body').value.length;
    $('c-count').textContent = n ? `${n.toLocaleString()} characters` : '';
  };

  // Recipients of a reply: Reply-To wins over From; reply-all adds
  // everyone on To and Cc except the reader's own addresses.
  const replyPrefix = (b, all) => {
    const own = ownAddresses();
    const subj = b.subject || '';
    const answer = b.reply_to || b.from;
    const answerAddr = firstAddr(answer).addr.toLowerCase();
    const keep = (a) => !own.has(a.addr.toLowerCase()) && a.addr.toLowerCase() !== answerAddr;
    const show = (a) => a.name && a.name !== a.addr ? `${a.name} <${a.addr}>` : a.addr;
    return {
      title: all ? 'Reply all' : 'Reply',
      account: b.account || sel.account,
      to: all ? [answer, ...addrList(b.to).filter(keep).map(show)].join(', ') : answer,
      cc: all ? addrList(b.cc).filter(keep).map(show).join(', ') : '',
      subject: /^re:/i.test(subj) ? subj : 'Re: ' + subj,
      text: quote(`On ${fmtFull(b.date)}, ${b.from} wrote:`, b.text),
      in_reply_to: b.message_id || '',
      references: b.references || b.message_id || '',
    };
  };

  const forwardOpts = (b) => ({
    title: 'Forward',
    account: sel.account,
    subject: /^fwd?:/i.test(b.subject || '') ? b.subject : 'Fwd: ' + (b.subject || ''),
    text: quote(`--- Forwarded message ---\nFrom: ${b.from}\nDate: ${fmtFull(b.date)}\nSubject: ${b.subject || ''}\nTo: ${b.to}`, b.text),
  });

  const openDraft = (m) => {
    openCompose({
      title: 'Draft',
      account: sel.account,
      to: m.to, subject: m.subject,
      text: '',
    });
  };

  const send = () => {
    if ($('c-send').disabled) return;
    if (!$('c-to').value.trim()) {
      $('c-error').textContent = 'Add at least one recipient.';
      $('c-error').hidden = false;
      $('c-to').focus();
      return;
    }
    const meta = $('compose')._meta || {};
    const seq = ++sendSeq;
    $('c-send').disabled = true;
    $('c-error').hidden = true;
    ipc({
      kind: 'send', account: $('c-from').value, to: $('c-to').value, cc: $('c-cc').value,
      bcc: $('c-bcc').value, subject: $('c-subject').value, text: $('c-body').value,
      in_reply_to: meta.in_reply_to, references: meta.references
    });
    setTimeout(() => { if (sendSeq === seq) $('c-send').disabled = false; }, 30000);
  };

  // ── Onboarding ────────────────────────────────────────────
  const showSetup = (withBack) => {
    $('app').hidden = true;
    $('setup').hidden = false;
    $('su-back').hidden = !withBack;
    $('su-error').hidden = true;
    $('su-email').focus();
  };
  const resetSetup = () => {
    for (const id of ['su-name', 'su-email', 'su-pass', 'su-user', 'su-imap-host', 'su-smtp-host']) $(id).value = '';
    $('su-imap-port').value = '993';
    $('su-smtp-port').value = '465';
    $('su-servers').hidden = true;
  };

  // ── The push entry point ──────────────────────────────────
  window.__cryo_mail = (msg) => {
    switch (msg.kind) {
      case 'state': {
        if (msg.incognito) {
          $('off').hidden = false;
          return;
        }
        accounts = msg.accounts || [];
        for (const a of accounts) statusByEmail[a.email] = a.status || '';
        if (sel && !findAccount(sel.account)) {
          sel = null;
          list = [];
          closeThread();
        }
        if (!accounts.length) {
          showSetup(false);
          paintFolders();
          return;
        }
        $('setup').hidden = true;
        $('app').hidden = false;
        paintFolders();
        paintHead();
        if (!sel) {
          const a = accounts[0];
          const inbox = (a.folders || []).find((f) => f.role === 'inbox') || (a.folders || [])[0];
          if (inbox) selectFolder(a.email, inbox.name);
          else paintList();
        }
        break;
      }
      case 'list': {
        if (!sel || sel.account !== msg.account || sel.folder !== msg.folder) break;
        list = msg.envelopes || [];
        loading = false;
        if (cursor && !threadOf(cursor)) closeThread();
        paintList();
        paintBody();
        break;
      }
      case 'body': {
        if (!sel || sel.account !== msg.account || sel.folder !== msg.folder) break;
        const row = list.find((e) => e.uid === msg.uid);
        if (row && !row.seen) { row.seen = true; paintList(); }
        if (!row || row.thread !== cursor) break;
        bodies[msg.uid] = msg;
        expanded.add(msg.uid);
        paintBody();
        break;
      }
      case 'flag': {
        if (!sel || sel.account !== msg.account || sel.folder !== msg.folder) break;
        const row = list.find((e) => e.uid === msg.uid);
        const prop = {seen: 'seen', flagged: 'flagged', answered: 'answered', draft: 'draft', deleted: 'deleted'}[msg.flag];
        if (row && prop) { row[prop] = msg.on; paintList(); }
        if (bodies[msg.uid] && prop) bodies[msg.uid][prop] = msg.on;
        paintBody();
        break;
      }
      case 'status': {
        const was = statusByEmail[msg.account];
        statusByEmail[msg.account] = msg.status || '';
        if (was === 'syncing' && !msg.status) lastSynced = Date.now();
        paintFolders();
        if (msg.status && msg.status !== 'syncing') toast(`${msg.account}: ${msg.status}`, true);
        break;
      }
      case 'account_reply': {
        const btn = $('su-go');
        btn.disabled = false;
        btn.textContent = 'Add account';
        if (msg.error) {
          $('su-error').textContent = msg.error;
          $('su-error').hidden = false;
        } else {
          $('su-error').hidden = true;
          resetSetup();
          toast('Account added.');
        }
        break;
      }
      case 'send_reply': {
        sendSeq++;
        if (msg.error) {
          $('c-error').textContent = msg.error + (files.length ? ' — re-attach your files before trying again.' : '');
          $('c-error').hidden = false;
          $('c-send').disabled = false;
          files = [];
          paintFiles();
        } else {
          files = [];
          closeCompose(true);
          toast('Sent.');
        }
        break;
      }
      case 'attachments': {
        files = msg.files || [];
        paintFiles();
        break;
      }
      case 'guess': {
        if (msg.imap_host) {
          $('su-imap-host').value = msg.imap_host;
          $('su-imap-port').value = msg.imap_port;
          $('su-imap-tls').value = msg.imap_tls;
          $('su-smtp-host').value = msg.smtp_host;
          $('su-smtp-port').value = msg.smtp_port;
          $('su-smtp-tls').value = msg.smtp_tls;
        }
        break;
      }
      case 'attachment_saved': {
        if (msg.error) toast(`Could not save ${msg.name}: ${msg.error}`, true);
        else toast(`Saved ${msg.name}`);
        break;
      }
      case 'error': {
        toast(msg.detail ? `${msg.op}: ${msg.detail}` : msg.op, true);
        break;
      }
    }
  };

  // ── Wiring ────────────────────────────────────────────────
  $('compose-btn').onclick = () => openCompose({});
  $('sync-btn').onclick = () => ipc({kind: 'sync'});
  $('add-account').onclick = () => showSetup(true);
  $('back-btn').onclick = closeReader;
  $('reply-btn').onclick = () => focusedBody() && openCompose(replyPrefix(focusedBody(), false));
  $('reply-all-btn').onclick = () => focusedBody() && openCompose(replyPrefix(focusedBody(), true));
  $('fwd-btn').onclick = () => focusedBody() && openCompose(forwardOpts(focusedBody()));
  $('read-btn').onclick = toggleThreadRead;
  $('flag-btn').onclick = () => { const m = focused(); if (m) setFlagOn(m, 'flagged', !m.flagged); };
  $('archive-btn').onclick = () => { const t = currentThread(); if (t) moveThread(t, 'archive'); };
  $('junk-btn').onclick = () => {
    const t = currentThread();
    if (!t) return;
    const junk = folderByRole(sel.account, 'junk');
    moveThread(t, junk && sel.folder === junk.name ? 'inbox' : 'junk');
  };
  $('move-sel').onchange = (e) => {
    const t = currentThread();
    if (!t || !e.target.value || e.target.selectedIndex === 0) return;
    moveMsgs(t.msgs, e.target.value);
    e.target.selectedIndex = 0;
    closeReader();
  };
  $('del-btn').onclick = () => { const t = currentThread(); if (t) deleteThread(t); };

  $('filter').addEventListener('input', (e) => {
    filter = e.target.value.trim().toLowerCase();
    paintList();
  });
  for (const chip of document.querySelectorAll('#views .chip')) {
    chip.onclick = () => { view = chip.dataset.view; paintHead(); paintList(); };
  }

  $('c-send').onclick = send;
  $('c-body').addEventListener('input', paintCount);
  $('c-attach').onclick = () => ipc({kind: 'attach_pick'});
  $('c-bcc-show').onclick = () => {
    $('c-bcc-row').hidden = false;
    $('c-bcc-show').hidden = true;
    $('c-bcc').focus();
  };
  $('c-draft').onclick = () => {
    ipc({
      kind: 'save_draft', account: $('c-from').value, to: $('c-to').value, cc: $('c-cc').value,
      bcc: $('c-bcc').value, subject: $('c-subject').value, text: $('c-body').value
    });
    files = [];
    closeCompose(true);
    toast('Draft saved.');
  };
  $('c-cancel').onclick = () => closeCompose(false);

  // Keys: the single letters every mail client answers to, only while
  // nothing is being typed. Modified keys belong to the chrome.
  document.addEventListener('keydown', (e) => {
    const composing = !$('compose').hidden;
    if (composing) {
      if (e.key === 'Escape') { e.preventDefault(); closeCompose(false); }
      else if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); send(); }
      return;
    }
    if (e.ctrlKey || e.altKey || e.metaKey) return;
    const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName);
    if (e.key === 'Escape') {
      if (typing && e.target === $('filter')) {
        if (filter) { e.target.value = ''; filter = ''; paintList(); }
        else e.target.blur();
      } else if (!typing && cursor) {
        closeReader();
      }
      return;
    }
    if (typing || $('app').hidden) return;
    const t = currentThread();
    const b = focusedBody();
    const m = focused();
    switch (e.key) {
      case 'j': case 'ArrowDown': moveCursor(1); break;
      case 'k': case 'ArrowUp': moveCursor(-1); break;
      case 'c': openCompose({}); break;
      case 'r': if (b) openCompose(replyPrefix(b, false)); break;
      case 'a': if (b) openCompose(replyPrefix(b, true)); break;
      case 'f': if (b) openCompose(forwardOpts(b)); break;
      case 's': if (m) setFlagOn(m, 'flagged', !m.flagged); break;
      case 'u': toggleThreadRead(); break;
      case 'e': if (t) moveThread(t, 'archive'); break;
      case '!': if (t) $('junk-btn').click(); break;
      case 'Delete': case '#': if (t) deleteThread(t); break;
      case '/': $('filter').focus(); break;
      default: return;
    }
    e.preventDefault();
  });

  // Onboarding: guess the servers once the address looks real.
  let guessTimer = 0;
  $('su-email').addEventListener('input', () => {
    clearTimeout(guessTimer);
    guessTimer = setTimeout(() => {
      const email = $('su-email').value.trim();
      if (email.includes('@') && !email.endsWith('@')) {
        ipc({kind: 'guess', email});
        $('su-servers').hidden = false;
      }
    }, 400);
  });
  $('su-adv').onclick = () => { $('su-servers').hidden = false; };
  $('su-back').onclick = () => {
    $('setup').hidden = true;
    $('app').hidden = false;
  };
  $('su-form').addEventListener('submit', (e) => {
    e.preventDefault();
    const btn = $('su-go');
    btn.disabled = true;
    btn.textContent = 'Checking…';
    ipc({
      kind: 'add_account',
      display: $('su-name').value, email: $('su-email').value,
      password: $('su-pass').value, username: $('su-user').value,
      imap_host: $('su-imap-host').value, imap_port: +$('su-imap-port').value || 993,
      imap_tls: $('su-imap-tls').value,
      smtp_host: $('su-smtp-host').value, smtp_port: +$('su-smtp-port').value || 465,
      smtp_tls: $('su-smtp-tls').value
    });
  });

  ipc({kind: 'hello'});
})();
