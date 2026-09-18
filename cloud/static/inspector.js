'use strict';
// MCP Apps 2026-01-26: dependency-free JSON-RPC bridge, no network or credentials.
(() => {
  const el = id => document.getElementById(id);
  const allowed = new Set(['list_resources', 'query_resource', 'search_resource', 'definition_update_status']);
  const pending = new Map();
  let nextId = 1, connected = false, ready = false, busy = false;
  const status = text => { el('status').textContent = text; };
  const render = (id, value) => { el(id).textContent = JSON.stringify(value, null, 2); };
  const send = message => window.parent.postMessage({ jsonrpc: '2.0', ...message }, '*');
  function request(method, params) {
    return new Promise((resolve, reject) => {
      const id = nextId++;
      const timer = setTimeout(() => { pending.delete(id); reject(new Error('The host did not respond. Retry the read.')); }, 30000);
      pending.set(id, { resolve, reject, timer });
      send({ id, method, params });
    });
  }
  window.addEventListener('message', event => {
    // A sibling frame or unrelated window cannot provide data or complete calls.
    if (event.source !== window.parent || !event.data || event.data.jsonrpc !== '2.0') return;
    const message = event.data;
    if (pending.has(message.id)) {
      const entry = pending.get(message.id);
      pending.delete(message.id); clearTimeout(entry.timer);
      if (message.error) entry.reject(new Error(message.error.message || 'Host request failed.'));
      else if ('result' in message) entry.resolve(message.result);
      else entry.reject(new Error('Invalid host response.'));
      return;
    }
    if (!connected) return;
    if (message.method === 'ui/notifications/tool-input') {
      const args = message.params?.arguments || {};
      for (const [key, id] of [['bog_id','bog'], ['workspace_id','workspace'], ['resource','resource'], ['job_id','job']]) {
        el(id).value = typeof args[key] === 'string' ? args[key] : '';
      }
      if (args.query && typeof args.query === 'object') el('query').value = JSON.stringify(args.query);
    } else if (message.method === 'ui/notifications/tool-result') {
      render('result', message.params?.structuredContent ?? message.params);
      status(message.params?.isError ? 'Read failed. See the result for details.' : 'Result received.');
    } else if (message.method === 'ui/notifications/tool-cancelled') {
      status('Read cancelled. You can retry.');
    } else if (message.method === 'ping' && message.id !== undefined) {
      send({ id: message.id, result: {} });
    }
  });
  async function call(name, args) {
    if (!ready || !allowed.has(name)) throw new Error('This host does not support this read. Use the ordinary MCP tool.');
    const result = await request('tools/call', { name, arguments: args });
    return result.structuredContent ?? result;
  }
  el('inspect').addEventListener('submit', async event => {
    event.preventDefault();
    if (!ready || busy) return;
    busy = true; el('run').disabled = true; el('comparison').hidden = true;
    // Never leave a previous successful read visible after a denied/error read.
    el('result').textContent = ''; el('comparison').textContent = '';
    try {
      const name = el('mode').value;
      const args = { bog_id: el('bog').value.trim() };
      if (el('workspace').value.trim()) args.workspace_id = el('workspace').value.trim();
      if (name === 'definition_update_status') args.job_id = el('job').value.trim();
      if (name === 'query_resource' || name === 'search_resource') {
        args.resource = el('resource').value.trim();
        args.query = JSON.parse(el('query').value);
        if (!args.query || typeof args.query !== 'object' || Array.isArray(args.query)) throw new Error('Query parameters must be a JSON object.');
      }
      status('Reading…');
      const result = await call(name, args);
      let failed = Boolean(result?.error || result?.isError);
      render('result', result);
      if (name === 'search_resource' && el('second').value.trim()) {
        el('comparison').hidden = false;
        const comparison = await call(name, { ...args, resource: el('second').value.trim() });
        failed ||= Boolean(comparison?.error || comparison?.isError);
        render('comparison', comparison);
      }
      status(failed ? 'Read failed. See the result for details.' : 'Read complete.');
    } catch (error) { status(error.message); }
    finally { busy = false; el('run').disabled = !ready; }
  });
  if (window.parent === window) { status('Open this inspector in an MCP Apps host. Ordinary MCP reads remain available.'); return; }
  request('ui/initialize', {
    protocolVersion: '2026-01-26', appInfo: { name: 'Bog resource inspector', version: '1.0.0' }, appCapabilities: {}
  }).then(result => {
    if (result.protocolVersion !== '2026-01-26') throw new Error('Unsupported App protocol. Use the ordinary MCP tools.');
    connected = true;
    ready = !!result.hostCapabilities?.serverTools;
    const mode = result.hostContext?.toolInfo?.tool?.name;
    if (allowed.has(mode)) el('mode').value = mode;
    send({ method: 'ui/notifications/initialized', params: {} });
    el('run').disabled = !ready;
    status(ready ? 'Connected. Read-only access through the current MCP connection.' : 'This host can display results but cannot run reads. Use the ordinary MCP tools.');
  }).catch(error => status(error.message));
})();
