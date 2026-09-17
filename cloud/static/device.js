'use strict';
const $ = id => document.getElementById(id);
let lookupGeneration = 0;
let reviewed = null;
let deciding = false;
$('code').value = new URLSearchParams(location.search).get('user_code') || '';

function pending(busy) {
  $('code').disabled = busy;
  $('lookup-submit').disabled = busy;
  $('approve').disabled = $('deny').disabled = busy || !reviewed;
}
function clearReview() {
  reviewed = null;
  $('review').hidden = true;
  $('login').hidden = true;
  $('approve').disabled = $('deny').disabled = true;
}
async function post(body, csrf) {
  const response = await fetch('/auth/device/approve', {
    method: 'POST', credentials: 'same-origin', cache: 'no-store',
    headers: { 'Content-Type': 'application/json', 'x-csrf-token': csrf },
    body: JSON.stringify(body)
  });
  const value = await response.json();
  if (!response.ok) throw Error(value.error?.message || 'Request unavailable');
  return value;
}
$('code').oninput = () => {
  if (deciding) return;
  ++lookupGeneration;
  clearReview();
  pending(false);
};
$('lookup').onsubmit = async event => {
  event.preventDefault();
  if (deciding) return;
  const generation = ++lookupGeneration;
  const code = $('code').value.trim().toUpperCase();
  clearReview();
  pending(true);
  try {
    if (!/^[0-9A-F]{8}$/.test(code)) throw Error('Enter the 8-character public code.');
    const response = await fetch('/console-session', { credentials: 'same-origin', cache: 'no-store' });
    if (generation !== lookupGeneration) return;
    if (response.status === 401) {
      $('login').href = '/auth/login?user_code=' + encodeURIComponent(code);
      $('login').hidden = false;
      $('status').textContent = 'Sign in to review this request.';
      return;
    }
    if (!response.ok) throw Error('Sign-in unavailable');
    const session = await response.json();
    if (generation !== lookupGeneration) return;
    const value = await post({ user_code: code }, session.csrf_token);
    if (generation !== lookupGeneration) return;
    // Bind the action to the same immutable request whose details are rendered.
    reviewed = Object.freeze({ code, csrf: session.csrf_token });
    $('name').textContent = value.name;
    $('access').textContent = value.access;
    $('review').hidden = false;
    $('status').textContent = 'Review the requested access, then choose approve or deny.';
  } catch (error) {
    if (generation === lookupGeneration) $('status').textContent = error.message;
  } finally {
    if (generation === lookupGeneration) pending(false);
  }
};
for (const [id, approve] of [['approve', true], ['deny', false]]) {
  $(id).onclick = async () => {
    if (deciding || !reviewed) return;
    const request = reviewed;
    deciding = true;
    pending(true);
    try {
      const result = await post({ user_code: request.code, approve }, request.csrf);
      if (result.redirect_uri) { location.replace(result.redirect_uri); return; }
      clearReview();
      $('status').textContent = approve
        ? 'Approved. Your agent can now collect its credential.'
        : 'Denied. No credential will be issued.';
    } catch (error) {
      $('status').textContent = error.message;
    } finally {
      deciding = false;
      pending(false);
    }
  };
}
pending(false);
if ($('code').value) $('lookup').requestSubmit();
