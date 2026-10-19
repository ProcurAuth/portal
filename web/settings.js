/* Two small helpers of the Users settings pages. Loaded deferred, on those pages only.

   1. The page of an account shows one view at a time: Access, Activity or Account.

   2. The permissions are a list of services and one panel. A click on a line of the list shows the
      panel of that service at once, without a new page. The boxes follow the tree of a service. A box with data-action and
      data-path stands for one grant: the path is empty for the whole service, else `host:prod` and
      so on. Ticking a box ticks the same action on each node below it. Unticking a box unticks the
      same action on each node below it and on each node above it. When the last node below a box
      is ticked, the box above it ticks too. Thus a click on the action of the whole service ticks
      it for every host, a host that is unticked after that leaves the whole service while the
      other hosts keep their own boxes, and every host ticked one by one ticks the whole service.
      A box that is disabled does not change. Without this script, the server drops the grants
      that a broader grant covers when it saves.
      The box `access` of a service locks the other boxes of the service: without it they are
      unticked and disabled. A box without a name belongs to the configuration and stays as it is.
      The count of a service and of a host (ticked/total) follows the boxes.

   3. "Generate" in the user forms of the Users settings page: a random password drawn from the browser's
   CSPRNG (rejection sampling, so every character is equally likely), shown in clear so it can be
   handed over. It never leaves the page until the form is submitted. Loaded deferred, on that page only. */
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

  function below(path, other) {
    return other.length > path.length && (path === "" || other.indexOf(path + "/") === 0);
  }

  function escapeId(id) {
    return window.CSS && window.CSS.escape ? window.CSS.escape(id) : id;
  }

  // Every box of one service: the box `access` in its line of the list, and the boxes of its panel.
  function boxesOf(id) {
    return document.querySelectorAll('[data-svc="' + escapeId(id) + '"] input[type="checkbox"]');
  }

  function setCount(node, boxes) {
    var ticked = 0;
    for (var i = 0; i < boxes.length; i += 1) if (boxes[i].checked) ticked += 1;
    if (node) node.textContent = ticked + "/" + boxes.length;
  }

  // The counts (ticked/total): `data-of=""` is the whole service, `data-of="host:prod"` is one node.
  function recount(id) {
    var sel = '[data-svc="' + escapeId(id) + '"]';
    var whole = document.querySelectorAll(sel + ' .perm__n[data-of=""]');
    var all = boxesOf(id);
    for (var i = 0; i < whole.length; i += 1) setCount(whole[i], all);
    var cols = document.querySelectorAll(sel + " [data-node]");
    for (var j = 0; j < cols.length; j += 1) {
      var key = cols[j].getAttribute("data-node");
      var spans = cols[j].querySelectorAll('.perm__n[data-of="' + escapeId(key) + '"]');
      var boxes = cols[j].querySelectorAll('input[type="checkbox"]');
      for (var k = 0; k < spans.length; k += 1) setCount(spans[k], boxes);
    }
  }

  document.addEventListener("change", function (event) {
    var box = event.target;
    if (!box || !box.hasAttribute || !box.hasAttribute("data-action")) return;
    var owner = box.closest("[data-svc]");
    if (!owner) return;
    var id = owner.getAttribute("data-svc");
    var action = box.getAttribute("data-action");
    var path = box.getAttribute("data-path") || "";
    var boxes = boxesOf(id);
    if (action === "access" && path === "") {
      // The box `access`: it locks and unlocks the other boxes of the service.
      for (var r = 0; r < boxes.length; r += 1) {
        if (boxes[r] === box || !boxes[r].hasAttribute("name")) continue;
        if (!box.checked) boxes[r].checked = false;
        boxes[r].disabled = !box.checked;
      }
      recount(id);
      return;
    }
    for (var i = 0; i < boxes.length; i += 1) {
      var other = boxes[i];
      if (other === box || other.disabled || other.getAttribute("data-action") !== action) continue;
      var there = other.getAttribute("data-path") || "";
      if (below(path, there)) other.checked = box.checked;
      else if (!box.checked && below(there, path)) other.checked = false;
    }
    if (box.checked) {
      // The nodes above, from the nearest: each one ticks when every node below it is ticked.
      var above = [];
      for (var j = 0; j < boxes.length; j += 1) {
        var up = boxes[j];
        if (up.disabled || up.getAttribute("data-action") !== action) continue;
        if (below(up.getAttribute("data-path") || "", path)) above.push(up);
      }
      above.sort(function (a, b) {
        return (b.getAttribute("data-path") || "").length - (a.getAttribute("data-path") || "").length;
      });
      for (var k = 0; k < above.length; k += 1) {
        var upPath = above[k].getAttribute("data-path") || "";
        var all = true;
        for (var m = 0; m < boxes.length; m += 1) {
          var under = boxes[m];
          if (under.getAttribute("data-action") !== action || under === above[k]) continue;
          if (below(upPath, under.getAttribute("data-path") || "") && !under.checked) all = false;
        }
        if (!all) break;
        above[k].checked = true;
      }
    }
    recount(id);
  });

  // The tree selects one panel. All the panels stay in the page, so a box that was ticked in one
  // panel is still ticked when another panel shows. Without this script, every panel shows under
  // the tree, and a line links to its panel.
  function select(panelId) {
    var rows = document.querySelectorAll(".perm__row");
    for (var i = 0; i < rows.length; i += 1) {
      var link = rows[i].querySelector("a.perm__link");
      var on = !!link && link.getAttribute("href") === "#" + panelId;
      rows[i].classList.toggle("is-selected", on);
      if (link && on) link.setAttribute("aria-current", "true");
      else if (link) link.removeAttribute("aria-current");
    }
    var panels = document.querySelectorAll(".perm__panel");
    for (var j = 0; j < panels.length; j += 1) {
      panels[j].classList.toggle("is-selected", panels[j].id === panelId);
    }
  }

  document.addEventListener("click", function (event) {
    var target = event.target;
    var link = target.closest && target.closest(".perm__row a.perm__link");
    if (!link) return;
    event.preventDefault();
    var panelId = link.getAttribute("href").slice(1);
    select(panelId);
    var panel = document.querySelector(".perm__panel.is-selected");
    // On a narrow window the panel is under the tree: bring it into view.
    if (panel && panel.getBoundingClientRect().top > window.innerHeight * 0.8) {
      panel.scrollIntoView({ block: "start" });
    }
  });

  // The views of the page of an account (Access, Activity, Account), as the tabs of the Explorer.
  // The view that the address names shows first.
  function showTab(id) {
    var links = document.querySelectorAll(".userTabs [data-tab]");
    for (var i = 0; i < links.length; i += 1) {
      var on = links[i].getAttribute("data-tab") === id;
      links[i].classList.toggle("is-active", on);
      if (on) links[i].setAttribute("aria-current", "true");
      else links[i].removeAttribute("aria-current");
    }
    var tabs = document.querySelectorAll(".userTab");
    for (var j = 0; j < tabs.length; j += 1) tabs[j].classList.toggle("is-selected", tabs[j].id === id);
  }

  document.addEventListener("click", function (event) {
    var link = event.target.closest && event.target.closest(".userTabs [data-tab]");
    if (!link) return;
    event.preventDefault();
    var id = link.getAttribute("data-tab");
    showTab(id);
    try {
      window.history.replaceState(null, "", "#" + id);
    } catch (e) {}
  });

  if (window.location.hash && document.querySelector(".userTabs")) {
    var wanted = window.location.hash.slice(1);
    if (document.querySelector('.userTab[id="' + escapeId(wanted) + '"]')) showTab(wanted);
  }

  // The search field of the tree: it keeps the services whose name, id or resources match, and
  // the group headings that still have a service.
  document.addEventListener("input", function (event) {
    var field = event.target;
    if (!field.classList || !field.classList.contains("perms__search")) return;
    var query = field.value.trim().toLowerCase();
    var list = field.closest(".perms").querySelector(".perms__list");
    var heading = null;
    var shown = 0;
    for (var i = 0; i < list.children.length; i += 1) {
      var node = list.children[i];
      if (node.classList.contains("perms__group")) {
        if (heading) heading.hidden = shown === 0;
        heading = node;
        shown = 0;
        continue;
      }
      var match = query === "" || (node.getAttribute("data-find") || "").indexOf(query) !== -1;
      node.hidden = !match;
      if (match) shown += 1;
    }
    if (heading) heading.hidden = shown === 0;
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
    if (status) status.textContent = "Generated. Note it down: it is not shown again.";
    try {
      window.navigator.clipboard.writeText(password).then(function () {
        if (status) status.textContent = "Generated and copied. Note it down: it is not shown again.";
      }, function () {});
    } catch (e) {}
  });
})();
