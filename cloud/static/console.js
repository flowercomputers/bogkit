'use strict';
const $ = id => document.getElementById(id);
const createKeys = new Map();
let csrf = '', workspace = '', owner = false, account = '', invitation = '', platformOperator = false;
let workspaceItems = [];
let pendingAppAccess = null;
// This reference grants no authority. Keep only a validated UUID across the
// same-tab sign-in redirect; credentials and handoff metadata stay in memory.
const handoffStorageKey = 'bog.pending-app-handoff';
const validHandoff = value => typeof value === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value);
function rememberHandoff(id) {
  if (!validHandoff(id)) return;
  try { sessionStorage.setItem(handoffStorageKey, id); } catch { /* Storage can be disabled. */ }
}
function clearHandoffReference() {
  try { sessionStorage.removeItem(handoffStorageKey); } catch { /* Storage can be disabled. */ }
  const params = new URLSearchParams(location.search);
  if (params.has('handoff')) {
    params.delete('handoff');
    history.replaceState(null, '', location.pathname + (params.size ? '?' + params.toString() : '') + location.hash);
  }
}
function restoreHandoffReference() {
  const params = new URLSearchParams(location.search);
  let id = params.get('handoff');
  if (!params.has('handoff')) {
    try { id = sessionStorage.getItem(handoffStorageKey); } catch { /* Storage can be disabled. */ }
  }
  if (!validHandoff(id)) { clearHandoffReference(); return null; }
  rememberHandoff(id);
  return id;
}
const requestedHandoff = restoreHandoffReference();
const fragment = new URLSearchParams(location.hash.slice(1));
if (fragment.has('invite')) { invitation = fragment.get('invite'); history.replaceState(null, '', location.pathname); }
function status(message, error = false) { $('status').hidden = !error && (message === 'Signed in. Your workspace is ready.' || message.endsWith(' is ready.'));  $('status').textContent = message; $('status').toggleAttribute('data-error', error); }
async function api(path, options = {}, retried = false) {
  const headers = { ...options.headers };
  if (options.body) headers['Content-Type'] = 'application/json';
  if (options.method && options.method !== 'GET') headers['x-csrf-token'] = csrf;
  const response = await fetch(path, { credentials: 'same-origin', ...options, headers, cache: 'no-store' });
  if (response.status === 401 && csrf && !retried && !path.startsWith('/auth/')) {
    const renewed = await fetch('/auth/refresh', { method: 'POST', credentials: 'same-origin', headers: { 'x-csrf-token': csrf } });
    if (renewed.ok) { const update = await renewed.json(); if (update.csrf_token) csrf = update.csrf_token; return api(path, options, true); }
  }
  const data = response.status === 204 ? {} : await response.json();
  if (!response.ok) { const error = new Error(data.error?.message || 'Request could not be completed. Please try again.'); error.status = response.status; throw error; }
  return data;
}
const selected = path => path + (path.includes('?') ? '&' : '?') + 'workspace_id=' + encodeURIComponent(workspace);
function element(tag, text, className) { const node = document.createElement(tag); node.textContent = text; if (className) node.className = className; return node; }
function action(text, run) { const button = element('button', text); button.type = 'button'; button.onclick = () => task(button, run); return button; }
async function task(button, run) { button.disabled = true; const errorBox = button.closest?.('dialog')?.querySelector('.dialog-error'); if (errorBox) errorBox.hidden = true; try { await run(); } catch (error) { if (errorBox) { errorBox.textContent = error.message; errorBox.hidden = false; } else status(error.message, true); } finally { button.disabled = false; } }
function reveal(label, secret) { $('secret-label').textContent = label; $('secret').textContent = secret; $('secret-panel').hidden = false; $('secret-panel').scrollIntoView({ block: 'center' }); }
$('hide-secret').onclick = () => { $('secret').textContent = ''; $('secret-panel').hidden = true; };
async function refresh() {
  $('hide-secret').click();
  const [bogData, memberData] = await Promise.all([api(selected('/v1/bogs')), api('/v1/workspaces/' + workspace + '/members')]);
  const bogs = bogData.bogs || []; $('bogs').replaceChildren(); $('token-bog').replaceChildren(); $('tokens').replaceChildren();
  const bogRows = makeTable($('bogs'), ['Name', 'Status', 'Storage', 'Requests · 1h', 'Waiting', 'Actions'], 'No Bogs yet. Create your first Bog above.');
  const keyRows = makeTable($('tokens'), ['Name', 'Bog', 'Permissions', 'Actions'], 'No application API keys yet.');
  for (const bog of bogs) {
    const row = element('tr', ''), info = element('td', bog.name), storage = element('td', ''), controls = element('td', '');
    info.append(element('small', bog.id)); const requests = element('td', '—'), waiting = element('td', '—'); row.append(info, element('td', bog.status || 'saved'), storage, requests, waiting); row.append(controls);
    try { const result = await api(selected('/v1/bogs/' + bog.id + '/metrics?window=1h')); const metrics = result.data || result; requests.textContent = (metrics.requests || []).reduce((n, r) => n + r.count, 0).toLocaleString() + (metrics.window_complete && !metrics.truncated ? '' : ' (partial)'); waiting.textContent = String(metrics.waits?.active ?? '—'); requests.title = 'Observed requests in the last hour; partial means collection started recently or observations were truncated.'; } catch { requests.title = waiting.title = 'Metrics unavailable'; }
    try { const usage = await api(selected('/v1/bogs/' + bog.id + '/usage')); const data = usage.data || usage; storage.append(element('span', `${(data.logical_bytes / 1048576).toFixed(2)} MiB used of ${(data.limit_bytes / 1048576).toFixed(0)} MiB`)); } catch { storage.append(element('span', 'Unavailable')); }
    const option = element('option', bog.name); option.value = bog.id; $('token-bog').append(option);
    if (owner) controls.append(action('Delete Bog', async () => {
      openBogDeletion(bog);
    }));
    addTableRow(bogRows, row);
    const tokenData = await api(selected('/v1/bogs/' + bog.id + '/tokens'));
    for (const token of tokenData.tokens || []) {
      if (token.revoked_at) continue;
      const line = element('tr', ''), label = element('td', token.label || 'App credential'), actions = element('td', ''); label.append(element('small', token.id)); line.append(label, element('td', bog.name), element('td', token.scope === 'write' ? 'Read and write' : 'Read only'), actions);
      if (owner) actions.append(action('Revoke', async () => { await api(selected('/v1/bogs/' + bog.id + '/tokens/' + token.id), { method: 'DELETE' }); await refresh(); status('Credential revoked.'); }));
      addTableRow(keyRows, line);
    }
  }
  const memberRows = makeTable($('members'), ['Account', 'Role', 'Status', 'Actions'], 'No members.');
  for (const member of memberData.members || []) {
    const row = element('tr', ''), actions = element('td', ''); const memberCell = element('td', ''); memberCell.append(identityLabel(member.account_id, `${member.account_id.slice(0,8)}${member.account_id === account ? ' (you)' : ''}`)); row.append(memberCell, element('td', member.role), element('td', 'Active'), actions);
    if (owner && member.account_id !== account) actions.append(action('Remove', async () => {
      if (!confirm('Remove this person and revoke the app credentials they issued in this workspace?')) return;
      await api('/v1/workspaces/' + workspace + '/members/' + member.account_id, { method: 'DELETE' }); await refresh(); status('Member removed.');
    })); addTableRow(memberRows, row);
  }
  $('invite').hidden = !owner; $('new-invite').hidden = !owner;
  if (owner) {
    const pending = await api('/v1/workspaces/' + workspace + '/invitations');
    for (const invite of pending.invitations || []) {
      if (invite.revoked_at || invite.accepted_at) continue;
      const row = element('tr', ''), actions = element('td', ''); const identity = element('td', 'Invitation link'); identity.append(element('small', invite.id.slice(0, 8)));
      const state = element('td', invite.expires_at * 1000 <= Date.now() ? 'Expired' : 'Pending'); state.append(element('small', 'Expires ' + new Date(invite.expires_at * 1000).toLocaleDateString()));
      row.append(identity, element('td', invite.role), state, actions);
      actions.append(action('Revoke invitation', async () => { await api('/v1/workspaces/' + workspace + '/invitations/' + invite.id, { method: 'DELETE' }); await refresh(); status('Invitation revoked.'); })); addTableRow(memberRows, row);
    }
  }
}
function renderOrganizationSwitcher() {
  const current = workspaceItems.find(item => item.id === workspace);
  $('organization-name').textContent = current?.name || 'Select organization';
  $('workspace-heading').textContent = 'Your Workspace: ' + (current?.name || 'Personal');
  $('organization-switcher').hidden = !workspaceItems.length;
  $('organization-options').replaceChildren();
  for (const item of workspaceItems) {
    const button = action('', async () => {
      if (item.id === workspace) { $('organization-switcher').open = false; return; }
      const previous = workspace;
      workspace = item.id; owner = item.role === 'owner'; $('workspace').value = workspace;
      $('organization-switcher').open = false;
      $('organization-switcher').querySelector('summary').focus();
      renderOrganizationSwitcher();
      try { await refresh(); status(`${item.name} is ready.`); }
      catch (error) { workspace = previous; $('workspace').value = previous; owner = workspaceItems.find(w => w.id === previous)?.role === 'owner'; renderOrganizationSwitcher(); throw error; }
    });
    button.setAttribute('aria-current', String(item.id === workspace));
    const check = element('span', item.id === workspace ? '✓' : ''); check.setAttribute('aria-hidden', 'true');
    const name = element('span', item.name); if (item.personal) name.append(element('small', 'Personal workspace'));
    button.append(check, name); $('organization-options').append(button);
  }
}
$('new-organization').onclick = () => {
  $('organization-switcher').open = false; $('organization-error').hidden = true;
  $('organizations').showModal(); $('workspace-name').focus();
};
for (const id of ['close-organization', 'cancel-organization']) $(id).onclick = () => $('organizations').close();
$('organizations').addEventListener('close', () => $('organization-switcher').querySelector('summary').focus());
document.addEventListener('click', event => {
  if (!$('organization-switcher').contains(event.target)) $('organization-switcher').open = false;
});
document.addEventListener('keydown', event => {
  if (event.key === 'Escape' && $('organization-switcher').open) {
    $('organization-switcher').open = false; $('organization-switcher').querySelector('summary').focus();
  }
});
$('workspace').onchange = async () => { workspace = $('workspace').value; owner = workspaceItems.find(item => item.id === workspace)?.role === 'owner'; renderOrganizationSwitcher(); try { await refresh(); status('Workspace ready.'); } catch (e) { status(e.message, true); } };
for (const id of ['create', 'issue', 'invite']) $(id).onsubmit = event => {
  event.preventDefault(); task(event.submitter, async () => {
    if (id === 'create') {
      const label = workspace + ':' + $('name').value; if (!createKeys.has(label)) createKeys.set(label, crypto.randomUUID());
      await api(selected('/v1/bogs'), { method: 'POST', headers: { 'Idempotency-Key': createKeys.get(label) }, body: JSON.stringify({ name: $('name').value }) }); createKeys.delete(label); $('name').value = ''; $('new-bog-dialog').close(); await refresh(); status('Bog created. Your app can start using it.');
    } else if (id === 'issue') {
      const prepared = await api(selected('/v1/bogs/' + $('token-bog').value + '/app-access'), { method: 'POST', body: JSON.stringify({ scope: $('scope').value, label: $('app-label').value }) });
      showAppHandoff(prepared); status('Private access prepared. Download explicitly within ten minutes.');
    } else {
      const invite = await api('/v1/workspaces/' + workspace + '/invitations', { method: 'POST', body: JSON.stringify({ role: $('role').value }) });
      $('new-invite-dialog').close(); await refresh();
      reveal('Share this single-use invitation. It expires in seven days.', location.origin + '/console#invite=' + encodeURIComponent(invite.secret));
    }
  });
};
$('logout').onclick = () => task($('logout'), async () => { await api('/auth/logout', { method: 'POST' }); location.replace('/'); });
$('accept-invite').onclick = () => task($('accept-invite'), async () => { await api('/v1/invitations/accept', { method: 'POST', body: JSON.stringify({ secret: invitation }) }); invitation = ''; location.reload(); });
async function start() {
  try {
    const service = await api('/v1');
    if (service.authentication_configured === false) {
      status('Public signup, workspace sharing, and invitations are unavailable. Ask the operator privately for a management credential to provision, or a single-Bog app credential to use an existing Bog. Existing credentials work through HTTP and MCP.');
      return;
    }
    const session = await api('/console-session'); csrf = session.csrf_token; account = session.account?.id || session.account_id || '';
    if (session.authentication_mode === 'github_native') { $('agent-access-option').hidden = false; $('agent-access-option').disabled = false; await refreshAgents(); }
    $('app').hidden = false; $('logout').hidden = false;
    const identity = account.slice(0, 8);
    const userMenu = document.createElement('details'); userMenu.className = 'user-menu';
    const userLabel = element('summary', ''); userLabel.setAttribute('aria-label', 'Account: ' + (identity || 'signed in'));
    const avatar = identityAvatar(account);
    userLabel.append(avatar, element('span', identity || 'Account'));
    $('logout').before(userMenu); userMenu.append(userLabel, $('logout'));
    for (const link of document.querySelectorAll('.site-header a[href="/docs"], .site-header a[href="/connect"], .site-header a[href="/console"], #mobile-navigation a[href="/docs"], #mobile-navigation a[href="/connect"], #mobile-navigation a[href="/console"]')) link.remove();
    document.addEventListener('click', event => { if (!userMenu.contains(event.target)) userMenu.open = false; });
    document.addEventListener('keydown', event => { if (event.key === 'Escape' && userMenu.open) { userMenu.open = false; userLabel.focus(); } });
    await loadWorkspaces();
    const me = await api('/v1/me'); if (me.workspace_id) $('workspace').value = me.workspace_id;
    platformOperator = me.platform_operator === true; $('platform').hidden = !platformOperator; $('admin-nav').hidden = !platformOperator; if (platformOperator) await refreshPlatform();
    workspace = $('workspace').value; owner = $('workspace').selectedOptions[0]?.dataset.role === 'owner';
    renderOrganizationSwitcher(); await refresh(); status('Signed in. Your workspace is ready.');
    if (location.hash === '#agent-access') selectAccess('agent');
    if (requestedHandoff) { selectAccess('application');
      try { showAppHandoff(await api('/v1/app-access/' + encodeURIComponent(requestedHandoff))); }
      catch (error) { if (error.status !== 401) clearHandoffReference(); throw error; }
    }
    if (invitation) { const preview = await api('/v1/invitations/preview', { method: 'POST', body: JSON.stringify({ secret: invitation }) }); $('invite-description').textContent = `You have been invited to join ${preview.workspace_name || preview.name} as ${preview.role}.`; $('invitation').hidden = false; }
  } catch (e) { status(e.status === 401 ? 'Sign in with GitHub to open your workspace.' : e.message, e.status !== 401); $('login').hidden = false; if (invitation) status('Sign in with GitHub, then open your invitation link again.'); }
}
function selectAccess(kind) {
  const agent = kind === 'agent' && !$('agent-access-option').disabled;
  $('access-kind').value = agent ? 'agent' : 'application';
  $('application-access').hidden = agent;
  $('agent-access').hidden = !agent;
  $('hide-secret').click();
}
$('access-kind').onchange = () => selectAccess($('access-kind').value);
async function refreshAgents() {
  const data = await api('/v1/agent-tokens');
  const rows = makeTable($('agent-tokens'), ['Agent', 'Access', 'Expires', 'Actions'], 'No agent credentials yet.');
  for (const token of data.tokens || []) {
    if (token.revoked_at) continue;
    const row = element('tr', ''), actions = element('td', '');
    const agentCell = element('td', ''); agentCell.append(identityLabel(token.id, token.name));
    row.append(agentCell, element('td', 'Your permitted workspaces'), element('td', new Date(token.expires_at * 1000).toLocaleDateString()), actions);
    actions.append(action('Revoke', async () => { await api('/v1/agent-tokens/' + token.id, {method:'DELETE'}); await refreshAgents(); status('Agent credential revoked.'); }));
    addTableRow(rows, row);
  }
}
$('agent-issue').onsubmit = event => { event.preventDefault();task(event.submitter,async()=>{const token=await api('/v1/agent-tokens',{method:'POST',body:JSON.stringify({name:$('agent-name').value})});await refreshAgents();$('agent-name').value='';$('new-access-dialog').close();reveal('Save this agent credential in a secret store. It is shown once and expires in 30 days.',token.token);status('Agent credential created.');}); };
async function loadWorkspaces(preferred = workspace) {
  const data = await api('/v1/workspaces'); workspaceItems = data.workspaces || [];
  $('workspace').replaceChildren();
  for (const item of workspaceItems) {
    const option = element('option', item.name + (item.personal ? ' · personal' : ' · organization') + (item.bog_limit === null ? ' · uncapped' : ''));
    option.value = item.id; option.dataset.role = item.role; $('workspace').append(option);
  }
  if (workspaceItems.some(item => item.id === preferred)) $('workspace').value = preferred;
  workspace = $('workspace').value; owner = $('workspace').selectedOptions[0]?.dataset.role === 'owner';
  renderOrganizationSwitcher();
}
$('create-workspace').onsubmit = event => {
  event.preventDefault(); task(event.submitter, async () => {
    const name = $('workspace-name').value, label = 'workspace:' + name;
    if (!createKeys.has(label)) createKeys.set(label, crypto.randomUUID());
    let result; $('organization-error').hidden = true;
    try { result = await api('/v1/workspaces', {method:'POST', headers:{'Idempotency-Key':createKeys.get(label)}, body:JSON.stringify({name})}); }
    catch (error) { $('organization-error').textContent = error.message; $('organization-error').hidden = false; return; }
    createKeys.delete(label); $('workspace-name').value = ''; $('organizations').close();
    await loadWorkspaces(result.workspace.id); await refresh();
    if (platformOperator) await refreshPlatform();
    status('Organization created. Invite your team through Workspace.');
  });
};
async function refreshPlatform() {
  const data = await api('/v1/platform');
  for (const kind of ['accounts', 'workspaces']) {
    const rows = makeTable($('platform-' + kind), [kind === 'accounts' ? 'Account' : 'Workspace', kind === 'accounts' ? 'Identity' : 'ID', 'Allowance', 'Actions'], 'No ' + kind + ' found.');
    for (const item of data[kind] || []) {
      const row = element('tr', ''), actions = element('td', '');
      const identity = kind === 'accounts' ? `${item.issuer === 'https://github.com' ? 'GitHub ID' : item.issuer} ${item.subject}` : item.id.slice(0, 8);
      const label = element('td', kind === 'accounts' ? item.id.slice(0, 8) : item.name); label.title = item.id; if (kind === 'accounts') { label.textContent = ''; label.append(identityLabel(item.id, item.id.slice(0,8))); }
      const identityCell = element('td', identity); if (kind === 'workspaces') identityCell.title = item.id;
      const inherited = kind === 'workspaces' && !item.uncapped_bogs && item.bog_limit === null;
      row.append(label, identityCell, element('td', item.uncapped_bogs ? 'Uncapped' : inherited ? 'Uncapped via account' : 'Standard'), actions);
      actions.append(action(item.uncapped_bogs ? 'Use standard allowance' : inherited ? 'Set workspace override' : 'Enable uncapped Bogs', async () => {
        await api('/v1/platform/' + kind + '/' + encodeURIComponent(item.id) + '/quota', {method:'PUT', body:JSON.stringify({uncapped_bogs:!item.uncapped_bogs})});
        await loadWorkspaces(); await refresh(); await refreshPlatform(); status('Allowance updated. Existing records are unchanged.');
      }));
      addTableRow(rows, row);
    }
  }
}
function showAppHandoff(prepared) {
  rememberHandoff(prepared.handoff_id);
  pendingAppAccess = prepared;
  $('handoff-description').textContent = `${prepared.label} · ${prepared.scope} access to Bog ${prepared.bog_id} in workspace ${prepared.workspace_id}. Expires ${new Date(prepared.expires_at * 1000).toLocaleTimeString()}.`;
  selectAccess('application');
  if (!$('new-access-dialog').open) $('new-access-dialog').showModal();
  $('app-handoff').hidden = false;
  $('download-app').disabled = false;
}
$('download-app').onclick = () => task($('download-app'), async () => {
  const prepared = pendingAppAccess;
  if (!prepared) { status('Prepare a new handoff before downloading.', true); return; }
  // Consume only on this explicit human action. No secret enters the page or WebMCP result.
  pendingAppAccess = null;
  clearHandoffReference();
  $('app-handoff').hidden = true;
  try {
    const credential = await api('/v1/app-access/' + encodeURIComponent(prepared.handoff_id) + '/redeem', {method:'POST',body:'{}'});
    const configuration = {BOG_CLOUD_URL:location.origin,BOG_ID:credential.bog_id,BOG_CLOUD_TOKEN:credential.token,credential_id:credential.id};
    const url = URL.createObjectURL(new Blob([JSON.stringify(configuration) + '\n'], {type:'application/json'}));
    const link = document.createElement('a'); link.href = url; link.download = 'bog-app-' + credential.id + '.json';
    document.body.append(link); link.click(); link.remove(); setTimeout(() => URL.revokeObjectURL(url), 1000);
    await refresh(); status('Private configuration downloaded. Keep it outside source control; if delivery failed, revoke the listed credential as a workspace owner, or ask an owner to revoke it, then prepare a new handoff.');
  } catch (error) { await refresh(); throw new Error('Download did not complete. Prepare a new handoff; revoke any unused issued credential as a workspace owner, or ask an owner to revoke it. ' + error.message); }
});
start();

document.addEventListener('bog-resources-changed', () => {
  if (workspace) loadWorkspaces().then(() => refresh()).catch(error => status(error.message, true));
});
document.addEventListener('bog-tool-status', async event => {
  const detail = event.detail || {};
  status(detail.message || 'Browser tool completed.', detail.state === 'error');
  if (detail.state === 'success' && validHandoff(detail.handoff_id)) {
    try { showAppHandoff(await api('/v1/app-access/' + encodeURIComponent(detail.handoff_id))); }
    catch (error) { status(error.message, true); }
  }
});

function makeTable(container, headings, emptyText) {
  container.replaceChildren(); container.className = 'table-card';
  const table = element('table', ''), head = element('thead', ''), header = element('tr', ''), body = element('tbody', '');
  for (const title of headings) { const cell = element('th', title); cell.setAttribute('scope', 'col'); header.append(cell); }
  head.append(header); const empty = element('tr', ''), cell = element('td', emptyText, 'empty-state'); cell.colSpan = headings.length; empty.append(cell); body.append(empty); body.emptyRow = empty;
  table.append(head, body); container.append(table); return body;
}
function addTableRow(body, row) { if (body.emptyRow) { body.emptyRow.remove(); body.emptyRow = null; } body.append(row); }
for (const [buttonId, dialogId, focusId] of [['new-bog','new-bog-dialog','name'], ['new-access','new-access-dialog','access-kind'], ['new-invite','new-invite-dialog','role']]) {
  $(buttonId).onclick = () => { const dialog = $(dialogId); dialog.querySelector('.dialog-error').hidden = true; dialog.showModal(); $(focusId).focus(); };
  $(dialogId).addEventListener('close', () => $(buttonId).focus());
}
for (const button of document.querySelectorAll('[data-close]')) button.onclick = () => $(button.dataset.close).close();

function identityAvatar(id) {
  let hash = 2166136261; for (const character of String(id)) hash = Math.imul(hash ^ character.charCodeAt(0), 16777619) >>> 0;
  const hue = hash % 360, other = (hue + 90 + ((hash >>> 8) % 150)) % 360;
  const avatar = element('span', '', 'account-avatar'); avatar.setAttribute('aria-hidden', 'true');
  avatar.style.background = `linear-gradient(90deg, oklch(72% 0.22 ${hue}) 0 50%, oklch(65% 0.24 ${other}) 50% 100%)`; return avatar;
}
function identityLabel(id, text) { const label = element('span', '', 'identity-label'); label.append(identityAvatar(id), element('span', text)); return label; }

let deletingBog = null;
function openBogDeletion(bog) {
  deletingBog = { ...bog, workspace_id: workspace };
  $('delete-bog-description').textContent = `Delete ${bog.name} and all its records? Type its full ID to confirm: ${bog.id}`;
  $('delete-bog-confirm').value = ''; $('delete-bog-dialog').querySelector('.dialog-error').hidden = true;
  $('delete-bog-dialog').showModal(); $('delete-bog-confirm').focus();
}
$('delete-bog-form').onsubmit = event => {
  event.preventDefault(); task(event.submitter, async () => {
    const target = deletingBog;
    if (!target || $('delete-bog-confirm').value !== target.id) throw new Error('Enter the exact Bog ID shown above.');
    await api('/v1/bogs/' + encodeURIComponent(target.id) + '?workspace_id=' + encodeURIComponent(target.workspace_id), {method:'DELETE',body:JSON.stringify({confirm:target.id})});
    $('delete-bog-dialog').close(); deletingBog = null; await refresh(); status('Bog deleted.');
  });
};
for (const dialog of document.querySelectorAll('dialog')) {
  let startedOutside = false;
  const outside = event => { const bounds = dialog.getBoundingClientRect(); return event.clientX < bounds.left || event.clientX > bounds.right || event.clientY < bounds.top || event.clientY > bounds.bottom; };
  dialog.addEventListener('pointerdown', event => { startedOutside = event.target === dialog && outside(event); });
  dialog.addEventListener('click', event => { if (startedOutside && event.target === dialog && outside(event)) dialog.close(); startedOutside = false; });
}
