// Checks the box "Keep me signed in" of the sign-in page in a real browser: the box, its label, the
// keyboard, the dark and the light scheme, the widths 1280 and 390, with and without script, and the
// attributes of the session cookie that the browser keeps after a real sign-in. Run it with
// tools/dev/ui-check.sh and UI_CHECK_SCRIPT=ui-check-keep.js. The check signs in many times: use a
// portal that is yours, never a shared demo. It changes nothing else.
//
// The portal needs the default values of `session.ttl` (24h) and `session.remember_ttl` (30d), or the
// values that UI_CHECK_TTL and UI_CHECK_REMEMBER_TTL give in seconds.
//
// Environment:
//   UI_CHECK_BASE          base URL of the portal (default http://127.0.0.1:18791)
//   UI_CHECK_LOGIN         login of the superadmin (default admin)
//   UI_CHECK_PASSWORD      its password
//   UI_CHECK_REMEMBER_TTL  the life of a remembered session, in seconds (default 2592000)
//   UI_CHECK_DAYS          the number in the label (default 30)
//   UI_CHECK_SHOTS         a directory for the screenshots (optional)
const { chromium } = require("playwright");

const BASE = process.env.UI_CHECK_BASE || "http://127.0.0.1:18791";
const LOGIN = process.env.UI_CHECK_LOGIN || "admin";
const PASSWORD = process.env.UI_CHECK_PASSWORD || "";
const REMEMBER = parseInt(process.env.UI_CHECK_REMEMBER_TTL || "2592000", 10);
const DAYS = process.env.UI_CHECK_DAYS || "30";
const SHOTS = process.env.UI_CHECK_SHOTS || "";
let failures = 0;

function check(name, ok, detail) {
  console.log((ok ? "ok    " : "FAIL  ") + name + (ok ? "" : " -> " + detail));
  if (!ok) failures += 1;
}

async function shot(page, name) {
  if (SHOTS) await page.screenshot({ path: SHOTS + "/" + name + ".png" });
}

async function open(browser, scheme, width, script) {
  const mobile = width < 600;
  const context = await browser.newContext({
    colorScheme: scheme,
    viewport: { width, height: 800 },
    javaScriptEnabled: script,
    isMobile: mobile,
    hasTouch: mobile,
  });
  const page = await context.newPage();
  await page.goto(BASE + "/auth/login");
  return { context, page };
}

// The state of the box and of its label, as the browser draws them.
async function state(page) {
  return page.evaluate(() => {
    const box = document.querySelector("input[name=keep]");
    if (!box) return null;
    const label = box.closest("label");
    const b = box.getBoundingClientRect();
    const l = label.getBoundingClientRect();
    const button = document.querySelector("button[type=submit]").getBoundingClientRect();
    return {
      checked: box.checked,
      text: label.textContent.trim(),
      box: [Math.round(b.width), Math.round(b.height)],
      label: [Math.round(l.width), Math.round(l.height)],
      buttonTop: Math.round(button.top),
      overflow: document.documentElement.scrollWidth > window.innerWidth,
      inlineStyle: document.querySelectorAll("[style]").length,
      ids: Array.from(document.querySelectorAll("input[name=keep], label")).filter((e) => e.id).length,
      color: getComputedStyle(label).color,
    };
  });
}

async function submit(page, user, password, keepBox) {
  await page.fill("input[name=login]", user);
  await page.fill("input[name=password]", password);
  if (keepBox) await page.check("input[name=keep]");
  await page.waitForTimeout(1300);
  await Promise.all([page.waitForLoadState("load"), page.click("button[type=submit]")]);
  await page.waitForLoadState("load");
}

async function sessionCookie(context) {
  const cookies = await context.cookies();
  return cookies.find((c) => c.name === "portal" || c.name === "__Host-portal");
}

(async () => {
  const browser = await chromium.launch();

  // 1. The box in the two schemes, the two widths, with and without script.
  for (const scheme of ["dark", "light"]) {
    for (const width of [1280, 390]) {
      for (const script of [true, false]) {
        const tag = `${scheme} ${width} ${script ? "script" : "noscript"}`;
        const { context, page } = await open(browser, scheme, width, script);
        const before = await state(page);
        check(`the box is there (${tag})`, before !== null, "no box");
        if (!before) {
          await context.close();
          continue;
        }
        check(`the box is not ticked (${tag})`, before.checked === false, "ticked");
        check(`the label has the number of days (${tag})`, before.text === `Keep me signed in for ${DAYS} days`, before.text);
        check(`no inline style and no identifier (${tag})`, before.inlineStyle === 0 && before.ids === 0, JSON.stringify(before));
        check(`no horizontal overflow (${tag})`, before.overflow === false, "overflow");
        const wanted = width < 600 ? 20 : 16;
        check(`the box has its size (${tag})`, before.box[0] === wanted && before.box[1] === wanted, before.box.join("x"));
        if (width < 600) check(`the label reaches 44 px on a touch screen (${tag})`, before.label[1] >= 44, String(before.label[1]));
        await shot(page, `keep-${scheme}-${width}-${script ? "js" : "nojs"}-off`);
        // A click on the words ticks the box.
        await page.locator("label", { has: page.locator("input[name=keep]") }).click({ position: { x: 60, y: 8 } });
        const ticked = await state(page);
        check(`a click on the label ticks the box (${tag})`, ticked.checked === true, "not ticked");
        check(`the page does not move when the box is ticked (${tag})`, ticked.buttonTop === before.buttonTop && ticked.label[1] === before.label[1], `${before.buttonTop} -> ${ticked.buttonTop}`);
        await shot(page, `keep-${scheme}-${width}-${script ? "js" : "nojs"}-on`);
        await page.locator("label", { has: page.locator("input[name=keep]") }).click({ position: { x: 60, y: 8 } });
        check(`a second click clears the box (${tag})`, (await state(page)).checked === false, "ticked");
        await context.close();
      }
    }
  }

  // 2. The keyboard: Tab from the password reaches the box (after the button that shows the password, with script), Space ticks it, Tab reaches the button.
  for (const script of [true, false]) {
    const { context, page } = await open(browser, "dark", 1280, script);
    const tag = script ? "script" : "noscript";
    await page.focus("input[name=password]");
    // With script, the button that shows the password stands between the password and the box.
    if (script) await page.keyboard.press("Tab");
    await page.keyboard.press("Tab");
    check(`Tab from the password reaches the box (${tag})`, await page.evaluate(() => document.activeElement.name === "keep"), "not focused");
    await shot(page, `keep-focus-${tag}`);
    await page.keyboard.press("Space");
    check(`Space ticks the box (${tag})`, (await state(page)).checked === true, "not ticked");
    await page.keyboard.press("Space");
    check(`Space clears the box (${tag})`, (await state(page)).checked === false, "ticked");
    await page.keyboard.press("Tab");
    check(`Tab leaves the box for the button (${tag})`, await page.evaluate(() => document.activeElement.type === "submit"), "not on the button");
    // The accessible name of the box is the text of its label.
    const name = await page.locator("input[name=keep]").evaluate((e) => e.labels[0].textContent.trim());
    check(`the box has the name of its label (${tag})`, name === `Keep me signed in for ${DAYS} days`, name);
    await context.close();
  }

  // 3. A refused sign-in with the box ticked: the page comes back with the box clear.
  {
    const { context, page } = await open(browser, "dark", 1280, true);
    await submit(page, LOGIN, "not the password", true);
    const after = await state(page);
    check("a refused sign-in shows the refusal", (await page.content()).includes("Invalid login or password"), "no refusal");
    check("the page after a refusal has the box clear", after !== null && after.checked === false, JSON.stringify(after));
    check("a refused sign-in sets no session cookie", (await sessionCookie(context)) === undefined, "cookie");
    await context.close();
  }

  // 4. A real sign-in without the box: a session cookie. With script and without script.
  for (const script of [true, false]) {
    const { context, page } = await open(browser, "dark", 1280, script);
    await submit(page, LOGIN, PASSWORD, false);
    const cookie = await sessionCookie(context);
    check(`a sign-in without the box gives a cookie (${script})`, cookie !== undefined, "no cookie");
    if (cookie) {
      check(`the cookie is a session cookie, no expiry (${script})`, cookie.expires === -1, String(cookie.expires));
      check(`the cookie is HttpOnly and SameSite=Strict (${script})`, cookie.httpOnly === true && cookie.sameSite === "Strict", JSON.stringify(cookie));
    }
    check(`the sign-in works (${script})`, !page.url().includes("/auth/login"), page.url());
    await context.close();
  }

  // 5. A real sign-in with the box: a persistent cookie of the life of the session.
  for (const script of [true, false]) {
    const { context, page } = await open(browser, "dark", 1280, script);
    await submit(page, LOGIN, PASSWORD, true);
    const cookie = await sessionCookie(context);
    check(`a sign-in with the box gives a cookie (${script})`, cookie !== undefined, "no cookie");
    if (cookie) {
      const left = cookie.expires - Date.now() / 1000;
      check(`the cookie expires in about ${DAYS} days (${script})`, Math.abs(left - REMEMBER) < 120, String(left));
      check(`the cookie is HttpOnly and SameSite=Strict (${script})`, cookie.httpOnly === true && cookie.sameSite === "Strict", JSON.stringify(cookie));
    }
    check(`the sign-in works (${script})`, !page.url().includes("/auth/login"), page.url());
    if (script) {
      // The Activity list marks the remembered session. The server writes the history every 15 seconds.
      let marked = false;
      for (let i = 0; i < 22 && !marked; i++) {
        await page.goto(BASE + "/auth/settings/users/" + encodeURIComponent(LOGIN));
        marked = (await page.content()).includes('title="Kept signed in"');
        if (!marked) await page.waitForTimeout(1000);
      }
      check("the Activity list marks the remembered session", marked, "no badge");
    }
    await context.close();
  }

  await browser.close();
  console.log(failures === 0 ? "ALL OK" : failures + " FAILED");
  process.exit(failures === 0 ? 0 : 1);
})().catch((e) => {
  console.error(e);
  process.exit(2);
});
