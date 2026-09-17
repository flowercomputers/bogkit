'use strict';
const $ = id => document.getElementById(id);
const createKeys = new Map();
let csrf = '', workspace = '', owner = false, account = '', invitation = '', platformOperator = false;
let workspaceItems = [];
const fragment = new URLSearchParams(location.hash.slice(1));
if (fragment.has('invite')) { invitation = fragment.get('invite'); history.replaceState(null, '', location.pathname); }
function status(message, error = false) { $('status').textContent = message; $('status').toggleAttribute('data-error', error); }
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
async function task(button, run) { button.disabled = true; try { await run(); } catch (error) { status(error.message, true); } finally { button.disabled = false; } }
function reveal(label, secret) { $('secret-label').textContent = label; $('secret').textContent = secret; $('secret-panel').hidden = false; $('secret-panel').scrollIntoView({ block: 'center' }); }
$('hide-secret').onclick = () => { $('secret').textContent = ''; $('secret-panel').hidden = true; };
async function refresh() {
  $('hide-secret').click();
  const [bogData, memberData] = await Promise.all([api(selected('/v1/bogs')), api('/v1/workspaces/' + workspace + '/members')]);
  const bogs = bogData.bogs || []; $('bogs').replaceChildren(); $('token-bog').replaceChildren(); $('tokens').replaceChildren();
  const current = workspaceItems.find(item => item.id === workspace);
  const allowance = current?.bog_limit === null ? 'Uncapped Bogs (host capacity still applies)' : `${current?.bog_limit ?? 3} Bogs allowed`;
  $('allowance').textContent = `${bogs.length} ${bogs.length === 1 ? 'Bog' : 'Bogs'} · ${allowance} · 16 MiB of JSON records each`;
  if (!bogs.length) $('bogs').append(element('p', 'Your first project can start here. Create a Bog above.'));
  for (const bog of bogs) {
    const row = element('div', '', 'row'), info = element('div', bog.name);
    info.append(element('small', `${bog.id} · ${bog.status || 'saved'}`)); row.append(info);
    try { const usage = await api(selected('/v1/bogs/' + bog.id + '/usage')); const data = usage.data || usage; info.append(element('small', `${(data.logical_bytes / 1048576).toFixed(2)} MiB used of ${(data.limit_bytes / 1048576).toFixed(0)} MiB`)); } catch { info.append(element('small', 'Usage temporarily unavailable')); }
    const option = element('option', bog.name); option.value = bog.id; $('token-bog').append(option);
    if (owner) row.append(action('Delete Bog', async () => {
      const confirm = prompt(`Permanently delete ${bog.name} and its records? Type its ID to confirm:\n${bog.id}`);
      if (confirm !== bog.id) return;
      await api(selected('/v1/bogs/' + bog.id), { method: 'DELETE', body: JSON.stringify({ confirm }) }); await refresh(); status('Bog deleted.');
    }));
    $('bogs').append(row);
    const tokenData = await api(selected('/v1/bogs/' + bog.id + '/tokens'));
    for (const token of tokenData.tokens || []) {
      if (token.revoked_at) continue;
      const line = element('div', '', 'row'); line.append(element('span', `${bog.name} · ${token.scope} · ${token.id}`));
      if (owner) line.append(action('Revoke', async () => { await api(selected('/v1/bogs/' + bog.id + '/tokens/' + token.id), { method: 'DELETE' }); await refresh(); status('Credential revoked.'); }));
      $('tokens').append(line);
    }
  }
  $('members').replaceChildren();
  for (const member of memberData.members || []) {
    const row = element('div', '', 'row'); row.append(element('span', `${member.account_id}${member.account_id === account ? ' (you)' : ''} · ${member.role}`));
    if (owner && member.account_id !== account) row.append(action('Remove', async () => {
      if (!confirm('Remove this person and revoke the app credentials they issued in this workspace?')) return;
      await api('/v1/workspaces/' + workspace + '/members/' + member.account_id, { method: 'DELETE' }); await refresh(); status('Member removed.');
    })); $('members').append(row);
  }
  $('invite').hidden = !owner;
  $('invitations').replaceChildren();
  if (owner) {
    const pending = await api('/v1/workspaces/' + workspace + '/invitations');
    for (const invite of pending.invitations || []) {
      if (invite.revoked_at || invite.accepted_at) continue;
      const row = element('div', '', 'row'); row.append(element('span', `Invitation · ${invite.role} · expires ${new Date(invite.expires_at * 1000).toLocaleDateString()}`));
      row.append(action('Revoke invitation', async () => { await api('/v1/workspaces/' + workspace + '/invitations/' + invite.id, { method: 'DELETE' }); await refresh(); status('Invitation revoked.'); })); $('invitations').append(row);
    }
  }
}
$('workspace').onchange = async () => { workspace = $('workspace').value; owner = $('workspace').selectedOptions[0].dataset.role === 'owner'; try { await refresh(); status('Workspace ready.'); } catch (e) { status(e.message, true); } };
for (const id of ['create', 'issue', 'invite']) $(id).onsubmit = event => {
  event.preventDefault(); task(event.submitter, async () => {
    if (id === 'create') {
      const label = workspace + ':' + $('name').value; if (!createKeys.has(label)) createKeys.set(label, crypto.randomUUID());
      await api(selected('/v1/bogs'), { method: 'POST', headers: { 'Idempotency-Key': createKeys.get(label) }, body: JSON.stringify({ name: $('name').value }) }); createKeys.delete(label); $('name').value = ''; await refresh(); status('Bog created. Your app can start using it.');
    } else if (id === 'issue') {
      const token = await api(selected('/v1/bogs/' + $('token-bog').value + '/tokens'), { method: 'POST', body: JSON.stringify({ scope: $('scope').value }) });
      await refresh(); reveal('Save this credential now. It will not be shown again.', token.token || token.secret); status('App credential issued.');
    } else {
      const invite = await api('/v1/workspaces/' + workspace + '/invitations', { method: 'POST', body: JSON.stringify({ role: $('role').value }) });
      reveal('Share this single-use invitation. It expires in seven days.', location.origin + '/console#invite=' + encodeURIComponent(invite.secret));
      const row = element('div', '', 'row'); row.append(element('span', 'Invitation · ' + invite.id)); row.append(action('Revoke invitation', async () => { await api('/v1/workspaces/' + workspace + '/invitations/' + invite.id, { method: 'DELETE' }); row.remove(); $('hide-secret').click(); status('Invitation revoked.'); })); $('invitations').append(row);
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
    if (session.authentication_mode === 'github_native') { $('agent-access').hidden = false; await refreshAgents(); }
    $('app').hidden = false; $('logout').hidden = false;
    await loadWorkspaces();
    const me = await api('/v1/me'); if (me.workspace_id) $('workspace').value = me.workspace_id;
    platformOperator = me.platform_operator === true; $('platform').hidden = !platformOperator; if (platformOperator) await refreshPlatform();
    workspace = $('workspace').value; owner = $('workspace').selectedOptions[0]?.dataset.role === 'owner';
    await refresh(); status('Signed in. Your workspace is ready.');
    if (invitation) { const preview = await api('/v1/invitations/preview', { method: 'POST', body: JSON.stringify({ secret: invitation }) }); $('invite-description').textContent = `You have been invited to join ${preview.workspace_name || preview.name} as ${preview.role}.`; $('invitation').hidden = false; }
  } catch (e) { status(e.status === 401 ? 'Sign in with GitHub to open your workspace.' : e.message, e.status !== 401); $('login').hidden = false; if (invitation) status('Sign in with GitHub, then open your invitation link again.'); }
}
async function refreshAgents() {
  const data = await api('/v1/agent-tokens'); $('agent-tokens').replaceChildren();
  for (const token of data.tokens || []) { if (token.revoked_at) continue; const row=element('div','','row'); row.append(element('span', `${token.name} · expires ${new Date(token.expires_at*1000).toLocaleDateString()}`)); row.append(action('Revoke',async()=>{await api('/v1/agent-tokens/'+token.id,{method:'DELETE'});await refreshAgents();status('Agent credential revoked.');}));$('agent-tokens').append(row); }
}
$('agent-issue').onsubmit = event => { event.preventDefault();task(event.submitter,async()=>{const token=await api('/v1/agent-tokens',{method:'POST',body:JSON.stringify({name:$('agent-name').value})});await refreshAgents();$('agent-name').value='';reveal('Save this agent credential in a secret store. It is shown once and expires in 30 days.',token.token);status('Agent credential created.');}); };
async function loadWorkspaces(preferred = workspace) {
  const data = await api('/v1/workspaces'); workspaceItems = data.workspaces || [];
  $('workspace').replaceChildren();
  for (const item of workspaceItems) {
    const option = element('option', item.name + (item.personal ? ' · personal' : ' · organization') + (item.bog_limit === null ? ' · uncapped' : ''));
    option.value = item.id; option.dataset.role = item.role; $('workspace').append(option);
  }
  if (workspaceItems.some(item => item.id === preferred)) $('workspace').value = preferred;
  workspace = $('workspace').value; owner = $('workspace').selectedOptions[0]?.dataset.role === 'owner';
}
$('create-workspace').onsubmit = event => {
  event.preventDefault(); task(event.submitter, async () => {
    const name = $('workspace-name').value, label = 'workspace:' + name;
    if (!createKeys.has(label)) createKeys.set(label, crypto.randomUUID());
    const result = await api('/v1/workspaces', {method:'POST', headers:{'Idempotency-Key':createKeys.get(label)}, body:JSON.stringify({name})});
    createKeys.delete(label); $('workspace-name').value = '';
    await loadWorkspaces(result.workspace.id); await refresh();
    if (platformOperator) await refreshPlatform();
    status('Organization created. Invite your team below.');
  });
};
async function refreshPlatform() {
  const data = await api('/v1/platform');
  for (const kind of ['accounts', 'workspaces']) {
    const container = $('platform-' + kind); container.replaceChildren();
    for (const item of data[kind] || []) {
      const row = element('div', '', 'row');
      const label = kind === 'accounts' ? `${item.issuer === 'https://github.com' ? 'GitHub ID' : item.issuer} ${item.subject} · account ${item.id}` : `${item.name} · ${item.id}`;
      const info = element('div', label);
      info.append(element('small', item.uncapped_bogs ? 'Uncapped flag enabled' : (kind === 'workspaces' && item.bog_limit === null ? 'Uncapped through account flag' : 'Standard allowance')));
      row.append(info, action(item.uncapped_bogs ? 'Use standard allowance' : 'Enable uncapped Bogs', async () => {
        await api('/v1/platform/' + kind + '/' + encodeURIComponent(item.id) + '/quota', {method:'PUT', body:JSON.stringify({uncapped_bogs:!item.uncapped_bogs})});
        await loadWorkspaces(); await refresh(); await refreshPlatform(); status('Allowance updated. Existing records are unchanged.');
      }));
      container.append(row);
    }
  }
}
start();
