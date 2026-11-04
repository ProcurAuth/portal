// Checks the notifications in a real browser: the inbox icon and its popover on the launcher, the
// toasts, the badge of the tab, the levels, the polling and the keyboard. Run it with
// tools/dev/ui-check.sh and UI_CHECK_SCRIPT=ui-check-notify.js. The checks send alerts: use a portal
// that is yours, never a shared demo.
//
// The portal needs these accounts and one API key (see the Inbox tests for a configuration):
//   root (the superadmin) with one active phone, so that an alert to an account is possible
//   mia   inbox.receive on infra and crit, alert.read on infra and crit
//   otto  inbox.receive on other
//   nina  keys.own.read only: no right of the Inbox
//   groups infra, other, and crit with critical = true
//   an API key with alert.send on the three groups and alert.send_user, and direct_alerts = true
//
// The section fileaccount needs two accounts that the configuration file declares (blocks `user`):
//   fiona  inbox.receive on infra, with one active phone
//   fran   inbox.receive on infra, with no phone
// An account of the file is an account like any other: root sends to it from the popup New alert,
// and it gets the toast and the badge. The test `demo_for_the_eyes` of tests/inbox_e2e.rs starts a
// portal with these two accounts and writes the port and the passwords in demo.json.
//
// Environment:
//   UI_CHECK_BASE      base URL of the portal (default http://127.0.0.1:18741)
//   UI_CHECK_TOKEN     the text of the API key
//   UI_CHECK_USER_PASSWORD  the password of mia, otto and nina (default pw-notify-test-1)
//   UI_CHECK_ROOT_PASSWORD  the password of root
//   UI_CHECK_FILE_USER      the account of the file with a phone (default fiona)
//   UI_CHECK_FILE_NO_PHONE  the account of the file with no phone (default fran)
//   UI_CHECK_FILE_PASSWORD  the password of these two accounts (default: UI_CHECK_USER_PASSWORD)
//   UI_CHECK_ONLY      sections to run, with commas: empty, noscript, flow, critical, settings, load,
//                      off, mobile, keyboard, fileaccount (default: all)
//   UI_CHECK_SHOTS     a directory for the screenshots (optional)
const { chromium } = require("playwright");

const BASE = process.env.UI_CHECK_BASE || "http://127.0.0.1:18741";
const TOKEN = process.env.UI_CHECK_TOKEN;
const SHOTS = process.env.UI_CHECK_SHOTS || "";
const ONLY = (process.env.UI_CHECK_ONLY || "").split(",").filter(Boolean);
const PW = process.env.UI_CHECK_USER_PASSWORD || "pw-notify-test-1";
const ROOT_PW = process.env.UI_CHECK_ROOT_PASSWORD || "a password for notify tests";
let failures = 0;
let counter = Date.now();

function check(name, ok, detail) {
  console.log((ok ? "ok    " : "FAIL  ") + name + (ok ? "" : " -> " + detail));
  if (!ok) failures += 1;
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function send(body) {
  counter += 1;
  const r = await fetch(BASE + "/api/v1/alerts", {
    method: "POST",
    headers: { Authorization: "Bearer " + TOKEN, "Idempotency-Key": "ui-" + counter, "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (r.status !== 201) console.log("send failed", r.status, await r.text());
  return r.status;
}
const toGroup = (group, title) => send({ group, title });
const toUser = (user, title) => send({ user, title });

async function login(browser, user, password, opts) {
  const context = await browser.newContext(Object.assign({ viewport: { width: 1280, height: 800 } }, opts || {}));
  const page = await context.newPage();
  await page.goto(BASE + "/auth/login");
  await page.fill("input[name=login]", user);
  await page.fill("input[name=password]", password);
  await page.waitForTimeout(1300);
  await page.click("button[type=submit]");
  await page.waitForLoadState("load");
  return { context, page };
}

async function shot(page, name) {
  if (SHOTS) await page.screenshot({ path: SHOTS + "/" + name + ".png" });
}

function wants(name) {
  return ONLY.length === 0 || ONLY.includes(name);
}

(async () => {
  const browser = await chromium.launch();

  // 1. The empty state, the closed icon and the popover, in the two themes and the two widths.
  if (wants("empty")) {
    for (const scheme of ["dark", "light"]) {
      for (const [w, h] of [[1280, 800], [390, 800]]) {
        const { context, page } = await login(browser, "otto", PW, { colorScheme: scheme, viewport: { width: w, height: h } });
        await page.goto(BASE + "/");
        const bell = page.locator("[data-notify-bell]");
        check(`bell present for otto (${scheme} ${w})`, (await bell.count()) === 1, "no bell");
        await shot(page, `empty-closed-${scheme}-${w}`);
        await bell.locator("summary").click();
        check(`popover opens (${scheme} ${w})`, await bell.evaluate((e) => e.open), "closed");
        // An account with no alert shows the empty state. Another account shows its list.
        const listed = await page.locator("[data-notify-list] li").count();
        const state = listed > 0 ? await page.locator("[data-notify-list]").isVisible() : await page.locator("[data-notify-empty]").isVisible();
        check(`empty state or list visible (${scheme} ${w})`, state, "neither");
        await shot(page, `empty-open-${scheme}-${w}`);
        await context.close();
      }
    }
    // A person with no right of the inbox has no icon.
    const { context, page } = await login(browser, "nina", PW);
    await page.goto(BASE + "/");
    check("no icon for nina", (await page.locator("[data-notify-bell]").count()) === 0, "icon present");
    check("no toast container for nina", (await page.locator("[data-notify]").count()) === 0, "container present");
    await context.close();
  }

  // 2. Without script: the popover opens, the forms work.
  if (wants("noscript")) {
    await toGroup("infra", "No script one");
    await toUser("root", "No script ring");
    for (const scheme of ["dark", "light"]) {
      const { context, page } = await login(browser, "mia", PW, { javaScriptEnabled: false, colorScheme: scheme });
      await page.goto(BASE + "/");
      const bell = page.locator("[data-notify-bell]");
      await bell.locator("summary").click();
      check(`noscript popover opens (${scheme})`, await bell.evaluate((e) => e.open), "closed");
      check(`noscript rows listed (${scheme})`, (await page.locator("[data-notify-list] li").count()) >= 2, "no rows");
      if (scheme === "dark") await shot(page, "noscript-open-dark");
      else await shot(page, "noscript-open-light");
      await context.close();
    }
    const { context, page } = await login(browser, "mia", PW, { javaScriptEnabled: false });
    await page.goto(BASE + "/");
    await page.locator("[data-notify-bell] summary").click();
    await page.locator("button[data-notify-level=important]").click();
    await page.waitForLoadState("load");
    check("noscript level form redirects home", new URL(page.url()).pathname === "/", page.url());
    await page.locator("[data-notify-bell] summary").click();
    check("level pressed after the form", (await page.locator("button[data-notify-level=important]").getAttribute("aria-pressed")) === "true", "not pressed");
    await page.locator("button[data-notify-level=all]").click();
    await page.waitForLoadState("load");
    await page.locator("[data-notify-bell] summary").click();
    await page.getByRole("button", { name: "Mark all read" }).click();
    await page.waitForLoadState("load");
    check("noscript mark read redirects home", new URL(page.url()).pathname === "/", page.url());
    check("badge hidden after mark read", !(await page.locator("[data-notify-badge]").first().isVisible()), "badge visible");
    await context.close();
  }

  // 3. The flow with script, as mia.
  if (wants("flow")) {
    for (const scheme of ["dark", "light"]) {
      const { context, page } = await login(browser, "mia", PW, { colorScheme: scheme });
      await page.goto(BASE + "/");
      // Read state so far.
      await page.locator("[data-notify-bell] summary").click();
      await page.getByRole("button", { name: "Mark all read" }).click();
      await page.waitForTimeout(600);
      check(`mark read through script keeps the page (${scheme})`, new URL(page.url()).pathname === "/", page.url());
      check(`badge hidden (${scheme})`, !(await page.locator("[data-notify-badge]").first().isVisible()), "badge visible");
      await page.keyboard.press("Escape");
      check(`Escape closes the popover (${scheme})`, !(await page.locator("[data-notify-bell]").evaluate((e) => e.open)), "still open");
      const requests = [];
      page.on("request", (r) => {
        if (r.url().includes("/notifications")) requests.push(Date.now());
      });
      const active = await page.evaluate(() => document.activeElement && document.activeElement.tagName);
      const began = Date.now();
      await toGroup("infra", "Disk usage at 91% on node-4");
      const toast = page.locator(".toast, [data-toast-title]").first();
      await page.waitForSelector("[data-toast-title]", { timeout: 20000 });
      check(`toast within one poll (${scheme})`, Date.now() - began < 16500, `${Date.now() - began} ms`);
      const activeAfter = await page.evaluate(() => document.activeElement && document.activeElement.tagName);
      check(`toast does not take the focus (${scheme})`, active === activeAfter, `${active} -> ${activeAfter}`);
      const text = await page.locator("[data-toast-polite]").innerText();
      check(`toast shows title group and level (${scheme})`, text.includes("Disk usage at 91% on node-4") && text.includes("Infra") && text.includes("Alert"), text);
      const href = await page.locator("[data-toast-link]").first().getAttribute("href");
      check(`toast links to the alert row (${scheme})`, /\/auth\/settings\/inbox\?open=[0-9a-f]+#a-/.test(href || ""), href);
      check(`badge shows 1 (${scheme})`, (await page.locator("[data-notify-n]").first().innerText()) === "1", "count");
      await shot(page, `toast-alert-${scheme}`);
      // Polite region is a status.
      check(`polite region role (${scheme})`, (await page.locator("[data-toast-polite]").getAttribute("aria-live")) === "polite", "aria-live");
      // The toast goes after about 8 seconds.
      const gone = Date.now();
      await page.waitForSelector("[data-toast-title]", { state: "detached", timeout: 15000 });
      const life = Date.now() - gone;
      check(`alert toast disappears after about 8 s (${scheme})`, life > 3000 && life < 12000, `${life} ms after the first check`);
      // The row is in the popover, unread.
      await page.locator("[data-notify-bell] summary").click();
      const rowText = await page.locator("[data-notify-list] li").first().innerText();
      check(`new row at the top of the popover (${scheme})`, rowText.includes("Disk usage at 91% on node-4"), rowText);
      check(`new row is unread (${scheme})`, (await page.locator("[data-notify-list] li[data-unread]").count()) >= 1, "not unread");
      await shot(page, `popover-rows-${scheme}`);
      await page.keyboard.press("Escape");
      await context.close();
      if (scheme === "dark") await sleep(1000);
    }
  }

  // 4. Critical sticks; the summary after three; pause on hover; keyboard.
  if (wants("critical")) {
    const { context, page } = await login(browser, "mia", PW, { colorScheme: "dark" });
    await page.goto(BASE + "/");
    await toGroup("crit", "Primary database is down");
    await page.waitForSelector("[data-toast-assertive] [data-toast-title]", { timeout: 20000 });
    check("critical toast in the assertive region", true, "");
    const t0 = Date.now();
    await shot(page, "toast-critical-dark");
    await page.waitForTimeout(11000);
    check("critical toast stays past 8 s", (await page.locator("[data-toast-assertive] [data-toast-title]").count()) === 1, "gone");
    // Keyboard: focus the close button, close it with Enter.
    const close = page.locator("[data-toast-assertive] [data-toast-close]");
    check("close button has an accessible name", (await close.getAttribute("aria-label")) === "Dismiss", "name");
    await close.focus();
    await page.keyboard.press("Enter");
    check("close button closes by keyboard", (await page.locator("[data-toast-assertive] [data-toast-title]").count()) === 0, "still there");
    // Escape closes the newest toast.
    await toGroup("crit", "Second critical");
    await page.waitForSelector("[data-toast-assertive] [data-toast-title]", { timeout: 20000 });
    await page.keyboard.press("Escape");
    check("Escape closes a toast", (await page.locator("[data-toast-assertive] [data-toast-title]").count()) === 0, "still there");
    // Five at once: three show, the summary says "+2 more".
    for (let i = 1; i <= 5; i += 1) await toGroup("infra", "Burst " + i);
    await page.waitForSelector("[data-toast-title]", { timeout: 20000 });
    await page.waitForTimeout(500);
    const visible = await page.locator("[data-toast-polite] > div:visible, [data-toast-polite] div:visible >> nth=0").count();
    const shown = await page.evaluate(() => Array.from(document.querySelectorAll("[data-toast-polite] [data-toast-title]")).filter((e) => e.offsetParent !== null).length);
    const more = await page.locator("[data-notify-more]").innerText().catch(() => "");
    check("at most three toasts show", shown === 3, `${shown}`);
    check('the rest is summarised "+2 more"', more.trim() === "+2 more", more);
    await shot(page, "toasts-more-dark");
    // Pause on hover: the toast stays while the pointer is on it.
    await page.locator("[data-toast-polite] [data-toast-title]:visible").first().hover();
    await page.waitForTimeout(10000);
    const still = await page.evaluate(() => Array.from(document.querySelectorAll("[data-toast-polite] [data-toast-title]")).filter((e) => e.offsetParent !== null).length);
    check("hover pauses the timer (toasts still there after 10 s)", still >= 1, `${still}`);
    await page.mouse.move(5, 5);
    await page.waitForTimeout(12000);
    const after = await page.evaluate(() => document.querySelectorAll("[data-toast-polite] [data-toast-title]").length);
    check("toasts leave after the pointer leaves", after === 0, `${after}`);
    await context.close();
  }

  // 5. Navigation: no repeat. Settings pages: badge on the tab, toasts, ring level.
  if (wants("settings")) {
    const { context, page } = await login(browser, "root", ROOT_PW, { colorScheme: "dark" });
    await page.goto(BASE + "/auth/settings");
    const tab = page.locator("a.contentTabs__tab", { hasText: "Inbox" });
    check("tab Inbox exists for the superadmin", (await tab.count()) === 1, "no tab");
    // Opening the inbox page moves the marker.
    await page.goto(BASE + "/auth/settings/inbox");
    await shot(page, "inbox-page-dark");
    check("level control in the header of the Inbox page", (await page.locator("form[data-notify-form] [data-notify-level]").count()) === 3, "no control");
    await page.goto(BASE + "/auth/settings");
    check("badge hidden at zero", !(await tab.locator("[data-notify-badge]").isVisible()), "visible");
    await toGroup("other", "Other group news");
    await page.waitForSelector("[data-toast-title]", { timeout: 20000 });
    check("badge on the tab shows 1", (await tab.locator("[data-notify-n]").innerText()) === "1", await tab.innerText());
    await shot(page, "settings-badge-toast-dark");
    // Navigate: no second toast for the same alert, the badge stays.
    await page.goto(BASE + "/auth/settings/services");
    await page.waitForTimeout(17000);
    check("no repeat after a navigation", (await page.locator("[data-toast-title]").count()) === 0, "toast again");
    const tab2 = page.locator("a.contentTabs__tab", { hasText: "Inbox" });
    check("badge still 1 after the navigation", (await tab2.locator("[data-notify-n]").innerText()) === "1", await tab2.innerText());
    const state = await page.evaluate(() => sessionStorage.getItem("notify-state"));
    check("cursor kept in sessionStorage", !!state && JSON.parse(state).s.length >= 1, state);
    await context.close();
  }

  // 6. The load: requests per minute of an open tab, and a hidden tab.
  if (wants("load")) {
    const { context, page } = await login(browser, "mia", PW);
    await page.goto(BASE + "/");
    const times = [];
    page.on("request", (r) => {
      if (r.url().includes("/notifications")) times.push(Date.now());
    });
    await page.waitForTimeout(120000);
    const gaps = times.slice(1).map((t, i) => t - times[i]);
    console.log("poll count in 120 s:", times.length, "gaps (ms):", gaps.join(","));
    check("poll every 10 to 15 s", times.length >= 7 && times.length <= 12 && gaps.every((g) => g >= 9500 && g <= 15500), String(times.length));
    // Hidden tab: no request. Visible again: one request at once.
    await page.evaluate(() => {
      Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
      document.dispatchEvent(new Event("visibilitychange"));
    });
    const hiddenFrom = times.length;
    await page.waitForTimeout(40000);
    check("no request while the tab is hidden", times.length === hiddenFrom, `${times.length - hiddenFrom} requests`);
    await page.evaluate(() => {
      Object.defineProperty(document, "hidden", { configurable: true, get: () => false });
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await page.waitForTimeout(1500);
    check("one request at once when the tab shows again", times.length === hiddenFrom + 1, `${times.length - hiddenFrom}`);
    await context.close();
  }

  // 7. Off: no polling, no toast, no badge; the list shows. Reduced motion.
  if (wants("off")) {
    const { context, page } = await login(browser, "root", ROOT_PW, { colorScheme: "light", reducedMotion: "reduce" });
    await page.goto(BASE + "/");
    const times = [];
    page.on("request", (r) => {
      if (r.url().includes("/notifications")) times.push(Date.now());
    });
    await toGroup("infra", "Motion check");
    await page.waitForSelector("[data-toast-title]", { timeout: 20000 });
    const anim = await page.evaluate(() => getComputedStyle(document.querySelector("[data-toast-title]").closest("div")).animationName);
    check("reduced motion: no animation on a toast", anim === "none", anim);
    await shot(page, "toast-reduced-motion-light");
    await page.locator("[data-notify-bell] summary").click();
    await page.locator("button[data-notify-level=off]").click();
    await page.waitForTimeout(800);
    check("Off: level pressed without a new page", (await page.locator("button[data-notify-level=off]").getAttribute("aria-pressed")) === "true" && new URL(page.url()).pathname === "/", "not pressed");
    check("Off: badge hidden", !(await page.locator("[data-notify-badge]").first().isVisible()), "visible");
    await shot(page, "popover-off-light");
    await page.keyboard.press("Escape");
    const before = times.length;
    await toGroup("infra", "While off");
    await page.waitForTimeout(32000);
    check("Off: no polling request", times.length === before, `${times.length - before}`);
    check("Off: no toast", (await page.locator("[data-toast-title]").count()) <= 1, "toast");
    // The page reloads: the list still shows the alert, with no badge.
    await page.reload();
    await page.locator("[data-notify-bell] summary").click();
    const rows = await page.locator("[data-notify-list]").innerText();
    check("Off: the list still shows the alerts", rows.includes("While off"), rows);
    // Alert and Critical only: a ring gives no toast, the badge still counts. Back to All at the end.
    await page.locator("button[data-notify-level=important]").click();
    await page.waitForTimeout(800);
    await page.keyboard.press("Escape");
    await page.waitForTimeout(15000);
    const t1 = times.length;
    await toUser("root", "Ring while important");
    await page.waitForTimeout(16000);
    check("Alert+Critical only: no toast for a ring", (await page.locator("[data-toast-title]").count()) === 0, "toast");
    const badge = await page.locator("[data-notify-n]").first().innerText();
    check("Alert+Critical only: the badge counts the ring", parseInt(badge, 10) >= 1, badge);
    await page.locator("[data-notify-bell] summary").click();
    check("Alert+Critical only: the list shows the ring", (await page.locator("[data-notify-list]").innerText()).includes("Ring while important"), "no row");
    await page.locator("button[data-notify-level=all]").click();
    await page.waitForTimeout(800);
    await page.keyboard.press("Escape");
    await toUser("root", "Ring at all");
    await page.waitForSelector("[data-toast-title]", { timeout: 20000 });
    const ring = await page.locator("[data-toast-polite]").innerText();
    check("All: a ring gives a toast with the group Direct", ring.includes("Direct") && ring.includes("Ring"), ring);
    await shot(page, "toast-ring-light");
    await context.close();
  }

  // 8. Narrow screens, with a toast and the popover.
  if (wants("mobile")) {
    for (const scheme of ["dark", "light"]) {
      const { context, page } = await login(browser, "mia", PW, { colorScheme: scheme, viewport: { width: 390, height: 780 }, hasTouch: true });
      await page.goto(BASE + "/");
      await toGroup("crit", "Mobile critical " + scheme);
      await page.waitForSelector("[data-toast-title]", { timeout: 20000 });
      await shot(page, `mobile-toast-${scheme}`);
      await page.locator("[data-notify-bell] summary").tap();
      await shot(page, `mobile-popover-${scheme}`);
      const box = await page.locator("[data-notify-list]").first().boundingBox();
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth > document.documentElement.clientWidth);
      check(`no horizontal scroll at 390 (${scheme})`, !overflow, "overflow");
      await context.close();
    }
  }

  // 9. The keyboard: the icon takes the focus in the tab order, Enter opens, Escape closes.
  if (wants("keyboard")) {
    const { context, page } = await login(browser, "mia", PW);
    await page.goto(BASE + "/");
    let reached = false;
    for (let i = 0; i < 12 && !reached; i += 1) {
      await page.keyboard.press("Tab");
      reached = await page.evaluate(() => {
        const a = document.activeElement;
        return !!a && a.tagName === "SUMMARY" && !!a.closest("[data-notify-bell]");
      });
    }
    check("the icon is in the tab order", reached, "not reached");
    await page.keyboard.press("Enter");
    check("Enter opens the popover", await page.locator("[data-notify-bell]").evaluate((e) => e.open), "closed");
    await page.keyboard.press("Tab");
    const inside = await page.evaluate(() => !!document.activeElement.closest("[data-notify-bell]"));
    check("the focus goes into the popover", inside, "outside");
    await page.keyboard.press("Escape");
    check("Escape closes the popover", !(await page.locator("[data-notify-bell]").evaluate((e) => e.open)), "open");
    await context.close();
  }

  // 10. An alert to an account of the file, sent from the popup New alert by the superadmin. The
  // account gets the toast and the badge. The account with no phone is disabled in the list.
  if (wants("fileaccount")) {
    const fileUser = process.env.UI_CHECK_FILE_USER || "fiona";
    const fileNoPhone = process.env.UI_CHECK_FILE_NO_PHONE || "fran";
    const filePw = process.env.UI_CHECK_FILE_PASSWORD || PW;
    const title = "Ring for a file account " + Date.now();
    const target = await login(browser, fileUser, filePw, { colorScheme: "dark" });
    await target.page.goto(BASE + "/");
    check("the account of the file has the icon", (await target.page.locator("[data-notify-bell]").count()) === 1, "no icon");
    // The account can have older alerts: the badge must grow by one.
    await target.page.waitForTimeout(1500);
    const unreadBefore = parseInt(await target.page.locator("[data-notify-n]").first().innerText(), 10) || 0;
    const sender = await login(browser, "root", ROOT_PW, { colorScheme: "dark" });
    await sender.page.goto(BASE + "/auth/settings/inbox");
    await sender.page.click("details.pop > summary.button--primary");
    await sender.page.click('label[for="rcpt-user"]');
    const row = sender.page.locator(`input[name="to"][value="user:${fileUser}"]`);
    check("the list of the popup has the account of the file", (await row.count()) === 1, "no row");
    check("the account of the file with a phone is enabled", (await row.isDisabled()) === false, "disabled");
    const none = sender.page.locator(`input[name="to"][value="user:${fileNoPhone}"]`);
    check("the account of the file with no phone is listed and disabled", (await none.count()) === 1 && (await none.isDisabled()), "not disabled");
    await sender.page.fill("#send-find", fileUser);
    check("the search finds the account of the file", await row.locator("xpath=..").isVisible(), "hidden");
    await row.locator("xpath=..").click();
    await sender.page.fill("#send-user-form-title", title);
    await Promise.all([sender.page.waitForNavigation(), sender.page.click(".rcptForm--user button[type=submit]")]);
    check("the popup shows no refusal", (await sender.page.locator("[data-send-error]").count()) === 0, "refused");
    check("the list Sent shows the alert for the account of the file", (await sender.page.locator(`tr[data-alert][data-to="user:${fileUser}"]`).count()) >= 1, "no row");
    await shot(sender.page, "fileaccount-sent");
    await target.page.waitForSelector("[data-toast-title]", { timeout: 20000 });
    const toast = await target.page.locator("[data-toast-polite]").innerText();
    check("the account of the file gets the toast", toast.includes(title), toast);
    const badge = await target.page.locator("[data-notify-n]").first().innerText();
    check("the badge of the account of the file counts the new alert", parseInt(badge, 10) === unreadBefore + 1, `${unreadBefore} -> ${badge}`);
    await shot(target.page, "fileaccount-toast");
    await sender.context.close();
    await target.context.close();
  }

  await browser.close();
  console.log(failures === 0 ? "ALL OK" : failures + " FAILED");
  process.exit(failures === 0 ? 0 : 1);
})().catch((e) => {
  console.error(e);
  process.exit(2);
});
