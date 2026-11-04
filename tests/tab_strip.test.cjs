const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { JSDOM } = require('jsdom');

const RENDER_JS = fs.readFileSync(
  path.join(__dirname, '../src/shell/chrome/render.js'), 'utf8');

function fixture() {
  const dom = new JSDOM(
    '<!doctype html><body>' +
    '<div id="tabs"></div><input id="url">' +
    '<button id="back"></button><button id="fwd"></button>' +
    '<button id="reload"></button><div id="loadbar"></div>' +
    '<button id="menu"></button><div id="menupop"></div>' +
    '<div id="update-item"></div><button id="star"></button>' +
    '<button id="signin"></button><button id="site"></button></body>',
    { runScripts: 'outside-only' });
  const { window } = dom;
  const { document } = window;
  const commands = [];
  const el = (id) => document.getElementById(id);
  Object.assign(window, {
    tabsEl: el('tabs'), urlEl: el('url'),
    backBtn: el('back'), fwdBtn: el('fwd'),
    reloadBtn: el('reload'), loadbar: el('loadbar'),
    menuPop: el('menupop'), updateItem: el('update-item'),
    updateCheckEl: null,
    signinBtn: el('signin'), siteBtn: el('site'), lastState: {},
    ipc: (cmd) => commands.push(cmd),
    renderDl() {}, renderBar() {},
    // The siblings render.js calls into. The tab-search overlay reads the
    // same pushed state, so a closed one is what the strip renders beside.
    tabfindOpen: false, paintTabfind() {},
  });
  window.eval(RENDER_JS);
  const state = {
    tabs: [{ id: 7, title: 'Frozen page', state: 'frozen', active: false }],
    active_url: '',
  };
  const render = () => window.__cryo_update(state);
  render();
  const press = (target, button = 0) => {
    target.dispatchEvent(new window.MouseEvent(
      'pointerdown', { button, bubbles: true, cancelable: true }));
  };
  const tab = () => window.tabsEl.children[0];
  const child = (cls) =>
    [...tab().children].find((c) => c.classList.contains(cls));
  return { commands, window, state, render, press, tab, child };
}

test('first press on a frozen tab activates before a state update replaces it', () => {
  const f = fixture();
  f.press(f.child('title'));
  f.render();
  assert.equal(f.commands.length, 1);
  assert.equal(f.commands[0].cmd, 'activate');
  assert.equal(f.commands[0].id, 7);
});

test('tab padding, frozen marker and favicon activate on first press', () => {
  for (const targetClass of [null, 'fzmark', 'fav']) {
    const f = fixture();
    f.state.tabs[0].favicon = 'data:image/png;base64,AA==';
    f.render();
    f.press(targetClass ? f.child(targetClass) : f.tab());
    assert.equal(f.commands.length, 1);
    assert.equal(f.commands[0].cmd, 'activate');
  }
});

test('freeze and close controls do not activate the tab', () => {
  const f = fixture();
  f.state.tabs[0].state = 'active';
  f.render();
  for (const control of ['fz', 'x']) {
    const target = f.child(control);
    f.press(target);
    target.dispatchEvent(new f.window.MouseEvent('click', { bubbles: true }));
  }
  assert.deepEqual(f.commands.map((c) => c.cmd), ['freeze', 'close']);
});

test('a store-managed build hides the manual update check', () => {
  const f = fixture();
  const item = f.window.document.createElement('div');
  f.window.updateCheckEl = item;
  f.state.store_managed = true;
  f.render();
  assert.equal(item.hidden, true);
  f.state.store_managed = false;
  f.render();
  assert.equal(item.hidden, false);
});

// A strip of three 100px tabs laid side by side; jsdom has no layout,
// so each tab reports the rectangle the real strip would give it.
function stripFixture() {
  const f = fixture();
  f.state.tabs = [
    { id: 1, title: 'A', state: 'active', active: false },
    { id: 2, title: 'B', state: 'active', active: false },
    { id: 3, title: 'C', state: 'active', active: true },
  ];
  const layout = () => [...f.window.tabsEl.children].forEach((el, i) => {
    el.getBoundingClientRect = () => ({ left: i * 100, width: 100 });
  });
  f.render();
  layout();
  const pointer = (type, clientX) => f.window.document.dispatchEvent(
    new f.window.MouseEvent(type, { clientX, bubbles: true }));
  const order = () => [...f.window.tabsEl.children].map((el) => el.dataset.id);
  return { ...f, layout, pointer, order };
}

test('dragging the last tab to the front relocates it at index 0', () => {
  const f = stripFixture();
  f.tab = () => f.window.tabsEl.children[2];
  f.tab().dispatchEvent(new f.window.MouseEvent(
    'pointerdown', { button: 0, clientX: 250, bubbles: true, cancelable: true }));
  // The press's activate echo rebuilds the strip before the pull starts.
  f.render();
  f.layout();
  f.pointer('pointermove', 240);
  f.pointer('pointermove', 30);
  assert.ok(f.window.tabsEl.classList.contains('reordering'));
  // A state push mid-drag must not rebuild the strip under the pointer.
  f.render();
  assert.ok(f.window.tabsEl.children[2].classList.contains('dragging'));
  f.pointer('pointerup', 30);
  assert.deepEqual(f.commands.map((c) => c.cmd), ['activate', 'relocate']);
  assert.equal(f.commands[1].id, 3);
  assert.equal(f.commands[1].index, 0);
  assert.deepEqual(f.order(), ['3', '1', '2']);
  assert.ok(!f.window.tabsEl.classList.contains('reordering'));
});

test('a press without a pull sends no relocate', () => {
  const f = stripFixture();
  f.window.tabsEl.children[0].dispatchEvent(new f.window.MouseEvent(
    'pointerdown', { button: 0, clientX: 50, bubbles: true, cancelable: true }));
  f.pointer('pointermove', 52);
  f.pointer('pointerup', 52);
  assert.deepEqual(f.commands.map((c) => c.cmd), ['activate']);
  assert.deepEqual(f.order(), ['1', '2', '3']);
});

test('the site button reads the active address', () => {
  const f = fixture();
  const site = f.window.document.getElementById('site');
  f.state.active_url = 'https://meet.google.com/';
  f.render();
  assert.equal(site.className, 'secure');
  f.state.active_url = 'http://example.test/';
  f.render();
  assert.equal(site.className, 'insecure');
  f.state.active_url = 'http://cryo.localhost/settings';
  f.render();
  assert.equal(site.className, 'internal');
  f.state.active_url = '';
  f.render();
  assert.equal(site.className, 'internal');
});

test('middle and right presses do not activate tabs', () => {
  const f = fixture();
  f.press(f.tab(), 1);
  f.press(f.tab(), 2);
  assert.equal(f.commands.length, 0);
});
