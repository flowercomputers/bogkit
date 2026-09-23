'use strict';
(() => {
  const form = document.getElementById('claimable-create');
  if (!form) return;
  const status = document.getElementById('claimable-status');
  const result = document.getElementById('claimable-result');
  let credential, bogId, expiresAt, pending;
  const randomHex = () => [...crypto.getRandomValues(new Uint8Array(32))].map(b => b.toString(16).padStart(2, '0')).join('');
  async function api(path, method = 'GET', body) {
    const response = await fetch(path, {
      method, headers: { Authorization: `Bearer ${credential}`, ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) },
      body: body === undefined ? undefined : JSON.stringify(body)
    });
    const data = await response.json();
    if (!response.ok) throw new Error(data.error?.message || `Request failed (${response.status})`);
    return data;
  }
  async function renew() {
    const claim = await api(`/v1/claimable-bogs/${bogId}/claim`, 'POST');
    document.getElementById('claimable-claim').href = claim.claim_url;
    status.textContent = 'Claim link ready. It expires in at most 15 minutes.';
  }
  form.addEventListener('submit', async event => {
    event.preventDefault();
    const button = form.querySelector('button');
    button.disabled = true;
    status.textContent = 'Creating your Bog…';
    try {
      const name = form.elements.name.value;
      if (!pending || pending.name !== name) pending = { name, key: crypto.randomUUID(), recovery: randomHex() };
      const response = await fetch('/v1/claimable-bogs', {
        method: 'POST', headers: { 'Content-Type': 'application/json', 'Idempotency-Key': pending.key },
        body: JSON.stringify({ name, recovery_secret: pending.recovery })
      });
      const data = await response.json();
      if (!response.ok) throw new Error(data.error?.message || `Creation failed (${response.status})`);
      pending = undefined;
      credential = data.credential.access_token;
      bogId = data.id;
      expiresAt = data.expires_at;
      document.getElementById('claimable-expiry').textContent = new Date(expiresAt * 1000).toLocaleString();
      document.getElementById('claimable-expiry').dateTime = new Date(expiresAt * 1000).toISOString();
      result.hidden = false;
      form.hidden = true;
      await renew();
      status.textContent = 'Temporary Bog created. Its claim link is ready.';
    } catch (error) { status.textContent = error.message; button.disabled = false; }
  });
  document.getElementById('claimable-renew').addEventListener('click', () => renew().catch(e => { status.textContent = e.message; }));
  document.getElementById('claimable-write').addEventListener('click', async () => {
    try {
      const note = document.getElementById('claimable-note').value;
      await api(`/v1/bogs/${bogId}/docs/example`, 'PUT', { title: note, body: note, updated_at: Date.now() });
      status.textContent = 'Note saved in your temporary Bog.';
    } catch (error) { status.textContent = error.message; }
  });
  document.getElementById('claimable-read').addEventListener('click', async () => {
    try {
      const data = await api(`/v1/bogs/${bogId}/views/docs?limit=10`);
      const notes = (data.data || []).map(item => `${item.value?.title || item.key}: ${item.value?.body || ''}`);
      document.getElementById('claimable-output').textContent = notes.join('\n') || 'No notes yet.';
    } catch (error) { status.textContent = error.message; }
  });
  setInterval(() => {
    if (!expiresAt) return;
    const remaining = Math.max(0, Math.ceil(expiresAt - Date.now() / 1000));
    const minutes = Math.floor(remaining / 60);
    document.getElementById('claimable-countdown').textContent = `${minutes}m ${String(remaining % 60).padStart(2, '0')}s`;
    if (remaining === 0) {
      status.textContent = 'The temporary Bog has expired.';
      for (const button of result.querySelectorAll('button')) button.disabled = true;
      credential = undefined;
    }
  }, 1000);
})();
