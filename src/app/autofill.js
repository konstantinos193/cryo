(() => {
  'use strict';
  if (window.top !== window || !window.ipc) return;

  const elementTokens = new WeakMap();
  const elements = new Map();
  let sequence = 0;

  const tokenFor = (element) => {
    let token = elementTokens.get(element);
    if (token) return token;
    token = (++sequence).toString(36) + Math.random().toString(36).slice(2, 9);
    elementTokens.set(element, token);
    elements.set(token, element);
    if (elements.size > 96) elements.delete(elements.keys().next().value);
    return token;
  };

  const send = (message) => {
    try { window.ipc.postMessage(JSON.stringify(message)); } catch (_) {}
  };

  let edited = false;
  document.addEventListener('input', (event) => {
    if (edited || !event.isTrusted) return;
    edited = true;
    send({kind: 'edited'});
  }, true);

  const autocomplete = (input) =>
    (input.autocomplete || '').toLowerCase().split(/\s+/).filter(Boolean);

  const usable = (input) => input instanceof HTMLInputElement &&
    !input.disabled && !input.readOnly && input.type !== 'hidden';

  const usernameFor = (password) => {
    const scope = password.form || password.closest('form') || document;
    const inputs = Array.from(scope.querySelectorAll('input')).filter(usable);
    const explicit = inputs.find((input) => autocomplete(input).includes('username'));
    if (explicit) return explicit;
    const before = inputs.slice(0, Math.max(0, inputs.indexOf(password)));
    return before.reverse().find((input) =>
      input.type === 'text' || input.type === 'email' || input.type === 'url') || null;
  };

  const currentPassword = (scope) => Array.from(scope.querySelectorAll('input[type="password"]'))
    .filter(usable)
    .find((input) => {
      const hint = autocomplete(input);
      return !hint.includes('new-password') && !hint.includes('one-time-code');
    }) || null;

  document.addEventListener('focusin', (event) => {
    const password = event.target;
    if (!usable(password) || password.type !== 'password') return;
    const hint = autocomplete(password);
    if (hint.includes('new-password') || hint.includes('one-time-code')) return;
    const username = usernameFor(password);
    send({
      kind: 'focus',
      username_field: username ? tokenFor(username) : null,
      password_field: tokenFor(password)
    });
  }, true);

  document.addEventListener('submit', (event) => {
    const scope = event.target instanceof HTMLFormElement ? event.target : document;
    const password = currentPassword(scope);
    if (!password || !password.value) return;
    const username = usernameFor(password);
    send({
      kind: 'submit',
      username: username ? username.value : '',
      password: password.value
    });
  }, true);

  const nativeValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
  const write = (token, value) => {
    if (!token) return;
    const input = elements.get(token);
    if (!input || !input.isConnected || !usable(input)) return;
    nativeValue.call(input, value);
    input.dispatchEvent(new Event('input', {bubbles: true}));
    input.dispatchEvent(new Event('change', {bubbles: true}));
  };

  Object.defineProperty(window, '__cryo_fill_password', {
    configurable: false,
    writable: false,
    value: (reply) => {
      write(reply.username_field, reply.username);
      write(reply.password_field, reply.password);
      const password = elements.get(reply.password_field);
      if (password && password.isConnected) password.focus();
    }
  });
})();
