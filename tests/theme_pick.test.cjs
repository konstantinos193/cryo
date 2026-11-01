// The theme a shell document paints in: the head script that applies the
// stored pick, and the settings row that changes it.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { JSDOM } = require('jsdom');

const read = (where) => fs.readFileSync(path.join(__dirname, '..', where), 'utf8');
const THEME_JS = read('src/shell/theme/theme.js');
const SETTINGS = read('src/shell/settings/settings.html');
const SETTINGS_JS = read('src/shell/settings/settings.js');

// What the internal protocol serves.
const THEMES = [
  { slug: 'glacier', name: 'Glacier' },
  { slug: 'light', name: 'Light' },
  { slug: 'lilac', name: 'Lilac' },
];
const ENGINES = [
  { slug: 'duckduckgo', name: 'DuckDuckGo', prefix: 'https://duckduckgo.com/?q=' },
];

// A shell document on the shell origin, with an optional stored pick.
// `location` is handed in rather than used off the window: jsdom cannot
// navigate, and the settings page reports its pick by navigating.
function shell(t, html, scripts, pick) {
  const { window } = new JSDOM(html, {
    url: 'http://cryo.localhost/settings',
    runScripts: 'outside-only',
  });
  t.after(() => window.close());
  if (pick !== undefined) window.localStorage.setItem('cryo.theme', pick);
  const went = [];
  window.__went = went;
  window.__loc = {
    get href() {
      return '/settings';
    },
    set href(where) {
      went.push(where);
    },
  };
  window.fetch = (where) => Promise.resolve({
    json: () => Promise.resolve(where === '/themes' ? THEMES : ENGINES),
  });
  for (const script of scripts) {
    window.eval(`(function (location) { ${script} })(window.__loc)`);
  }
  return window;
}

const HEAD = `<!doctype html><html><head>
  <link rel="icon" type="image/svg+xml" href="/theme/icon-glacier.svg">
  </head><body></body></html>`;

const page = (t, pick) => shell(t, HEAD, [THEME_JS], pick);
const settings = (t, pick) => shell(t, SETTINGS, [THEME_JS, SETTINGS_JS], pick);
const settle = () => new Promise((done) => setImmediate(done));

const painted = (window) => ({
  theme: window.document.documentElement.dataset.theme,
  icon: window.document.querySelector('link[rel="icon"]').getAttribute('href'),
});

test('a document with no stored pick paints the default', (t) => {
  assert.deepEqual(painted(page(t)), {
    theme: 'glacier',
    icon: '/theme/icon-glacier.svg',
  });
});

test('the stored pick is painted before anything else runs', (t) => {
  assert.deepEqual(painted(page(t, 'lilac')), {
    theme: 'lilac',
    icon: '/theme/icon-lilac.svg',
  });
});

test('a pick no theme answers to paints the default, not nothing', (t) => {
  for (const bad of ['solarized', '', 'Light', '../etc/passwd']) {
    assert.deepEqual(painted(page(t, bad)).theme, 'glacier', `accepted ${bad}`);
  }
});

test('another shell document changing the pick repaints this one', (t) => {
  const window = page(t, 'glacier');
  window.localStorage.setItem('cryo.theme', 'light');
  window.dispatchEvent(new window.StorageEvent('storage', { key: 'cryo.theme' }));
  assert.deepEqual(painted(window), { theme: 'light', icon: '/theme/icon-light.svg' });
  // A cleared storage area arrives with no key at all.
  window.localStorage.removeItem('cryo.theme');
  window.dispatchEvent(new window.StorageEvent('storage', { key: null }));
  assert.equal(painted(window).theme, 'glacier');
  // Another key is somebody else's business.
  window.localStorage.setItem('cryo.theme', 'lilac');
  window.dispatchEvent(new window.StorageEvent('storage', { key: 'cryo.searchEngine' }));
  assert.equal(painted(window).theme, 'glacier');
});

test('the settings page lists every theme and marks the stored one', async (t) => {
  const window = settings(t, 'light');
  await settle();
  const rows = [...window.document.querySelectorAll('#theme-pop .xdrop-item')];
  assert.deepEqual(rows.map((r) => r.textContent), ['Glacier', 'Light', 'Lilac']);
  assert.deepEqual(rows.filter((r) => r.classList.contains('on')).map((r) => r.dataset.v),
    ['light']);
  assert.equal(window.document.getElementById('theme-label').textContent, 'Light');
  assert.equal(painted(window).theme, 'light');
});

test('a stored pick is painted but not reported again at load', async (t) => {
  const window = settings(t, 'lilac');
  await settle();
  assert.deepEqual(window.__went.filter((w) => w.startsWith('/settings/theme')), []);
});

test('picking a theme stores it, paints this page, and tells the app', async (t) => {
  const window = settings(t);
  await settle();
  const pick = (slug) =>
    window.document.querySelector(`#theme-pop .xdrop-item[data-v="${slug}"]`).click();

  pick('lilac');
  assert.equal(window.localStorage.getItem('cryo.theme'), 'lilac');
  assert.deepEqual(painted(window), { theme: 'lilac', icon: '/theme/icon-lilac.svg' });
  assert.equal(window.document.getElementById('theme-label').textContent, 'Lilac');
  assert.deepEqual(window.__went.slice(-1), ['/settings/theme/lilac']);
  assert.ok(window.document.getElementById('theme-drop').classList.contains('open') === false,
    'the list shuts behind the pick');

  pick('glacier');
  assert.equal(window.localStorage.getItem('cryo.theme'), 'glacier');
  assert.equal(painted(window).theme, 'glacier');
  assert.deepEqual(window.__went.slice(-1), ['/settings/theme/glacier']);
});

test('the two pickers open and shut on their own', async (t) => {
  const window = settings(t);
  await settle();
  const { document } = window;
  const open = (id) => {
    document.getElementById(id).dispatchEvent(
      new window.MouseEvent('click', { bubbles: true }));
    return document.getElementById(`${id}-drop`).classList.contains('open');
  };
  assert.ok(open('theme'), 'the theme list opens');
  assert.ok(open('engine'), 'the engine list opens');
  assert.ok(!document.getElementById('theme-drop').classList.contains('open'),
    'opening one shuts the other');
  document.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.ok(!document.getElementById('engine-drop').classList.contains('open'),
    'a click elsewhere shuts the list');
});

test('the search engine pick still works beside the theme pick', async (t) => {
  const window = settings(t);
  await settle();
  assert.equal(window.document.getElementById('engine-label').textContent, 'DuckDuckGo');
  assert.equal(window.localStorage.getItem('cryo.searchEngine'), 'duckduckgo');
  window.document.querySelector('#engine-pop .xdrop-item[data-v="custom"]').click();
  assert.ok(!window.document.getElementById('engine-tpl').hidden, 'the template box opens');
  const tpl = window.document.getElementById('engine-tpl');
  tpl.value = 'https://kagi.com/search?q=%s';
  tpl.dispatchEvent(new window.Event('input'));
  assert.equal(window.localStorage.getItem('cryo.searchEngine'), 'https://kagi.com/search?q=%s');
});
