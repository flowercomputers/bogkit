'use strict';
fetch('/v1', { cache: 'no-store' }).then(response => response.json()).then(service => {
  if (service.authentication_configured !== false) return;
  document.querySelector('.hero .intro').textContent = 'Create a Bog, store JSON records, and read Fold views through HTTP or MCP. Existing token access is available now; GitHub sign-in is still being configured.';
  document.querySelector('#connect .section-head > p:last-child').textContent = 'Connect with your existing bearer credential. GitHub approval and automatic personal workspaces will become available once sign-in setup is complete. Keep credentials in your local secret store, never in a conversation.';
  document.querySelector('.hero .quiet').textContent = 'Token-access preview · HTTP + MCP · GitHub sign-in pending';
  for (const link of document.querySelectorAll('a[href="/console"]')) link.textContent = 'Sign-in status ↗';
}).catch(() => {});
for (const button of document.querySelectorAll('[data-copy]')) {
  button.addEventListener('click', async () => {
    const text = document.getElementById(button.dataset.copy).textContent;
    const status = document.getElementById('copy-status');
    try {
      await navigator.clipboard.writeText(text);
      button.textContent = 'Copied';
      status.textContent = 'Example copied to clipboard.';
      setTimeout(() => { button.textContent = 'Copy'; }, 1800);
    } catch {
      status.textContent = 'Clipboard unavailable. Select and copy the example manually.';
      button.textContent = 'Select text to copy';
    }
  });
}
