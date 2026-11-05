const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { JSDOM } = require('jsdom');

const OMNIBOX_JS = fs.readFileSync(
  path.join(__dirname, '../src/shell/chrome/omnibox.js'), 'utf8');

const HOTEL = 'https://test.hotelpoint.io/reservations';
const BOARD = 'http://localhost:4545/log';

function fixture() {
  const dom = new JSDOM(
    '<!doctype html><body><input id="url"><div id="omnipop"></div>' +
    '<div id="dial"></div></body>',
    { runScripts: 'outside-only' });
  const { window } = dom;
  const { document } = window;
  // jsdom has no second view to lose focus to; this is the page's answer
  // when the controller's focus moves into a content view.
  let pageFocused = true;
  document.hasFocus = () => pageFocused;
  Object.assign(window, {
    urlEl: document.getElementById('url'),
    omniPop: document.getElementById('omnipop'),
    omniOpen: false, lastState: {}, dialEntries: [],
    setOmni() {}, enginePick: () => null, openLib() {},
    ipc() {},
    fetch: () => new Promise(() => {}),
  });
  window.eval(`${OMNIBOX_JS};window.paintAddress = paintAddress;`);
  const { urlEl } = window;
  const push = (id, url) => window.paintAddress({
    tabs: [{ id, active: true }], active_url: url,
  });
  const typeText = (text) => {
    urlEl.focus();
    urlEl.value = text;
    urlEl.dispatchEvent(new window.Event('input', { bubbles: true }));
  };
  const leaveToPage = () => { pageFocused = false; };
  const key = (name) => urlEl.dispatchEvent(new window.KeyboardEvent(
    'keydown', { key: name, bubbles: true, cancelable: true }));
  return { window, urlEl, push, typeText, leaveToPage, key };
}

test('the box follows navigation while the input keeps DOM focus', () => {
  const f = fixture();
  f.push(1, HOTEL);
  // Focus into the box, then away into the page: the chrome document's
  // activeElement never leaves the input.
  f.urlEl.focus();
  f.leaveToPage();
  f.push(1, BOARD);
  assert.equal(f.urlEl.value, BOARD);
});

test('a focused box with nothing typed follows the page', () => {
  const f = fixture();
  f.push(1, HOTEL);
  f.urlEl.focus();
  f.push(1, BOARD);
  assert.equal(f.urlEl.value, BOARD);
});

test('switching tabs replaces a half-typed address, focused or not', () => {
  const f = fixture();
  f.push(1, HOTEL);
  // A tab's pointerdown is prevented, so the press leaves focus here.
  f.typeText('https://test.hotelpo');
  f.push(2, BOARD);
  assert.equal(f.urlEl.value, BOARD);
});

test('an accepted inline completion does not outlive a tab switch', () => {
  const f = fixture();
  f.push(1, 'http://localhost:4545/');
  f.typeText('https://test.hotelpo');
  f.window.__cryo_suggest({
    q: 'https://test.hotelpo',
    rows: [{ label: 'hotel', input: HOTEL, complete: HOTEL }],
  });
  f.urlEl.setSelectionRange(f.urlEl.value.length, f.urlEl.value.length);
  f.key('End');
  assert.equal(f.urlEl.value, HOTEL);
  assert.notEqual(f.urlEl.selectionStart, f.urlEl.selectionEnd);
  f.push(2, BOARD);
  assert.equal(f.urlEl.value, BOARD);
  assert.equal(f.urlEl.selectionStart, f.urlEl.selectionEnd);
});

test('completing with End counts as editing: a push leaves it alone', () => {
  const f = fixture();
  f.push(1, 'http://localhost:4545/');
  f.urlEl.focus();
  f.window.__cryo_suggest({
    q: 'http://localhost:4545/',
    rows: [{ label: 'log', input: BOARD, complete: BOARD }],
  });
  f.urlEl.setSelectionRange(f.urlEl.value.length, f.urlEl.value.length);
  f.key('End');
  assert.equal(f.urlEl.value, BOARD);
  f.push(1, 'http://localhost:4545/');
  assert.equal(f.urlEl.value, BOARD);
});

test('text being typed survives pushes and the page navigating', () => {
  const f = fixture();
  f.push(1, HOTEL);
  f.typeText('local');
  f.push(1, HOTEL);
  assert.equal(f.urlEl.value, 'local');
  f.push(1, BOARD);
  assert.equal(f.urlEl.value, 'local');
});

test('a draft left behind survives pushes until the page moves', () => {
  const f = fixture();
  f.push(1, HOTEL);
  f.typeText('local');
  f.leaveToPage();
  f.push(1, HOTEL);
  assert.equal(f.urlEl.value, 'local');
  f.push(1, BOARD);
  assert.equal(f.urlEl.value, BOARD);
});

test('committing an address hands the box back to the page', () => {
  const f = fixture();
  f.push(1, HOTEL);
  f.typeText(BOARD);
  f.key('Enter');
  // The same address: a reload moves nothing, and the box still lets go.
  f.push(1, HOTEL);
  assert.equal(f.urlEl.value, HOTEL);
});

test('a push with the same address does not disturb a selection', () => {
  const f = fixture();
  f.push(1, HOTEL);
  f.urlEl.focus();
  f.urlEl.select();
  f.push(1, HOTEL);
  assert.equal(f.urlEl.selectionStart, 0);
  assert.equal(f.urlEl.selectionEnd, HOTEL.length);
});
