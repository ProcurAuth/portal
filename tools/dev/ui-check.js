// Checks the editor of grants in a real browser: the rule "any right on a service opens the service",
// the focus and the scroll after a tick, the note "Access off" with its Undo button, the live region,
// the keys of the tree, and a few rules of the pages of the Inbox. Run it with tools/dev/ui-check.sh. The portal is the demo (demo/up.sh): the check needs
// the services ClickHouse Dash, Notes, Wiki and Big database, and the superadmin.
//
// Environment:
//   UI_CHECK_BASE        base URL of the portal (default http://localhost:8088)
//   UI_CHECK_LOGIN       login of the superadmin (default admin)
//   UI_CHECK_PASSWORD    password of the superadmin (default test)
//   UI_CHECK_KEY_LOGIN   login of an account that can make keys (optional, for the form of a key)
//   UI_CHECK_KEY_PASSWORD  its password
//   UI_CHECK_SHOTS       a directory for the screenshots (optional)
//   UI_CHECK_INBOX       1: run the checks of the Inbox pages too. They send alerts. They need the
//                        accounts carol (no phone, no right of the Inbox) and bob (receives only):
//                        UI_CHECK_CAROL_PASSWORD and UI_CHECK_BOB_PASSWORD. They also check, on a
//                        portal that is yours: Revoke (0) of the phones (needs one phone), the popup of
//                        an enrollment code, and the refusal of a key without the right to open the
//                        service (UI_CHECK_KEY_LOGIN and UI_CHECK_KEY_PASSWORD: an account that makes
//                        MCP keys and holds a right on the service Notes). Each run makes one enrollment
//                        code, and an account holds only a few unused codes for 10 minutes.
//   UI_CHECK_LIMITS_NAME with UI_CHECK_INBOX=1: the names, with commas, of services in the editor whose
//                        document has a budget, to check the limits of `access` on the line of the
//                        source (the demo service Notes has one).
//   UI_CHECK_WIDE        1: look at the six pages at 1440, 1920 and 2560 px, dark and light.
//
// The editor checks write nothing to the portal: they never click Save. The Inbox checks send alerts,
// so run them against a portal that is yours (UI_CHECK_INBOX=1).

const { chromium } = require("playwright");

const BASE = process.env.UI_CHECK_BASE || "http://localhost:8088";
const SHOTS = process.env.UI_CHECK_SHOTS || "";
const CH = "ClickHouse Dash";
const CH_ID = "data-clickhouse-dash";

let failures = 0;
function check(name, ok, detail) {
  console.log((ok ? "ok    " : "FAIL  ") + name + (ok ? "" : " -> " + detail));
  if (!ok) failures += 1;
}

async function open(browser, user, password, path) {
  const context = await browser.newContext({ viewport: { width: 1400, height: 900 } });
  const page = await context.newPage();
  await page.goto(BASE + "/auth/login");
  await page.fill("#login", user);
  await page.fill("#password", password);
  // The sign-in form has a minimum time between its display and its submit.
  await page.waitForTimeout(1200);
  await page.click("button[type=submit]");
  await page.goto(BASE + path);
  await page.waitForSelector(".ge__svc");
  return page;
}

function row(page, name) {
  return page.locator(".ge__svc", { has: page.locator(`.ge__svcName:text-is("${name}")`) });
}

async function state(page, name) {
  const r = row(page, name);
  return {
    on: await r.locator("input[data-switch]").isChecked(),
    info: await r.locator(".ge__svcInfo").innerText(),
  };
}

function fields(page) {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll(".grantEditor__fields input[name^=grant]")).map((i) => i.value)
  );
}

async function expand(page, name) {
  const r = row(page, name);
  if (!(await r.evaluate((el) => el.classList.contains("is-expanded")))) await r.locator("[data-toggle]").click();
  await page.waitForSelector(".ge__svc.is-expanded .ge__tree");
}

async function node(page, path) {
  await page.waitForSelector(`.ge__svc.is-expanded .ge__node[data-path="${path}"]`);
  await page.click(`.ge__svc.is-expanded .ge__node[data-path="${path}"]`);
}

async function tick(page, action, on = true) {
  const box = page.locator(`.ge__svc.is-expanded input[data-action="${action}"]`);
  if ((await box.isChecked()) !== on) await box.click();
}

async function shot(page, name) {
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/${name}.png` });
}

// After a right, the switch is on, the info line counts the rights, and the field `access` is sent.
async function expectOpen(page, name, id, rights, label) {
  const s = await state(page, name);
  check(`${label}: the switch is on`, s.on, JSON.stringify(s));
  check(
    `${label}: the row says "${rights}"`,
    s.info.includes(rights) && !s.info.includes("do nothing") && !s.info.includes("Cannot open"),
    s.info
  );
  const sent = await fields(page);
  check(`${label}: the form sends access on service:${id}`, sent.includes(`access on service:${id}`), JSON.stringify(sent));
}

async function newUser(browser) {
  const login = process.env.UI_CHECK_LOGIN || "admin";
  const password = process.env.UI_CHECK_PASSWORD || "test";
  const page = await open(browser, login, password, "/auth/settings/new-user");

  // A right on the whole service.
  await expand(page, CH);
  await node(page, "");
  await tick(page, "query.run.read");
  await expectOpen(page, CH, CH_ID, "1 permission", "whole service");
  await shot(page, "whole-service");

  // The last right goes: the switch stays on.
  await tick(page, "query.run.read", false);
  let s = await state(page, CH);
  check("last right removed: the switch stays on", s.on, JSON.stringify(s));
  check("last right removed: the access field stays", (await fields(page)).join() === `access on service:${CH_ID}`, (await fields(page)).join());

  // The switch off with no right: nothing is removed.
  await row(page, CH).locator("input[data-switch]").uncheck();
  s = await state(page, CH);
  check("switch off with no right: the row says Cannot open", !s.on && s.info.includes("Cannot open"), JSON.stringify(s));

  // A right on a resource.
  await node(page, "host:ch-eu1");
  await tick(page, "query.run.read");
  await expectOpen(page, CH, CH_ID, "1 permission", "resource");
  await page.waitForTimeout(100);
  const said = await page.locator("[data-ge-live]").innerText();
  check("the live region says that the service opened", said.includes("can open"), said);

  // A right on a sub-resource: the databases come from the options route.
  await page.waitForSelector('.ge__svc.is-expanded .ge__node[data-path="host:ch-eu1/database:default"]');
  await node(page, "host:ch-eu1/database:default");
  await tick(page, "query.run.write");
  await expectOpen(page, CH, CH_ID, "2 permissions", "sub-resource");
  await shot(page, "sub-resource");

  // The box of a group on the whole service.
  await node(page, "");
  await page.locator('.ge__svc.is-expanded input[data-group-box="query"]').check();
  await expectOpen(page, CH, CH_ID, "5 permissions", "group box");

  // The service has the switch: the pane has no second control for the same right.
  check("no row access in the pane", (await page.locator('.ge__svc.is-expanded input[data-action="access"]').count()) === 0, "row found");

  // The keyboard: a tick keeps the focus on its box.
  const key = page.locator('.ge__svc.is-expanded input[data-action="query.export"]');
  await key.focus();
  await page.keyboard.press("Space");
  let active = await page.evaluate(() => (document.activeElement && document.activeElement.dataset.action) || "");
  check("a tick with the keyboard keeps the focus on the box", active === "query.export", active);
  // The same for the switch: the keyboard turns it off, and the note shows.
  const gate = row(page, CH).locator("input[data-switch]");
  const count = (await fields(page)).length - 1;
  await gate.focus();
  await page.keyboard.press("Space");
  active = await page.evaluate(() => (document.activeElement && document.activeElement.dataset.switch) || "");
  check("a tick of the switch keeps the focus on the switch", active === CH_ID, active);

  // The tree: one stop of the key Tab, and the arrow keys move.
  const stops = await page.locator('.ge__svc.is-expanded .ge__node[tabindex="0"]').count();
  check("the tree has one stop of the key Tab", stops === 1, String(stops));
  const names = await page.locator(".ge__svc.is-expanded .ge__tree").evaluate((el) => el.getAttribute("aria-label"));
  check("the tree has a name", !!names, String(names));
  await page.locator('.ge__svc.is-expanded .ge__node[data-path=""]').focus();
  await page.keyboard.press("ArrowDown");
  active = await page.evaluate(() => (document.activeElement && document.activeElement.dataset.path) || "none");
  check("ArrowDown moves to the next row of the tree", active !== "" && active !== "none", active);

  // The switch off takes the rights with it, says how many, and the note stays with a button Undo.
  s = await state(page, CH);
  check("switch off: the row says Access off and the count", !s.on && s.info.includes("Access off") && s.info.includes(`${count} permissions removed`), JSON.stringify(s));
  check("switch off: no field is sent", (await fields(page)).length === 0, (await fields(page)).join());
  await shot(page, "switch-off");
  await page.waitForTimeout(5500);
  s = await state(page, CH);
  check("the note stays after 5 seconds", s.info.includes("Access off"), JSON.stringify(s));
  check("the note has an Undo button", (await row(page, CH).locator("button[data-undo]").count()) === 1, s.info);
  const said2 = await page.locator("[data-ge-live]").innerText();
  check("the live region says the note", said2.includes("Access off"), said2);

  // Undo puts the grants back, and the focus goes to the switch.
  await row(page, CH).locator("button[data-undo]").click();
  s = await state(page, CH);
  check("Undo: the switch and the rights are back", s.on && (await fields(page)).length === count + 1, JSON.stringify(s) + (await fields(page)).length);
  active = await page.evaluate(() => (document.activeElement && document.activeElement.dataset.switch) || "");
  check("Undo: the focus is on the switch", active === CH_ID, active);

  // A new change takes the note away. The switch on again brings only access.
  await gate.uncheck();
  await tick(page, "query.export");
  s = await state(page, CH);
  check("a new change takes the note away", !s.info.includes("Access off"), s.info);
  await gate.uncheck();
  await gate.check();
  s = await state(page, CH);
  check("switch on again: only access is back", s.on && (await fields(page)).join() === `access on service:${CH_ID}`, JSON.stringify(s));

  // A table, four levels below the service (Big database).
  await expand(page, "Big database");
  for (const path of ["cluster:eu1", "cluster:eu1/host:ch-1-1", "cluster:eu1/host:ch-1-1/database:db1"]) await node(page, path);
  await node(page, "cluster:eu1/host:ch-1-1/database:db1/table:t1");
  // A long list starts with its groups closed: open the first one.
  await page.locator(".ge__svc.is-expanded .ge__group").first().click();
  const first = await page.locator(".ge__svc.is-expanded .ge__act input[data-action]:not([disabled])").first().getAttribute("data-action");
  await tick(page, first);
  await expectOpen(page, "Big database", "bigdb", "1 permission", "table");

  // The scroll of the list stays after a tick.
  for (let n = 0; n < 30; n += 1) {
    const closed = page.locator('.ge__svc.is-expanded .ge__chev[aria-expanded="false"]');
    if (!(await closed.count())) break;
    await closed.first().click();
  }
  const list = page.locator(".ge__svc.is-expanded .ge__list");
  await list.evaluate((el) => (el.scrollTop = 200));
  const before = await list.evaluate((el) => el.scrollTop);
  const second = page.locator(".ge__svc.is-expanded .ge__act input[data-action]:not([disabled]):not(:checked)").nth(3);
  await second.scrollIntoViewIfNeeded();
  const moved = await list.evaluate((el) => el.scrollTop);
  await second.click();
  const after = await list.evaluate((el) => el.scrollTop);
  check("the scroll of the list stays after a tick", before > 0 && after === moved, `${before} ${moved} ${after}`);
  const width = await page.locator(".ge__svc.is-expanded .ge__list").evaluate((el) => el.scrollWidth - el.clientWidth);
  check("the list of the actions has no horizontal scroll", width <= 0, String(width));

  // A right of the portal features adds no `access on service:portal`.
  await page.locator('.featBox input[data-cell-action="keys.own.read"]').check();
  const sent = await fields(page);
  check("portal feature: the right is sent without access on the portal", sent.includes("keys.own.read on service:portal") && !sent.includes("access on service:portal"), sent.join());
  await page.close();
}

async function keyForm(browser) {
  const login = process.env.UI_CHECK_KEY_LOGIN;
  if (!login) {
    console.log("skip  form of a key: UI_CHECK_KEY_LOGIN is not set");
    return;
  }
  const page = await open(browser, login, process.env.UI_CHECK_KEY_PASSWORD || "", "/auth/keys/new?kind=mcp");
  const names = await page.locator(".ge__svcName").allInnerTexts();
  check("form of a key: the editor lists a service", names.length > 0, names.join());
  const name = names[0];
  await row(page, name).locator("[data-same]").click();
  const s = await state(page, name);
  check(`form of a key: Same as mine opens ${name}`, s.on && !s.info.includes("do nothing"), JSON.stringify(s));
  await shot(page, "same-as-mine");
  await page.close();
}

// ---- the pages of the Inbox. These checks send alerts: they need a portal that is yours.

async function signIn(browser, user, password, path, viewport, scheme) {
  const context = await browser.newContext({ viewport: viewport || { width: 1440, height: 900 }, colorScheme: scheme || "dark" });
  const page = await context.newPage();
  await page.goto(BASE + "/auth/login");
  await page.fill("#login", user);
  await page.fill("#password", password);
  // The sign-in form has a minimum time between its display and its submit.
  await page.waitForTimeout(1200);
  await page.click("button[type=submit]");
  const response = await page.goto(BASE + path);
  return { page, response };
}

function contrast(page, selector) {
  return page.evaluate((sel) => {
    const parse = (text) => text.match(/[\d.]+/g).map(Number);
    const light = (rgb) => {
      const [r, g, b] = rgb.map((v) => {
        v /= 255;
        return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
      });
      return 0.2126 * r + 0.7152 * g + 0.0722 * b;
    };
    const el = document.querySelector(sel);
    const style = getComputedStyle(el);
    const a = light(parse(style.color)), b = light(parse(style.backgroundColor));
    return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
  }, selector);
}

async function inbox(browser) {
  const login = process.env.UI_CHECK_LOGIN || "admin";
  const password = process.env.UI_CHECK_PASSWORD || "test";
  const NEW = "details.pop > summary.button--primary";
  for (const scheme of ["dark", "light"]) {
    const { page } = await signIn(browser, login, password, "/auth/settings/inbox", { width: 1280, height: 800 }, scheme);
    await page.click(NEW);
    check(`${scheme}: the open button of New alert has a contrast of 4.5`, (await contrast(page, NEW)) >= 4.5, String(await contrast(page, NEW)));
    const opened = await page.locator("details.pop[open]").count();
    check(`${scheme}: New alert opens`, opened === 1, String(opened));
    await page.keyboard.press("Escape");
    check(`${scheme}: Escape closes New alert`, (await page.locator("details.pop[open]").count()) === 0, "still open");
    const onButton = await page.evaluate(() => document.activeElement && document.activeElement.matches("summary.button--primary"));
    check(`${scheme}: the focus goes back to the button`, onButton, "elsewhere");
    await page.click('summary[aria-label="Documentation for senders"]');
    await page.keyboard.press("Escape");
    check(`${scheme}: Escape closes the documentation`, (await page.locator("details.pop[open]").count()) === 0, "still open");
    // The popup keeps its height and the title its place between the two recipients.
    await page.click(NEW);
    const box = async () =>
      page.evaluate(() => {
        const menu = document.querySelector("details.pop[open] .pop__menu").getBoundingClientRect();
        const visible = Array.from(document.querySelectorAll("details.pop[open] input[name=title]")).find((el) => el.offsetParent);
        return { h: Math.round(menu.height), y: Math.round(visible.getBoundingClientRect().top) };
      });
    const group = await box();
    await page.click('label[for="rcpt-user"]');
    const user = await box();
    check(`${scheme}: the popup has the same height for a group and for an account`, group.h === user.h && group.y === user.y, JSON.stringify([group, user]));
    await shot(page, `inbox-popup-${scheme}`);
    if (scheme === "dark") {
      // A refused form comes back open, with the title. The account carol has no phone.
      await page.evaluate(() => {
        const radio = document.querySelector('input[name="to"][value="user:carol"]');
        radio.disabled = false;
        radio.checked = true;
      });
      await page.fill("#send-user-form-title", "Keep this title");
      await page.evaluate(() => document.querySelector(".rcptForm--user").dispatchEvent(new Event("input", { bubbles: true })));
      await Promise.all([page.waitForNavigation(), page.click(".rcptForm--user button[type=submit]")]);
      check("a refused form keeps the popup open", (await page.locator("details.pop[open]").count()) === 1, "closed");
      const typed = await page.locator("#send-user-form-title").inputValue();
      check("a refused form keeps the title", typed === "Keep this title", typed);
      const why = await page.locator("[data-send-error]").innerText();
      check("a refused form says why", why.includes("no phone"), why);
      check("a refused form shows no banner that moves the page", (await page.locator(".uiBanner").count()) === 0, "banner");
      await shot(page, "inbox-refused");
      // A group with no phone: the alert is made and has its own state.
      await page.goto(BASE + "/auth/settings/inbox");
      await page.click(NEW);
      await page.click('label[for="rcpt-group"]');
      await page.fill("#send-group-form-title", "Nobody is home");
      await Promise.all([page.waitForNavigation(), page.click(".rcptForm--group button[type=submit]")]);
      check("a new alert shows no banner", (await page.locator(".uiBanner").count()) === 0, "banner");
      const state = await page.locator("tr[data-alert] .st--off").count();
      check("an alert that no phone can receive is not a success", state >= 1 && (await page.locator('a.chip[href*="a=none"]').count()) === 1, String(state));
      const years = await page.locator("tr[data-alert] time.when[data-short]").first().innerText();
      check("a date of this year has no year", /^\d\d-\d\d \d\d:\d\d$/.test(years), years);
      const cols = await page.evaluate(() => {
        const cells = Array.from(document.querySelectorAll("tr[data-alert]")[0].children).map((el) => Math.round(el.getBoundingClientRect().width));
        return { sentBy: cells[4], title: cells[3] };
      });
      check("the column Sent by keeps 128 px and the title has more", cols.sentBy === 128 && cols.title > cols.sentBy, JSON.stringify(cols));
    }
    await page.context().close();
  }
  // The pages that say no have the frame of the settings pages.
  const { page: bad, response: bad400 } = await signIn(browser, login, password, "/auth/settings/inbox?p=bad");
  check("the page 400 has the frame and a way back", bad400.status() === 400 && (await bad.locator(".empty a[href='/auth/settings/inbox']").count()) === 1, String(bad400.status()));
  await shot(bad, "inbox-400");
  await bad.context().close();
  // carol has no right of the Inbox, and bob can receive only.
  if (process.env.UI_CHECK_CAROL_PASSWORD) {
    const { page: carol, response: no } = await signIn(browser, "carol", process.env.UI_CHECK_CAROL_PASSWORD, "/auth/settings/inbox");
    check("the page 403 has the frame", no.status() === 403 && (await carol.locator(".empty").count()) >= 1, String(no.status()));
    await carol.context().close();
  } else console.log("skip  page 403: UI_CHECK_CAROL_PASSWORD is not set");
  if (process.env.UI_CHECK_BOB_PASSWORD) {
    const { page: bob } = await signIn(browser, "bob", process.env.UI_CHECK_BOB_PASSWORD, "/auth/settings/inbox", { width: 1440, height: 800 });
    const width = await bob.locator(".zone").first().evaluate((el) => Math.round(el.getBoundingClientRect().width));
    check("My phones uses the width of the page", width > 1000, String(width));
    await shot(bob, "inbox-bob");
    await bob.context().close();
  } else console.log("skip  My phones: UI_CHECK_BOB_PASSWORD is not set");
}


// ---- the checks of the leftovers of the review. They write to the portal: a key, an enrollment
// code. Use a portal that is yours (UI_CHECK_INBOX=1). They need the accounts alice (the rights to
// make an MCP key and to add a phone) and a phone of any account in the Inbox.

// "Revoke (0)" is not a stop of the key Tab, and the keyboard does not open it, in the Inbox and in
// the list of keys. A ticked row gives it back.
async function revokeZero(browser, login, password) {
  const { page } = await signIn(browser, login, password, "/auth/settings/inbox");
  const summary = page.locator('[data-bulk="revoke-phones"] > summary');
  check("Revoke (0) of the phones: it exists", (await summary.count()) === 1, "no button");
  check("Revoke (0) of the phones: tabindex is -1 and aria-disabled is true", (await summary.getAttribute("tabindex")) === "-1" && (await summary.getAttribute("aria-disabled")) === "true", await summary.evaluate((el) => el.outerHTML));
  await page.locator("input.bulk__box").first().focus();
  for (let n = 0; n < 4; n += 1) await page.keyboard.press("Tab");
  const onSummary = await page.evaluate(() => !!(document.activeElement && document.activeElement.matches("[data-bulk] > summary")));
  check("Revoke (0) of the phones: the key Tab does not stop on it", !onSummary, "the focus is on the button");
  await page.locator("input.bulk__box").first().check();
  check("Revoke (1) of the phones: tabindex is 0", (await summary.getAttribute("tabindex")) === "0", String(await summary.getAttribute("tabindex")));
  await page.context().close();
}

// The popup of a new enrollment code: the focus goes into it, it has a name, the code is a text
// that can be read, and "Done" takes it away and gives the focus back to the button.
async function enrollmentCode(browser, login, password) {
  const { page } = await signIn(browser, login, password, "/auth/settings/inbox");
  await Promise.all([page.waitForNavigation(), page.click('button[aria-label="Add a phone"]')]);
  const pop = page.locator("[data-code-pop]");
  // An account holds a few codes that are not used (inbox.enrollment.max_active_per_account): each run makes one.
  check("the code popup is on the page", (await pop.count()) === 1, "absent: " + (await page.locator(".uiBanner, .empty").allInnerTexts()).join(" | "));
  check("the code popup has a role and a name", (await pop.getAttribute("role")) === "group" && !!(await pop.getAttribute("aria-label")), "no name");
  const where = await page.evaluate(() => {
    const el = document.activeElement;
    return { inside: !!(el && el.closest("[data-code-pop]")), label: el ? el.getAttribute("aria-label") || el.tagName : "" };
  });
  check("the focus is inside the code popup", where.inside, JSON.stringify(where));
  const code = await page.locator("#enroll-code").inputValue();
  check("the code is a text with the address of the server", /^http/.test(code) || code.length > 20, code.slice(0, 20));
  await shot(page, "inbox-code");
  await page.locator("[data-code-done]").click();
  check("Done takes the code away", (await page.locator("[data-code-pop]").count()) === 0, "still there");
  const back = await page.evaluate(() => !!(document.activeElement && document.activeElement.matches('[aria-label="Add a phone"]')));
  check("Done gives the focus back to the button Add a phone", back, "elsewhere");
  await page.context().close();
}

// The limits of the action `access` show on the line of the source, at the right, when the switch
// is on, and the pane has no second row for the same grant. The service has a budget in its document.
async function accessLimits(browser, login, password, name) {
  for (const width of [1280, 1440]) {
    const { page } = await signIn(browser, login, password, "/auth/settings/new-user", { width, height: 900 });
    await page.waitForSelector(".ge__svc");
    await expand(page, name);
    await row(page, name).locator("input[data-switch]").check();
    const line = page.locator(".ge__svc.is-expanded .ge__svcTop .srcLine");
    check(`limits of access at ${width}: ${name} has a source line`, (await line.count()) === 1, String(await line.count()));
    const fields = page.locator(".ge__svc.is-expanded .ge__svcTop .ge__limitsInline input");
    check(`limits of access at ${width}: ${name} shows its limit fields`, (await fields.count()) > 0, "no field");
    const same = await page.evaluate(() => {
      const top = document.querySelector(".ge__svc.is-expanded .ge__svcTop");
      const src = top.querySelector(".srcLine").getBoundingClientRect();
      const boxes = Array.from(top.querySelectorAll(".ge__limitsInline input")).map((el) => el.getBoundingClientRect());
      const edge = top.getBoundingClientRect();
      return {
        // One budget fits on the line of the source. Several budgets may go under it.
        ok:
          boxes.length > 0 &&
          boxes.every((b) => b.right <= edge.right + 1) &&
          (top.querySelectorAll(".ge__limit").length > 1 || boxes.every((b) => b.top >= src.top - 8 && b.bottom <= src.bottom + 8 && b.left >= src.right - 1)),
        scroll: top.scrollWidth - top.clientWidth,
      };
    });
    check(`limits of access at ${width}: the fields are inside the pane, and at the right of the source line for one budget`, same.ok && same.scroll <= 0, JSON.stringify(same));
    await shot(page, `access-limits-${width}`);
    // The pane has no row access: the switch is the grant.
    check(`limits of access at ${width}: no row access in the pane`, (await page.locator('.ge__svc.is-expanded input[data-action="access"]').count()) === 0, "row found");
    await page.context().close();
  }
}

// A right without the right to open the service is refused, and the form comes back with what
// the person typed.
async function editRefused(browser, login, password) {
  const { page } = await signIn(browser, login, password, "/auth/keys/new?kind=mcp");
  await page.waitForSelector(".ge__svc");
  await page.fill("#key-name", "i3-refused");
  await expand(page, "Notes");
  await node(page, "");
  await tick(page, "notes.read");
  await Promise.all([page.waitForNavigation(), page.click('button[type=submit].button--primary')]);
  await page.goto(BASE + "/auth/settings");
  const href = await page.evaluate(() => {
    const row = Array.from(document.querySelectorAll("tr")).find((r) => r.textContent.includes("i3-refused"));
    const a = row && row.querySelector('a[href^="/auth/keys/edit?id="]');
    return a ? a.getAttribute("href") : "";
  });
  check("edit refused: the key is in the list", href !== "", "no edit link");
  await page.goto(BASE + href);
  await page.waitForSelector(".ge__svc");
  await expand(page, "Notes");
  await node(page, "");
  await tick(page, "notes.read", false);
  await tick(page, "notes.read", true);
  // The editor ticked the switch too. Take it away from the fields: the form has a right and no access.
  await page.evaluate(() => {
    document.querySelectorAll('.grantEditor__fields input[value^="access on service:notes"]').forEach((el) => el.remove());
  });
  const before = (await fields(page)).join();
  await Promise.all([page.waitForNavigation(), page.click('button[type=submit].button--primary')]);
  const html = await page.content();
  check("edit refused: the page is the form again", (await page.locator("#key-form").count()) === 1 && html.includes("Edit key"), page.url());
  check("edit refused: the name is still there", (await page.locator("#key-name").inputValue()) === "i3-refused", await page.locator("#key-name").inputValue());
  const notice = await page.locator('[role="alert"], .uiBanner, .uiField__error').allInnerTexts();
  check("edit refused: the notice names the service", notice.join(" ").includes("Notes"), notice.join(" | "));
  const kept = await fields(page);
  check("edit refused: the form keeps the rights that were typed", kept.some((v) => v.startsWith("notes.read on service:notes")), before + " -> " + kept.join());
  await shot(page, "edit-refused");
  await page.context().close();
}

// Each page at the wide widths, dark and light: no scroll of the page where the window has room,
// no horizontal scroll, and the first zone does not stretch past what a person reads.
const WIDE_PAGES = [
  ["access", "/auth/settings"],
  ["services", "/auth/settings/services"],
  ["activity", "/auth/settings/activity"],
  ["inbox", "/auth/settings/inbox"],
  ["new-user", "/auth/settings/new-user"],
  ["new-key", "/auth/keys/new?kind=api"],
];

async function wide(browser, login, password) {
  for (const width of [1440, 1920, 2560]) {
    const height = width === 2560 ? 1440 : width === 1920 ? 1080 : 900;
    for (const scheme of ["dark", "light"]) {
      const context = await browser.newContext({ viewport: { width, height }, colorScheme: scheme });
      const page = await context.newPage();
      await page.goto(BASE + "/auth/login");
      await page.fill("#login", login);
      await page.fill("#password", password);
      await page.waitForTimeout(1200);
      await page.click("button[type=submit]");
      for (const [name, path] of WIDE_PAGES) {
        await page.goto(BASE + path);
        await page.waitForLoadState("load");
        const m = await page.evaluate(() => {
          const doc = document.documentElement;
          const inputs = Array.from(document.querySelectorAll("input.uiInput:not([type=checkbox]):not([type=hidden])")).map((el) => el.getBoundingClientRect().width);
          return { vScroll: doc.scrollHeight - window.innerHeight, hScroll: doc.scrollWidth - window.innerWidth, widest: Math.round(Math.max(0, ...inputs)) };
        });
        check(`${name} ${width}x${height} ${scheme}: no horizontal scroll of the page`, m.hScroll <= 0, JSON.stringify(m));
        // Where the page fills the window at 1440, it must do the same at the wide widths.
        check(`${name} ${width}x${height} ${scheme}: no scroll of the page`, m.vScroll <= 0, JSON.stringify(m));
        check(`${name} ${width}x${height} ${scheme}: no text field stretches past 900 px`, m.widest <= 900, JSON.stringify(m));
        await shot(page, `wide-${name}-${width}-${scheme}`);
      }
      await context.close();
    }
  }
}

(async () => {
  const browser = await chromium.launch();
  try {
    await newUser(browser);
    await keyForm(browser);
    if (process.env.UI_CHECK_INBOX === "1") {
      await inbox(browser);
      const login = process.env.UI_CHECK_LOGIN || "admin";
      const password = process.env.UI_CHECK_PASSWORD || "test";
      await revokeZero(browser, login, password);
      await enrollmentCode(browser, login, password);
      if (process.env.UI_CHECK_LIMITS_NAME) {
        for (const name of process.env.UI_CHECK_LIMITS_NAME.split(",")) await accessLimits(browser, login, password, name);
      }
      if (process.env.UI_CHECK_KEY_LOGIN) await editRefused(browser, process.env.UI_CHECK_KEY_LOGIN, process.env.UI_CHECK_KEY_PASSWORD || "");
      else console.log("skip  edit refused: UI_CHECK_KEY_LOGIN is not set");
    }
    if (process.env.UI_CHECK_WIDE === "1") await wide(browser, process.env.UI_CHECK_LOGIN || "admin", process.env.UI_CHECK_PASSWORD || "test");
  } finally {
    await browser.close();
  }
  console.log(failures ? `${failures} check(s) failed` : "all checks passed");
  process.exit(failures ? 1 : 0);
})().catch((error) => {
  console.error(error);
  process.exit(2);
});
