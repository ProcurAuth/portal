// Checks the theme selector of the launcher in a real browser: the list starts closed, the button opens it, the list
// holds only the two other modes, the keys (arrows, Home, End, Escape, Tab) and a click outside work, and a choice
// changes the mode. The selector has no fixed class name or attribute name (the masking pass changes them), so the
// check finds it by its roles. Run it with tools/dev/ui-check.sh and UI_CHECK_SCRIPT=ui-check-theme.js.
// The check writes nothing to the portal. It only signs in.
//
// Environment:
//   UI_CHECK_BASE      base URL of the portal (default http://localhost:8088)
//   UI_CHECK_LOGIN     login (default admin)
//   UI_CHECK_PASSWORD  its password (default test)
const { chromium } = require("playwright");

const BASE = process.env.UI_CHECK_BASE || "http://localhost:8088";
const LOGIN = process.env.UI_CHECK_LOGIN || "admin";
const PASSWORD = process.env.UI_CHECK_PASSWORD || "test";
let failures = 0;

function check(name, ok, detail) {
  console.log((ok ? "ok    " : "FAIL  ") + name + (ok ? "" : " -> " + detail));
  if (!ok) failures += 1;
}

async function main() {
  const browser = await chromium.launch();
  for (const width of [1280, 390]) {
    const context = await browser.newContext({ viewport: { width, height: 800 } });
    const page = await context.newPage();
    await page.goto(BASE + "/auth/login");
    await page.fill("input[name=login]", LOGIN);
    await page.fill("input[name=password]", PASSWORD);
    await page.waitForTimeout(1300);
    await Promise.all([page.waitForNavigation(), page.click("button[type=submit]")]);
    if (page.url().includes("/auth/login")) throw new Error("the sign-in failed: " + page.url());
    await page.goto(BASE + "/");
    const tag = " (" + width + ")";
    const trigger = page.locator("button[aria-haspopup=listbox]");
    const menu = page.locator("[role=listbox]").first();
    const shown = () => page.locator("[role=listbox] [role=option]:visible");
    const expanded = async () => (await trigger.getAttribute("aria-expanded")) === "true";
    const focusedMode = () => page.evaluate(() => (document.activeElement && document.activeElement.getAttribute("role") === "option" ? document.activeElement.getAttribute("aria-label") : ""));
    check("one theme button" + tag, (await trigger.count()) === 1, await trigger.count());

    check("the list starts closed" + tag, !(await expanded()) && !(await menu.isVisible()), "");
    await trigger.click();
    check("a click opens the list" + tag, (await expanded()) && (await menu.isVisible()), "");
    check("the list holds only the two other modes" + tag, (await shown().count()) === 2, await shown().count());
    const first = await focusedMode();
    check("the focus goes to the first option" + tag, /theme$/i.test(first), first);
    await page.keyboard.press("End");
    const last = await focusedMode();
    await page.keyboard.press("Home");
    const home = await focusedMode();
    check("Home and End go to the first and the last option" + tag, home === first && last !== "" && last !== first, JSON.stringify([home, first, last]));
    await page.keyboard.press("ArrowDown");
    check("ArrowDown goes to the next option" + tag, (await focusedMode()) === last, await focusedMode());
    await page.keyboard.press("ArrowDown");
    check("ArrowDown wraps to the first option" + tag, (await focusedMode()) === first, await focusedMode());
    await page.keyboard.press("Escape");
    check("Escape closes the list and the focus goes back to the button" + tag, !(await expanded()) && (await page.evaluate(() => document.activeElement.getAttribute("aria-haspopup"))) === "listbox", "");

    await page.keyboard.press("ArrowDown");
    check("ArrowDown opens the list from the button" + tag, await expanded(), "");
    await page.keyboard.press("End");
    await page.keyboard.press("Tab");
    check("a Tab out of the list closes it" + tag, !(await expanded()), "");

    await trigger.click();
    // A point that no link, button or field holds: the click goes to the page body.
    const point = await page.evaluate(() => {
      for (let y = 780; y > 300; y -= 20) {
        const el = document.elementFromPoint(window.innerWidth - 4, y);
        if (el && !el.closest("a, button, input, select, textarea, [role=listbox]")) return { x: window.innerWidth - 4, y };
      }
      return null;
    });
    check("a free point exists for the click outside" + tag, !!point, "");
    if (point) await page.mouse.click(point.x, point.y);
    check("a click outside closes the list" + tag, !(await expanded()), "");

    const before = await trigger.getAttribute("aria-label");
    await trigger.click();
    const choice = shown().first();
    const label = await choice.getAttribute("aria-label");
    const name = label.replace(/ theme$/i, "");
    await choice.click();
    const after = await trigger.getAttribute("aria-label");
    await page.reload();
    const kept = await page.locator("button[aria-haspopup=listbox]").getAttribute("aria-label");
    check("the mode stays after a reload" + tag, kept === after, JSON.stringify([after, kept]));
    check("a choice changes the mode and closes the list" + tag, before !== after && !(await expanded()) && new RegExp(name, "i").test(after), JSON.stringify([before, after, name]));
    await trigger.click();
    check("the list opens again with the two modes" + tag, (await expanded()) && (await shown().count()) === 2, "");
    const hiddenNow = await page.locator("[role=listbox] [role=option]:visible[aria-label=\"" + label + "\"]").count();
    check("the chosen mode leaves the list" + tag, hiddenNow === 0, hiddenNow);
    await context.close();
  }
  await browser.close();
  console.log(failures === 0 ? "all checks passed" : failures + " checks failed");
  process.exit(failures === 0 ? 0 : 1);
}

main().catch((e) => {
  console.error(e);
  process.exit(2);
});
