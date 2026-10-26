const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { JSDOM } = require('jsdom');

const PANE_JS = fs.readFileSync(
  path.join(__dirname, '../src/shell/chrome/extensions.js'), 'utf8');

const INSTALLED = [
  {
    slug: 'tab-tidier', name: 'Tab Tidier', version: '1.2.3',
    manifest_version: 3, dir: 'C:\\Users\\me\\AppData\\Local\\cryo\\extensions\\tab-tidier',
  },
  {
    slug: 'old-thing', name: 'Old Thing', version: '0.9', manifest_version: 2,
    dir: 'C:\\Users\\me\\AppData\\Local\\cryo\\extensions\\old-thing',
  },
];

function fixture(libData) {
  const dom = new JSDOM('<!doctype html><body><div id="body"></div></body>',
    { runScripts: 'outside-only' });
  const { window } = dom;
  const { document } = window;
  const commands = [];
  const libBody = document.getElementById('body');
  Object.assign(window, {
    libData,
    libBody,
    // Copied into this realm: deepStrictEqual rejects a jsdom prototype.
    ipc: (cmd) => commands.push({ ...cmd }),
    // The house helpers, from library.js.
    node: (tag, cls, text) => {
      const n = document.createElement(tag);
      if (cls) n.className = cls;
      if (text !== undefined) n.textContent = text;
      return n;
    },
    button: (label, cls, onclick) => {
      const b = document.createElement('button');
      b.className = 'lbtn' + (cls ? ' ' + cls : '');
      b.textContent = label;
      b.onclick = onclick;
      return b;
    },
  });
  // paintExtensions is scoped to the script, so the export rides along in
  // the same eval rather than being reached from outside it.
  window.eval(PANE_JS + '\nwindow.__paint = paintExtensions;');
  window.__paint();
  const text = () => libBody.textContent;
  const buttons = () => [...libBody.querySelectorAll('button')]
    .map((b) => b.textContent);
  const click = (label) => {
    const target = [...libBody.querySelectorAll('button')]
      .find((b) => b.textContent === label);
    assert.ok(target, `no button labelled ${label}`);
    target.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  };
  return { window, libBody, commands, text, buttons, click };
}

const supported = (extensions) => ({
  pane: 'extensions', extensions, extensions_supported: true, incognito: false,
});

test('installed extensions list with version, manifest version and folder', () => {
  const f = fixture(supported(INSTALLED));
  assert.match(f.text(), /2 extensions/);
  assert.match(f.text(), /Tab Tidier/);
  assert.match(f.text(), /Version 1\.2\.3 — Manifest V3/);
  assert.match(f.text(), /Version 0\.9 — Manifest V2/);
  // The folder is shown so it can be found and left alone.
  assert.ok(f.libBody.querySelector('.u.path'));
  assert.match(f.text(), /extensions\\tab-tidier/);
});

test('the pane says unpacked-only and that loading waits for a new tab', () => {
  const f = fixture(supported(INSTALLED));
  assert.match(f.text(), /Unpacked folders only/);
  assert.match(f.text(), /not a \.crx or a Web Store link/);
  assert.match(f.text(), /tabs opened after it is added/i);
  assert.match(f.text(), /editing or moving that copy/i);
});

test('removing sends the slug, never a path', () => {
  const f = fixture(supported(INSTALLED));
  f.click('Remove');
  assert.deepEqual(f.commands, [{ cmd: 'extension_remove', slug: 'tab-tidier' }]);
  assert.ok(!JSON.stringify(f.commands).includes('\\'));
});

test('adding asks the app to raise the picker and carries no path', () => {
  const f = fixture(supported(INSTALLED));
  f.click('Add unpacked folder');
  assert.deepEqual(f.commands, [{ cmd: 'extension_add' }]);
});

test('nothing installed invites a folder instead of looking broken', () => {
  const f = fixture(supported([]));
  assert.match(f.text(), /1 extension|0 extensions/);
  assert.match(f.text(), /Nothing installed/);
  assert.match(f.text(), /manifest\.json/);
  assert.deepEqual(f.buttons(), ['Add unpacked folder']);
});

test('an unsupported platform says so and offers no add button', () => {
  const f = fixture({
    pane: 'extensions', extensions: [], extensions_supported: false,
    incognito: false,
  });
  assert.match(f.text(), /cannot run them/);
  assert.ok(f.libBody.querySelector('.lnote.warn'));
  assert.deepEqual(f.buttons(), []);
});

test('an incognito window gives the incognito reason, not the platform one', () => {
  const f = fixture({
    pane: 'extensions', extensions: [], extensions_supported: false,
    incognito: true,
  });
  assert.match(f.text(), /incognito window: it loads no extensions/);
  assert.doesNotMatch(f.text(), /WebView2/);
  assert.deepEqual(f.buttons(), []);
});

test('a pane with no extensions key at all still paints', () => {
  const f = fixture({ pane: 'extensions', extensions_supported: true, incognito: false });
  assert.match(f.text(), /0 extensions/);
  assert.match(f.text(), /Nothing installed/);
});
