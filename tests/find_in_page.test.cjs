const assert = require('node:assert/strict');
const fs = require('node:fs');
const { test } = require('node:test');
const { JSDOM } = require('jsdom');
const html = fs.readFileSync('src/shell/chrome/chrome.html', 'utf8');
const script = fs.readFileSync('src/shell/chrome/find.js', 'utf8');
function fixture() {
  const dom = new JSDOM(html, { runScripts:'outside-only', url:'http://cryo.localhost/chrome' });
  const { window } = dom;
  const commands = [];
  window.ipc = value => commands.push(JSON.parse(JSON.stringify(value)));
  window.eval(script);
  const bar = window.document.getElementById('findbar');
  const input = bar.querySelector('input[type=search]');
  const type = text => { input.value=text; input.dispatchEvent(new window.Event('input')); };
  return { window, bar, input, commands, type, count:bar.querySelector('output'), next:bar.querySelector('[data-find=next]') };
}
test('opening find focuses its field and preserves the query on repeated Ctrl+F', () => {
  const f=fixture(); f.window.__cryo_find_open(true); f.type('ice');
  f.window.__cryo_find_open(true);
  assert.equal(f.window.document.activeElement,f.input); assert.equal(f.input.value,'ice');
});
test('queries and case choices go to the engine without writing browser storage', () => {
  const f=fixture(); f.window.__cryo_find_open(true); f.type('Ice');
  const matchCase=f.bar.querySelector('input[type=checkbox]'); matchCase.checked=true;
  matchCase.dispatchEvent(new f.window.Event('change'));
  assert.deepEqual(f.commands.at(-1),{cmd:'find_query',query:'Ice',match_case:true});
  assert.equal(f.window.localStorage.length,0); assert.equal(f.window.sessionStorage.length,0);
});
test('matches enable next/previous; Enter and Shift+Enter choose direction', () => {
  const f=fixture(); f.window.__cryo_find_open(true); f.type('ice');
  assert.equal(f.next.disabled,true);
  f.window.__cryo_find_result({active:1,total:3}); assert.equal(f.count.textContent,'1 / 3');
  for (const backwards of [false,true]) {
    f.input.dispatchEvent(new f.window.KeyboardEvent('keydown',{key:'Enter',shiftKey:backwards,bubbles:true}));
    assert.deepEqual(f.commands.at(-1),{cmd:'find_next',backwards});
  }
});
test('no matches and engine errors disable navigation and render as text', () => {
  const f=fixture(); f.window.__cryo_find_open(true); f.type('ice');
  f.window.__cryo_find_result({active:0,total:0}); assert.equal(f.count.textContent,'No matches');
  f.window.__cryo_find_result({error:'<b>Unavailable</b>'});
  assert.equal(f.count.querySelector('b'),null); assert.equal(f.next.disabled,true);
});
test('Escape clears the private query and ignores results delivered after closing', () => {
  const f=fixture(); f.window.__cryo_find_open(true); f.type('private words');
  f.input.dispatchEvent(new f.window.KeyboardEvent('keydown',{key:'Escape',bubbles:true}));
  assert.equal(f.bar.hidden,true); assert.equal(f.input.value,'');
  assert.deepEqual(f.commands.at(-1),{cmd:'find_close'});
  f.window.__cryo_find_result({active:2,total:3}); assert.equal(f.count.textContent,'');
});
test('an unsupported engine states its limitation and keeps Close available', () => {
  const f=fixture(); f.window.__cryo_find_open(false);
  assert.equal(f.input.disabled,true); assert.match(f.count.textContent,/unavailable/);
  assert.equal(f.bar.querySelector('[data-find=close]').disabled,false);
});
