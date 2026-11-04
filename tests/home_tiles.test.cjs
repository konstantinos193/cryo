const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { JSDOM } = require('jsdom');

const HOME = fs.readFileSync(
  path.join(__dirname, '../src/shell/newtab/newtab.html'), 'utf8');
const HOME_JS = fs.readFileSync(
  path.join(__dirname, '../src/shell/newtab/newtab.js'), 'utf8');

// What the internal protocol serves a content view.
const DIALS = [
  { name: 'Google', url: 'https://www.google.com' },
  { name: 'Reddit', url: 'https://www.reddit.com' },
];
const ENGINES = [
  { slug: 'duckduckgo', name: 'DuckDuckGo', prefix: 'https://duckduckgo.com/?q=' },
];

// The page on its own origin, with the two fetches answered and an
// optional list already in that origin's localStorage. The document's
// own <script src> is never fetched here, so the file is run by hand
// once the stubs are in place. The window is closed with the test: the
// page's clock re-arms itself every minute and would hold node open.
function home(t, seed) {
  const { window } = new JSDOM(HOME, {
    url: 'http://cryo.localhost/newtab',
    runScripts: 'outside-only',
  });
  t.after(() => window.close());
  if (seed) window.localStorage.setItem('cryo.dials', JSON.stringify(seed));
  window.fetch = (where) => Promise.resolve({
    json: () => Promise.resolve(where === '/dials' ? DIALS : ENGINES),
  });
  // jsdom has no media queries; the page asks one about reduced motion.
  window.matchMedia = () => ({ matches: false });
  window.eval(HOME_JS);
  return window;
}

const settle = () => new Promise((done) => setImmediate(done));

const names = (window) =>
  [...window.document.querySelectorAll('.slot .tile .name')]
    .map((n) => n.textContent);

const kept = (window) =>
  JSON.parse(window.localStorage.getItem('cryo.dials') || 'null');

function add(window, name, url) {
  window.document.querySelector('.tile.add').click();
  window.document.getElementById('e-name').value = name;
  window.document.getElementById('e-url').value = url;
  window.document.getElementById('editor').dispatchEvent(
    new window.Event('submit', { bubbles: true, cancelable: true }));
}

test('the shipped list seeds the row and stores nothing', async (t) => {
  const window = home(t);
  await settle();
  assert.deepEqual(names(window), ['Google', 'Reddit']);
  assert.equal(kept(window), null);
  assert.ok(window.document.querySelector('.tile.add'), 'the add tile is offered');
  assert.ok(window.document.getElementById('reset').hidden, 'nothing to restore yet');
});

test('an added tile is normalized, painted and kept', async (t) => {
  const window = home(t);
  await settle();
  add(window, 'Docs', 'docs.rs');
  assert.deepEqual(names(window), ['Google', 'Reddit', 'Docs']);
  assert.deepEqual(kept(window), [
    { name: 'Google', url: 'https://www.google.com/' },
    { name: 'Reddit', url: 'https://www.reddit.com/' },
    { name: 'Docs', url: 'https://docs.rs/' },
  ]);
  assert.ok(window.document.getElementById('scrim').hidden, 'the dialog closes');
});

test('a tile the URL parser refuses never reaches the store', async (t) => {
  const window = home(t);
  await settle();
  for (const bad of ['javascript:alert(1)', 'not a url', '']) {
    add(window, 'Bad', bad);
    assert.deepEqual(names(window), ['Google', 'Reddit']);
    assert.equal(kept(window), null);
    assert.ok(window.document.getElementById('e-url').classList.contains('bad'));
    assert.ok(!window.document.getElementById('e-error').hidden, 'it says why');
    assert.ok(!window.document.getElementById('scrim').hidden, 'the dialog stays up');
  }
});

test('a stored list wins over the shipped one and survives a removal', async (t) => {
  const window = home(t, [
    { name: 'Crates', url: 'https://crates.io' },
    { name: 'Docs', url: 'https://docs.rs' },
  ]);
  await settle();
  assert.deepEqual(names(window), ['Crates', 'Docs']);
  window.document.querySelector('.slot .kill').click();
  assert.deepEqual(names(window), ['Docs']);
  assert.deepEqual(kept(window), [{ name: 'Docs', url: 'https://docs.rs/' }]);
});

test('restoring drops the stored list back to the shipped one', async (t) => {
  const window = home(t, [{ name: 'Crates', url: 'https://crates.io' }]);
  await settle();
  const reset = window.document.getElementById('reset');
  assert.ok(!reset.hidden, 'the way back is offered once a list is stored');
  reset.click();
  assert.deepEqual(names(window), ['Google', 'Reddit']);
  assert.equal(kept(window), null);
  assert.ok(reset.hidden);
});

test('the mail card asks the app for a glance and paints what comes back', async (t) => {
  const window = home(t);
  await settle();
  const card = window.document.getElementById('mail');
  assert.equal(card.getAttribute('href'), '/mail');
  assert.ok(!card.hidden, 'the card is a link before any push lands');
  assert.equal(window.document.getElementById('mail-title').textContent, 'Mail');

  window.__cryo_mail_glance({ incognito: false, unseen: 0, accounts: [] });
  assert.equal(window.document.getElementById('mail-sub').textContent, 'Set up your inbox');
  assert.ok(window.document.getElementById('mail-n').hidden);

  window.__cryo_mail_glance({
    incognito: false, unseen: 3,
    accounts: [{ email: 'ann@example.com', display: 'Ann', unseen: 3, status: '' }],
  });
  assert.equal(window.document.getElementById('mail-title').textContent, '3 unread messages');
  assert.equal(window.document.getElementById('mail-sub').textContent, 'ann@example.com');
  assert.equal(window.document.getElementById('mail-n').textContent, '3');
  assert.ok(!window.document.getElementById('mail-n').hidden);

  window.__cryo_mail_glance({
    incognito: false, unseen: 0,
    accounts: [{ email: 'ann@example.com', display: 'Ann', unseen: 0, status: 'syncing' }],
  });
  assert.ok(card.classList.contains('syncing'));
  assert.equal(window.document.getElementById('mail-sub').textContent, 'Syncing ann@example.com');

  window.__cryo_mail_glance({ incognito: true, unseen: 0, accounts: [] });
  assert.ok(card.hidden, 'incognito has no mail to glance at');
});

test('the home page sends exactly one question over the page channel', async (t) => {
  const sent = [];
  const { window } = new JSDOM(HOME, {
    url: 'http://cryo.localhost/newtab',
    runScripts: 'outside-only',
  });
  t.after(() => window.close());
  window.fetch = () => Promise.resolve({ json: () => Promise.resolve([]) });
  window.matchMedia = () => ({ matches: false });
  window.ipc = { postMessage: (body) => sent.push(JSON.parse(body)) };
  window.eval(HOME_JS);
  await settle();
  assert.deepEqual(sent, [{ kind: 'mail_glance' }]);
});

test('stored junk is dropped instead of painted', async (t) => {
  const window = home(t, [
    { name: 'Evil', url: 'javascript:alert(1)' },
    { name: '', url: 'https://example.com' },
    { name: 'Fine', url: 'https://example.com' },
  ]);
  await settle();
  assert.deepEqual(names(window), ['Fine']);
});
