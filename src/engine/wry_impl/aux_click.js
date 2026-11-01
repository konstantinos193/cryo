addEventListener('auxclick', e => {
  if (e.button !== 1) return;
  const a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
  if (!a || !/^https?:/.test(a.href)) return;
  e.preventDefault();
  window.open(a.href, '_blank');
}, true);
