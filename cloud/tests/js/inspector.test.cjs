const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const code = fs.readFileSync('cloud/static/inspector.js', 'utf8');
function harness() {
  const elements = new Map(), sent = [], listeners = {};
  const el = id => {
    if (!elements.has(id)) elements.set(id, { value: '', textContent: '', disabled: true, hidden: true, addEventListener: (name, fn) => { listeners[id + ':' + name] = fn; } });
    return elements.get(id);
  };
  const parent = { postMessage: message => sent.push(message) };
  const window = { parent, addEventListener: (name, fn) => { listeners[name] = fn; } };
  vm.runInNewContext(code, { window, document: { getElementById: el }, setTimeout: () => 1, clearTimeout() {} });
  const message = data => listeners.message({ source: parent, data: { jsonrpc: '2.0', ...data } });
  const flush = () => new Promise(resolve => setImmediate(resolve));
  const init = async capabilities => { message({ id: sent[0].id, result: { protocolVersion: '2026-01-26', hostCapabilities: capabilities } }); await flush(); };
  return { el, sent, message, init, flush, listeners };
}
test('host negotiation gates calls and preserves display-only fallback', async () => {
  const h = harness(); assert.equal(h.sent[0].method, 'ui/initialize');
  await h.init({}); assert.equal(h.el('run').disabled, true);
  await h.listeners['inspect:submit']({ preventDefault() {} });
  assert.equal(h.sent.some(m => m.method === 'tools/call'), false);
  h.message({ method: 'ui/notifications/tool-result', params: { structuredContent: { hello: 'world' } } });
  assert.match(h.el('result').textContent, /world/);
});
test('read calls carry explicit workspace and comparison changes only resource', async () => {
  const h = harness(); await h.init({ serverTools: {} });
  Object.entries({ bog: 'bog-id', workspace: 'shared-id', mode: 'search_resource', resource: 'lexical', second: 'semantic', query: '{"query":"example","limit":2}' }).forEach(([k,v]) => { h.el(k).value = v; });
  const run = h.listeners['inspect:submit']({ preventDefault() {} });
  let call = h.sent.at(-1); assert.equal(call.method, 'tools/call'); assert.equal(call.params.arguments.workspace_id, 'shared-id');
  h.message({ id: call.id, result: { structuredContent: { data: ['one'] } } }); await h.flush();
  call = h.sent.at(-1); assert.equal(call.params.arguments.resource, 'semantic'); assert.equal(call.params.name, 'search_resource');
  h.message({ id: call.id, result: { structuredContent: { data: ['two'] } } }); await run;
  assert.match(h.el('comparison').textContent, /two/); assert.equal(h.el('run').disabled, false);
});
test('untrusted text stays text, foreign messages ignored and writes impossible', async () => {
  const h = harness(); await h.init({ serverTools: {} });
  h.listeners.message({ source: {}, data: { jsonrpc: '2.0', method: 'ui/notifications/tool-result', params: { bad: 'foreign' } } });
  assert.equal(h.el('result').textContent, '');
  h.message({ method: 'ui/notifications/tool-result', params: { structuredContent: { data: '<img src=x onerror=alert(1)>' } } });
  assert.match(h.el('result').textContent, /<img/); assert.equal(h.el('result').innerHTML, undefined);
  h.el('mode').value = 'issue_token';
  await h.listeners['inspect:submit']({ preventDefault() {} });
  assert.equal(h.sent.some(m => m.method === 'tools/call'), false);
});
test('permission errors replace previous results, management status remains a normal authorized read', async () => {
  const h = harness(); await h.init({ serverTools: {} });
  h.el('result').textContent = 'previous private data';
  h.el('mode').value = 'definition_update_status'; h.el('bog').value = 'bog'; h.el('job').value = 'job';
  const run = h.listeners['inspect:submit']({ preventDefault() {} });
  const call = h.sent.at(-1); assert.equal(call.params.name, 'definition_update_status'); assert.equal(call.params.arguments.job_id, 'job');
  h.message({ id: call.id, result: { isError: true, structuredContent: { error: { code: 'forbidden' } } } }); await run;
  assert.doesNotMatch(h.el('result').textContent, /previous/); assert.match(h.el('status').textContent, /failed/);
});
