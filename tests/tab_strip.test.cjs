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
    '<button id="signin"></button></body>',
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
    signinBtn: el('signin'), lastState: {},
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

test('middle and right presses do not activate tabs', () => {
  const f = fixture();
  f.press(f.tab(), 1);
  f.press(f.tab(), 2);
  assert.equal(f.commands.length, 0);
});
