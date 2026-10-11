/* Theme selector: system (default), dark or light, remembered in this browser only.
   Loaded synchronously in <head> so the theme is applied before the first paint. It is the only script
   of the launcher and the login page; the settings pages add settings.js. */
(function () {
  "use strict";
  var KEY = "portal.theme";
  var root = document.documentElement;
  var mode = "system";
  try {
    var stored = window.localStorage.getItem(KEY);
    if (stored === "dark" || stored === "light") mode = stored;
  } catch (e) {}

  function apply(next) {
    if (next === "system") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", next);
    root.setAttribute("data-theme-mode", next);
  }

  root.classList.add("js");
  apply(mode);

  document.addEventListener("DOMContentLoaded", function () {
    var group = document.querySelector("[data-theme-switch]");
    if (!group) return;
    var buttons = group.querySelectorAll("button[data-theme-value]");

    function mark() {
      for (var i = 0; i < buttons.length; i += 1) {
        buttons[i].setAttribute("aria-pressed", String(buttons[i].getAttribute("data-theme-value") === mode));
      }
    }

    group.addEventListener("click", function (event) {
      var button = event.target.closest("button[data-theme-value]");
      if (!button) return;
      mode = button.getAttribute("data-theme-value");
      try {
        if (mode === "system") window.localStorage.removeItem(KEY);
        else window.localStorage.setItem(KEY, mode);
      } catch (e) {}
      apply(mode);
      mark();
    });
    mark();
  });
})();
