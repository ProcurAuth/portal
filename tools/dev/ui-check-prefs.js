// Checks the page "Inbox > Preferences" in a real browser. Run it with tools/dev/ui-check.sh and
// UI_CHECK_SCRIPT=ui-check-prefs.js. The checks WRITE: they save the preferences of the owner. Use a server that the test
// `prefs_demo_for_the_eyes` of tests/prefs_e2e.rs starts (it writes demo.json with the values below). Never use a shared demo.
//
// For each of the widths 390 and 1440, and for the dark and the light theme, the page must load, have no sideways scroll,
// and show the form of the mock: the summary with its three choices, the quiet hours, the default of the file and the
// buttons. The form must work with JavaScript off.
//
// Environment:
//   UI_CHECK_BASE        base URL of the server, for example http://127.0.0.1:41000
//   UI_CHECK_USER        the owner (default dana)
//   UI_CHECK_USER_PASSWORD  the password (from demo.json)
//   UI_CHECK_SHOTS       a directory for the screenshots (optional)
const { chromium } = require("playwright");

const BASE = process.env.UI_CHECK_BASE;
const OWNER = process.env.UI_CHECK_USER || "dana";
const PW = process.env.UI_CHECK_USER_PASSWORD;
const SHOTS = process.env.UI_CHECK_SHOTS || "";
const INBOX = "/auth/settings/inbox";
const PREFS = INBOX + "/prefs";
let failures = 0;

function check(name, ok, detail) {
  console.log((ok ? "ok    " : "FAIL  ") + name + (ok ? "" : " -> " + detail));
  if (!ok) failures += 1;
}

async function signIn(browser, user, password) {
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
  const page = await context.newPage();
  await page.goto(BASE + "/auth/login");
  await page.fill("input[name=login]", user);
  await page.fill("input[name=password]", password);
  await page.waitForTimeout(1300);
  await Promise.all([page.waitForNavigation(), page.click("button[type=submit]")]);
  if (page.url().includes("/auth/login")) throw new Error("the sign-in of " + user + " failed");
  const state = await context.storageState();
  await context.close();
  return state;
}

async function shot(page, name) {
  if (SHOTS) await page.screenshot({ path: SHOTS + "/" + name + ".png", fullPage: true });
}

const noSideways = (page) =>
  page.evaluate(() => {
    const main = document.querySelector("main");
    return [document.documentElement.scrollWidth - document.documentElement.clientWidth, main ? main.scrollWidth - main.clientWidth : 0];
  });

const box = async (page, selector) => (await page.locator(selector).first().boundingBox()) || { x: -1, y: -1, width: 0, height: 0 };

async function main() {
  const browser = await chromium.launch();
  const owner = await signIn(browser, OWNER, PW);

  // ---- the page at each size and in each theme, with JavaScript off
  for (const scheme of ["dark", "light"]) {
    for (const width of [390, 1440]) {
      const tag = " (" + width + ", " + scheme + ")";
      const context = await browser.newContext({ storageState: owner, viewport: { width, height: 900 }, colorScheme: scheme, javaScriptEnabled: false });
      const page = await context.newPage();
      // The link is in the bar of the page Inbox, next to History.
      await page.goto(BASE + INBOX);
      check("the link Preferences is on the page Inbox" + tag, (await page.locator("a.button[href='" + PREFS + "']").count()) === 1, "");
      const response = await page.goto(BASE + PREFS);
      check("the page loads" + tag, response.status() === 200, response.status());
      check("the page has Cache-Control no-store" + tag, (response.headers()["cache-control"] || "").includes("no-store"), response.headers()["cache-control"]);
      const [doc, mainWidth] = await noSideways(page);
      check("no sideways scroll" + tag, doc <= 0 && mainWidth <= 0, JSON.stringify([doc, mainWidth]));
      check("the three choices of the summary" + tag, (await page.locator("input[name=digest]").count()) === 3, "");
      check("the file default is daily 09:00 and is checked" + tag, await page.locator("input[name=digest][value=daily]").isChecked(), "");
      check("the zone list holds the whole database" + tag, (await page.locator("select[name=tz] option").count()) > 400, await page.locator("select[name=tz] option").count());
      check("the zone of the file is selected" + tag, (await page.locator("select[name=tz]").inputValue()) === "Europe/Paris", "");
      check("the quiet hours have two times and the ring box" + tag, (await page.locator("input[name=from], input[name=to]").count()) === 2 && (await page.locator("input[name=high]").count()) === 1, "");
      check("the default of the file is shown" + tag, (await page.content()).includes("daily at 09:00, time zone Europe/Paris"), "");
      const name = await page.evaluate(() => document.body.innerText.toLowerCase());
      check("no product name on the page" + tag, !/inbox|inbox|circular/.test(name), "");
      // Two columns on a wide screen, one column on a narrow one.
      const label = await box(page, ".form__row .lbl");
      const control = await box(page, ".form__row .stack");
      if (width >= 1000) check("label and control side by side" + tag, control.x > label.x + 100 && Math.abs(control.y - label.y) < 20, JSON.stringify([label, control]));
      else check("label above control" + tag, control.y > label.y + label.height - 1 && Math.abs(control.x - label.x) < 8, JSON.stringify([label, control]));
      // Every control stays inside the viewport.
      const outside = await page.evaluate((w) => [...document.querySelectorAll("main input, main select, main button, main a.button")].filter((e) => e.getBoundingClientRect().right > w + 0.5).length, width);
      check("every control is inside the screen" + tag, outside === 0, outside);
      const controls = await page.evaluate(() => [...document.querySelectorAll(".form input[type=time], .form select, .form input[type=number]")].map((e) => Math.round(e.getBoundingClientRect().height)));
      check("the controls have the height of the others (28 px)" + tag, controls.every((h) => h >= 26 && h <= 32), JSON.stringify(controls));
      await shot(page, "prefs." + width + "." + scheme);
      await context.close();
    }
  }

  // ---- the form, with JavaScript off, at 390
  {
    const context = await browser.newContext({ storageState: owner, viewport: { width: 390, height: 900 }, javaScriptEnabled: false });
    const page = await context.newPage();
    await page.goto(BASE + PREFS);
    check("an account with no value has no reset button", (await page.locator("button[name=reset]").count()) === 0, "");
    await page.check("input[name=digest][value=every]");
    await page.fill("input[name=hours]", "6");
    await page.selectOption("select[name=tz]", "Asia/Tokyo");
    await page.fill("input[name=from]", "22:00");
    await page.fill("input[name=to]", "07:00");
    await page.check("input[name=high]");
    await Promise.all([page.waitForNavigation(), page.click("button.button--primary")]);
    check("the save gives the notice", page.url().includes("s=saved") && (await page.content()).includes("Your preferences are saved"), page.url());
    check("the values are back in the form", (await page.locator("input[name=digest][value=every]").isChecked()) && (await page.locator("input[name=hours]").inputValue()) === "6" && (await page.locator("select[name=tz]").inputValue()) === "Asia/Tokyo" && (await page.locator("input[name=from]").inputValue()) === "22:00" && (await page.locator("input[name=high]").isChecked()), "");
    check("the reset button shows now", (await page.locator("button[name=reset]").count()) === 1, "");
    await shot(page, "prefs-saved.390.dark");

    // A value that the server refuses: the page answers 422 and keeps what was typed. The browser does not send it, so the
    // request is made by hand, with the token of the page.
    const csrf = await page.locator("form.form input[name=csrf]").inputValue();
    const bad = await context.request.post(BASE + PREFS, { form: { csrf, digest: "daily", at: "09:00", tz: "Mars/Base" }, headers: { Origin: BASE } });
    const text = await bad.text();
    check("a bad zone name gets 422 and a message", bad.status() === 422 && text.includes("Nothing is saved") && text.includes("Mars/Base"), bad.status());
    await page.goto(BASE + PREFS);
    check("the refused value changed nothing", (await page.locator("select[name=tz]").inputValue()) === "Asia/Tokyo" && (await page.locator("input[name=hours]").inputValue()) === "6", "");

    // Back to the default of the file.
    await Promise.all([page.waitForNavigation(), page.click("button[name=reset]")]);
    check("the reset gives the notice and the defaults of the file", page.url().includes("s=reset") && (await page.locator("input[name=digest][value=daily]").isChecked()) && (await page.locator("select[name=tz]").inputValue()) === "Europe/Paris" && (await page.locator("input[name=from]").inputValue()) === "", page.url());
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
