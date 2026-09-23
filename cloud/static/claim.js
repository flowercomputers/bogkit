'use strict';
(async () => {
  const box = document.getElementById('claim-details');
  if (!box) return;
  const session = await fetch('/console-session', { credentials: 'same-origin' });
  if (!session.ok) return;
  const data = await session.json();
  const form = document.createElement('form');
  const label = document.createElement('label');
  label.textContent = 'Keep this Bog in ';
  const select = document.createElement('select');
  for (const workspace of data.workspaces || []) {
    const option = document.createElement('option');
    option.value = workspace.id;
    option.textContent = workspace.name;
    select.append(option);
  }
  label.append(select);
  const nameLabel = document.createElement('label');
  nameLabel.textContent = 'Bog name ';
  const name = document.createElement('input');
  name.name = 'name';
  name.required = true;
  name.maxLength = 100;
  name.value = box.dataset.name || '';
  nameLabel.append(name);
  const button = document.createElement('button');
  button.type = 'submit';
  button.textContent = 'Claim Bog';
  const status = document.createElement('p');
  status.setAttribute('role', 'status');
  form.append(label, nameLabel, button, status);
  box.append(form);
  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    button.disabled = true;
    const response = await fetch(location.pathname, {
      method: 'POST', credentials: 'same-origin',
      headers: { 'content-type': 'application/json', 'x-csrf-token': data.csrf_token },
      body: JSON.stringify({ workspace_id: select.value, name: name.value })
    });
    const result = await response.json();
    if (response.ok) {
      status.textContent = 'Claimed. Temporary access has ended.';
      const link = document.createElement('a');
      link.href = '/console#resources';
      link.textContent = 'Open your workspace';
      box.append(link);
      form.remove();
    } else {
      status.textContent = result.error?.message || 'Could not claim this Bog.';
      button.disabled = false;
    }
  });
})();
