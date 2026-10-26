const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { JSDOM } = require('jsdom');

const TABFIND_JS = fs.readFileSync(
  path.join(__dirname, '../src/shell/chrome/tabfind.js'), 'utf8');

// The overlay's markup, as chrome.html carries it.
const MARKUP =
  '<!doctype html><body><div id="tabfind"><div class="panel">' +
  '<input type="text"><div class="results"></div><div class="count"></div>' +
  '</div></div></body>';

const TABS = [
  { id: 1, title: 'Cryo on GitHub', url: 'https://github.com/konstantinos193/cryo', state: 'active', active: true },
  { id: 2, title: 'Rust docs', url: 'https://doc.rust-lang.org/std/', state: 'active', active: false },
  { id: 3, title: 'WebView2 reference', url: 'https://learn.microsoft.com/webview2', state: 'frozen', active: false },
  { id: 4, title: 'Old tab', url: 'https://example.com/gone', state: 'discarded', active: false },
];

function fixture(tabs = TABS) {
  const dom = new JSDOM(MARKUP, { runScripts: 'outside-only' });
  const { window } = dom;
  const { document } = window;
  // jsdom has no layout, so it ships no scrollIntoView. WebView2 does;
  // keeping the overlay's scroll-to-highlight real and stubbing it here
  // beats weakening the code for the test's benefit.
  window.Element.prototype.scrollIntoView = function scrollIntoView() {};
  const commands = [];
  const overlayReports = [];
  Object.assign(window, {
    lastState: { tabs: tabs.map((t) => ({ ...t })) },
    // Copied into this realm: an object built inside the jsdom window
    // carries that realm's prototype, which deepStrictEqual rejects.
    ipc: (cmd) => commands.push({ ...cmd }),
    reportOverlay: () => overlayReports.push(
      document.getElementById('tabfind').classList.contains('open')),
    dismissOverlays: () => {},
    // The house DOM helper, from library.js. Always textContent: every
    // string here came from a web page's title.
    node: (tag, cls, text) => {
      const n = document.createElement(tag);
      if (cls) n.className = cls;
      if (text !== undefined) n.textContent = text;
      return n;
    },
  });
  window.eval(TABFIND_JS);
  const input = document.querySelector('#tabfind input');
  const rows = () => [...document.querySelectorAll('.tf-row')];
  const titles = () => rows().map((r) => r.querySelector('.nm').textContent);
  const highlighted = () => {
    const at = rows().findIndex((r) => r.classList.contains('at'));
    return at < 0 ? null : titles()[at];
  };
  const type = (value) => {
    input.value = value;
    input.dispatchEvent(new window.Event('input', { bubbles: true }));
  };
  const key = (k, shift = false) => input.dispatchEvent(
    new window.KeyboardEvent('keydown', { key: k, shiftKey: shift, bubbles: true, cancelable: true }));
  return {
    window, document, commands, overlayReports, input, rows, titles,
    highlighted, type, key,
    open: () => window.__cryo_tabfind_toggle(),
    isOpen: () => document.getElementById('tabfind').classList.contains('open'),
    count: () => document.querySelector('#tabfind .count').textContent,
  };
}

test('opening lists every tab in registry order, frozen ones included', () => {
  const f = fixture();
  f.open();
  assert.ok(f.isOpen());
  assert.deepEqual(f.titles(), [
    'Cryo on GitHub', 'Rust docs', 'WebView2 reference', 'Old tab']);
  assert.equal(f.count(), '4 tabs');
  const states = f.rows().map((r) => r.querySelector('.tf-state')?.textContent);
  assert.deepEqual(states, ['Current', undefined, '❄ Frozen', 'Discarded']);
});

test('typing filters on title and on address, keeping strip order', () => {
  const f = fixture();
  f.open();
  f.type('rust');
  assert.deepEqual(f.titles(), ['Rust docs']);
  // "learn.microsoft" appears only in an address, never in a title.
  f.type('learn.microsoft');
  assert.deepEqual(f.titles(), ['WebView2 reference']);
  f.type('https://');
  assert.deepEqual(f.titles(), [
    'Cryo on GitHub', 'Rust docs', 'WebView2 reference', 'Old tab']);
  assert.equal(f.count(), '4 of 4 tabs');
});

test('a query matching nothing says so instead of showing every tab', () => {
  const f = fixture();
  f.open();
  f.type('no such page');
  assert.deepEqual(f.titles(), []);
  assert.equal(f.count(), 'No tab matches that');
});

test('Enter takes the highlighted tab through the ordinary activate path', () => {
  const f = fixture();
  f.open();
  f.type('rust');
  f.key('Enter');
  assert.deepEqual(f.commands, [{ cmd: 'activate', id: 2 }]);
  // Taking a tab closes the overlay, so the app restores the bounds.
  assert.equal(f.isOpen(), false);
  assert.equal(f.overlayReports.at(-1), false);
});

test('a frozen tab is picked with the same command as a live one', () => {
  const f = fixture();
  f.open();
  f.type('webview2');
  f.key('Enter');
  assert.deepEqual(f.commands, [{ cmd: 'activate', id: 3 }]);
});

test('the arrows move the highlight and wrap at both ends', () => {
  const f = fixture();
  f.open();
  assert.equal(f.highlighted(), 'Cryo on GitHub');
  f.key('ArrowDown');
  assert.equal(f.highlighted(), 'Rust docs');
  f.key('ArrowUp');
  f.key('ArrowUp');
  assert.equal(f.highlighted(), 'Old tab');
  f.key('ArrowDown');
  assert.equal(f.highlighted(), 'Cryo on GitHub');
  f.key('Tab');
  assert.equal(f.highlighted(), 'Rust docs');
  f.key('Tab', true);
  assert.equal(f.highlighted(), 'Cryo on GitHub');
});

test('a new query moves the highlight back to the first result', () => {
  const f = fixture();
  f.open();
  f.key('ArrowDown');
  f.key('ArrowDown');
  assert.equal(f.highlighted(), 'WebView2 reference');
  f.type('t');
  assert.equal(f.highlighted(), f.titles()[0]);
  f.key('Enter');
  assert.equal(f.commands[0].id, 1);
});

test('clicking a row takes that row, not the highlighted one', () => {
  const f = fixture();
  f.open();
  f.rows()[3].dispatchEvent(new f.window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(f.commands, [{ cmd: 'activate', id: 4 }]);
});

test('the shortcut toggles: a second press shuts it and clears the query', () => {
  const f = fixture();
  f.open();
  f.type('rust');
  f.open();
  assert.equal(f.isOpen(), false);
  assert.equal(f.input.value, '');
  assert.deepEqual(f.commands, []);
  f.open();
  assert.deepEqual(f.titles(), [
    'Cryo on GitHub', 'Rust docs', 'WebView2 reference', 'Old tab']);
});

test('opening reports the overlay so the app can stretch the view', () => {
  const f = fixture();
  f.open();
  assert.deepEqual(f.overlayReports, [true]);
});

test('with no tabs to list it opens empty rather than throwing', () => {
  const f = fixture([]);
  f.open();
  assert.ok(f.isOpen());
  assert.deepEqual(f.titles(), []);
  assert.equal(f.count(), '');
  f.key('Enter');
  f.key('ArrowDown');
  assert.deepEqual(f.commands, []);
});

test('the field takes focus, because the shortcut is the way in', async () => {
  const f = fixture();
  f.open();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(f.document.activeElement, f.input);
});
