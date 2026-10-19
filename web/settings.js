/* Two small helpers of the Users settings pages. Loaded deferred, on those pages only.

   1. The permissions are a list of services and one panel. A click on a line of the list shows the
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

   2. "Generate" in the user forms of the Users settings page: a random password drawn from the browser's
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

  function recount(id) {
    var sel = '[data-svc="' + escapeId(id) + '"]';
    var all = boxesOf(id);
    var counts = document.querySelectorAll(sel + " > .perm__link .perm__n, " + sel + " > .perm__panelhead .perm__n");
    for (var i = 0; i < counts.length; i += 1) setCount(counts[i], all);
    var nodes = document.querySelectorAll(sel + " .perm__node");
    for (var j = 0; j < nodes.length; j += 1) {
      setCount(nodes[j].querySelector(".perm__n"), nodes[j].querySelectorAll('input[type="checkbox"]'));
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

  // The list of services selects one panel. All the panels stay in the page, so a box that was
  // ticked in one panel is still ticked when another panel shows. Without this script, every
  // panel shows under the list, and the line of a service links to its panel.
  function select(id) {
    var rows = document.querySelectorAll(".perm__row");
    for (var i = 0; i < rows.length; i += 1) {
      var on = rows[i].getAttribute("data-svc") === id;
      rows[i].classList.toggle("is-selected", on);
      var link = rows[i].querySelector(".perm__link");
      if (link && on) link.setAttribute("aria-current", "true");
      else if (link) link.removeAttribute("aria-current");
    }
    var panels = document.querySelectorAll(".perm__panel");
    for (var j = 0; j < panels.length; j += 1) {
      panels[j].classList.toggle("is-selected", panels[j].getAttribute("data-svc") === id);
    }
  }

  document.addEventListener("click", function (event) {
    var link = event.target.closest && event.target.closest(".perm__row a.perm__link");
    if (!link) return;
    event.preventDefault();
    var row = link.closest(".perm__row");
    select(row.getAttribute("data-svc"));
    var panel = document.querySelector(".perm__panel.is-selected");
    // On a narrow window the panel is under the list: bring it into view.
    if (panel && panel.getBoundingClientRect().top > window.innerHeight * 0.8) {
      panel.scrollIntoView({ block: "start" });
    }
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
