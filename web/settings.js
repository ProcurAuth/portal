/* Two small helpers of the Users settings pages. Loaded deferred, on those pages only.

   1. The boxes of the permission cards follow the tree of a service. A box with data-action and
      data-path stands for one grant: the path is empty for the whole service, else `host:prod` and
      so on. Ticking a box ticks the same action on each node below it. Unticking a box unticks the
      same action on each node below it and on each node above it. When the last node below a box
      is ticked, the box above it ticks too. Thus a click on the action of the whole service ticks
      it for every host, a host that is unticked after that leaves the whole service while the
      other hosts keep their own boxes, and every host ticked one by one ticks the whole service.
      A box that is disabled does not change. Without this script, the server drops the grants
      that a broader grant covers when it saves.
      The box `access` of a card locks the other boxes of the card: without it they are unticked
      and disabled. A box without a name belongs to the configuration and stays as it is.
      The count of a card and of a host (ticked/total) follows the boxes.

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

  function recount(scope) {
    var head = scope.querySelector(":scope > summary .perm__n");
    if (!head) return;
    var all = scope.querySelectorAll('input[type="checkbox"]');
    var ticked = 0;
    for (var i = 0; i < all.length; i += 1) if (all[i].checked) ticked += 1;
    head.textContent = ticked + "/" + all.length;
  }

  function recountCard(card) {
    recount(card);
    var nodes = card.querySelectorAll(".perm__node");
    for (var i = 0; i < nodes.length; i += 1) recount(nodes[i]);
  }

  document.addEventListener("change", function (event) {
    var box = event.target;
    if (!box || !box.hasAttribute || !box.hasAttribute("data-action")) return;
    var card = box.closest(".perm");
    if (!card) return;
    var action = box.getAttribute("data-action");
    if (action === "access" && (box.getAttribute("data-path") || "") === "") {
      // The box `access`: it locks and unlocks the other boxes of the card.
      var rest = card.querySelectorAll('input[type="checkbox"][name]');
      for (var r = 0; r < rest.length; r += 1) {
        if (rest[r] === box) continue;
        if (!box.checked) rest[r].checked = false;
        rest[r].disabled = !box.checked;
      }
      recountCard(card);
      return;
    }
    var path = box.getAttribute("data-path") || "";
    var boxes = card.querySelectorAll("input[data-action]");
    for (var i = 0; i < boxes.length; i += 1) {
      var other = boxes[i];
      if (other === box || other.disabled || other.getAttribute("data-action") !== action) continue;
      var there = other.getAttribute("data-path") || "";
      if (below(path, there)) other.checked = box.checked;
      else if (!box.checked && below(there, path)) other.checked = false;
    }
    if (!box.checked) {
      recountCard(card);
      return;
    }
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
    recountCard(card);
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
