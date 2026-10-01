// Applies the saved theme before the first paint (loaded synchronously in <head>).
'use strict';
(() => {
  try {
    const s = JSON.parse(localStorage.getItem('inbox.settings') || '{}');
    if (s.theme === 'light' || s.theme === 'dark') {
      document.documentElement.dataset.theme = s.theme;
    }
  } catch (_) {
    // Storage unavailable: follow the system theme.
  }
})();
