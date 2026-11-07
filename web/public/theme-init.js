// Runs synchronously from <head>, before the first paint, so a stored theme does not flash.
// A classic script (not a module) because CSP only allows same-origin scripts and a module would
// run after the page has been painted. The key must stay equal to the `theme` preference in
// src/stores/prefs.ts ('apm.v1.' + 'theme', stored as JSON); theme.test.ts checks that.
(() => {
  try {
    const stored = JSON.parse(localStorage.getItem('apm.v1.theme'));
    if (stored === 'light' || stored === 'dark') {
      document.documentElement.setAttribute('data-theme', stored);
    }
  } catch {
    // Storage is blocked or holds something else: follow the system theme.
  }
})();
