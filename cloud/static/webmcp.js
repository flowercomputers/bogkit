'use strict';
// Current WebMCP draft plus the earlier navigator API used by preview browsers.
// HTTP and remote MCP remain available when the browser has no WebMCP support.
(async () => {
  const context = document.modelContext || navigator.modelContext;
  if (!context || typeof context.registerTool !== 'function') return;
  const result = value => ({ content: [{ type: 'text', text: JSON.stringify(value) }] });
  async function request(path, options = {}) {
    const response = await fetch(path, { credentials: 'same-origin', cache: 'no-store', ...options });
    const data = await response.json();
    if (!response.ok) throw new Error(`${data.error?.message || 'Request failed'}${data.request_id ? ` (request ${data.request_id})` : ''}`);
    return data;
  }
  async function register(name, description, properties, required, execute, readOnly = true) {
    await context.registerTool({ name, description,
      inputSchema: { type: 'object', properties, required, additionalProperties: false },
      annotations: { readOnlyHint: readOnly },
      execute: async args => result(await execute(args))
    });
  }
  try {
    await register('bog_service_info', 'Discover Bog Cloud API operations, authentication, free prototype allowance and limits. No sign-in required.', {}, [], () => request('/v1'));
    await register('bog_templates', 'List the available Bog templates and the default template. No sign-in required.', {}, [], () => request('/v1/templates'));
    if (location.pathname !== '/console') return;
    // Never hand browser cookies, CSRF values or issued credentials to the model.
    const session = await request('/console-session');
    if (!session.csrf_token) return;
    const uuid = { type: 'string', format: 'uuid' };
    const workspace = { ...uuid, description: 'Explicit workspace. Omit for your personal workspace; the visible selector is never inherited.' };
    function selected(path, id) {
      if (id === undefined) return path;
      if (!/^[0-9a-f]{8}-[0-9a-f-]{27}$/i.test(id)) throw new Error('workspace_id must be a UUID');
      return `${path}?workspace_id=${encodeURIComponent(id)}`;
    }
    await register('bog_list_workspaces', 'List workspaces accessible to the signed-in person and their effective Bog allowances.', {}, [], () => request('/v1/workspaces'));
    await register('bog_list_bogs', 'List Bogs in your personal workspace or an explicitly selected shared workspace.', { workspace_id: workspace }, [], args => request(selected('/v1/bogs', args.workspace_id)));
    await register('bog_describe_bog', 'Describe one accessible Bog. Shared workspaces must be selected explicitly.', { bog_id: uuid, workspace_id: workspace }, ['bog_id'], args => request(selected(`/v1/bogs/${encodeURIComponent(args.bog_id)}`, args.workspace_id)));
    await register('bog_create_bog', 'Create a Bog using records-v1. Reuse the same idempotency_key and name when retrying. Omitted workspace means personal. This consumes a Bog allowance.', {
      name: { type: 'string', minLength: 1, maxLength: 80 },
      idempotency_key: { type: 'string', minLength: 1, maxLength: 128 }, workspace_id: workspace
    }, ['name', 'idempotency_key'], async args => {
      // Resolve a fresh session for each write so logout/refresh cannot leave a
      // stale cached authorization in the browser tool implementation.
      const current = await request('/console-session');
      const created = await request(selected('/v1/bogs', args.workspace_id), {
        method: 'POST', headers: { 'Content-Type': 'application/json', 'x-csrf-token': current.csrf_token, 'Idempotency-Key': args.idempotency_key },
        body: JSON.stringify({ name: args.name })
      });
      document.dispatchEvent(new Event('bog-resources-changed'));
      return created;
    }, false);
  } catch {
    // Unsupported preview browsers and signed-out consoles retain the normal UI.
    // Registration or sign-in failures must not break the page or leak details.
  }
})();
