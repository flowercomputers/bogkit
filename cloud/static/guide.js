'use strict';
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
