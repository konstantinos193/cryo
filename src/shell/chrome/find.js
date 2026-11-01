// Active-document find. The engine owns searching; this field owns no history.
const findbar = document.getElementById('findbar');
const findInput = findbar.querySelector('input[type="search"]');
const findCase = findbar.querySelector('input[type="checkbox"]');
const findCount = findbar.querySelector('output');
const findPrevious = findbar.querySelector('[data-find="previous"]');
const findNext = findbar.querySelector('[data-find="next"]');
let findSupported = false;

const searchPage = () => {
  findCount.textContent = findInput.value ? 'Searching…' : '';
  findPrevious.disabled = findNext.disabled = true;
  ipc({cmd: 'find_query', query: findInput.value, match_case: findCase.checked});
};
const stepFind = (backwards) => {
  if (findSupported && findInput.value && !findNext.disabled)
    ipc({cmd: 'find_next', backwards});
};
const closeFind = () => {
  window.__cryo_find_close();
  ipc({cmd: 'find_close'});
};
window.__cryo_find_open = (supported) => {
  findSupported = supported;
  findbar.hidden = false;
  findInput.disabled = findCase.disabled = !supported;
  if (!supported) findCount.textContent = 'Find is unavailable on this engine';
  else {
    findInput.focus();
    findInput.select();
  }
};
window.__cryo_find_close = () => {
  findbar.hidden = true;
  findInput.value = '';
  findCase.checked = false;
  findCount.textContent = '';
  findPrevious.disabled = findNext.disabled = true;
};
window.__cryo_find_result = (result) => {
  if (findbar.hidden) return;
  findCount.textContent = result.error || (findInput.value
    ? (result.total ? result.active + ' / ' + result.total : 'No matches') : '');
  findPrevious.disabled = findNext.disabled = !!result.error || !result.total;
};
findInput.addEventListener('input', (event) => {
  if (!event.isComposing) searchPage();
});
findInput.addEventListener('compositionend', searchPage);
findCase.addEventListener('change', searchPage);
findPrevious.onclick = () => stepFind(true);
findNext.onclick = () => stepFind(false);
findbar.querySelector('[data-find="close"]').onclick = closeFind;
findbar.addEventListener('click', (event) => event.stopPropagation());
document.addEventListener('keydown', (event) => {
  if (findbar.hidden) return;
  if (event.key === 'Escape') {
    event.preventDefault(); event.stopImmediatePropagation(); closeFind();
  } else if (event.key === 'Enter' && findbar.contains(event.target)) {
    event.preventDefault(); event.stopImmediatePropagation(); stepFind(event.shiftKey);
  }
}, true);
window.__cryo_find_close();
