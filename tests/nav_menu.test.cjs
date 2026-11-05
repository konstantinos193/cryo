const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { JSDOM } = require('jsdom');

const NAVMENU_JS = fs.readFileSync(
  path.join(__dirname, '../src/shell/chrome/navmenu.js'), 'utf8');

// The elements the script reaches for, as chrome.html carries them.
const MARKUP =
  '<!doctype html><body><button id="back"></button><button id="fwd"></button>' +
  '<div id="nav-pop"></div></body>';

const BACK_ROWS = [
  { entry: 41, title: 'Rust docs', url: 'https://doc.rust-lang.org/', favicon: 'data:image/png;base64,AA==' },
  { entry: 40, title: '', url: 'https://example.com/untitled', favicon: null },
];

function fixture() {
  const dom = new JSDOM(MARKUP, { runScripts: 'outside-only' });
  const { window } = dom;
  const { document } = window;
  window.Element.prototype.scrollIntoView = function scrollIntoView() {};
  const commands = [];
  const library = [];
  const overlayReports = [];
  Object.assign(window, {
    backBtn: document.getElementById('back'),
    fwdBtn: document.getElementById('fwd'),
    ipc: (cmd) => commands.push({ ...cmd }),
    reportOverlay: () => overlayReports.push(
      document.getElementById('nav-pop').classList.contains('open')),
    openLib: (pane) => library.push(pane),
    // bootstrap.js's list, trimmed to the one overlay under test.
    dismissOverlays: () => window.setNavMenu(false),
    node: (tag, cls, text) => {
      const n = document.createElement(tag);
      if (cls) n.className = cls;
      if (text !== undefined) n.textContent = text;
      return n;
    },
  });
  window.eval(NAVMENU_JS + '\nwindow.setNavMenu = setNavMenu;');
  const pop = document.getElementById('nav-pop');
  const fire = (el, type, init = {}) => {
    const e = new window.MouseEvent(type, { bubbles: true, cancelable: true, ...init });
    el.dispatchEvent(e);
    return e;
  };
  return {
    window, document, pop, commands, library, overlayReports, fire,
    back: window.backBtn,
    fwd: window.fwdBtn,
    isOpen: () => pop.classList.contains('open'),
    rows: () => [...pop.querySelectorAll('.nav-row')],
    reply: (forward, rows = BACK_ROWS, tab = 7) =>
      window.__cryo_nav_menu({ tab, forward, rows }),
    key: (k, init = {}) => pop.dispatchEvent(new window.KeyboardEvent(
      'keydown', { key: k, bubbles: true, cancelable: true, ...init })),
  };
}

function opened() {
  const f = fixture();
  f.fire(f.back, 'contextmenu');
  f.reply(false);
  return f;
}

test('a right-click asks the app and suppresses the native menu', () => {
  const f = fixture();
  const e = f.fire(f.back, 'contextmenu');
  assert.equal(e.defaultPrevented, true);
  assert.deepEqual(f.commands, [{ cmd: 'nav_menu', forward: false }]);
  f.fire(f.fwd, 'contextmenu');
  assert.deepEqual(f.commands[1], { cmd: 'nav_menu', forward: true });
});

test('the popup opens with the reply, not before it', () => {
  const f = fixture();
  f.fire(f.back, 'contextmenu');
  assert.equal(f.isOpen(), false);
  assert.deepEqual(f.overlayReports, []);
  f.reply(false);
  assert.equal(f.isOpen(), true);
  assert.deepEqual(f.overlayReports, [true]);
});

test('rows show the title, fall back to the address, and link to full history', () => {
  const f = opened();
  assert.deepEqual(f.rows().map((r) => r.textContent),
    ['Rust docs', 'https://example.com/untitled', 'Show full history']);
  assert.equal(f.rows()[0].querySelector('img').getAttribute('src'), BACK_ROWS[0].favicon);
  assert.ok(f.rows()[1].querySelector('.fav.blank'));
  assert.equal(f.rows()[0].title, BACK_ROWS[0].url);
});

test('a page title is text, never markup', () => {
  const f = fixture();
  f.fire(f.back, 'contextmenu');
  f.reply(false, [{ entry: 1, title: '<img id="pwn">', url: 'https://x.test/', favicon: null }]);
  assert.equal(f.document.getElementById('pwn'), null);
  assert.equal(f.rows()[0].textContent, '<img id="pwn">');
});

test('picking a row jumps to that entry of that tab and closes', () => {
  const f = opened();
  f.commands.length = 0;
  f.fire(f.rows()[1], 'click');
  assert.deepEqual(f.commands, [{ cmd: 'nav_go', tab: 7, entry: 40 }]);
  assert.equal(f.isOpen(), false);
});

test('Ctrl and the middle button open the page in a new tab instead', () => {
  const f = opened();
  f.commands.length = 0;
  f.fire(f.rows()[0], 'click', { ctrlKey: true });
  assert.deepEqual(f.commands, [
    { cmd: 'history_open', url: BACK_ROWS[0].url, ctrl: true, shift: false },
  ]);

  const g = opened();
  g.commands.length = 0;
  g.fire(g.rows()[1], 'auxclick', { button: 1, shiftKey: true });
  assert.deepEqual(g.commands, [
    { cmd: 'history_open', url: BACK_ROWS[1].url, ctrl: true, shift: true },
  ]);
  assert.equal(g.isOpen(), false);
});

test('the last row opens the library on history', () => {
  const f = opened();
  f.commands.length = 0;
  f.fire(f.rows()[2], 'click');
  assert.deepEqual(f.library, ['history']);
  assert.deepEqual(f.commands, []);
  assert.equal(f.isOpen(), false);
});

test('a reply nobody is waiting for paints nothing', () => {
  const f = fixture();
  f.reply(false);
  assert.equal(f.isOpen(), false);

  // Asked, then dismissed while the reply travelled.
  f.fire(f.back, 'contextmenu');
  f.window.dismissOverlays();
  f.reply(false);
  assert.equal(f.isOpen(), false);

  // Asked for one arrow, then the other: only the second answer counts.
  f.fire(f.back, 'contextmenu');
  f.fire(f.fwd, 'contextmenu');
  f.reply(false);
  assert.equal(f.isOpen(), false);
  f.reply(true);
  assert.equal(f.isOpen(), true);
});

test('an empty list opens nothing', () => {
  const f = fixture();
  f.fire(f.back, 'contextmenu');
  f.reply(false, []);
  assert.equal(f.isOpen(), false);
});

test('a second right-click on the same arrow closes without asking again', () => {
  const f = opened();
  f.commands.length = 0;
  f.fire(f.back, 'contextmenu');
  assert.equal(f.isOpen(), false);
  assert.deepEqual(f.commands, []);
});

test('a right-click on the other arrow swaps the menu', () => {
  const f = opened();
  f.commands.length = 0;
  f.fire(f.fwd, 'contextmenu');
  assert.equal(f.isOpen(), false);
  assert.deepEqual(f.commands, [{ cmd: 'nav_menu', forward: true }]);
  f.reply(true, [BACK_ROWS[0]]);
  assert.equal(f.isOpen(), true);
  assert.equal(f.rows().length, 2);
});

test('a right-click elsewhere dismisses, one on a row does not', () => {
  const f = opened();
  f.fire(f.rows()[0], 'contextmenu');
  assert.equal(f.isOpen(), true);
  f.fire(f.document.body, 'contextmenu');
  assert.equal(f.isOpen(), false);
});

test('a click inside does not reach the document', () => {
  const f = opened();
  let reached = false;
  f.document.addEventListener('click', () => { reached = true; });
  f.fire(f.rows()[0], 'click');
  assert.equal(reached, false);
});

test('the arrow keys wrap, Enter picks, nothing is highlighted at first', () => {
  const f = opened();
  const at = () => f.rows().findIndex((r) => r.classList.contains('at'));
  assert.equal(at(), -1);
  f.commands.length = 0;
  f.key('Enter');
  assert.deepEqual(f.commands, []);
  assert.equal(f.isOpen(), true);

  f.key('ArrowUp');
  assert.equal(at(), 2);
  f.key('ArrowDown');
  assert.equal(at(), 0);
  f.key('ArrowDown');
  assert.equal(at(), 1);
  f.key('Enter');
  assert.deepEqual(f.commands, [{ cmd: 'nav_go', tab: 7, entry: 40 }]);
  assert.equal(f.isOpen(), false);
});

test('hover and the arrow keys share one highlight', () => {
  const f = opened();
  f.fire(f.rows()[1], 'mouseenter', { bubbles: false });
  f.key('ArrowUp');
  const lit = f.rows().filter((r) => r.classList.contains('at'));
  assert.deepEqual(lit, [f.rows()[0]]);
});
