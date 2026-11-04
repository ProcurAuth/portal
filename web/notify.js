/* Notifications: the badge, the toasts and the list of the inbox icon.
   The page works without this script. The script asks the portal for the alerts of the account
   every 10 to 15 seconds, and it stops while the tab is hidden. It keeps the last cursor and the
   ids that it showed in sessionStorage, so a page change never shows an alert twice. */
(function () {
  "use strict";
  var root = document.querySelector("[data-notify]");
  if (!root || !window.fetch || !window.URLSearchParams) return;

  var url = root.getAttribute("data-notify");
  var level = root.getAttribute("data-level");
  var KEEP = "notify-state";
  var BASE = 10000;
  var SPREAD = 5000;
  var LIMIT = 120000;
  var SHOWN = 3;
  var LIFE = 8000;
  var ROWS = 8;

  var rendered = Number(root.getAttribute("data-cursor")) || 0;
  var cursor = rendered;
  var seen = [];
  var toasts = [];
  var timer = null;
  var busy = false;
  var stopped = false;
  var failures = 0;

  /* The state of the last page of this tab. A cursor older than a minute is not used: the page
     then shows nothing that happened before it. */
  try {
    var raw = window.sessionStorage.getItem(KEEP);
    var saved = raw ? JSON.parse(raw) : null;
    if (saved && typeof saved.c === "number" && saved.c <= rendered && rendered - saved.c < 60000) {
      cursor = saved.c;
    }
    if (saved && Array.isArray(saved.s)) seen = saved.s.slice(-50);
  } catch (e) {}

  function save() {
    try {
      window.sessionStorage.setItem(KEEP, JSON.stringify({ c: cursor, s: seen.slice(-50) }));
    } catch (e) {}
  }

  function all(selector, from) {
    return (from || document).querySelectorAll(selector);
  }

  function one(selector, from) {
    return (from || document).querySelector(selector);
  }

  /* Badges ------------------------------------------------------------------------------- */

  function setCount(count) {
    var badges = all("[data-notify-badge]");
    for (var i = 0; i < badges.length; i += 1) {
      var number = one("[data-notify-n]", badges[i]);
      if (number) number.textContent = count >= 100 ? "99+" : String(count);
      badges[i].hidden = count < 1 || level === "off";
    }
  }

  function markRead() {
    var rows = all("[data-unread]");
    for (var i = 0; i < rows.length; i += 1) rows[i].removeAttribute("data-unread");
  }

  /* The list of the popover --------------------------------------------------------------- */

  function keepIcon(node, kind) {
    var icons = all("[data-for]", node);
    for (var i = 0; i < icons.length; i += 1) {
      if (icons[i].getAttribute("data-for") !== kind) icons[i].remove();
    }
  }

  function addRow(item) {
    var list = one("[data-notify-list]");
    var template = one("[data-notify-row]");
    if (!list || !template || !template.content) return;
    var node = template.content.firstElementChild.cloneNode(true);
    node.setAttribute("data-level", item.level);
    node.setAttribute("data-unread", "");
    one("[data-row-title]", node).textContent = item.title;
    one("[data-row-group]", node).textContent = item.group;
    one("[data-row-time]", node).textContent = "now";
    var link = one("[data-row-link]", node);
    if (item.href) {
      link.setAttribute("href", item.href);
    } else {
      link.removeAttribute("href");
    }
    link.removeAttribute("data-row-link");
    keepIcon(node, item.level);
    list.insertBefore(node, list.firstChild);
    while (list.children.length > ROWS) list.removeChild(list.lastChild);
    list.hidden = false;
    var empty = one("[data-notify-empty]");
    if (empty) empty.hidden = true;
  }

  /* Toasts ------------------------------------------------------------------------------- */

  function startTimer(toast) {
    if (toast.critical || toast.timer || toast.paused) return;
    toast.began = Date.now();
    toast.timer = window.setTimeout(function () {
      dismiss(toast);
    }, toast.left);
  }

  function pauseTimer(toast) {
    toast.paused = true;
    if (toast.timer) {
      window.clearTimeout(toast.timer);
      toast.timer = null;
      toast.left = Math.max(2000, toast.left - (Date.now() - toast.began));
    }
  }

  function resumeTimer(toast) {
    toast.paused = false;
    if (!toast.node.hidden) startTimer(toast);
  }

  /* The newest toasts show, at most three. The others are in the summary "+2 more". */
  function layout() {
    var more = 0;
    for (var i = 0; i < toasts.length; i += 1) {
      var hide = i >= SHOWN;
      toasts[i].node.hidden = hide;
      if (hide) more += 1;
      else startTimer(toasts[i]);
    }
    var summary = one("[data-notify-more]", root);
    if (summary) {
      summary.hidden = more === 0;
      summary.textContent = "+" + more + " more";
    }
  }

  function dismiss(toast) {
    if (toast.timer) window.clearTimeout(toast.timer);
    var at = toasts.indexOf(toast);
    if (at >= 0) toasts.splice(at, 1);
    if (toast.node.parentNode) toast.node.parentNode.removeChild(toast.node);
    layout();
  }

  function toast(item) {
    var template = one("[data-notify-toast]", root);
    if (!template || !template.content) return;
    var node = template.content.firstElementChild.cloneNode(true);
    node.setAttribute("data-level", item.level);
    one("[data-toast-title]", node).textContent = item.title;
    one("[data-toast-group]", node).textContent = item.group;
    one("[data-toast-level]", node).textContent = item.word;
    var link = one("[data-toast-link]", node);
    if (item.href) link.setAttribute("href", item.href);
    else link.remove();
    keepIcon(node, item.level);
    var critical = item.level === "critical";
    var region = one(critical ? "[data-toast-assertive]" : "[data-toast-polite]", root);
    var entry = { node: node, critical: critical, timer: null, paused: false, left: LIFE, began: 0 };
    node.addEventListener("mouseenter", function () {
      pauseTimer(entry);
    });
    node.addEventListener("mouseleave", function () {
      resumeTimer(entry);
    });
    node.addEventListener("focusin", function () {
      pauseTimer(entry);
    });
    node.addEventListener("focusout", function () {
      resumeTimer(entry);
    });
    one("[data-toast-close]", node).addEventListener("click", function () {
      dismiss(entry);
    });
    region.insertBefore(node, region.firstChild);
    toasts.unshift(entry);
    layout();
  }

  /* The state ---------------------------------------------------------------------------- */

  function setLevel(next, now) {
    var before = level;
    level = next;
    root.setAttribute("data-level", next);
    var buttons = all("[data-notify-level]");
    for (var i = 0; i < buttons.length; i += 1) {
      buttons[i].setAttribute("aria-pressed", String(buttons[i].getAttribute("data-notify-level") === next));
    }
    if (next === "off") {
      stop();
    } else if (before === "off") {
      /* The alerts of the time with no toast are not shown now. */
      if (typeof now === "number") cursor = now;
      schedule(0);
    }
  }

  function state(data) {
    if (data.level && data.level !== level) setLevel(data.level, data.now);
    setCount(data.unread);
    if (data.unread === 0) markRead();
  }

  function apply(data) {
    state(data);
    if (typeof data.cursor === "number" && data.cursor > cursor) cursor = data.cursor;
    var items = data.items || [];
    for (var i = 0; i < items.length; i += 1) {
      if (seen.indexOf(items[i].id) >= 0) continue;
      seen.push(items[i].id);
      if (level === "off") continue;
      addRow(items[i]);
      if (items[i].toast) toast(items[i]);
    }
    if (seen.length > 50) seen = seen.slice(-50);
    save();
  }

  /* The polling -------------------------------------------------------------------------- */

  function stop() {
    if (timer) window.clearTimeout(timer);
    timer = null;
  }

  function schedule(wait) {
    if (timer || stopped || level === "off" || document.hidden) return;
    var delay = wait;
    if (typeof delay !== "number") {
      delay = BASE + Math.random() * SPREAD;
      if (failures) delay = Math.min(delay * Math.pow(2, failures), LIMIT);
    }
    timer = window.setTimeout(poll, delay);
  }

  /* A redirect, a 401, a 403 or a 404 mean that the session or the right is gone: the script stops. */
  function poll() {
    timer = null;
    if (stopped || busy || level === "off" || document.hidden) return;
    busy = true;
    window
      .fetch(url + "?after=" + cursor, {
        credentials: "same-origin",
        redirect: "manual",
        cache: "no-store",
        headers: { Accept: "application/json" }
      })
      .then(function (response) {
        if (response.type === "opaqueredirect" || [401, 403, 404].indexOf(response.status) >= 0) {
          stopped = true;
          return null;
        }
        if (!response.ok) throw new Error("status");
        return response.json();
      })
      .then(function (data) {
        busy = false;
        if (data) {
          failures = 0;
          apply(data);
        }
        schedule();
      })
      .catch(function () {
        busy = false;
        failures += 1;
        schedule();
      });
  }

  document.addEventListener("visibilitychange", function () {
    if (document.hidden) {
      stop();
      return;
    }
    stop();
    schedule(0);
  });

  /* The forms of the popover and of the page Inbox: the script sends them without a new page. */
  document.addEventListener("submit", function (event) {
    var form = event.target;
    if (!form.hasAttribute || !form.hasAttribute("data-notify-form") || !event.submitter) return;
    event.preventDefault();
    var body = new window.URLSearchParams();
    for (var i = 0; i < form.elements.length; i += 1) {
      var field = form.elements[i];
      if (field.name && field.type !== "submit" && field.type !== "button") body.append(field.name, field.value);
    }
    var button = event.submitter;
    if (button.name) body.append(button.name, button.value);
    window
      .fetch(form.action, {
        method: "POST",
        credentials: "same-origin",
        redirect: "manual",
        headers: { Accept: "application/json", "Content-Type": "application/x-www-form-urlencoded" },
        body: body.toString()
      })
      .then(function (response) {
        if (!response.ok) throw new Error("status");
        return response.json();
      })
      .then(state)
      .catch(function () {
        /* The plain form still works. */
        form.removeAttribute("data-notify-form");
        if (button.name) {
          var hidden = document.createElement("input");
          hidden.type = "hidden";
          hidden.name = button.name;
          hidden.value = button.value;
          form.appendChild(hidden);
        }
        form.submit();
      });
  });

  /* Escape closes the popover first, then the newest toast. A click outside closes the popover. */
  document.addEventListener("keydown", function (event) {
    if (event.key !== "Escape") return;
    var open = one("details[open]");
    if (open) {
      if (open.hasAttribute("data-notify-bell")) open.removeAttribute("open");
      return;
    }
    if (toasts.length) dismiss(toasts[0]);
  });

  document.addEventListener("click", function (event) {
    var bell = one("[data-notify-bell][open]");
    if (bell && !bell.contains(event.target)) bell.removeAttribute("open");
  });

  save();
  if (level !== "off") schedule();
})();
