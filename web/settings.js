/* Small helpers of the settings pages. Loaded deferred, on each settings page.

   1. Each <time class="when"> shows its time in the time zone of the browser. The server writes UTC in
      the element, for a page without script. The title keeps the UTC text.

   2. A form with the attribute data-gate keeps its submit buttons disabled until its fields are valid.
      A button outside the form names the form in its attribute `form`.

   3. A Copy button copies the value of the field that it names.

   4. "Generate" in the user forms of the Users settings page: a random password drawn from the browser's
      CSPRNG (rejection sampling, so every character is equally likely), shown in clear so it can be
      handed over. It never leaves the page until the form is submitted.

   5. The editor of grants, at the end of the file. */
(function () {
  "use strict";
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

  function two(n) {
    return n < 10 ? "0" + n : String(n);
  }

  // The time of the browser: 2026-10-10 14:03, with the seconds when the element has data-secs.
  function localTimes() {
    var nodes = document.querySelectorAll("time.when[datetime]");
    for (var i = 0; i < nodes.length; i += 1) {
      var node = nodes[i];
      var at = new Date(node.getAttribute("datetime"));
      if (isNaN(at.getTime())) continue;
      var text =
        at.getFullYear() + "-" + two(at.getMonth() + 1) + "-" + two(at.getDate()) + " " +
        two(at.getHours()) + ":" + two(at.getMinutes());
      if (node.hasAttribute("data-secs")) text += ":" + two(at.getSeconds());
      node.setAttribute("title", node.textContent);
      node.textContent = text;
    }
  }
  localTimes();

  // A form with data-gate: the submit buttons stay disabled until the browser finds the fields valid.
  function gateForm(form) {
    var buttons = Array.prototype.slice.call(form.querySelectorAll('[type="submit"]'));
    if (form.id) {
      var outside = document.querySelectorAll('[type="submit"][form="' + escapeId(form.id) + '"]');
      for (var i = 0; i < outside.length; i += 1) buttons.push(outside[i]);
    }
    var update = function () {
      var valid = form.checkValidity();
      for (var j = 0; j < buttons.length; j += 1) buttons[j].disabled = !valid;
    };
    form.addEventListener("input", update);
    form.addEventListener("change", update);
    update();
  }

  var gated = document.querySelectorAll("form[data-gate]");
  for (var g = 0; g < gated.length; g += 1) gateForm(gated[g]);

  function escapeId(id) {
    return window.CSS && window.CSS.escape ? window.CSS.escape(id) : id;
  }

  // A Copy button: it copies the value of the field that it names, then shows the check for two seconds.
  document.addEventListener("click", function (event) {
    var button = event.target.closest && event.target.closest("[data-copy]");
    if (!button) return;
    var field = document.getElementById(button.getAttribute("data-copy"));
    if (!field) return;
    field.focus();
    field.select();
    var done = function () {
      button.classList.add("is-copied");
      window.setTimeout(function () {
        button.classList.remove("is-copied");
      }, 2000);
    };
    try {
      window.navigator.clipboard.writeText(field.value).then(done, function () {});
    } catch (e) {}
  });

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
    // The result shows in the button itself, at the same size. A screen reader hears the status.
    var done = function (text) {
      if (status) status.textContent = text;
      button.classList.add("is-done");
      window.setTimeout(function () {
        button.classList.remove("is-done");
      }, 2500);
    };
    done("Password generated. Note it down: it is not shown again.");
    try {
      window.navigator.clipboard.writeText(password).then(function () {
        if (status) status.textContent = "Password generated and copied. Note it down: it is not shown again.";
      }, function () {});
    } catch (e) {}
  });
})();

/* Popovers (details.pop), confirmations in place (details.ask) and the bulk action (data-bulk). The
   markup works without this code: a details element opens and closes on its own. The code adds the
   rest: one panel open at a time, a click outside and the key Escape close it, and the focus goes
   to the first field of a popover. The bulk action counts the ticked rows and shows its button
   only while a row is ticked. */
(function () {
  "use strict";
  var PANELS = "details.pop, details.ask";

  function closeAll(except) {
    var open = document.querySelectorAll(PANELS + "[open]");
    for (var i = 0; i < open.length; i += 1) {
      if (open[i] !== except) open[i].removeAttribute("open");
    }
  }

  // The event `toggle` does not bubble: listen in the capture phase.
  document.addEventListener(
    "toggle",
    function (event) {
      var panel = event.target;
      if (!panel.matches || !panel.matches(PANELS) || !panel.open) return;
      closeAll(panel);
      // A popover gives the focus to its first field. A confirmation keeps it on its cross, so
      // that a second key press cannot confirm. The confirmation of a name asks for the name.
      var target = panel.classList.contains("pop")
        ? panel.querySelector("input:not([type=hidden]), select, textarea")
        : panel.querySelector(".ask__name");
      if (target) target.focus();
    },
    true
  );

  document.addEventListener("click", function (event) {
    if (event.target.closest && event.target.closest(PANELS)) return;
    closeAll(null);
  });

  document.addEventListener("keydown", function (event) {
    if (event.key !== "Escape") return;
    var open = document.querySelector(PANELS + "[open]");
    if (!open) return;
    open.removeAttribute("open");
    var summary = open.querySelector("summary");
    if (summary) summary.focus();
  });

  // "Done" in the popover of an enrollment code: the code leaves the page. Without script the link
  // loads the page again, and the code is gone too.
  document.addEventListener("click", function (event) {
    var done = event.target.closest && event.target.closest("[data-code-done]");
    if (!done) return;
    event.preventDefault();
    var pop = done.closest("[data-code-pop]");
    if (pop) pop.remove();
  });

  function esc(id) {
    return window.CSS && window.CSS.escape ? window.CSS.escape(id) : id;
  }

  function boxes(id) {
    return document.querySelectorAll('input.bulk__box[form="' + esc(id) + '"]');
  }

  function updateBulk(id) {
    var list = boxes(id);
    var n = 0;
    for (var i = 0; i < list.length; i += 1) if (list[i].checked) n += 1;
    var control = document.querySelector('[data-bulk="' + esc(id) + '"]');
    if (control) {
      control.classList.toggle("has-count", n > 0);
      control.querySelector("[data-bulk-n]").textContent = "(" + n + ")";
      control.querySelector("[data-bulk-q]").textContent =
        control.getAttribute("data-verb") + " " + n + " " +
        (n === 1 ? control.getAttribute("data-one") : control.getAttribute("data-many")) + "?";
      if (!n) control.removeAttribute("open");
    }
    var all = document.querySelector('[data-bulk-all="' + esc(id) + '"]');
    if (all) {
      all.checked = n > 0 && n === list.length;
      all.indeterminate = n > 0 && n < list.length;
    }
  }

  document.addEventListener("change", function (event) {
    var box = event.target;
    if (!box.classList) return;
    if (box.classList.contains("bulk__box")) {
      updateBulk(box.getAttribute("form"));
    } else if (box.hasAttribute("data-bulk-all")) {
      var id = box.getAttribute("data-bulk-all");
      var list = boxes(id);
      for (var i = 0; i < list.length; i += 1) list[i].checked = box.checked;
      updateBulk(id);
    }
  });

  var controls = document.querySelectorAll("[data-bulk]");
  for (var c = 0; c < controls.length; c += 1) updateBulk(controls[c].getAttribute("data-bulk"));
})();

/* The group chips of the pages of an account and the summary of an API key. The editor of grants tells
   them its state after each change (the event `grants-state`). A click on a chip asks the editor to tick or
   to untick the right `inbox.receive` on the group (the event `group-chip`), so the chip and the column
   Receive of the editor always show the same grant. The chips are buttons that the markup disables:
   without script they only show the state. The summary lists what the ticked rights allow, with the calls
   of the API of the inbox. */
(function () {
  "use strict";
  var chips = document.querySelectorAll("[data-group-chip]");
  var summary = document.querySelector("[data-summary]");
  var calls = document.querySelector("[data-calls]");
  if (!chips.length && !summary) return;
  var editor = document.querySelector(".grantEditor");
  var WINDOW_NAMES = { second: "second", minute: "minute", hour: "hour", day: "day" };

  Array.prototype.forEach.call(chips, function (chip) {
    chip.disabled = false;
    chip.addEventListener("click", function () {
      if (!editor) return;
      var on = chip.getAttribute("aria-pressed") !== "true";
      editor.dispatchEvent(new CustomEvent("group-chip", { detail: { id: chip.getAttribute("data-group-chip"), on: on } }));
    });
  });

  function holds(held, action, path) {
    return held.some(function (item) {
      return item.action === action && item.path === path;
    });
  }

  function updateChips(held) {
    Array.prototype.forEach.call(chips, function (chip) {
      var id = chip.getAttribute("data-group-chip");
      var own = held.filter(function (item) {
        return item.action === "inbox.receive" && item.path === "inbox_group:" + id;
      })[0];
      var whole = holds(held, "inbox.receive", "");
      var on = !!own || whole;
      chip.setAttribute("aria-pressed", on ? "true" : "false");
      chip.classList.toggle("is-on", on);
      // A grant of the configuration file, or a grant on every group, is not changed by a chip.
      chip.disabled = (!!own && own.locked) || (whole && !own);
    });
  }

  function labelsOf(feature, held, action) {
    var names = {};
    feature.nodes.forEach(function (n) {
      if (n.kind === "inbox_group") names[n.path] = n.label;
    });
    if (holds(held, action, "")) return ["every group"];
    return held
      .filter(function (item) {
        return item.action === action && names[item.path];
      })
      .map(function (item) {
        return names[item.path];
      });
  }

  function groupIds(feature, held, action) {
    if (holds(held, action, "")) return ["{group}"];
    return held
      .filter(function (item) {
        return item.action === action && item.path.indexOf("inbox_group:") === 0;
      })
      .map(function (item) {
        return item.path.slice("inbox_group:".length);
      });
  }

  function setText(name, text) {
    var cell = summary.querySelector('[data-s="' + name + '"]');
    if (cell) cell.textContent = text;
  }

  function updateSummary(feature, held) {
    var actions = {};
    feature.actions.forEach(function (a) {
      actions[a.name] = true;
    });
    // A row of a right that the document does not have is absent.
    [["send", "alert.send"], ["read", "alert.read"], ["user", "alert.send_user"], ["limit", "alert.send"]].forEach(function (pair) {
      var row = summary.querySelector('[data-s-row="' + pair[0] + '"]');
      if (row && !actions[pair[1]]) row.parentNode.removeChild(row);
    });
    var send = labelsOf(feature, held, "alert.send");
    var read = labelsOf(feature, held, "alert.read");
    var toUser = held.some(function (item) {
      return item.action === "alert.send_user";
    });
    setText("send", send.length ? send.join(", ") : "no group");
    setText("read", read.length ? read.join(", ") : "no group");
    setText("user", toUser ? "yes, to any account" : "no");
    var budget = held.filter(function (item) {
      return item.action === "access" && item.path === "";
    })[0];
    var windows = budget && budget.limits && typeof budget.limits.alerts === "object" ? budget.limits.alerts : {};
    var parts = Object.keys(WINDOW_NAMES)
      .filter(function (w) {
        return windows[w];
      })
      .map(function (w) {
        return windows[w] + " alerts each " + WINDOW_NAMES[w];
      });
    setText("limit", parts.length ? parts.join(", ") : "none");
    if (!calls) return;
    var lines = [];
    var sendIds = groupIds(feature, held, "alert.send");
    if (sendIds.length) {
      lines.push("POST /api/v1/alerts  (group: " + sendIds.join(", ") + ")");
      lines.push("POST /api/v1/notify/{group}");
      lines.push("POST /api/v1/webhooks/alertmanager/{group}");
    }
    var readIds = groupIds(feature, held, "alert.read");
    if (readIds.length) lines.push("GET /api/v1/alerts/{id}  (alerts of: " + readIds.join(", ") + ")");
    if (toUser) lines.push("POST /api/v1/alerts  (user: an account)");
    calls.textContent = "";
    if (!lines.length) {
      calls.textContent = "No call yet. Tick a right.";
      return;
    }
    lines.forEach(function (line, at) {
      if (at) calls.appendChild(document.createElement("br"));
      calls.appendChild(document.createTextNode(line));
    });
  }

  document.addEventListener("grants-state", function (event) {
    var detail = event.detail;
    updateChips(detail.held);
    if (summary) updateSummary(detail.feature, detail.held);
  });
})();

/* The popup "New alert". The switch between a group and an account works without this code. The code adds
   the counter of the title, the search of the accounts and the line that tells where the alert goes. */
(function () {
  "use strict";
  var counters = document.querySelectorAll("[data-title-count]");
  Array.prototype.forEach.call(counters, function (counter) {
    var input = document.getElementById(counter.getAttribute("data-title-count"));
    if (!input) return;
    var update = function () {
      counter.textContent = input.value.length + "/" + input.maxLength;
    };
    input.addEventListener("input", update);
    update();
  });

  function plural(n, one, many) {
    return n + " " + (n === 1 ? one : many);
  }

  var select = document.getElementById("send-group");
  var groupHint = document.querySelector("[data-hint-group]");
  if (select && groupHint) {
    var showGroup = function () {
      var option = select.options[select.selectedIndex];
      var members = option ? Number(option.getAttribute("data-members")) : 0;
      groupHint.textContent = "The alert goes to the " + plural(members, "account", "accounts") + " of the group. It rings on their phones.";
    };
    select.addEventListener("change", showGroup);
    showGroup();
  }

  var find = document.querySelector("[data-ufind]");
  var userHint = document.querySelector("[data-hint-user]");
  var rows = document.querySelectorAll(".rcptForm--user .urow");
  if (find) {
    find.addEventListener("input", function () {
      var text = find.value.trim().toLowerCase();
      Array.prototype.forEach.call(rows, function (row) {
        row.hidden = text !== "" && row.textContent.toLowerCase().indexOf(text) < 0;
      });
    });
  }
  if (userHint) {
    document.addEventListener("change", function (event) {
      var radio = event.target;
      if (!radio || radio.name !== "to" || radio.value.indexOf("user:") !== 0) return;
      var phones = Number(radio.getAttribute("data-phones"));
      userHint.textContent = "The alert goes to the " + plural(phones, "phone", "phones") + " of " + radio.value.slice(5) + ".";
    });
  }
})();

/* The editor of grants (the forms of the keys and of the accounts). The page holds the actions and
   the tree of each service as a JSON block, and the ticked grants as hidden fields. This code draws
   the editor from the block: the tree of the resources on the left, the actions of the chosen node
   on the right, and the ticked grants under them, grouped by node. After each change it writes the
   hidden fields again, with the names that the server reads: `grant<n>` and
   `limit<n>.<name>[.<window>]`.

   A pane is the tree of one service and its actions. The page of an account (data-layout="groups")
   has two groups side by side. The group "Services" has one row for each service, and the pane
   opens under the chosen row. The group "Portal features" has one pane, always open. The forms of
   the keys have one list: the features are one more row.

   A person who gives a grant does not know the resources of a service. When a node can have
   children that the document does not list, the editor asks the portal for them
   (`/auth/options`, which asks the service) and shows them in the tree, page by page. */
(function () {
  "use strict";
  var WINDOWS = [["second", "/s"], ["minute", "/min"], ["hour", "/h"], ["day", "/day"]];
  var MAX_GRANTS = 512;
  // A list of more actions than this starts with its groups closed.
  var LONG_LIST = 60;

  function node(tag, className, text) {
    var item = document.createElement(tag);
    if (className) item.className = className;
    if (text !== undefined) item.textContent = text;
    return item;
  }

  function icon(id, className) {
    var ns = "http://www.w3.org/2000/svg";
    var svg = document.createElementNS(ns, "svg");
    svg.setAttribute("class", "icon " + (className || "icon--sm"));
    svg.setAttribute("aria-hidden", "true");
    var use = document.createElementNS(ns, "use");
    use.setAttribute("href", "#i-" + id);
    svg.appendChild(use);
    return svg;
  }

  function parentOf(path) {
    var at = path.lastIndexOf("/");
    return at < 0 ? "" : path.slice(0, at);
  }

  // An id as the protocol writes it in a path: each byte outside A-Z a-z 0-9 - . _ ~ is %XX.
  function encodeId(id) {
    return encodeURIComponent(id).replace(/[!'()*]/g, function (c) {
      return "%" + c.charCodeAt(0).toString(16).toUpperCase();
    });
  }

  function plural(n, one, many) {
    return n + " " + (n === 1 ? one : many);
  }

  function groupOf(name) {
    var dot = name.indexOf(".");
    return dot > 0 ? name.slice(0, dot) : "";
  }

  function init(root) {
    var script = root.querySelector(".grantEditor__data");
    var fields = root.querySelector(".grantEditor__fields");
    if (!script || !fields) return;
    var data;
    try {
      data = JSON.parse(script.textContent);
    } catch (e) {
      return;
    }
    // The editor has two parts: the services (the workloads of the launcher) and the portal
    // features (keys, inbox). With the layout "groups" the features have a group of their own.
    // Without it they are one more row, after the services.
    var groups = root.dataset.layout === "groups";
    var rowList = data.services.slice();
    var featureSvc = null;
    if (data.features) {
      data.features.feature = true;
      if (groups) featureSvc = data.features;
      else rowList.push(data.features);
    }
    if (!rowList.length && !featureSvc) {
      root.insertBefore(node("div", "uiState uiState--compact", "No service to give access to."), fields);
      return;
    }
    var formId = root.dataset.form || "";

    // The ticked grants. A limit value is the text that the field holds. A locked grant comes from
    // the configuration: the editor shows it and does not change it.
    var ticked = new Map();
    function keyOf(service, action, path) {
      return service + "\u0001" + action + "\u0001" + path;
    }
    function limitsToText(limits) {
      var out = {};
      Object.keys(limits || {}).forEach(function (name) {
        var value = limits[name];
        if (value.c !== undefined) out[name] = String(value.c);
        else {
          out[name] = {};
          Object.keys(value.b || {}).forEach(function (w) {
            out[name][w] = String(value.b[w]);
          });
        }
      });
      return out;
    }
    data.ticked.forEach(function (g) {
      ticked.set(keyOf(g[1], g[0], g[2]), { action: g[0], service: g[1], path: g[2], limits: limitsToText(g[3]) });
    });
    (data.locked || []).forEach(function (g) {
      ticked.set(keyOf(g[1], g[0], g[2]), { action: g[0], service: g[1], path: g[2], limits: limitsToText(g[3]), locked: true });
    });

    // Save stays grey until a grant changes (CTRL-17). The first state is the baseline.
    function signature() {
      return JSON.stringify(
        sortedTicked()
          .filter(function (i) {
            return !i.locked;
          })
          .map(function (i) {
            return [i.service, i.action, i.path, i.limits];
          })
      );
    }
    var baseline = signature();
    var saveButtons = document.querySelectorAll("[data-save]");
    function updateSave() {
      var dirty = signature() !== baseline;
      for (var s = 0; s < saveButtons.length; s += 1) saveButtons[s].disabled = !dirty;
    }

    var serviceAt = 0;
    var query = new URLSearchParams(window.location.search);
    var wanted = query.get("service");
    if (wanted) {
      for (var w = 0; w < rowList.length; w += 1) if (rowList[w].id === wanted) serviceAt = w;
    } else if (ticked.size) {
      var first = ticked.values().next().value;
      for (var i = 0; i < rowList.length; i += 1) if (rowList[i].id === first.service) serviceAt = i;
    }
    // The service whose permissions are open under its row. -1: none.
    var expandedAt = rowList.length === 1 ? 0 : wanted ? serviceAt : -1;

    function owns(serviceId, action, path) {
      if (data.owner === null) return true;
      return data.owner.some(function (g) {
        return g[0] === action && g[1] === serviceId && (g[2] === "" || path === g[2] || path.indexOf(g[2] + "/") === 0);
      });
    }

    function applies(action, item) {
      if (item.more.indexOf(action.name) >= 0) return true;
      if (item.path === "") return action.whole;
      return action.types.indexOf(item.kind) >= 0 && (item.only.length === 0 || item.only.indexOf(action.name) >= 0);
    }

    function actionOf(svc, name) {
      for (var j = 0; j < svc.actions.length; j += 1) if (svc.actions[j].name === name) return svc.actions[j];
      return null;
    }

    function serviceById(id) {
      for (var j = 0; j < rowList.length; j += 1) if (rowList[j].id === id) return rowList[j];
      return null;
    }

    // The types of the children of a node. The root has the children of the type `service`.
    function childTypes(svc, item) {
      var kind = item.path === "" ? "service" : item.kind;
      return (svc.types || []).filter(function (t) {
        return t.parent === kind;
      });
    }

    function canAsk(svc, item) {
      return !!svc.fetch && !item.leaf && childTypes(svc, item).length > 0;
    }

    // ---- the grants as lines
    function sortedTicked() {
      var list = Array.from(ticked.values());
      list.sort(function (a, b) {
        var order = [a.service, a.path, a.action];
        var other = [b.service, b.path, b.action];
        for (var j = 0; j < order.length; j += 1) {
          if (order[j] < other[j]) return -1;
          if (order[j] > other[j]) return 1;
        }
        return 0;
      });
      return list;
    }

    function textOf(item) {
      return item.action + " on service:" + item.service + (item.path ? "/" + item.path : "");
    }

    function syncFields() {
      fields.textContent = "";
      sortedTicked().forEach(function (item, n) {
        if (item.locked) return;
        function add(name, value) {
          var input = document.createElement("input");
          input.type = "hidden";
          input.name = name;
          input.value = value;
          if (formId) input.setAttribute("form", formId);
          fields.appendChild(input);
        }
        add("grant" + n, textOf(item));
        Object.keys(item.limits).forEach(function (name) {
          var value = item.limits[name];
          if (typeof value === "string") {
            if (value !== "") add("limit" + n + "." + name, value);
          } else {
            Object.keys(value).forEach(function (w) {
              if (value[w] !== "") add("limit" + n + "." + name + "." + w, value[w]);
            });
          }
        });
      });
      updateSave();
    }

    function ownerValue(item, name) {
      if (data.owner === null) return "";
      var found = "";
      data.owner.forEach(function (g) {
        if (found || g[0] !== item.action || g[1] !== item.service) return;
        if (!(g[2] === "" || item.path === g[2] || item.path.indexOf(g[2] + "/") === 0)) return;
        var value = g[3] && g[3][name];
        if (!value) return;
        if (value.c !== undefined) found = String(value.c);
        else
          found = Object.keys(value.b || {})
            .map(function (w) {
              return value.b[w] + " each " + w;
            })
            .join(", ");
      });
      return found;
    }

    // The nearest node above `path` where the same action is ticked: it covers the node. `null`: none.
    function coveredBy(serviceId, action, path) {
      var up = path;
      while (up !== "") {
        up = parentOf(up);
        if (ticked.has(keyOf(serviceId, action, up))) return up;
      }
      return null;
    }

    function setGrant(action, path, on, serviceId) {
      var k = keyOf(serviceId, action, path);
      if (ticked.has(k) && ticked.get(k).locked) return;
      if (on) {
        if (!ticked.has(k)) ticked.set(k, { action: action, service: serviceId, path: path, limits: {} });
      } else {
        ticked.delete(k);
      }
    }

    // ---- the limit fields of a ticked action, on the line of the action
    function limitFields(cell, item, svc) {
      var action = actionOf(svc, item.action);
      if (!action || !action.limits.length) return;
      action.limits.forEach(function (decl) {
        var owner = ownerValue(item, decl.name);
        var holder = node("span", "ge__limit");
        var label = node("span", "muted", decl.label);
        label.title = decl.title;
        holder.appendChild(label);
        function field(set, get, suffix, placeholder) {
          var input = node("input", "uiInput");
          input.type = "text";
          input.inputMode = "numeric";
          input.value = get();
          if (placeholder) input.placeholder = placeholder;
          input.setAttribute("aria-label", decl.label + (suffix ? " " + suffix : ""));
          var hint = decl.title;
          if (decl.range) hint += " From " + decl.range[0] + " to " + decl.range[1] + ".";
          hint += " Empty: no value, the limits of the service apply.";
          if (owner) hint += " Your value: " + owner + ".";
          input.title = hint;
          input.addEventListener("input", function () {
            set(input.value.trim());
            syncFields();
          });
          holder.appendChild(input);
          if (suffix) holder.appendChild(node("span", "muted", suffix));
        }
        if (decl.budget) {
          WINDOWS.forEach(function (w) {
            field(
              function (value) {
                if (typeof item.limits[decl.name] !== "object") item.limits[decl.name] = {};
                item.limits[decl.name][w[0]] = value;
              },
              function () {
                var held = item.limits[decl.name];
                return held && typeof held === "object" && held[w[0]] ? held[w[0]] : "";
              },
              w[1],
              ""
            );
          });
        } else {
          // The field shows the largest value that the document allows: it is a hint, not a value.
          field(
            function (value) {
              item.limits[decl.name] = value;
            },
            function () {
              return typeof item.limits[decl.name] === "string" ? item.limits[decl.name] : "";
            },
            "",
            decl.range ? String(decl.range[1]) : ""
          );
        }
        cell.appendChild(holder);
      });
    }

    // ---- a pane: the tree of one service on the left and its actions on the right. `list` holds
    // the services that the pane can show. `stacked`: the tree sits above the actions.
    function makePane(list, start, stacked) {
      var at = start;
      var current = "";
      var opened = new Set([""]);
      var toggledGroups = new Set();
      var onlyTicked = false;
      var trees = {};
      // What the portal asked for, to ask once: "service|parent|type|query|after".
      var asked = {};
      var next = {};

      function service() {
        return list[at];
      }

      function treeOf(svc) {
        if (trees[svc.id]) return trees[svc.id];
        var byPath = {};
        svc.nodes.forEach(function (n) {
          byPath[n.path] = n;
        });
        var children = {};
        svc.nodes.forEach(function (n) {
          if (n.path === "") return;
          var parent = parentOf(n.path);
          while (parent !== "" && !byPath[parent]) parent = parentOf(parent);
          (children[parent] = children[parent] || []).push(n);
        });
        trees[svc.id] = { byPath: byPath, children: children };
        return trees[svc.id];
      }

      function ticksUnder(path) {
        var n = 0;
        var id = service().id;
        ticked.forEach(function (item) {
          if (item.service !== id) return;
          if (path === "" || item.path === path || item.path.indexOf(path + "/") === 0) n += 1;
        });
        return n;
      }

      // ---- the page of the pane
      var ui = node("div", "ge" + (stacked ? " ge--stack" : ""));
      var side = node("div", "ge__side");
      var treeSearch = node("input", "uiInput");
      treeSearch.type = "search";
      treeSearch.placeholder = "Find a resource";
      treeSearch.setAttribute("aria-label", "Find a resource");
      treeSearch.autocomplete = "off";
      side.appendChild(treeSearch);
      var treeList = node("ul", "ge__tree");
      treeList.setAttribute("role", "tree");
      side.appendChild(treeList);
      var main = node("div", "ge__main");
      var bar = node("div", "ge__bar");
      var title = node("strong", "ge__title");
      var count = node("span", "count");
      var actSearch = node("input", "uiInput ge__filter");
      actSearch.type = "search";
      actSearch.placeholder = "Filter the actions";
      actSearch.setAttribute("aria-label", "Filter the actions");
      actSearch.autocomplete = "off";
      var seg = node("div", "segmented");
      var allButton = node("button", "segmented__option is-active", "All");
      var tickedButton = node("button", "segmented__option");
      allButton.type = tickedButton.type = "button";
      seg.appendChild(allButton);
      seg.appendChild(tickedButton);
      bar.appendChild(title);
      bar.appendChild(count);
      bar.appendChild(actSearch);
      bar.appendChild(seg);
      var actList = node("div", "ge__list");
      main.appendChild(bar);
      main.appendChild(actList);
      ui.appendChild(side);
      ui.appendChild(main);

      // ---- the children that the service lists (portal 3.1a)
      function addChildren(svc, parent, type, answer) {
        var tree = treeOf(svc);
        answer.items.forEach(function (item) {
          var path = (parent.path ? parent.path + "/" : "") + type + ":" + encodeId(item.id);
          if (tree.byPath[path]) {
            tree.byPath[path].leaf = !item.has_children;
            return;
          }
          svc.nodes.push({
            path: path,
            kind: type,
            label: item.label || item.id,
            only: [],
            more: [],
            leaf: !item.has_children,
          });
        });
        delete trees[svc.id];
      }

      // Ask for one page of the children of `parent` that have the type `type`. `after` is the cursor
      // of the previous page. `text` is a search text. The answer adds nodes to the tree.
      function ask(svc, parent, type, after, text) {
        var key = [svc.id, parent.path, type, text, after].join("\u0001");
        if (asked[key]) return;
        asked[key] = true;
        var url =
          "/auth/options?service=" + encodeURIComponent(svc.id) + "&type=" + encodeURIComponent(type) +
          (parent.path ? "&parent=" + encodeURIComponent(parent.path) : "") +
          (text ? "&q=" + encodeURIComponent(text) : "") +
          (after ? "&after=" + encodeURIComponent(after) : "");
        parent.pending = (parent.pending || 0) + 1;
        window
          .fetch(url, { credentials: "same-origin", headers: { Accept: "application/json" } })
          .then(function (response) {
            return response.ok ? response.json() : { items: [], next: null, unavailable: true };
          })
          .catch(function () {
            return { items: [], next: null, unavailable: true };
          })
          .then(function (answer) {
            parent.pending -= 1;
            if (answer.unavailable) parent.unavailable = true;
            addChildren(svc, parent, type, answer);
            next[[svc.id, parent.path, type, text].join("\u0001")] = answer.next || null;
            if (!text) parent.loaded = (parent.loaded || 0) + 1;
            if (svc === service()) {
              renderTree();
              renderActions();
            }
          });
      }

      function loadChildren(svc, item) {
        if (!canAsk(svc, item)) return;
        childTypes(svc, item).forEach(function (t) {
          ask(svc, item, t.name, "", "");
        });
      }

      // ---- the tree
      function renderTree() {
        var svc = service();
        var tree = treeOf(svc);
        // A short tree needs no search (CTRL-18).
        treeSearch.hidden = stacked && svc.nodes.length <= 10;
        var text = treeSearch.value.trim().toLowerCase();
        var shown = null;
        if (text) {
          shown = new Set([""]);
          svc.nodes.forEach(function (n) {
            if (n.path.toLowerCase().indexOf(text) >= 0 || n.label.toLowerCase().indexOf(text) >= 0) {
              var path = n.path;
              while (path !== "") {
                shown.add(path);
                path = parentOf(path);
              }
            }
          });
        }
        // The list is drawn again at each choice and at each answer of the service. A row that has the
        // focus gets it back.
        var focused = treeList.contains(document.activeElement) ? document.activeElement : null;
        var focusPath = focused && focused.classList.contains("ge__node") ? focused.dataset.path : null;
        treeList.textContent = "";
        function row(item, depth) {
          var kids = tree.children[item.path] || [];
          var more = canAsk(svc, item);
          var isOpen = shown !== null || opened.has(item.path);
          var li = node("li", "ge__node" + (item.path === current ? " is-selected" : ""));
          li.setAttribute("role", "treeitem");
          li.tabIndex = 0;
          li.dataset.path = item.path;
          li.style.paddingLeft = 6 + depth * 14 + "px";
          var twist = node("span", "ge__twist");
          if (kids.length || more) {
            twist.dataset.twist = "1";
            twist.appendChild(icon(isOpen ? "chevron-down" : "chevron-right"));
          }
          li.appendChild(twist);
          li.appendChild(node("span", "ge__nodeName", item.path === "" ? (svc.feature ? "All features" : "Whole service") : item.label));
          if (item.path !== "") {
            var kindLabel = node("span", "ge__kind", item.kind);
            var typeInfo = (svc.types || []).filter(function (t) {
              return t.name === item.kind;
            })[0];
            if (typeInfo && typeInfo.description) kindLabel.title = typeInfo.description;
            li.appendChild(kindLabel);
          }
          var n = ticksUnder(item.path);
          li.appendChild(node("span", "ge__ticks", n ? String(n) : ""));
          treeList.appendChild(li);
          if (!isOpen) return;
          kids.forEach(function (kid) {
            if (shown === null || shown.has(kid.path)) row(kid, depth + 1);
          });
          if (item.pending) {
            var wait = node("li", "ge__note", "Loading…");
            wait.style.paddingLeft = 6 + (depth + 1) * 14 + "px";
            treeList.appendChild(wait);
          } else if (more) {
            childTypes(svc, item).forEach(function (t) {
              var cursor = next[[svc.id, item.path, t.name, ""].join("\u0001")];
              if (cursor) {
                var li2 = node("li", "ge__more", "Show more " + t.name);
                li2.tabIndex = 0;
                li2.setAttribute("role", "button");
                li2.dataset.path = item.path;
                li2.dataset.type = t.name;
                li2.dataset.after = cursor;
                li2.style.paddingLeft = 6 + (depth + 1) * 14 + "px";
                treeList.appendChild(li2);
              }
            });
            if (item.unavailable && !kids.length) {
              var none = node("li", "ge__note", "The service lists no resource here.");
              none.style.paddingLeft = 6 + (depth + 1) * 14 + "px";
              treeList.appendChild(none);
            }
          }
        }
        row(tree.byPath[""], 0);
        if (focusPath !== null) {
          var back = Array.prototype.find.call(treeList.querySelectorAll(".ge__node"), function (el) {
            return el.dataset.path === focusPath;
          });
          if (back) back.focus();
        }
      }

      // ---- the actions of the chosen node
      function renderActions() {
        var svc = service();
        var tree = treeOf(svc);
        var item = tree.byPath[current] || tree.byPath[""];
        current = item.path;
        var text = actSearch.value.trim().toLowerCase();
        var scroll = actList.scrollTop;
        actList.textContent = "";
        var rows = svc.actions.filter(function (a) {
          if (!applies(a, item)) return false;
          return owns(svc.id, a.name, item.path) || ticked.has(keyOf(svc.id, a.name, item.path));
        });
        var available = rows.length;
        var here = rows.filter(function (a) {
          return ticked.has(keyOf(svc.id, a.name, item.path));
        }).length;
        title.textContent = item.path === "" ? (svc.feature ? "All features" : "Whole service") : item.label;
        count.textContent = here + "/" + available;
        tickedButton.textContent = "Ticked " + here;
        rows = rows.filter(function (a) {
          if (onlyTicked && !ticked.has(keyOf(svc.id, a.name, item.path))) return false;
          return !text || a.name.toLowerCase().indexOf(text) >= 0 || a.description.toLowerCase().indexOf(text) >= 0;
        });
        var byGroup = {};
        var order = [];
        rows.forEach(function (a) {
          var name = groupOf(a.name);
          if (!byGroup[name]) {
            byGroup[name] = [];
            order.push(name);
          }
          byGroup[name].push(a);
        });
        order.forEach(function (name) {
          var list2 = byGroup[name];
          var ticks = list2.filter(function (a) {
            return ticked.has(keyOf(svc.id, a.name, item.path));
          }).length;
          // A long list starts with its groups closed, but the groups that hold a ticked action.
          var closedByDefault = available > LONG_LIST && ticks === 0;
          var closed = (closedByDefault !== toggledGroups.has(item.path + "|" + name)) && !text;
          var header = node("div", "ge__group");
          header.dataset.group = name;
          header.tabIndex = 0;
          header.setAttribute("role", "button");
          header.setAttribute("aria-expanded", closed ? "false" : "true");
          header.appendChild(icon(closed ? "chevron-right" : "chevron-down"));
          var master = node("input");
          master.type = "checkbox";
          master.dataset.groupBox = name;
          master.checked = ticks === list2.length;
          master.indeterminate = ticks > 0 && ticks < list2.length;
          master.setAttribute("aria-label", "Tick every action of " + (name || "the group"));
          header.appendChild(master);
          header.appendChild(node("span", "ge__groupName", name || "service"));
          header.appendChild(node("span", "ge__groupCount", ticks + "/" + list2.length));
          actList.appendChild(header);
          if (closed) return;
          list2.forEach(function (a) {
            var isTicked = ticked.has(keyOf(svc.id, a.name, item.path));
            var line = node("label", "ge__act" + (isTicked ? " is-on" : ""));
            var box = node("input");
            box.type = "checkbox";
            box.checked = isTicked;
            box.dataset.action = a.name;
            var held = ticked.get(keyOf(svc.id, a.name, item.path));
            var via = isTicked ? null : coveredBy(svc.id, a.name, item.path);
            if (held && held.locked) box.disabled = true;
            if (via !== null) {
              box.checked = true;
              box.disabled = true;
              line.className += " is-on";
            }
            line.appendChild(box);
            line.appendChild(node("code", "", a.name));
            var desc = node("span", "ge__desc", a.description);
            desc.title = a.description;
            line.appendChild(desc);
            if (via !== null) {
              line.appendChild(node("span", "ge__limits", via === "" ? "via the whole service" : "via " + via));
            } else if (held && held.locked) {
              var lock = node("span", "statusIcon");
              lock.title = "This permission comes from the configuration file.";
              lock.appendChild(icon("lock"));
              line.appendChild(lock);
            } else {
              if (isTicked && !owns(svc.id, a.name, item.path)) {
                var warn = node("span", "statusIcon statusIcon--warn");
                warn.title = "Your rights do not cover this permission any more.";
                warn.appendChild(icon("alert-triangle"));
                line.appendChild(warn);
              }
              if (a.limits.length) {
                // A ticked action shows its limit fields on its own line: no table is needed.
                var inline = node("span", isTicked ? "ge__limitsInline" : "ge__limits");
                if (isTicked) limitFields(inline, held, svc);
                else
                  inline.textContent = a.limits
                    .map(function (l) {
                      return l.name;
                    })
                    .join(", ");
                line.appendChild(inline);
              }
            }
            actList.appendChild(line);
          });
        });
        if (!rows.length) {
          var empty = node("div", "uiState uiState--compact");
          empty.appendChild(node("p", "uiState__body", "No action matches."));
          actList.appendChild(empty);
        }
        actList.scrollTop = scroll;
      }

      function select(path) {
        current = path;
        toggledGroups.clear();
        var item = treeOf(service()).byPath[path];
        if (item && canAsk(service(), item)) {
          opened.add(path);
          loadChildren(service(), item);
        }
      }

      // The pane shows the service number `n` of its list, from its root.
      function show(n) {
        at = n;
        current = "";
        treeSearch.value = "";
        actSearch.value = "";
        select("");
      }

      // ---- events
      treeList.addEventListener("click", function (event) {
        var more = event.target.closest(".ge__more");
        if (more) {
          var svc = service();
          var parent = treeOf(svc).byPath[more.dataset.path];
          var key = [svc.id, parent.path, more.dataset.type, ""].join("\u0001");
          ask(svc, parent, more.dataset.type, more.dataset.after, "");
          delete next[key];
          renderTree();
          return;
        }
        var li = event.target.closest(".ge__node");
        if (!li) return;
        var path = li.dataset.path;
        if (event.target.closest("[data-twist]")) {
          if (opened.has(path)) opened.delete(path);
          else {
            opened.add(path);
            var item = treeOf(service()).byPath[path];
            if (item) loadChildren(service(), item);
          }
          current = path;
          toggledGroups.clear();
        } else {
          select(path);
        }
        renderTree();
        renderActions();
      });
      // A search asks the service too: the node that is chosen lists its children that match.
      var searching = null;
      treeSearch.addEventListener("input", function () {
        renderTree();
        window.clearTimeout(searching);
        searching = window.setTimeout(function () {
          var text = treeSearch.value.trim();
          var svc = service();
          var item = treeOf(svc).byPath[current];
          if (!text || !item || !canAsk(svc, item)) return;
          childTypes(svc, item).forEach(function (t) {
            ask(svc, item, t.name, "", text);
          });
        }, 300);
      });
      actSearch.addEventListener("input", renderActions);
      allButton.addEventListener("click", function () {
        onlyTicked = false;
        allButton.classList.add("is-active");
        tickedButton.classList.remove("is-active");
        renderActions();
      });
      tickedButton.addEventListener("click", function () {
        onlyTicked = true;
        tickedButton.classList.add("is-active");
        allButton.classList.remove("is-active");
        renderActions();
      });
      actList.addEventListener("change", function (event) {
        var box = event.target;
        if (box.dataset.action) {
          setGrant(box.dataset.action, current, box.checked, service().id);
        } else if (box.dataset.groupBox !== undefined) {
          var group = box.dataset.groupBox;
          var text = actSearch.value.trim().toLowerCase();
          var svc = service();
          var item = treeOf(svc).byPath[current];
          svc.actions.forEach(function (a) {
            if (groupOf(a.name) !== group || !applies(a, item)) return;
            if (!(owns(svc.id, a.name, current) || ticked.has(keyOf(svc.id, a.name, current)))) return;
            if (text && a.name.toLowerCase().indexOf(text) < 0 && a.description.toLowerCase().indexOf(text) < 0) return;
            setGrant(a.name, current, box.checked, svc.id);
          });
        } else {
          return;
        }
        render();
      });
      actList.addEventListener("click", function (event) {
        var header = event.target.closest(".ge__group");
        if (!header || event.target.closest("input")) return;
        var key = current + "|" + header.dataset.group;
        if (toggledGroups.has(key)) toggledGroups.delete(key);
        else toggledGroups.add(key);
        renderActions();
      });
      // Keyboard: Enter or Space on a row does what a click does. The list is drawn again after the
      // action, so the focus goes back to the same row.
      function pressable(listEl, selector, same) {
        listEl.addEventListener("keydown", function (event) {
          if (event.key !== "Enter" && event.key !== " ") return;
          var row = event.target.closest(selector);
          if (!row || event.target !== row) return;
          event.preventDefault();
          var mark = same(row);
          row.click();
          var again = Array.prototype.find.call(listEl.querySelectorAll(selector), function (el) {
            return same(el) === mark;
          });
          if (again) again.focus();
        });
      }
      pressable(treeList, ".ge__node, .ge__more", function (el) {
        return (el.dataset.type || "") + "|" + el.dataset.path;
      });
      pressable(actList, ".ge__group", function (el) {
        return el.dataset.group;
      });

      return {
        ui: ui,
        service: service,
        show: show,
        select: select,
        draw: function () {
          renderTree();
          renderActions();
        },
      };
    }

    // ---- the portal features: plain rows and compact matrices. The document of the service
    // `portal` has actions on the service (the keys, the phones), actions on a kind of key
    // (`key_kind`) and actions on a group of the inbox (`inbox_group`). Only what the document
    // holds is drawn: a feature that the file turns off has no row and no column.
    var KEY_ROWS = ["keys.own.read", "keys.own.edit", "keys.own.revoke", "users.read", "keys.all.read", "keys.all.revoke"];
    var DIRECT_ROWS = ["alert.send_user"];
    var PHONE_ROWS = ["phone.read", "phone.revoke", "phone.log"];
    var INBOX_COLUMNS = [
      ["inbox.receive", "Receive", "Receive the alerts of the group on the phones", ""],
      ["alert.send", "Send", "Send alerts to the group", "send"],
      ["alert.read", "Read", "See the alerts of the group and their delivery", ""],
    ];
    var DAY = 86400;

    function makeFeatures(svc) {
      var ui = node("div", "featBox");
      var actions = {};
      svc.actions.forEach(function (a) {
        actions[a.name] = a;
      });
      // The API kind comes first.
      var kinds = svc.nodes
        .filter(function (n) {
          return n.kind === "key_kind";
        })
        .sort(function (a, b) {
          return (a.path === "key_kind:api" ? 0 : 1) - (b.path === "key_kind:api" ? 0 : 1);
        });
      // The critical groups come last.
      var groupNodes = svc.nodes
        .filter(function (n) {
          return n.kind === "inbox_group";
        })
        .sort(function (a, b) {
          return (a.critical ? 1 : 0) - (b.critical ? 1 : 0);
        });

      function holds(name) {
        return ticked.has(keyOf(svc.id, name, ""));
      }
      function needsMet(name) {
        var a = actions[name];
        return !a || (a.needs || []).every(holds);
      }
      function isLocked(action, path) {
        var item = ticked.get(keyOf(svc.id, action, path));
        return !!(item && item.locked);
      }
      // An action that needs this one goes when this one goes (CTRL-38).
      function dropDependents(name) {
        svc.actions.forEach(function (a) {
          if ((a.needs || []).indexOf(name) < 0) return;
          Array.from(ticked.values()).forEach(function (item) {
            if (item.service === svc.id && item.action === a.name && !item.locked) ticked.delete(keyOf(svc.id, a.name, item.path));
          });
          dropDependents(a.name);
        });
      }

      function sub(iconName, label) {
        var head = node("div", "sub");
        head.appendChild(icon(iconName, "icon--sm"));
        head.appendChild(node("span", "", label));
        return head;
      }

      // One cell: a box for one action on one node.
      function cell(action, path, label, tip) {
        var key = keyOf(svc.id, action, path);
        var on = ticked.has(key);
        var owned = owns(svc.id, action, path);
        var lost = on && !owned && data.owner !== null;
        var box = node("input");
        box.type = "checkbox";
        box.checked = on;
        box.dataset.cellAction = action;
        box.dataset.cellPath = path;
        box.setAttribute("aria-label", label);
        box.disabled = isLocked(action, path) || (!on && (!owned || !needsMet(action)));
        var wrap = node("label", "mc" + (lost ? " is-lost" : ""));
        if (tip) wrap.title = tip;
        if (lost) wrap.title = "Without effect: the owner does not hold this permission any more.";
        else if (!on && !owned) wrap.title = "Your account does not hold this permission.";
        else if (!on && !needsMet(action)) wrap.title = "Needs " + actions[action].needs.join(", ") + ".";
        wrap.appendChild(box);
        if (lost) wrap.appendChild(icon("alert-triangle"));
        else if (isLocked(action, path)) wrap.appendChild(icon("lock"));
        return wrap;
      }

      // A row of the service: a box, the name of the action and its description.
      function flatRow(name) {
        var a = actions[name];
        var on = holds(name);
        var owned = owns(svc.id, name, "");
        var locked = !on && !needsMet(name);
        var row = node("label", "frow" + (on ? " is-on" : "") + (locked ? " is-locked" : ""));
        var box = node("input");
        box.type = "checkbox";
        box.checked = on;
        box.dataset.cellAction = name;
        box.dataset.cellPath = "";
        box.disabled = isLocked(name, "") || (!on && (!owned || locked));
        box.setAttribute("aria-label", name);
        row.appendChild(box);
        row.appendChild(node("code", "", name));
        var desc = node("span", "desc", a.description);
        desc.title = locked ? "Needs " + a.needs.join(", ") + ". " + a.description : a.description;
        row.appendChild(desc);
        if (on && !owned && data.owner !== null) {
          var warn = node("span", "statusIcon statusIcon--warn");
          warn.title = "Your rights do not cover this permission any more.";
          warn.appendChild(icon("alert-triangle"));
          row.appendChild(warn);
        } else if (isLocked(name, "")) {
          var lock = node("span", "statusIcon");
          lock.title = "This permission comes from the configuration file.";
          lock.appendChild(icon("lock"));
          row.appendChild(lock);
        }
        return row;
      }

      function flatSection(iconName, label, names) {
        var present = names.filter(function (n) {
          return actions[n];
        });
        if (!present.length) return;
        ui.appendChild(sub(iconName, label));
        present.forEach(function (n) {
          ui.appendChild(flatRow(n));
        });
      }

      // The matrix of the kinds of key: the rows are "Make a key" and "Use the keys".
      function kindMatrix() {
        if (!kinds.length || !(actions["keys.own.create"] || actions["keys.own.use"])) return;
        ui.appendChild(sub("key", "Key kinds"));
        var table = node("div", "matrix matrix--" + kinds.length);
        table.appendChild(node("div", "mh", "Right"));
        kinds.forEach(function (n) {
          var head = node("div", "mh mc");
          head.appendChild(icon(n.path === "key_kind:api" ? "webhook" : "robot", "icon--sm"));
          head.appendChild(node("span", "", n.label.replace(/ keys$/, "")));
          table.appendChild(head);
        });
        [["keys.own.create", "Make a key"], ["keys.own.use", "Use the keys"]].forEach(function (def) {
          if (!actions[def[0]]) return;
          table.appendChild(node("div", "mname", def[1]));
          kinds.forEach(function (n) {
            table.appendChild(cell(def[0], n.path, def[1] + ": " + n.label, actions[def[0]].description));
          });
        });
        ui.appendChild(table);
        // The longest lifetime of a key of each kind: a ceiling on `keys.own.use`.
        if (actions["keys.own.use"] && actions["keys.own.use"].limits.length) {
          var row = node("div", "limitRow");
          row.appendChild(node("span", "label", "Longest key lifetime"));
          kinds.forEach(function (n) {
            var item = ticked.get(keyOf(svc.id, "keys.own.use", n.path));
            var unit = node("span", "u");
            unit.appendChild(node("span", "unit", n.label.replace(/ keys$/, "")));
            var input = node("input", "num-input num-input--s");
            input.type = "text";
            input.inputMode = "numeric";
            input.disabled = !item || !!item.locked;
            input.setAttribute("aria-label", "Longest lifetime of a " + n.label.replace(/ keys$/, "") + " key, in days");
            var seconds = item && typeof item.limits.max_key_lifetime === "string" ? item.limits.max_key_lifetime : "";
            input.value = seconds !== "" && !isNaN(Number(seconds)) ? String(Math.round((Number(seconds) / DAY) * 100) / 100) : seconds;
            input.addEventListener("input", function () {
              var held = ticked.get(keyOf(svc.id, "keys.own.use", n.path));
              if (!held) return;
              var text = input.value.trim();
              held.limits.max_key_lifetime = text === "" || isNaN(Number(text)) ? text : String(Math.round(Number(text) * DAY));
              syncFields();
            });
            unit.appendChild(input);
            unit.appendChild(node("span", "unit", "days"));
            row.appendChild(unit);
          });
          ui.appendChild(row);
        }
      }

      // The matrix of the inbox: a row for each group and a column for each action.
      function inboxMatrix() {
        var columns = INBOX_COLUMNS.filter(function (c) {
          return actions[c[0]];
        });
        if (!groupNodes.length || !columns.length) return;
        var head = sub("inbox", "Inbox");
        var lostCount = 0;
        columns.forEach(function (c) {
          groupNodes.forEach(function (g) {
            var on = ticked.has(keyOf(svc.id, c[0], g.path));
            if (on && !owns(svc.id, c[0], g.path) && data.owner !== null) lostCount += 1;
          });
        });
        if (lostCount) head.appendChild(node("span", "sub__note sub__note--warn", plural(lostCount, "right has", "rights have") + " no effect"));
        var sending = groupNodes.filter(function (g) {
          return ticked.has(keyOf(svc.id, "alert.send", g.path));
        });
        var skipped = sending.filter(function (g) {
          return g.critical;
        });
        var counted = sending.length - skipped.length;
        if (actions["alert.send"]) {
          if (sending.length && !counted) head.appendChild(node("span", "sub__note", "No limit applies: critical groups only"));
          else if (skipped.length)
            head.appendChild(
              node("span", "sub__note", "Not counted: " + skipped.map(function (g) { return g.label; }).join(", ") + " (critical)")
            );
        }
        ui.appendChild(head);
        var table = node("div", "matrix matrix--" + columns.length);
        table.appendChild(node("div", "mh", "Group"));
        columns.forEach(function (c) {
          var th = node("label", "mh mc");
          var avail = groupNodes.filter(function (g) {
            return owns(svc.id, c[0], g.path) || ticked.has(keyOf(svc.id, c[0], g.path));
          });
          var onCount = avail.filter(function (g) {
            return ticked.has(keyOf(svc.id, c[0], g.path));
          }).length;
          var all = node("input");
          all.type = "checkbox";
          all.checked = avail.length > 0 && onCount === avail.length;
          all.indeterminate = onCount > 0 && onCount < avail.length;
          all.disabled = !avail.length || !needsMet(c[0]);
          all.dataset.columnAction = c[0];
          all.setAttribute("aria-label", "Tick " + c[1] + " for every group");
          th.title = c[2];
          th.appendChild(all);
          th.appendChild(node("span", "", c[1]));
          table.appendChild(th);
        });
        groupNodes.forEach(function (g) {
          var name = node("div", "mname");
          name.appendChild(node("span", "", g.label));
          if (g.critical) {
            var crit = node("span", "crit");
            crit.title = "Critical group: its alerts always ring, and no limit counts them.";
            crit.appendChild(icon("bell-ringing"));
            name.appendChild(crit);
          }
          table.appendChild(name);
          columns.forEach(function (c) {
            table.appendChild(cell(c[0], g.path, c[1] + " " + g.label, ""));
          });
        });
        ui.appendChild(table);
        // The budget of the alerts (portal 15.3): one value for the account, on `access`.
        if (actions.access && actions.access.limits.length && actions["alert.send"]) {
          var item = ticked.get(keyOf(svc.id, "access", ""));
          var budget = item && item.limits.alerts && typeof item.limits.alerts === "object" ? item.limits.alerts : {};
          var has = WINDOWS.some(function (w) {
            return budget[w[0]];
          });
          var off = !counted && !has;
          var row = node("div", "limitRow" + (off ? " is-off" : ""));
          row.appendChild(node("span", "label", "Alerts sent"));
          WINDOWS.forEach(function (w) {
            var unit = node("span", "u");
            var input = node("input", "num-input num-input--s");
            input.type = "text";
            input.inputMode = "numeric";
            input.value = budget[w[0]] || "";
            input.disabled = off || !!(item && item.locked);
            input.setAttribute("aria-label", "Alerts sent for each " + w[0]);
            input.addEventListener("input", function () {
              var k = keyOf(svc.id, "access", "");
              var held = ticked.get(k);
              if (!held) {
                held = { action: "access", service: svc.id, path: "", limits: {} };
                ticked.set(k, held);
              }
              if (typeof held.limits.alerts !== "object") held.limits.alerts = {};
              held.limits.alerts[w[0]] = input.value.trim();
              // The grant `access` of the portal has no use but the budget: no value, no grant.
              var any = WINDOWS.some(function (x) {
                return held.limits.alerts[x[0]];
              });
              if (!any && !held.locked) ticked.delete(k);
              syncFields();
            });
            unit.appendChild(input);
            unit.appendChild(node("span", "unit", w[1]));
            row.appendChild(unit);
          });
          ui.appendChild(row);
        }
      }

      function draw() {
        var focus = ui.contains(document.activeElement) ? document.activeElement : null;
        var again = focus && focus.dataset ? [focus.dataset.cellAction, focus.dataset.cellPath, focus.dataset.columnAction] : null;
        ui.textContent = "";
        flatSection("key", "Keys", KEY_ROWS);
        kindMatrix();
        inboxMatrix();
        // The row of the direct alerts exists only when the document of the portal has the action.
        flatSection("inbox", "Direct alerts", DIRECT_ROWS);
        flatSection("device-mobile", "Phones", PHONE_ROWS);
        if (again) {
          var boxes = ui.querySelectorAll("input[type=checkbox]");
          for (var b = 0; b < boxes.length; b += 1) {
            var d = boxes[b].dataset;
            if (d.cellAction === again[0] && d.cellPath === again[1] && d.columnAction === again[2] && !boxes[b].disabled) {
              boxes[b].focus();
              break;
            }
          }
        }
      }

      ui.addEventListener("change", function (event) {
        var box = event.target;
        if (box.dataset.cellAction !== undefined) {
          setGrant(box.dataset.cellAction, box.dataset.cellPath, box.checked, svc.id);
          if (!box.checked && box.dataset.cellPath === "") dropDependents(box.dataset.cellAction);
        } else if (box.dataset.columnAction !== undefined) {
          groupNodes.forEach(function (g) {
            var action = box.dataset.columnAction;
            if (box.checked && owns(svc.id, action, g.path)) setGrant(action, g.path, true, svc.id);
            else if (!box.checked) setGrant(action, g.path, false, svc.id);
          });
        } else {
          return;
        }
        render();
      });

      return { ui: ui, draw: draw };
    }

    // ---- where the document of a service comes from, and how old the copy is
    var SOURCE_PATH = "/.well-known/procurauth/actions";
    function minutes(secs) {
      var m = Math.max(0, Math.round(secs / 60));
      return m < 1 ? "less than a minute" : m + " min";
    }
    function sourceLine(svc) {
      var src = svc.source || { state: "absent" };
      var line = node("div", "srcLine");
      var text = node("span");
      if (src.state === "fresh") {
        line.className += " srcLine--ok";
        line.appendChild(icon("circle-check"));
        text.appendChild(document.createTextNode("Published by the service at "));
        text.appendChild(node("code", "", SOURCE_PATH));
        text.appendChild(document.createTextNode(" \u00b7 read " + minutes(src.secs) + " ago"));
      } else if (src.state === "stale") {
        line.className += " srcLine--stale";
        line.appendChild(icon("clock"));
        text.textContent = "Last good copy from " + minutes(src.secs) + " ago. The last read failed.";
      } else {
        line.className += " srcLine--none";
        line.appendChild(icon("ban"));
        text.textContent = "Not published: access only. The service gives no document at " + SOURCE_PATH + ".";
      }
      line.appendChild(text);
      return line;
    }

    // ---- the rows of the services: the switch of `access`, a short sentence, the button
    var services = rowList.length ? makePane(rowList, serviceAt, false) : null;
    var features = featureSvc ? makeFeatures(featureSvc) : null;
    var strip = node("div", groups ? "ge__groups" : "ge__svcList");
    strip.setAttribute("role", "group");
    strip.setAttribute("aria-label", "Permissions");
    var rowsEl = groups ? node("div") : strip;
    var servicesCount = node("span", "count");
    var featuresCount = node("span", "count");
    if (groups) {
      if (rowList.length) {
        var svcGroup = node("section", "ge__grp");
        var head = node("div", "ge__grpHead");
        head.appendChild(node("h2", "title", "Services"));
        head.appendChild(servicesCount);
        svcGroup.appendChild(head);
        rowsEl = node("div", "ge__svcList");
        svcGroup.appendChild(rowsEl);
        strip.appendChild(svcGroup);
      }
      if (featureSvc) {
        var featGroup = node("section", "ge__grp");
        var featHead = node("div", "ge__grpHead");
        featHead.appendChild(node("h2", "title", "Portal features"));
        featHead.appendChild(featuresCount);
        featGroup.appendChild(featHead);
        var portalLine = node("div", "srcLine srcLine--ok");
        portalLine.title = "The portal builds this document in its own code. It does not serve it over HTTP.";
        portalLine.appendChild(icon("circle-check"));
        portalLine.appendChild(node("span", "", "Published by the portal. Built in, always current."));
        featGroup.appendChild(portalLine);
        var featBody = node("div", "ge__featBody");
        featBody.appendChild(features.ui);
        featGroup.appendChild(featBody);
        strip.appendChild(featGroup);
      }
    }
    root.insertBefore(strip, fields);

    function renderList() {
      rowsEl.textContent = "";
      var lastGroup = "";
      var openCount = 0;
      rowList.forEach(function (svc, at) {
        if (!groups) {
          var group = svc.feature ? "Portal features" : "Services";
          if (group !== lastGroup) {
            rowsEl.appendChild(node("h3", "ge__groupTitle", group));
            lastGroup = group;
          }
        }
        var open = ticked.get(keyOf(svc.id, "access", ""));
        if (open) openCount += 1;
        var others = 0;
        ticked.forEach(function (item) {
          if (item.service === svc.id && !(item.action === "access" && item.path === "")) others += 1;
        });
        var expanded = at === expandedAt;
        var row = node("div", "ge__svc" + (open ? " is-open" : "") + (expanded ? " is-expanded" : ""));
        row.dataset.at = String(at);
        var head = node("div", "ge__svcHead");
        if (!svc.feature) {
          // The switch of a service is its grant `access`. A portal feature has no switch: no path
          // belongs to it, and its permissions work as they are.
          var box = node("input");
          box.type = "checkbox";
          box.checked = !!open;
          box.dataset.switch = svc.id;
          box.setAttribute("aria-label", "Can open " + svc.name);
          if (open && open.locked) box.disabled = true;
          head.appendChild(box);
        }
        head.appendChild(node("span", "ge__svcName", svc.name));
        var origin = (svc.source || {}).state;
        if (!svc.feature && origin === "stale") head.appendChild(node("span", "svc__flag svc__flag--stale", "stale copy"));
        else if (!svc.feature && origin !== "fresh") head.appendChild(node("span", "svc__flag", "access only"));
        var info = open ? (groups ? "Opens" : "Can open") : "Cannot open";
        if (svc.feature) {
          info = others ? plural(others, "permission", "permissions") : "No permission";
        } else if (others) info += " · " + plural(others, "permission", "permissions");
        if (others && !open && !svc.feature) {
          info += ": turn the switch on, or the permissions do nothing";
          row.classList.add("is-orphan");
        }
        head.appendChild(node("span", "ge__svcInfo", info));
        if (data.owner !== null) {
          var same = node("button", "button button--small ge__same", "Same as mine");
          same.type = "button";
          same.dataset.same = svc.id;
          same.title = "Give this key all the permissions that you hold on " + svc.name;
          head.appendChild(same);
        }
        var toggle = node("button", groups ? "ib ge__toggle" : "button button--small ge__toggle");
        toggle.type = "button";
        toggle.dataset.toggle = String(at);
        toggle.setAttribute("aria-expanded", expanded ? "true" : "false");
        toggle.appendChild(icon(expanded ? "chevron-down" : "chevron-right"));
        if (groups) {
          toggle.setAttribute("aria-label", "Permissions of " + svc.name);
          toggle.title = "Permissions of " + svc.name;
        } else {
          toggle.appendChild(document.createTextNode("Permissions"));
        }
        head.appendChild(toggle);
        row.appendChild(head);
        if (expanded) {
          var body = node("div", "ge__svcBody");
          body.appendChild(sourceLine(svc));
          body.appendChild(services.ui);
          row.appendChild(body);
        }
        rowsEl.appendChild(row);
      });
      if (ticked.size > MAX_GRANTS) {
        rowsEl.appendChild(
          node("p", "uiBanner uiBanner--warn", "A key holds " + MAX_GRANTS + " permissions at most. Untick some.")
        );
      }
      servicesCount.textContent = "Opens " + openCount + " of " + rowList.length;
      if (featureSvc) {
        var held = 0;
        ticked.forEach(function (item) {
          if (item.service === featureSvc.id && item.action !== "access") held += 1;
        });
        var total = 0;
        featureSvc.actions.forEach(function (a) {
          if (a.name === "access") return;
          total += featureSvc.nodes.filter(function (n) {
            return applies(a, n);
          }).length;
        });
        featuresCount.textContent = held + " of " + total + " rights";
      }
    }

    function render() {
      renderList();
      if (services && expandedAt >= 0) services.draw();
      if (features) features.draw();
      syncFields();
      announce();
    }

    // ---- the group chips of the page and the summary of the key use the grants of the editor. The
    // editor tells them its state after each change, and it listens to a click on a chip.
    function receivePath(id) {
      return "inbox_group:" + id;
    }
    function announce() {
      if (!featureSvc) return;
      var held = Array.from(ticked.values()).filter(function (item) {
        return item.service === featureSvc.id;
      });
      root.dispatchEvent(
        new CustomEvent("grants-state", {
          bubbles: true,
          detail: {
            feature: featureSvc,
            held: held.map(function (item) {
              return { action: item.action, path: item.path, limits: item.limits, locked: !!item.locked };
            }),
          },
        })
      );
    }
    root.addEventListener("group-chip", function (event) {
      if (!featureSvc) return;
      var id = event.detail.id;
      var wanted = event.detail.on;
      var path = receivePath(id);
      var item = ticked.get(keyOf(featureSvc.id, "inbox.receive", path));
      if (item && item.locked) return;
      setGrant("inbox.receive", path, wanted, featureSvc.id);
      render();
    });

    function chooseService(at) {
      if (expandedAt === at) {
        expandedAt = -1;
        render();
        return;
      }
      expandedAt = at;
      services.show(at);
      render();
    }
    strip.addEventListener("change", function (event) {
      var box = event.target;
      if (box.dataset.switch === undefined) return;
      setGrant("access", "", box.checked, box.dataset.switch);
      render();
    });
    strip.addEventListener("click", function (event) {
      var same = event.target.closest("[data-same]");
      if (same) {
        var id = same.dataset.same;
        (data.owner || []).forEach(function (g) {
          if (g[1] === id) setGrant(g[0], g[2], true, id);
        });
        render();
        return;
      }
      var toggle = event.target.closest("[data-toggle]");
      if (toggle) {
        chooseService(Number(toggle.dataset.toggle));
        return;
      }
      if (event.target.closest("input") || event.target.closest(".ge__svcBody")) return;
      var head = event.target.closest(".ge__svcHead");
      if (head) chooseService(Number(head.parentNode.dataset.at));
    });
    // "Give access" opens the page of the account with the service chosen. The grant `access` on the
    // whole service starts ticked. Nothing is saved until the person clicks Save.
    if (query.get("give") === "1" && wanted && serviceById(wanted)) {
      var key = keyOf(wanted, "access", "");
      if (!ticked.has(key)) ticked.set(key, { action: "access", service: wanted, path: "", limits: {} });
    }
    if (services && expandedAt >= 0) services.select("");
    render();
  }

  var editors = document.querySelectorAll(".grantEditor");
  for (var at = 0; at < editors.length; at += 1) init(editors[at]);
})();

/* The expiry date of a new key. Without script the page has a text field with a date in UTC. With
   script the field is hidden and the person picks a preset (7, 30 or 90 days, or never) or a date
   and a time in the time zone of the browser. The hidden field keeps the name `expires_at` and
   gets the date in UTC, the form that the server reads. A longest lifetime (`data-max`, in
   seconds) disables the presets that go beyond it and the choice "Never". */
(function () {
  "use strict";
  var DAY = 86400;
  var PRESETS = [[7, "7 days"], [30, "30 days"], [90, "90 days"], [0, "Never"]];

  function pad(n) {
    return n < 10 ? "0" + n : String(n);
  }

  function localValue(date) {
    return (
      date.getFullYear() + "-" + pad(date.getMonth() + 1) + "-" + pad(date.getDate()) + "T" +
      pad(date.getHours()) + ":" + pad(date.getMinutes())
    );
  }

  function init(root) {
    var field = root.querySelector('input[name="expires_at"]');
    var hint = root.querySelector(".expiry__hint");
    if (!field) return;
    var max = root.hasAttribute("data-max") ? Number(root.getAttribute("data-max")) : null;
    var start = new Date(field.value);
    field.hidden = true;
    if (hint) hint.textContent = max === null ? "" : "The longest lifetime is " + Math.floor(max / DAY) + " days.";

    var row = document.createElement("div");
    row.className = "expiry__row";
    var group = document.createElement("div");
    group.className = "segmented";
    var picker = document.createElement("input");
    picker.type = "datetime-local";
    picker.className = "uiInput expiry__date";
    picker.setAttribute("aria-label", "Expiry date and time");
    var buttons = [];
    PRESETS.forEach(function (preset) {
      var button = document.createElement("button");
      button.type = "button";
      button.className = "segmented__option";
      button.textContent = preset[1];
      var seconds = preset[0] * DAY;
      button.dataset.days = String(preset[0]);
      if (max !== null && (preset[0] === 0 || seconds > max)) button.disabled = true;
      buttons.push(button);
      group.appendChild(button);
    });
    row.appendChild(group);
    row.appendChild(picker);
    field.parentNode.insertBefore(row, field.nextSibling);

    function set(date, days) {
      if (date) {
        picker.value = localValue(date);
        field.value = date.toISOString().slice(0, 19) + "Z";
      } else {
        picker.value = "";
        field.value = "";
      }
      buttons.forEach(function (button) {
        button.classList.toggle("is-active", days !== undefined && button.dataset.days === String(days));
      });
      field.dispatchEvent(new Event("change", { bubbles: true }));
    }

    var now = new Date();
    picker.min = localValue(now);
    if (max !== null) picker.max = localValue(new Date(now.getTime() + max * 1000));
    // The first value: the preset that the server chose, or its date.
    if (field.value && !isNaN(start.getTime())) {
      var days = Math.round((start.getTime() - now.getTime()) / (DAY * 1000));
      var preset = PRESETS.filter(function (p) {
        return p[0] === days && p[0] > 0;
      })[0];
      set(start, preset ? preset[0] : undefined);
    } else if (max === null) {
      set(null, 0);
    }
    group.addEventListener("click", function (event) {
      var button = event.target.closest("button");
      if (!button || button.disabled) return;
      var chosen = Number(button.dataset.days);
      set(chosen ? new Date(Date.now() + chosen * DAY * 1000) : null, chosen);
    });
    picker.addEventListener("change", function () {
      var date = new Date(picker.value);
      if (isNaN(date.getTime())) return;
      set(date);
      picker.value = localValue(date);
    });
  }

  var roots = document.querySelectorAll("[data-expiry]");
  for (var at = 0; at < roots.length; at += 1) init(roots[at]);
})();
