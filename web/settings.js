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

/* The editor of grants (the forms of the keys and of the accounts). The page holds the actions and
   the tree of each service as a JSON block, and the ticked grants as hidden fields. This code draws
   the editor from the block: the tree of the resources on the left, the actions of the chosen node
   on the right, and the ticked grants under them, grouped by node. After each change it writes the
   hidden fields again, with the names that the server reads: `grant<n>` and
   `limit<n>.<name>[.<window>]`.

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
    if (!data.services.length) {
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

    var services = data.services;
    var serviceAt = 0;
    var query = new URLSearchParams(window.location.search);
    var wanted = query.get("service");
    if (wanted) {
      for (var w = 0; w < services.length; w += 1) if (services[w].id === wanted) serviceAt = w;
    } else if (ticked.size) {
      var first = ticked.values().next().value;
      for (var i = 0; i < services.length; i += 1) if (services[i].id === first.service) serviceAt = i;
    }
    // The service whose permissions are open under its row. -1: none.
    var expandedAt = services.length === 1 ? 0 : wanted ? serviceAt : -1;
    var current = "";
    var opened = new Set([""]);
    var toggledGroups = new Set();
    var onlyTicked = false;
    var trees = {};
    // What the portal asked for, to ask once: "service|parent|type|query|after".
    var asked = {};
    var next = {};

    function service() {
      return services[serviceAt];
    }

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

    function actionOf(svc, name) {
      for (var j = 0; j < svc.actions.length; j += 1) if (svc.actions[j].name === name) return svc.actions[j];
      return null;
    }

    function serviceById(id) {
      for (var j = 0; j < services.length; j += 1) if (services[j].id === id) return services[j];
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

    // ---- the page of the editor
    var ui = node("div", "ge");
    var side = node("div", "ge__side");
    // The services as a list. Each row has a switch (the grant `access`), what the holder can do in
    // short words, and a button that opens the permissions of the service under the row.
    var strip = node("div", "ge__svcList");
    strip.setAttribute("role", "group");
    strip.setAttribute("aria-label", "Services");
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
    var count = node("span", "pagePart__count");
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
    root.insertBefore(strip, fields);

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

    function ticksUnder(path) {
      var n = 0;
      var id = service().id;
      ticked.forEach(function (item) {
        if (item.service !== id) return;
        if (path === "" || item.path === path || item.path.indexOf(path + "/") === 0) n += 1;
      });
      return n;
    }

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
        li.appendChild(node("span", "ge__nodeName", item.path === "" ? "Whole service" : item.label));
        if (item.path !== "") li.appendChild(node("span", "ge__kind", item.kind));
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

    // The nearest node above `path` where the same action is ticked: it covers the node. `null`: none.
    function coveredBy(serviceId, action, path) {
      var up = path;
      while (up !== "") {
        up = parentOf(up);
        if (ticked.has(keyOf(serviceId, action, up))) return up;
      }
      return null;
    }

    function groupOf(name) {
      var dot = name.indexOf(".");
      return dot > 0 ? name.slice(0, dot) : "";
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
      title.textContent = item.path === "" ? "Whole service" : item.label;
      count.textContent = here + "/" + available;
      tickedButton.textContent = "Ticked " + here;
      rows = rows.filter(function (a) {
        if (onlyTicked && !ticked.has(keyOf(svc.id, a.name, item.path))) return false;
        return !text || a.name.toLowerCase().indexOf(text) >= 0 || a.description.toLowerCase().indexOf(text) >= 0;
      });
      var groups = [];
      var byGroup = {};
      rows.forEach(function (a) {
        var name = groupOf(a.name);
        if (!byGroup[name]) {
          byGroup[name] = [];
          groups.push(name);
        }
        byGroup[name].push(a);
      });
      groups.forEach(function (name) {
        var list = byGroup[name];
        var ticks = list.filter(function (a) {
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
        master.checked = ticks === list.length;
        master.indeterminate = ticks > 0 && ticks < list.length;
        master.setAttribute("aria-label", "Tick every action of " + (name || "the group"));
        header.appendChild(master);
        header.appendChild(node("span", "ge__groupName", name || "service"));
        header.appendChild(node("span", "ge__groupCount", ticks + "/" + list.length));
        actList.appendChild(header);
        if (closed) return;
        list.forEach(function (a) {
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

    function plural(n, one, many) {
      return n + " " + (n === 1 ? one : many);
    }

    // The rows of the services: the switch of `access`, a short sentence, the buttons.
    function renderList() {
      strip.textContent = "";
      services.forEach(function (svc, at) {
        var open = ticked.get(keyOf(svc.id, "access", ""));
        var others = 0;
        ticked.forEach(function (item) {
          if (item.service === svc.id && !(item.action === "access" && item.path === "")) others += 1;
        });
        var expanded = at === expandedAt;
        var row = node("div", "ge__svc" + (open ? " is-open" : "") + (expanded ? " is-expanded" : ""));
        row.dataset.at = String(at);
        var head = node("div", "ge__svcHead");
        var box = node("input");
        box.type = "checkbox";
        box.checked = !!open;
        box.dataset.switch = svc.id;
        box.setAttribute("aria-label", "Can open " + svc.name);
        if (open && open.locked) box.disabled = true;
        head.appendChild(box);
        head.appendChild(node("span", "ge__svcName", svc.name));
        var info = open ? "Can open" : "Cannot open";
        if (others) info += " · " + plural(others, "permission", "permissions");
        if (others && !open) {
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
        var toggle = node("button", "button button--small ge__toggle");
        toggle.type = "button";
        toggle.dataset.toggle = String(at);
        toggle.setAttribute("aria-expanded", expanded ? "true" : "false");
        toggle.appendChild(icon(expanded ? "chevron-down" : "chevron-right"));
        toggle.appendChild(document.createTextNode("Permissions"));
        head.appendChild(toggle);
        row.appendChild(head);
        if (expanded) {
          var body = node("div", "ge__svcBody");
          body.appendChild(ui);
          row.appendChild(body);
        }
        strip.appendChild(row);
      });
      if (ticked.size > MAX_GRANTS) {
        strip.appendChild(
          node("p", "uiBanner uiBanner--warn", "A key holds " + MAX_GRANTS + " permissions at most. Untick some.")
        );
      }
    }

    function render() {
      renderList();
      if (expandedAt >= 0) {
        renderTree();
        renderActions();
      }
      syncFields();
    }

    function setGrant(action, path, on, serviceId) {
      var id = serviceId || service().id;
      var k = keyOf(id, action, path);
      if (ticked.has(k) && ticked.get(k).locked) return;
      if (on) {
        if (!ticked.has(k)) ticked.set(k, { action: action, service: id, path: path, limits: {} });
      } else {
        ticked.delete(k);
      }
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
    function chooseService(at) {
      if (expandedAt === at) {
        expandedAt = -1;
        render();
        return;
      }
      serviceAt = at;
      expandedAt = at;
      current = "";
      treeSearch.value = "";
      actSearch.value = "";
      select("");
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
        setGrant(box.dataset.action, current, box.checked);
      } else if (box.dataset.groupBox !== undefined) {
        var group = box.dataset.groupBox;
        var text = actSearch.value.trim().toLowerCase();
        var svc = service();
        var item = treeOf(svc).byPath[current];
        svc.actions.forEach(function (a) {
          if (groupOf(a.name) !== group || !applies(a, item)) return;
          if (!(owns(svc.id, a.name, current) || ticked.has(keyOf(svc.id, a.name, current)))) return;
          if (text && a.name.toLowerCase().indexOf(text) < 0 && a.description.toLowerCase().indexOf(text) < 0) return;
          setGrant(a.name, current, box.checked);
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
    function pressable(list, selector, same) {
      list.addEventListener("keydown", function (event) {
        if (event.key !== "Enter" && event.key !== " ") return;
        var row = event.target.closest(selector);
        if (!row || event.target !== row) return;
        event.preventDefault();
        var mark = same(row);
        row.click();
        var again = Array.prototype.find.call(list.querySelectorAll(selector), function (el) {
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
    // "Give access" opens the page of the account with the service chosen. The grant `access` on the
    // whole service starts ticked. Nothing is saved until the person clicks Save.
    if (query.get("give") === "1" && wanted && serviceById(wanted)) {
      var key = keyOf(wanted, "access", "");
      if (!ticked.has(key)) ticked.set(key, { action: "access", service: wanted, path: "", limits: {} });
    }
    if (expandedAt >= 0) select("");
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
