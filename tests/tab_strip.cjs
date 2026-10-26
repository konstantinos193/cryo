const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { test } = require('node:test');

class Element {
  constructor() {
    this.children = [];
    this.listeners = {};
    this.classList = { toggle() {}, add() {}, remove() {} };
    this.dataset = {};
  }
  set innerHTML(value) { this.children = []; }
  append(...children) { this.children.push(...children); }
  appendChild(child) { this.children.push(child); }
  addEventListener(name, callback) { this.listeners[name] = callback; }
  remove() {}
  closest(selector) {
    return selector.split(',').some((s) => s.trim() === '.' + this.className)
      ? this : null;
  }
}

function fixture() {
  const commands = [];
  const tabsEl = new Element();
  const state = {
    tabs: [{ id: 7, title: 'Frozen page', state: 'frozen', active: false }],
    active_url: '',
  };
  const context = {
    window: {}, tabsEl, lastState: {}, ipc: (cmd) => commands.push(cmd),
    document: { createElement: () => new Element(), getElementById: () => new Element() },
    urlEl: new Element(), reloadBtn: new Element(), loadbar: new Element(),
    backBtn: new Element(), fwdBtn: new Element(), menuPop: new Element(),
    updateItem: new Element(), signinBtn: new Element(),
    renderDl() {}, renderBar() {},
  };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../src/shell/chrome/render.js'), 'utf8'), context);
  const render = () => context.window.__cryo_update(state);
  render();
  const press = (target, button = 0) => {
    const tab = tabsEl.children[0];
    tab.listeners.pointerdown?.({ target, button, preventDefault() {} });
  };
  return { commands, tabsEl, state, render, press };
}

test('first press on a frozen tab activates before a state update replaces it', () => {
  const f = fixture();
  const title = f.tabsEl.children[0].children.find((c) => c.className === 'title');
  f.press(title);
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
    const tab = f.tabsEl.children[0];
    f.press(targetClass ? tab.children.find((c) => c.className === targetClass) : tab);
    assert.equal(f.commands.length, 1);
    assert.equal(f.commands[0].cmd, 'activate');
  }
});

test('freeze and close controls do not activate the tab', () => {
  const f = fixture();
  f.state.tabs[0].state = 'active';
  f.render();
  for (const control of ['fz', 'x']) {
    const target = f.tabsEl.children[0].children.find((c) => c.className === control);
    f.press(target);
    target.onclick();
  }
  assert.deepEqual(f.commands.map((c) => c.cmd), ['freeze', 'close']);
});

test('middle and right presses do not activate tabs', () => {
  const f = fixture();
  f.press(f.tabsEl.children[0], 1);
  f.press(f.tabsEl.children[0], 2);
  assert.equal(f.commands.length, 0);
});
