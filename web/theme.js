/* Theme selector: system (default), dark or light, remembered in this browser only.
   Loaded synchronously in <head> so the theme is applied before the first paint. */
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

  // The settings popup opens on the #settings anchor and works without script. With script it also
  // behaves as the dialog it announces: focus moves in, the page behind is inert, and closing hands
  // focus back to the gear without leaving an extra history entry or jumping to the top.
  var openedInPage = false;

  function settings() {
    return document.getElementById("settings");
  }

  function settingsOpen() {
    var modal = settings();
    return !!modal && window.location.hash === "#settings" && !modal.classList.contains("is-closed");
  }

  function setBackgroundInert(on) {
    var modal = settings();
    var parts = document.body.children;
    for (var i = 0; i < parts.length; i += 1) {
      var part = parts[i];
      if (part === modal || part.tagName === "SCRIPT" || part.namespaceURI === "http://www.w3.org/2000/svg") continue;
      if (on) part.setAttribute("inert", "");
      else part.removeAttribute("inert");
    }
  }

  function enterSettings() {
    var modal = settings();
    if (!modal) return;
    modal.classList.remove("is-closed");
    setBackgroundInert(true);
    var panel = modal.querySelector(".modal__panel");
    panel.setAttribute("tabindex", "-1");
    // The outcome of the last action is read first when there is one.
    (modal.querySelector(".notice") || panel).focus({ preventScroll: true });
  }

  function leaveSettings() {
    setBackgroundInert(false);
    var gear = document.querySelector('a[href="#settings"]');
    // preventScroll: the gear sits at the top of the page; focusing it must not scroll there.
    if (gear) gear.focus({ preventScroll: true });
  }

  function closeSettings() {
    var modal = settings();
    if (!modal || !settingsOpen()) return;
    if (openedInPage) {
      // The gear pushed an entry: pop it. The hashchange below hands the focus back.
      openedInPage = false;
      window.history.back();
    } else {
      // Loaded on #settings (after an action): drop the anchor and the outcome code in place.
      modal.classList.add("is-closed");
      window.history.replaceState(null, "", window.location.pathname);
      leaveSettings();
    }
  }

  window.addEventListener("hashchange", function () {
    if (window.location.hash === "#settings") enterSettings();
    else leaveSettings();
  });

  document.addEventListener("click", function (event) {
    var target = event.target;
    if (!target || !target.closest) return;
    if (target.closest('a[href="#settings"]')) {
      openedInPage = true;
    } else if (target.closest(".modal__close, .modal__backdrop")) {
      event.preventDefault();
      closeSettings();
    }
  });

  document.addEventListener("keydown", function (event) {
    if (event.key === "Escape" && settingsOpen()) {
      event.preventDefault();
      closeSettings();
    }
  });

  // Loaded on #settings (after an action): the browser applies the anchor around the load, so
  // the popup is entered as soon as the DOM is there and again once it is displayed.
  document.addEventListener("DOMContentLoaded", function () {
    if (settingsOpen()) enterSettings();
  });
  window.addEventListener("load", function () {
    var modal = settings();
    if (settingsOpen() && !modal.contains(document.activeElement)) enterSettings();
  });

  // "Generate" in the add-user form: a random password drawn from the browser's CSPRNG (rejection
  // sampling, so every character is equally likely), shown in clear so it can be handed over. It
  // never leaves the page until the form is submitted.
  var ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
  var PASSWORD_LENGTH = 20;

  function randomPassword() {
    var limit = 256 - (256 % ALPHABET.length);
    var out = "";
    while (out.length < PASSWORD_LENGTH) {
      var bytes = window.crypto.getRandomValues(new Uint8Array(PASSWORD_LENGTH * 2));
      for (var i = 0; i < bytes.length && out.length < PASSWORD_LENGTH; i += 1) {
        if (bytes[i] < limit) out += ALPHABET.charAt(bytes[i] % ALPHABET.length);
      }
    }
    return out;
  }

  document.addEventListener("click", function (event) {
    var button = event.target.closest && event.target.closest("[data-generate-password]");
    if (!button || !window.crypto || !window.crypto.getRandomValues) return;
    var id = button.getAttribute("data-generate-password");
    var input = document.getElementById(id);
    var status = document.getElementById(id + "-status");
    if (!input) return;
    var password = randomPassword();
    input.value = password;
    input.type = "text";
    input.focus();
    input.select();
    if (status) status.textContent = "Generated. Note it down: it is not shown again.";
    try {
      window.navigator.clipboard.writeText(password).then(function () {
        if (status) status.textContent = "Generated and copied. Note it down: it is not shown again.";
      }, function () {});
    } catch (e) {}
  });

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
