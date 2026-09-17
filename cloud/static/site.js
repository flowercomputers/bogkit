'use strict';
const menuButton = document.querySelector('.site-menu-button');
const mobileNavigation = document.getElementById('mobile-navigation');
const main = document.querySelector('main');
const footer = document.querySelector('.site-footer');
function setMenu(open) {
  menuButton.setAttribute('aria-expanded', String(open));
  menuButton.setAttribute('aria-label', open ? 'Close navigation' : 'Open navigation');
  document.body.classList.toggle('is-mobile-navigation-open', open);
  main.inert = open; footer.inert = open;
  mobileNavigation.hidden = !open;
  requestAnimationFrame(() => mobileNavigation.classList.toggle('is-open', open));
  if (!open) menuButton.focus();
}
menuButton.addEventListener('click', () => setMenu(menuButton.getAttribute('aria-expanded') !== 'true'));
window.addEventListener('keydown', event => {
  if (menuButton.getAttribute('aria-expanded') !== 'true') return;
  if (event.key === 'Escape') setMenu(false);
  if (event.key === 'Tab') {
    const links = [...document.querySelectorAll('.site-brand a'), menuButton, ...mobileNavigation.querySelectorAll('a,button:not([hidden])')];
    const first = links[0], last = links[links.length - 1];
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
  }
});
matchMedia('(min-width:641px)').addEventListener('change', event => { if(event.matches && menuButton.getAttribute('aria-expanded') === 'true') setMenu(false); });
const logout = document.getElementById('logout'), mobileLogout = document.getElementById('mobile-logout');
if (logout && mobileLogout) {
  new MutationObserver(() => { mobileLogout.hidden = logout.hidden; }).observe(logout, {attributes:true,attributeFilter:['hidden']});
  mobileLogout.addEventListener('click', () => logout.click());
}
for (const a of document.querySelectorAll('.site-nav a')) if (a.getAttribute('href') === location.pathname) a.setAttribute('aria-current','page');
