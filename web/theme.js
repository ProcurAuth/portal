/* Theme selector: system (default), dark or light, remembered in this browser only.
   Loaded synchronously in <head> so the theme is applied before the first paint. It is the only script
   of the launcher and the login page. It also shows the button that reveals the password and the Copy button.
   The settings pages add settings.js. */
(function () {
  "use strict";
  var KEY = "ui-color-mode";
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

  // A Copy button (the attribute data-copy, with no value) copies the field that stands in the same box. The page
  // finds the field from the button, so the names of the elements can change.
  document.addEventListener("click", function (event) {
    var button = event.target.closest && event.target.closest("[data-copy]");
    if (!button || !button.parentNode || button.getAttribute("data-copy") !== "") return;
    var field = button.parentNode.querySelector("textarea");
    if (!field) return;
    field.focus();
    field.select();
    var label = button.textContent;
    try {
      window.navigator.clipboard.writeText(field.value).then(function () {
        button.textContent = "Copied";
        window.setTimeout(function () {
          button.textContent = label;
        }, 2000);
      }, function () {});
    } catch (e) {}
  });

  // The button that shows the password (the attribute data-reveal, with no value). The page has it hidden, so a browser
  // without script shows no button. The button names its field with aria-controls. The pressed state is the state of
  // the button: the stylesheet reads it to draw the right icon, and a screen reader says it.
  document.addEventListener("DOMContentLoaded", function () {
    var buttons = document.querySelectorAll("[data-reveal]");
    for (var i = 0; i < buttons.length; i += 1) buttons[i].removeAttribute("hidden");
  });

  document.addEventListener("click", function (event) {
    var button = event.target.closest && event.target.closest("[data-reveal]");
    if (!button) return;
    var field = document.getElementById(button.getAttribute("aria-controls"));
    if (!field) return;
    var show = field.type === "password";
    field.type = show ? "text" : "password";
    button.setAttribute("aria-pressed", String(show));
  });

  // The selector of ClickHouse Dash: the button shows the mode, the list holds the two other modes. Escape or a
  // click outside closes the list. Escape and a choice send the focus back to the button. A Tab out of the list closes it. The arrow keys
  // open the list from the button and move over the options. Home and End go to the first and the last option.
  document.addEventListener("DOMContentLoaded", function () {
    var group = document.querySelector("[data-theme-switch]");
    if (!group) return;
    var trigger = group.querySelector("button");
    var menu = group.querySelector("[role=listbox]");
    if (!trigger || !menu) return;
    var options = menu.querySelectorAll("button[data-theme-value]");

    function mark() {
      trigger.setAttribute("aria-label", "Theme: " + mode);
      for (var i = 0; i < options.length; i += 1) {
        var current = options[i].getAttribute("data-theme-value") === mode;
        options[i].setAttribute("aria-selected", String(current));
        options[i].hidden = current;
      }
    }

    // The button already shows the current mode: the list holds only the other modes.
    function visible() {
      return Array.prototype.filter.call(options, function (option) {
        return !option.hidden;
      });
    }

    function isOpen() {
      return trigger.getAttribute("aria-expanded") === "true";
    }

    function close(focus) {
      menu.hidden = true;
      trigger.setAttribute("aria-expanded", "false");
      if (focus) trigger.focus();
    }

    function open() {
      menu.hidden = false;
      trigger.setAttribute("aria-expanded", "true");
      var shown = visible();
      if (shown[0]) shown[0].focus();
    }

    trigger.addEventListener("click", function () {
      if (isOpen()) close(false);
      else open();
    });

    menu.addEventListener("click", function (event) {
      var button = event.target.closest("button[data-theme-value]");
      if (!button) return;
      mode = button.getAttribute("data-theme-value");
      try {
        if (mode === "system") window.localStorage.removeItem(KEY);
        else window.localStorage.setItem(KEY, mode);
      } catch (e) {}
      apply(mode);
      mark();
      close(true);
    });

    group.addEventListener("keydown", function (event) {
      var key = event.key;
      if (key === "Escape" && isOpen()) {
        event.preventDefault();
        close(true);
        return;
      }
      if (!isOpen()) {
        // The arrow keys open the closed list from the button.
        if (document.activeElement === trigger && (key === "ArrowDown" || key === "ArrowUp")) {
          event.preventDefault();
          open();
          if (key === "ArrowUp") {
            var shown = visible();
            if (shown.length) shown[shown.length - 1].focus();
          }
        }
        return;
      }
      if (key !== "ArrowDown" && key !== "ArrowUp" && key !== "Home" && key !== "End") return;
      event.preventDefault();
      var list = visible();
      if (!list.length) return;
      var at = list.indexOf(document.activeElement);
      var next;
      if (key === "Home") next = 0;
      else if (key === "End") next = list.length - 1;
      else if (key === "ArrowDown") next = (at + 1) % list.length;
      else next = at <= 0 ? list.length - 1 : at - 1;
      list[next].focus();
    });

    // A Tab that leaves the group closes the list.
    group.addEventListener("focusout", function (event) {
      if (isOpen() && event.relatedTarget && !group.contains(event.relatedTarget)) close(false);
    });

    document.addEventListener("click", function (event) {
      if (isOpen() && !group.contains(event.target)) close(false);
    });

    mark();
  });
})();
