'use strict';
for (const button of document.querySelectorAll('[data-copy]')) {
  button.addEventListener('click', async () => {
    const source = document.getElementById(button.dataset.copy);
    const text = source.textContent.replace('the service URL from this page', location.origin);
    const original = [...button.childNodes].map(node => node.cloneNode(true));
    const status = document.getElementById('copy-status');
    try {
      await navigator.clipboard.writeText(text);
      button.textContent = 'Copied';
      if (status) status.textContent = 'Example copied to clipboard.';
      setTimeout(() => { button.replaceChildren(...original); }, 1800);
    } catch {
      if (status) status.textContent = 'Clipboard unavailable. Select and copy the example manually.';
      source.hidden = false;
      button.textContent = 'Select text below to copy';
    }
  });
}
