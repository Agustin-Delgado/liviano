// Runs at the start of every top-level document (RF-14). It never fills
// anything on its own: it tells the app when a login form is sent, whether
// the page that follows still asks for a password, and when the person
// clicks a login field; it fills only when the app calls the fill function
// after the person picked an account.
(() => {
  'use strict';
  if (window.top !== window) return;
  const webview = window.chrome && window.chrome.webview;
  if (!webview) return;

  const post = (message) => {
    message.liviano = 'vault';
    try { webview.postMessage(message); } catch (_) { /* the view is closing */ }
  };
  const valueSetter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
  const TEXT_TYPES = ['text', 'email', 'tel', 'url'];
  const isInput = (el) => el instanceof HTMLInputElement;
  const isPassword = (el) => isInput(el) && el.type === 'password';
  const isText = (el) => isInput(el) && TEXT_TYPES.includes(el.type);
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0 && getComputedStyle(el).visibility !== 'hidden';
  };
  const scopeOf = (el) => (el && (el.form || (el.closest && el.closest('form')))) || document;
  const passwords = (root) => Array.from(root.querySelectorAll('input[type=password]')).filter(visible);
  const tagged = (el) => /\b(username|email)\b/.test(el.autocomplete || '');

  // The user name of a password field: the one marked as such, else the
  // last text field before it, else the first one of its form.
  const usernameFor = (password) => {
    const fields = Array.from(scopeOf(password).querySelectorAll('input')).filter(isText);
    const marked = fields.find(tagged);
    if (marked) return marked;
    const shown = fields.filter(visible);
    let before = null;
    for (const el of shown) {
      if (el.compareDocumentPosition(password) & Node.DOCUMENT_POSITION_FOLLOWING) before = el;
    }
    return before || shown[0] || null;
  };
  const looksLikeUsername = (el) => isText(el) && (tagged(el) || passwords(scopeOf(el)).length > 0);

  // A login that stays on the same document (a single-page app) is over
  // when its password field goes away.
  let watching = 0;
  const capture = (root) => {
    const password = passwords(root).find((el) => el.value);
    if (!password) return;
    const user = usernameFor(password);
    post({ kind: 'submit', username: user ? user.value : '', password: password.value });
    clearInterval(watching);
    let ticks = 0;
    watching = setInterval(() => {
      ticks += 1;
      if (!password.isConnected || !visible(password)) {
        clearInterval(watching);
        post({ kind: 'page', hasPassword: passwords(document).length > 0 });
      } else if (ticks > 20) {
        clearInterval(watching);
      }
    }, 500);
  };

  const offer = (el) => {
    const r = el.getBoundingClientRect();
    const scale = window.devicePixelRatio || 1;
    post({ kind: 'field', left: Math.round(r.left * scale), bottom: Math.round(r.bottom * scale) });
  };

  let lastKey = '';
  document.addEventListener('submit', (e) => capture(scopeOf(e.target)), true);
  document.addEventListener('keydown', (e) => {
    lastKey = e.key;
    if (e.isTrusted && e.key === 'Enter' && isPassword(e.target)) capture(scopeOf(e.target));
  }, true);
  document.addEventListener('click', (e) => {
    if (!e.isTrusted) return;
    const target = e.target;
    if (isPassword(target) || looksLikeUsername(target)) {
      offer(target);
      return;
    }
    const button = target.closest && target.closest('button, input[type=submit], input[type=image], [role=button]');
    if (button) capture(scopeOf(button));
  }, true);
  document.addEventListener('focusin', (e) => {
    if (e.isTrusted && lastKey === 'Tab' && (isPassword(e.target) || looksLikeUsername(e.target))) offer(e.target);
  }, true);

  // Whether the page that loaded asks for a password: right after a login,
  // that means it failed and nothing is offered.
  window.addEventListener('load', () => {
    setTimeout(() => post({ kind: 'page', hasPassword: passwords(document).length > 0 }), 300);
  }, { once: true });

  const set = (el, value) => {
    el.focus();
    valueSetter.call(el, value);
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  };
  // Called by the app with the account the person chose. It checks the
  // origin again: the page may have moved on since the menu opened.
  Object.defineProperty(window, '__livianoVaultFill', {
    value: (origin, username, password) => {
      if (location.origin !== origin) return false;
      const active = document.activeElement;
      const anchor = isInput(active) ? active : null;
      const field = isPassword(anchor) ? anchor : passwords(scopeOf(anchor))[0] || passwords(document)[0];
      const user = field ? usernameFor(field) : (anchor && looksLikeUsername(anchor) ? anchor : null);
      if (user) set(user, username);
      if (field) set(field, password);
      return Boolean(user || field);
    },
  });
})();
