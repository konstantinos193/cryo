// Permission consent belongs to trusted chrome. Origin and capability are
// text, and the app-issued token binds each answer to its pending request.
const permissionPop = document.getElementById('permission-pop');
let permissionOpen = false;
let permissionToken = null;
const denyPermission = () => {
  if (permissionToken !== null) ipc({cmd: 'permission_answer', token: permissionToken, allow: false});
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
  const deny = document.createElement('button');
  deny.textContent = 'Block';
  deny.onclick = denyPermission;
  const allow = document.createElement('button');
  allow.textContent = 'Allow for this session';
  allow.onclick = () => ipc({cmd: 'permission_answer', token: data.token, allow: true});
  permissionPop.append(title, origin, deny, allow);
};
permissionPop.addEventListener('click', e => e.stopPropagation());
