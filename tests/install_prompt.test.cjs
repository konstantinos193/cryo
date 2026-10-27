const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { JSDOM } = require('jsdom');

const PROMPT_JS = fs.readFileSync(
  path.join(__dirname, '../src/shell/chrome/install.js'), 'utf8');

function fixture() {
  const dom = new JSDOM(
    '<!doctype html><body><div id="install-pop"></div></body>',
    { runScripts: 'outside-only' });
  const { window } = dom;
  const { document } = window;
  const commands = [];
  const dismissed = [];
  const pop = document.getElementById('install-pop');
  Object.assign(window, {
    ipc: (cmd) => commands.push({ ...cmd }),
    reportOverlay: () => {},
    dismissOverlays: () => dismissed.push(true),
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
  window.eval(PROMPT_JS);
  const click = (label) => {
    const target = [...pop.querySelectorAll('button')]
      .find((b) => b.textContent === label);
    assert.ok(target, `no button labelled ${label}`);
    target.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  };
  return {
    window, pop, commands, dismissed, click,
    show: (data) => window.__cryo_install_prompt(data),
    isOpen: () => pop.classList.contains('open'),
  };
}

const PROMPT = {
  token: 7, id: 'nkbihfbeogaeaoehlefnkodbefgpgknn', name: null, fetching: false,
};

test('the prompt names the id and says what will be checked', () => {
  const f = fixture();
  f.show(PROMPT);
  assert.ok(f.isOpen());
  assert.match(f.pop.textContent, /Install this extension\?/);
  assert.match(f.pop.textContent, /nkbihfbeogaeaoehlefnkodbefgpgknn/);
  assert.match(f.pop.textContent, /signed by the key its id comes from/);
  // Opening it puts away whatever else was on screen.
  assert.deepEqual(f.dismissed, [true]);
});

test('the fetching state shows a bar and no buttons to press', () => {
  const f = fixture();
  f.show(PROMPT);
  f.show({ ...PROMPT, fetching: true });
  assert.match(f.pop.textContent, /Installing extension/);
  assert.match(f.pop.textContent, /Fetching from the Chrome Web Store/);
  // Nothing to answer any more, so nothing to press.
  assert.equal(f.pop.querySelectorAll('button').length, 0);
  // Indeterminate until a length arrives.
  assert.ok(f.pop.querySelector('.ins-track').classList.contains('ind'));
});

test('progress fills the bar and counts in MB once a total is known', () => {
  const f = fixture();
  f.show(PROMPT);
  f.show({ ...PROMPT, fetching: true });
  f.window.__cryo_install_progress(5 * 1048576, 20 * 1048576);
  const track = f.pop.querySelector('.ins-track');
  assert.equal(track.classList.contains('ind'), false);
  // Read back as a number: jsdom normalises "25.0%" to "25%".
  assert.equal(parseFloat(track.firstChild.style.width), 25);
  assert.match(f.pop.querySelector('.ins-count').textContent, /5\.0 MB of 20\.0 MB/);
});

test('without a declared total the bar stays indeterminate but still counts', () => {
  const f = fixture();
  f.show(PROMPT);
  f.show({ ...PROMPT, fetching: true });
  f.window.__cryo_install_progress(3 * 1048576, null);
  const track = f.pop.querySelector('.ins-track');
  assert.ok(track.classList.contains('ind'), 'no total means no honest share');
  assert.match(f.pop.querySelector('.ins-count').textContent, /^3\.0 MB$/);
});

test('the result lands in the same card and then puts itself away', () => {
  const f = fixture();
  f.show(PROMPT);
  f.show({ ...PROMPT, fetching: true });
  f.window.__cryo_install_done({ ok: true, text: 'MetaMask 13.53.0.0' });
  assert.match(f.pop.textContent, /Extension installed/);
  assert.match(f.pop.textContent, /MetaMask 13\.53\.0\.0/);
  assert.ok(f.isOpen(), 'still up so the result can be read');
});

test('a failure says why, in the card, rather than vanishing', () => {
  const f = fixture();
  f.show(PROMPT);
  f.show({ ...PROMPT, fetching: true });
  f.window.__cryo_install_done({ ok: false, text: 'the package is larger than 128 MiB' });
  assert.match(f.pop.textContent, /Not installed/);
  assert.match(f.pop.textContent, /larger than 128 MiB/);
  assert.ok(f.pop.querySelector('.ins-mark.bad'));
});

test('progress after the card is gone is dropped, not resurrected', () => {
  const f = fixture();
  f.show(PROMPT);
  f.show(null);
  // Must not throw, and must not reopen anything.
  f.window.__cryo_install_progress(1024, 2048);
  f.window.__cryo_install_done({ ok: true, text: 'late' });
  assert.equal(f.isOpen(), false);
  assert.equal(f.pop.textContent, '');
});

test('Install answers with the token the app issued', () => {
  const f = fixture();
  f.show(PROMPT);
  f.click('Install');
  assert.deepEqual(f.commands, [
    { cmd: 'extension_answer', token: 7, install: true },
  ]);
});

test('Not now refuses with the same token rather than going quiet', () => {
  const f = fixture();
  f.show(PROMPT);
  f.click('Not now');
  assert.deepEqual(f.commands, [
    { cmd: 'extension_answer', token: 7, install: false },
  ]);
});

test('a second prompt replaces the first, and answers carry the new token', () => {
  const f = fixture();
  f.show(PROMPT);
  f.show({ ...PROMPT, token: 8, id: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' });
  assert.equal(f.pop.querySelectorAll('button').length, 2);
  f.click('Install');
  assert.deepEqual(f.commands, [
    { cmd: 'extension_answer', token: 8, install: true },
  ]);
});

test('closing the prompt installs nothing', () => {
  const f = fixture();
  f.show(PROMPT);
  f.show(null);
  assert.equal(f.isOpen(), false);
  assert.equal(f.pop.querySelectorAll('button').length, 0);
  assert.deepEqual(f.commands, []);
});

test('a known name is shown instead of the id when the app has one', () => {
  const f = fixture();
  f.show({ ...PROMPT, name: 'MetaMask' });
  assert.match(f.pop.textContent, /MetaMask/);
  assert.doesNotMatch(f.pop.textContent, /Extension nkbih/);
});
