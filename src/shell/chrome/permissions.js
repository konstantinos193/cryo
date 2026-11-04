// Permission consent belongs to trusted chrome. Origin and capability are
// text, and the app-issued token binds each answer to its pending request.
// The two buttons are remembered for the site; a prompt that merely
// goes away (Escape, a click elsewhere) denies that one request only.
const permissionPop = document.getElementById('permission-pop');
let permissionOpen = false;
let permissionToken = null;
const denyPermission = () => {
  if (permissionToken !== null) ipc({cmd: 'permission_dismiss', token: permissionToken});
};
window.__cryo_permission = (data) => {
  permissionOpen = !!data;
  permissionToken = data ? data.token : null;
  permissionPop.classList.toggle('open', permissionOpen);
  permissionPop.innerHTML = '';
  if (!data) return;
  const title = document.createElement('strong');
  title.textContent = 'Allow ' + data.capability + ' access?';
  const origin = document.createElement('p');
  origin.textContent = data.origin;
  const note = document.createElement('p');
  note.className = 'perm-note';
  note.textContent = 'Your choice is remembered for this site. Change it any time from the lock in the address bar.';
  const deny = document.createElement('button');
  deny.textContent = 'Block';
  deny.onclick = () => ipc({cmd: 'permission_answer', token: data.token, allow: false});
  const allow = document.createElement('button');
  allow.className = 'perm-allow';
  allow.textContent = 'Allow';
  allow.onclick = () => ipc({cmd: 'permission_answer', token: data.token, allow: true});
  permissionPop.append(title, origin, note, deny, allow);
};
permissionPop.addEventListener('click', e => e.stopPropagation());
