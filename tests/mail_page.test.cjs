const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { JSDOM } = require('jsdom');

const PAGE = fs.readFileSync(
  path.join(__dirname, '../src/shell/mail/mail.html'), 'utf8');
const PAGE_JS = fs.readFileSync(
  path.join(__dirname, '../src/shell/mail/mail.js'), 'utf8');

// The page on its own origin, with window.ipc capturing every command
// it sends. Pushes go in through window.__cryo_mail, as the app evals
// them.
function mail(t) {
  const sent = [];
  const { window } = new JSDOM(PAGE, {
    url: 'http://cryo.localhost/mail',
    runScripts: 'outside-only',
    pretendToBeVisual: true,
  });
  t.after(() => window.close());
  window.ipc = { postMessage: (body) => sent.push(JSON.parse(body)) };
  window.confirm = () => true;
  window.HTMLElement.prototype.scrollIntoView = () => {};
  window.eval(PAGE_JS);
  return { window, sent, push: (msg) => window.__cryo_mail(msg) };
}

const ANN = { email: 'ann@example.com', display: 'Ann Rivers', status: '', folders: [
  { name: 'INBOX', display: 'Inbox', role: 'inbox', unseen: 2, exists: 4 },
  { name: 'Sent', display: 'Sent', role: 'sent' },
  { name: 'Archive', display: 'Archive', role: 'archive' },
  { name: 'Junk', display: 'Junk', role: 'junk' },
  { name: 'Trash', display: 'Trash', role: 'trash' },
] };

const row = (uid, from, subject, date, thread, extra) => Object.assign({
  uid, from, to: 'ann@example.com', cc: '', subject, date, size: 10,
  seen: true, flagged: false, answered: false, draft: false, thread,
}, extra);

// Two conversations: "Invoice" is three messages (two unread), "Lunch?"
// is one.
const ROWS = [
  row(4, 'Bob Stone <bob@example.com>', 'Re: Invoice', 1700000400, '1', { seen: false }),
  row(3, 'Ann Rivers <ann@example.com>', 'Re: Invoice', 1700000300, '1'),
  row(2, 'carol@example.com', 'Lunch?', 1700000200, '2', { answered: true }),
  row(1, 'Bob Stone <bob@example.com>', 'Invoice', 1700000100, '1', { seen: false, flagged: true }),
];

const body = (uid, extra) => Object.assign({
  kind: 'body', account: ANN.email, folder: 'INBOX', uid,
  from: 'Bob Stone <bob@example.com>', to: 'Ann Rivers <ann@example.com>', cc: '',
  subject: 'Re: Invoice', date: 1700000400, html: null, text: 'body ' + uid, blocked_remote: 0,
  remote_loaded: false, attachments: [], reply_to: '', unsubscribe: '',
  message_id: `<m${uid}@x>`, references: '<m1@x> <m3@x>', seen: true, flagged: false,
}, extra);

const rowsOf = (window) => [...window.document.querySelectorAll('#list .msg')];
const whos = (window) => rowsOf(window).map((r) => r.querySelector('.who').textContent);
const key = (window, k, extra) => window.document.dispatchEvent(
  new window.KeyboardEvent('keydown', Object.assign({ key: k, bubbles: true, cancelable: true }, extra)));

function opened(t) {
  const m = mail(t);
  m.push({ kind: 'state', incognito: false, accounts: [ANN] });
  // Rows are copied per test: the page marks them read in place.
  m.push({ kind: 'list', account: ANN.email, folder: 'INBOX', envelopes: ROWS.map((r) => ({ ...r })) });
  return m;
}

test('the page says hello and shows onboarding until an account exists', (t) => {
  const { window, sent, push } = mail(t);
  assert.deepEqual(sent, [{ kind: 'hello' }]);
  push({ kind: 'state', incognito: false, accounts: [] });
  assert.ok(!window.document.getElementById('setup').hidden, 'onboarding is up');
  assert.ok(window.document.getElementById('app').hidden, 'the app is not');
  assert.ok(window.document.getElementById('su-back').hidden, 'nowhere to go back to');
});

test('incognito gets its own notice, never the form', (t) => {
  const { window, push } = mail(t);
  push({ kind: 'state', incognito: true, accounts: [] });
  assert.ok(!window.document.getElementById('off').hidden);
  assert.ok(window.document.getElementById('setup').hidden);
  assert.ok(window.document.getElementById('app').hidden);
});

test('the first state opens the inbox, and the rail marks it', (t) => {
  const { window, sent, push } = mail(t);
  push({ kind: 'state', incognito: false, accounts: [ANN] });
  assert.deepEqual(sent.at(-1), { kind: 'select_folder', account: ANN.email, folder: 'INBOX' });
  assert.equal(window.document.querySelector('.fold.sel .name').textContent, 'Inbox');
  assert.equal(window.document.getElementById('folder-name').textContent, 'Inbox');
  assert.equal(window.document.getElementById('folder-count').textContent, '2 unread');
  assert.ok(window.document.querySelector('#list .skel'), 'the column waits with placeholders');
});

test('a listing is grouped into conversations, newest first', (t) => {
  const { window } = opened(t);
  const rows = rowsOf(window);
  assert.equal(rows.length, 2, 'four messages, two conversations');
  assert.deepEqual(whos(window), ['Bob', 'carol@example.com']);
  assert.equal(rows[0].querySelector('.cnt').textContent, '3');
  assert.ok(rows[0].classList.contains('unread'));
  assert.ok(rows[0].querySelector('.ic.star'), 'a star anywhere in the thread shows on the row');
  assert.equal(rows[0].querySelector('.subj').textContent, 'Re: Invoice');
  assert.ok(!rows[1].classList.contains('unread'));
  assert.ok(!rows[1].querySelector('.cnt'), 'a lone message has no count');
});

test('the view chips narrow to unread or starred conversations', (t) => {
  const { window } = opened(t);
  const chip = (v) => window.document.querySelector(`#views .chip[data-view="${v}"]`);
  chip('unread').click();
  assert.deepEqual(whos(window), ['Bob']);
  chip('starred').click();
  assert.deepEqual(whos(window), ['Bob']);
  chip('all').click();
  assert.equal(rowsOf(window).length, 2);
});

test('opening a conversation fetches the newest and the unread, and stacks the bodies', (t) => {
  const { window, sent, push } = opened(t);
  rowsOf(window)[0].click();
  const opens = sent.filter((m) => m.kind === 'open').map((m) => m.uid);
  assert.deepEqual(opens, [4, 1], 'newest plus unread; the read middle one waits');
  assert.equal(sent.at(-1).remote, false, 'remote images are off until asked');
  assert.ok(rowsOf(window)[0].classList.contains('sel'));
  const d = window.document;
  assert.equal(d.getElementById('r-subject').textContent, 'Re: Invoice');
  const cards = [...d.querySelectorAll('#r-thread .m')];
  assert.deepEqual(cards.map((c) => c.dataset.uid), ['1', '3', '4'], 'oldest first');
  assert.deepEqual(cards.map((c) => c.classList.contains('open')), [true, false, true]);
  assert.ok(cards[2].querySelector('.skel-text'), 'the open body waits with placeholders');

  push(body(4));
  push(body(1, { subject: 'Invoice', text: 'first', flagged: true,
    attachments: [{ index: 0, name: 'inv.pdf', mime: 'application/pdf', size: 20480 }] }));
  const after = [...d.querySelectorAll('#r-thread .m')];
  assert.equal(after[2].querySelector('.r-body pre').textContent, 'body 4');
  assert.equal(after[0].querySelector('.r-body pre').textContent, 'first');
  assert.equal(after[0].querySelector('.att .name').textContent, 'inv.pdf');
  assert.ok(!d.getElementById('reply-btn').disabled, 'the newest is focused and replyable');
  assert.ok(!rowsOf(window)[0].classList.contains('unread'), 'opened bodies read their rows');

  // The collapsed middle message opens on click and asks for its body.
  after[1].querySelector('.m-head').click();
  assert.deepEqual(sent.at(-1), { kind: 'open', account: ANN.email, folder: 'INBOX', uid: 3, remote: false });
  assert.ok(d.querySelector('#r-thread .m[data-uid="3"]').classList.contains('open'));
});

test('blocked remote images offer a one-off load and a remembered sender', (t) => {
  const { window, sent, push } = opened(t);
  rowsOf(window)[1].click();
  push(body(2, { from: 'carol@example.com', subject: 'Lunch?', blocked_remote: 3 }));
  const d = window.document;
  const notice = d.querySelector('#r-thread .notice');
  assert.match(notice.textContent, /3 remote images blocked/);
  notice.querySelectorAll('button')[0].click();
  assert.deepEqual(sent.at(-1), { kind: 'open', account: ANN.email, folder: 'INBOX', uid: 2, remote: true });
  notice.querySelectorAll('button')[1].click();
  assert.deepEqual(JSON.parse(window.localStorage.getItem('cryo.mail.remote')), ['carol@example.com']);
  push(body(2, { from: 'carol@example.com', subject: 'Lunch?', blocked_remote: 0, remote_loaded: true }));
  assert.ok(!d.querySelector('#r-thread .notice'), 'loaded content has no notice');
  // Next time this sender's message opens, remote goes along from the start.
  window.document.getElementById('back-btn').click();
  rowsOf(window)[1].click();
  assert.equal(sent.at(-1).remote, true);
});

test('reply honours Reply-To, reply-all drops the reader, forward quotes the headers', (t) => {
  const { window, push } = opened(t);
  rowsOf(window)[0].click();
  push(body(4, { reply_to: 'Desk <desk@example.com>', to: 'Ann Rivers <ann@example.com>, Dan <dan@example.com>',
    cc: 'carol@example.com', unsubscribe: 'https://x.test/unsub' }));
  const d = window.document;
  assert.equal(d.querySelector('#r-thread .m[data-uid="4"] .unsub').href, 'https://x.test/unsub');
  key(window, 'r');
  assert.equal(d.getElementById('c-to').value, 'Desk <desk@example.com>');
  assert.equal(d.getElementById('c-subject').value, 'Re: Invoice');
  assert.equal(d.getElementById('compose')._meta.references, '<m1@x> <m3@x>');
  assert.equal(d.getElementById('compose')._meta.in_reply_to, '<m4@x>');
  key(window, 'Escape');
  key(window, 'a');
  assert.equal(d.getElementById('c-to').value, 'Desk <desk@example.com>, Dan <dan@example.com>');
  assert.equal(d.getElementById('c-cc').value, 'carol@example.com');
  key(window, 'Escape');
  key(window, 'f');
  assert.equal(d.getElementById('c-subject').value, 'Fwd: Re: Invoice');
  assert.match(d.getElementById('c-body').value, /From: Bob Stone <bob@example.com>/);
});

test('archive, junk and delete act on the whole conversation', (t) => {
  const { window, sent } = opened(t);
  rowsOf(window)[0].click();
  key(window, 'e');
  const moves = sent.filter((m) => m.kind === 'move_to');
  assert.deepEqual(moves.map((m) => [m.uid, m.target]), [[4, 'Archive'], [3, 'Archive'], [1, 'Archive']]);
  assert.ok(window.document.getElementById('reader').hidden, 'the pane closes behind the move');
  rowsOf(window)[1].click();
  key(window, '!');
  assert.deepEqual(sent.at(-1), { kind: 'move_to', account: ANN.email, folder: 'INBOX', uid: 2, target: 'Junk' });
  rowsOf(window)[0].click();
  key(window, '#');
  assert.deepEqual(sent.filter((m) => m.kind === 'delete').map((m) => m.uid), [4, 3, 1]);
});

test('row hover actions star and delete without opening', (t) => {
  const { window, sent } = opened(t);
  const acts = rowsOf(window)[1].querySelectorAll('.acts button');
  acts[0].click();
  assert.deepEqual(sent.at(-1), { kind: 'set_flag', account: ANN.email, folder: 'INBOX', uid: 2, flag: 'flagged', on: true });
  assert.ok(window.document.getElementById('reader').hidden, 'a row action is not an open');
  acts[2].click();
  assert.deepEqual(sent.at(-1), { kind: 'delete', account: ANN.email, folder: 'INBOX', uid: 2 });
});

test('j and k walk conversations; u toggles the whole thread read', (t) => {
  const { window, sent } = opened(t);
  const selected = () => rowsOf(window).findIndex((r) => r.classList.contains('sel'));
  key(window, 'j');
  assert.equal(selected(), 0);
  key(window, 'j');
  assert.equal(selected(), 1);
  assert.equal(sent.filter((m) => m.kind === 'open').at(-1).uid, 2);
  key(window, 'k');
  assert.equal(selected(), 0);
  key(window, 'u');
  const flags = sent.filter((m) => m.kind === 'set_flag' && m.flag === 'seen');
  assert.deepEqual(flags.map((m) => [m.uid, m.on]), [[4, true], [1, true]],
    'an unread thread becomes read; rows already read are left alone');
});

test('compose carries Bcc and picked files, and refuses an empty To', (t) => {
  const { window, sent, push } = opened(t);
  const d = window.document;
  d.getElementById('compose-btn').click();
  d.getElementById('c-send').click();
  assert.ok(!d.getElementById('c-error').hidden, 'nothing goes without a recipient');
  d.getElementById('c-attach').click();
  assert.deepEqual(sent.at(-1), { kind: 'attach_pick' });
  push({ kind: 'attachments', files: [{ name: 'notes.txt', size: 2048 }, { name: 'pic.png', size: 40960 }] });
  const chips = [...d.querySelectorAll('#c-files .att .name')].map((n) => n.textContent);
  assert.deepEqual(chips, ['notes.txt', 'pic.png']);
  d.querySelectorAll('#c-files .att button')[0].click();
  assert.deepEqual(sent.at(-1), { kind: 'attach_drop', index: 0 });
  d.getElementById('c-bcc-show').click();
  assert.ok(!d.getElementById('c-bcc-row').hidden);
  d.getElementById('c-to').value = 'x@example.com';
  d.getElementById('c-bcc').value = 'quiet@example.com';
  key(window, 'Enter', { ctrlKey: true });
  assert.equal(sent.at(-1).kind, 'send');
  assert.equal(sent.at(-1).bcc, 'quiet@example.com');
  push({ kind: 'send_reply', error: '' });
  assert.ok(d.getElementById('compose').hidden);
});

test('discarding a compose with files tells the app to drop them', (t) => {
  const { window, sent, push } = opened(t);
  window.document.getElementById('compose-btn').click();
  push({ kind: 'attachments', files: [{ name: 'a.txt', size: 1 }] });
  key(window, 'Escape');
  assert.deepEqual(sent.at(-1), { kind: 'attach_clear' });
  assert.ok(window.document.getElementById('compose').hidden);
});

test('the filter narrows conversations by any message in them', (t) => {
  const { window } = opened(t);
  const filter = window.document.getElementById('filter');
  filter.value = 'carol';
  filter.dispatchEvent(new window.Event('input', { bubbles: true }));
  assert.deepEqual(whos(window), ['carol@example.com']);
  filter.value = 'Invoice';
  filter.dispatchEvent(new window.Event('input', { bubbles: true }));
  assert.deepEqual(whos(window), ['Bob']);
  filter.focus();
  filter.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  assert.equal(rowsOf(window).length, 2);
});

test('status pushes drive the rail dot and the footer line', (t) => {
  const { window, push } = opened(t);
  push({ kind: 'status', account: ANN.email, status: 'syncing' });
  assert.ok(window.document.querySelector('.acct-head .dot.busy'));
  assert.equal(window.document.getElementById('status').textContent, 'Syncing…');
  push({ kind: 'status', account: ANN.email, status: '' });
  assert.match(window.document.getElementById('status').textContent, /^Synced /);
  push({ kind: 'status', account: ANN.email, status: 'login failed' });
  assert.ok(window.document.querySelector('.acct-head .dot.bad'));
  assert.ok(window.document.getElementById('toast').classList.contains('bad'));
});

test('the onboarding form submits what the wire expects and guesses servers', async (t) => {
  const { window, sent, push } = mail(t);
  push({ kind: 'state', incognito: false, accounts: [] });
  const d = window.document;
  d.getElementById('su-email').value = 'ann@example.com';
  d.getElementById('su-email').dispatchEvent(new window.Event('input', { bubbles: true }));
  await new Promise((done) => setTimeout(done, 450));
  assert.deepEqual(sent.at(-1), { kind: 'guess', email: 'ann@example.com' });
  push({ kind: 'guess', imap_host: 'imap.example.com', imap_port: 993, imap_tls: 'tls',
         smtp_host: 'smtp.example.com', smtp_port: 587, smtp_tls: 'starttls' });
  assert.equal(d.getElementById('su-smtp-port').value, '587');
  d.getElementById('su-pass').value = 'hunter2';
  d.getElementById('su-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  const add = sent.at(-1);
  assert.equal(add.kind, 'add_account');
  assert.equal(add.password, 'hunter2');
  push({ kind: 'account_reply', error: 'imap login: no' });
  assert.equal(d.getElementById('su-error').textContent, 'imap login: no');
  assert.ok(!d.getElementById('su-go').disabled);
});

const PLUS = { ...ANN, imap: 'mail.a.test:993', smtp: 'mail.a.test:465', username: 'ann@example.com',
  signature: '<p>Ann <b>Rivers</b></p>', photo: '' };

test('your own folders nest under their parents, after the specials', (t) => {
  const { window, push } = mail(t);
  const folder = (name, display, path, depth, role = 'other', selectable = true) =>
    ({ name, display, path, depth, role, selectable });
  push({ kind: 'state', incognito: false, accounts: [{ ...ANN, folders: [
    folder('INBOX.Zeta', 'Zeta', 'Zeta', 0),
    folder('INBOX.Clients.Hotels', 'Hotels', 'Clients/Hotels', 1),
    folder('INBOX.Clients', 'Clients', 'Clients', 0, 'other', false),
    folder('INBOX.Trash', 'Trash', 'Trash', 0, 'trash'),
    folder('INBOX', 'Inbox', 'INBOX', 0, 'inbox'),
  ] }] });
  const rail = [...window.document.querySelectorAll('#accounts .fold')];
  assert.deepEqual(rail.map((r) => r.querySelector('.name').textContent),
    ['Inbox', 'Trash', 'Clients', 'Hotels', 'Zeta']);
  assert.ok(rail[2].disabled, 'a parent that holds no mail is not a target');
  assert.ok(parseInt(rail[3].style.paddingLeft, 10) > parseInt(rail[4].style.paddingLeft, 10),
    'the child is indented');
  assert.equal(window.document.querySelector('.fold-label').textContent, 'Folders');
});

test('a parent with no mail is never opened, even when it lists first', (t) => {
  const { sent, push } = mail(t);
  push({ kind: 'state', incognito: false, accounts: [{ ...ANN, folders: [
    { name: 'Parent', display: 'Parent', path: 'Parent', depth: 0, role: 'other', selectable: false },
    { name: 'Notes', display: 'Notes', path: 'Notes', depth: 0, role: 'other', selectable: true },
  ] }] });
  assert.deepEqual(sent.filter((m) => m.kind === 'select_folder').map((m) => m.folder), ['Notes']);
});

test('html mail sits on paper; plain text stays on the theme', (t) => {
  const { window, push } = opened(t);
  rowsOf(window)[1].click();
  push(body(2, { html: '<p style="color:#000">dark words</p>', from: 'carol@example.com', subject: 'Lunch?' }));
  assert.ok(window.document.querySelector('.r-body').classList.contains('paper'));
  rowsOf(window)[0].click();
  push(body(4));
  assert.ok(!window.document.querySelector('.r-body').classList.contains('paper'));
});

test('a sync finishing leaves the add-account form where it is', (t) => {
  const { window, push } = opened(t);
  window.document.getElementById('add-account').click();
  assert.ok(!window.document.getElementById('setup').hidden);
  push({ kind: 'state', incognito: false, accounts: [ANN] });
  assert.ok(!window.document.getElementById('setup').hidden, 'the form stayed up');
  assert.ok(window.document.getElementById('app').hidden);
  push({ kind: 'account_reply', error: '' });
  assert.ok(window.document.getElementById('setup').hidden, 'success is what closes it');
  assert.ok(!window.document.getElementById('app').hidden);
});

test('the rail says when the last sync finished', (t) => {
  const { window, push } = opened(t);
  push({ kind: 'status', account: ANN.email, status: 'syncing' });
  push({ kind: 'state', incognito: false, accounts: [ANN] });
  assert.match(window.document.getElementById('status').textContent, /^Synced /);
});

test('senders are looked up once and wear their picture when it comes', (t) => {
  const { window, sent, push } = opened(t);
  const asks = () => sent.filter((m) => m.kind === 'faces');
  const asked = asks().flatMap((m) => m.addrs);
  assert.ok(asked.includes('bob@example.com'));
  assert.ok(asked.includes('carol@example.com'));
  assert.equal(new Set(asked).size, asked.length, 'each address once');
  const before = asks().length;
  push({ kind: 'list', account: ANN.email, folder: 'INBOX', envelopes: ROWS.map((r) => ({ ...r })) });
  assert.equal(asks().length, before, 'a repaint asks nothing new');
  push({ kind: 'face', addr: 'carol@example.com', src: 'data:image/png;base64,AA' });
  const carol = rowsOf(window).find((r) => r.querySelector('.who').textContent === 'carol@example.com');
  assert.equal(carol.querySelector('.avatar img').getAttribute('src'), 'data:image/png;base64,AA');
});

test('your own picture needs no lookup and beats one', (t) => {
  const { window, sent, push } = mail(t);
  push({ kind: 'state', incognito: false, accounts: [{ ...PLUS, photo: 'data:image/jpeg;base64,ME' }] });
  assert.equal(window.document.querySelector('.acct-head .avatar img').getAttribute('src'),
    'data:image/jpeg;base64,ME');
  assert.ok(!sent.filter((m) => m.kind === 'faces').flatMap((m) => m.addrs).includes(ANN.email));
});

test('turning lookups off takes their pictures down and asks only for chosen ones', (t) => {
  const { window, sent, push } = opened(t);
  push({ kind: 'face', addr: 'bob@example.com', src: 'data:image/png;base64,AA' });
  assert.ok(window.document.querySelector('#list .avatar img'));
  window.document.querySelector('.acct-head .gear').click();
  const toggle = window.document.getElementById('st-pictures');
  assert.ok(toggle.checked, 'on until someone says otherwise');
  toggle.checked = false;
  toggle.onchange({ target: toggle });
  assert.ok(!window.document.querySelector('#list .avatar img'));
  push({ kind: 'list', account: ANN.email, folder: 'INBOX', envelopes: [row(9, 'new@example.com', 'x', 1700000500, '9')] });
  const ask = sent.at(-1);
  assert.deepEqual(ask, { kind: 'faces', addrs: ['new@example.com'], lookup: false });
  toggle.checked = true;
  toggle.onchange({ target: toggle });
  assert.deepEqual(sent.at(-1).lookup, true, 'turned back on, the lookups owed go out');
  assert.ok(sent.at(-1).addrs.includes('new@example.com'));
});

test('a picture you chose for a sender stands with lookups off, and can be removed', (t) => {
  const { window, sent, push } = opened(t);
  const $ = (id) => window.document.getElementById(id);
  const toggle = () => {
    window.document.querySelector('.acct-head .gear').click();
    $('st-pictures').checked = false;
    $('st-pictures').onchange({ target: $('st-pictures') });
    $('st-cancel').click();
  };
  toggle();
  push({ kind: 'face', addr: 'BOB@example.com', src: 'data:image/png;base64,MINE', chosen: true });
  const bob = () => rowsOf(window).find((r) => r.querySelector('.who').textContent.startsWith('Bob'));
  assert.equal(bob().querySelector('.avatar img').getAttribute('src'), 'data:image/png;base64,MINE');
  rowsOf(window)[0].click();
  push(body(4));
  const disc = window.document.querySelector('.m.open .m-head .avatar');
  assert.ok(disc);
  disc.click();
  assert.ok(!$('face-menu').hidden);
  assert.ok(!$('face-clear').hidden, 'there is something of yours to remove');
  $('face-clear').click();
  assert.ok($('face-menu').hidden);
  assert.deepEqual(sent.at(-1), { kind: 'set_face', addr: 'bob@example.com', photo: '' });
  push({ kind: 'face', addr: 'bob@example.com', src: '', chosen: true });
  assert.ok(!bob().querySelector('.avatar img'));
});

test('your own disc in a message opens settings, not the sender menu', (t) => {
  const { window, push } = mail(t);
  const $ = (id) => window.document.getElementById(id);
  push({ kind: 'state', incognito: false, accounts: [PLUS] });
  push({ kind: 'list', account: ANN.email, folder: 'INBOX', envelopes: [row(3, 'Ann Rivers <ann@example.com>', 'x', 1700000300, '3')] });
  rowsOf(window)[0].click();
  push(body(3, { from: 'Ann Rivers <ann@example.com>' }));
  window.document.querySelector('.m.open .m-head .avatar').click();
  assert.ok($('face-menu').hidden);
  assert.ok(!$('settings').hidden);
});

function settings(t) {
  const m = mail(t);
  m.push({ kind: 'state', incognito: false, accounts: [PLUS] });
  m.window.document.querySelector('.acct-head .gear').click();
  return m;
}

test('settings shows the account, and Save sends what was edited', (t) => {
  const { window, sent, push } = settings(t);
  const $ = (id) => window.document.getElementById(id);
  assert.ok(!$('settings').hidden);
  assert.equal($('st-name').value, 'Ann Rivers');
  assert.equal($('st-sig').innerHTML, '<p>Ann <b>Rivers</b></p>');
  assert.equal($('st-imap').textContent, 'mail.a.test:993');
  $('st-name').value = 'Ann R';
  $('st-sig').innerHTML = '<p>New</p>';
  $('st-save').click();
  assert.deepEqual(sent.at(-1), { kind: 'save_profile', email: ANN.email, display: 'Ann R',
    signature: '<p>New</p>', photo: '' });
  push({ kind: 'profile_reply', error: 'too large' });
  assert.equal($('st-error').textContent, 'too large');
  assert.ok(!$('settings').hidden, 'a refusal stays on the page');
  $('st-save').click();
  push({ kind: 'profile_reply', error: '' });
  assert.ok($('settings').hidden);
  assert.ok(!$('app').hidden);
});

test('a sync finishing leaves settings open with its edits', (t) => {
  const { window, push } = settings(t);
  const $ = (id) => window.document.getElementById(id);
  $('st-name').value = 'half typed';
  push({ kind: 'state', incognito: false, accounts: [PLUS] });
  assert.ok(!$('settings').hidden);
  assert.ok($('app').hidden);
  assert.equal($('st-name').value, 'half typed');
});

test('typed signature source reaches the editor only through the app', (t) => {
  const { window, sent, push } = settings(t);
  const $ = (id) => window.document.getElementById(id);
  $('st-sig-source').click();
  assert.equal($('st-sig-html').value, '<p>Ann <b>Rivers</b></p>');
  $('st-sig-html').value = '<img src=x onerror=alert(1)><p>typed</p>';
  $('st-sig-source').click();
  assert.deepEqual(sent.at(-1), { kind: 'clean_signature', html: '<img src=x onerror=alert(1)><p>typed</p>' });
  assert.ok(!$('st-sig-html').hidden, 'still source until the answer');
  push({ kind: 'signature_clean', html: '<p>typed</p>' });
  assert.ok($('st-sig-html').hidden);
  assert.equal($('st-sig').innerHTML, '<p>typed</p>');
});

test('removing an account lives in settings and asks first', (t) => {
  const { window, sent } = settings(t);
  window.document.getElementById('st-remove').click();
  assert.deepEqual(sent.at(-1), { kind: 'remove_account', email: ANN.email });
});

function signedReply(t) {
  const m = mail(t);
  m.push({ kind: 'state', incognito: false, accounts: [PLUS] });
  m.push({ kind: 'list', account: ANN.email, folder: 'INBOX', envelopes: ROWS.map((r) => ({ ...r })) });
  rowsOf(m.window)[0].click();
  m.push(body(4));
  m.window.document.getElementById('reply-btn').click();
  return m;
}

test('a reply shows the signature and sends it between writing and quote', (t) => {
  const { window, sent } = signedReply(t);
  const $ = (id) => window.document.getElementById(id);
  assert.ok(!$('c-sig').hidden);
  assert.equal($('c-sig-body').innerHTML, PLUS.signature);
  const typed = 'Thanks!' + $('c-body').value;
  $('c-body').value = typed;
  $('c-send').click();
  const msg = sent.at(-1);
  assert.equal(msg.kind, 'send');
  assert.equal(msg.text, 'Thanks!');
  assert.match(msg.quote, /wrote:\n> body 4/);
  assert.equal(msg.sign, true);
});

test('the signature can come off one message and go back on', (t) => {
  const { window, sent } = signedReply(t);
  const $ = (id) => window.document.getElementById(id);
  $('c-sig-off').click();
  assert.ok($('c-sig').hidden);
  assert.ok(!$('c-sig-on').hidden);
  $('c-send').click();
  assert.equal(sent.at(-1).sign, false);
  $('c-sig-on').click();
  assert.ok(!$('c-sig').hidden);
});

test('an edited quote is the writer\'s own text', (t) => {
  const { window, sent } = signedReply(t);
  const $ = (id) => window.document.getElementById(id);
  $('c-body').value = 'rewrote it all';
  $('c-send').click();
  assert.equal(sent.at(-1).text, 'rewrote it all');
  assert.equal(sent.at(-1).quote, '');
});
