// Helpers shared by the dashboard (app.js) and the integrations page (doc.js).
// Every URL is relative so the front works at the root or under any proxy prefix.
'use strict';

window.Inbox = (() => {
  const $ = (id) => document.getElementById(id);

  function el(tag, attrs, ...children) {
    const e = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === undefined || v === null || v === false) continue;
      if (k === 'class') e.className = v;
      else if (k === 'text') e.textContent = v;
      else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
      else e.setAttribute(k, v === true ? '' : v);
    }
    for (const c of children) {
      if (c === null || c === undefined || c === false) continue;
      e.append(c instanceof Node ? c : document.createTextNode(String(c)));
    }
    return e;
  }

  const icon = (name) => el('span', { class: 'mdi mdi-' + name, 'aria-hidden': 'true' });

  // Admin API call. POSTs carry a JSON body and the X-Inbox-Request header: a
  // cross-site page cannot send either without a CORS preflight, which the
  // server never grants (anti-CSRF; the interface has no password).
  async function api(method, path, body) {
    const opts = { method, cache: 'no-store', headers: {} };
    if (method !== 'GET') {
      opts.headers['content-type'] = 'application/json';
      opts.headers['x-inbox-request'] = '1';
      opts.body = JSON.stringify(body === undefined ? {} : body);
    }
    const res = await fetch(path, opts);
    const data = await res.json().catch(() => ({}));
    const hostError = $('host-error');
    if (!res.ok) {
      const err = new Error((data.error && data.error.message) || 'HTTP ' + res.status);
      err.status = res.status;
      err.code = data.error && data.error.code;
      if (err.code === 'host_not_allowed' && hostError) {
        $('host-error-host').textContent = location.host;
        hostError.hidden = false;
      }
      throw err;
    }
    if (hostError) hostError.hidden = true;
    return data;
  }

  let toastTimer = null;
  function toast(text) {
    const t = $('toast');
    if (!t) return;
    t.textContent = text;
    t.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { t.hidden = true; }, 3500);
  }

  // Copies `text`. The Clipboard API needs a secure context (HTTPS or localhost):
  // over plain HTTP the target element is selected so Ctrl+C finishes the job.
  async function copy(text, target) {
    if (navigator.clipboard && window.isSecureContext) {
      try {
        await navigator.clipboard.writeText(text);
        toast('Copied');
        return true;
      } catch (_) {
        // Fall through to the selection fallback.
      }
    }
    if (target) {
      const range = document.createRange();
      range.selectNodeContents(target);
      const sel = window.getSelection();
      sel.removeAllRanges();
      sel.addRange(range);
      let ok = false;
      try {
        ok = document.execCommand('copy');
      } catch (_) {
        ok = false;
      }
      toast(ok ? 'Copied' : 'Text selected: press Ctrl+C to copy');
      return ok;
    }
    toast('Copy unavailable');
    return false;
  }

  // Older versions stored display settings here; the dashboard has none now.
  try {
    localStorage.removeItem('inbox.settings');
  } catch (_) {
    // Storage unavailable: nothing to clean.
  }

  // Theme: "system" (default, follows the browser), "light" or "dark". The
  // choice is applied before first paint by a tiny inline script in <head>.
  const THEME_KEY = 'inbox.theme';
  function readTheme() {
    try {
      const t = localStorage.getItem(THEME_KEY);
      return t === 'light' || t === 'dark' ? t : 'system';
    } catch (_) {
      return 'system';
    }
  }
  function applyTheme(theme) {
    if (theme === 'light' || theme === 'dark') document.documentElement.dataset.theme = theme;
    else delete document.documentElement.dataset.theme;
    try {
      if (theme === 'system') localStorage.removeItem(THEME_KEY);
      else localStorage.setItem(THEME_KEY, theme);
    } catch (_) {
      // Storage unavailable: the choice lasts for this page only.
    }
    for (const b of document.querySelectorAll('[data-theme-value]')) {
      b.setAttribute('aria-pressed', b.dataset.themeValue === theme ? 'true' : 'false');
    }
  }
  for (const b of document.querySelectorAll('[data-theme-value]')) {
    b.addEventListener('click', () => applyTheme(b.dataset.themeValue));
  }
  applyTheme(readTheme());

  return { $, el, icon, api, toast, copy };
})();
