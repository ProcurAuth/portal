// Checks the web pages of the mode `enrolled` in a real browser: the form "Add agent", the page of the token, the button "Reset
// token" and the line "Also asked by". Run it with tools/dev/ui-check.sh and UI_CHECK_SCRIPT=ui-check-agents-enrolled.js.
// The checks WRITE: they add agents and reset tokens. Use a server that the test `agents_enrolled_demo_for_the_eyes` of
// tests/agents_web_e2e.rs starts (it writes demo.json with the values below). Never run it against a shared demo.
//
// For each of the widths 390 and 1440, and for the dark and the light theme, each page must load, have no sideways scroll and
// carry `Cache-Control: no-store`. The forms must work with JavaScript off. With JavaScript on, the copy button must work.
//
// Environment:
//   UI_CHECK_BASE        base URL of the server, for example http://127.0.0.1:41000
//   UI_CHECK_USER        the owner of the agents (default dana)
//   UI_CHECK_USER_PASSWORD  the password (from demo.json)
//   UI_CHECK_IDS         the JSON object `ids` of demo.json
//   UI_CHECK_SHOTS       a directory for the screenshots (optional)
const { chromium } = require("playwright");

const BASE = process.env.UI_CHECK_BASE;
const OWNER = process.env.UI_CHECK_USER || "dana";
const PW = process.env.UI_CHECK_USER_PASSWORD;
const IDS = JSON.parse(process.env.UI_CHECK_IDS || "{}");
const SHOTS = process.env.UI_CHECK_SHOTS || "";
const INBOX = "/auth/settings/inbox";
const AGENTS = INBOX + "/agents";
let failures = 0;

function check(name, ok, detail) {
  console.log((ok ? "ok    " : "FAIL  ") + name + (ok ? "" : " -> " + detail));
  if (!ok) failures += 1;
}

async function signIn(browser) {
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
  const page = await context.newPage();
  await page.goto(BASE + "/auth/login");
  await page.fill("input[name=login]", OWNER);
  await page.fill("input[name=password]", PW);
  await page.waitForTimeout(1300);
  await Promise.all([page.waitForNavigation(), page.click("button[type=submit]")]);
  if (page.url().includes("/auth/login")) throw new Error("the sign-in failed");
  const state = await context.storageState();
  await context.close();
  return state;
}

async function open(browser, state, opts) {
  const context = await browser.newContext({ storageState: state, ...opts });
  return { context, page: await context.newPage() };
}

// The page scrolls inside `main`: a tall window shows all of it in one picture.
async function shot(page, name) {
  if (!SHOTS) return;
  const size = page.viewportSize();
  await page.setViewportSize({ width: size.width, height: 1800 });
  await page.screenshot({ path: SHOTS + "/" + name + ".png", fullPage: true });
  await page.setViewportSize(size);
}

const noSideways = (page) =>
  page.evaluate(() => {
    const main = document.querySelector("main");
    return [document.documentElement.scrollWidth - document.documentElement.clientWidth, main ? main.scrollWidth - main.clientWidth : 0];
  });

async function basics(page, response, name, tag) {
  check(name + " loads" + tag, response.status() === 200, response.status());
  const [doc, mainWidth] = await noSideways(page);
  check(name + " has no sideways scroll" + tag, doc <= 0 && mainWidth <= 0, JSON.stringify([doc, mainWidth]));
  check(name + " has Cache-Control no-store" + tag, (response.headers()["cache-control"] || "").includes("no-store"), response.headers()["cache-control"]);
  const text = (await page.locator("main").innerText()).toLowerCase();
  check(name + " does not name the product" + tag, !text.includes("portal"), "");
}

async function main() {
  const browser = await chromium.launch();
  const owner = await signIn(browser);
  let tokens = [];
  for (const scheme of ["dark", "light"]) {
    for (const width of [390, 1440]) {
      const tag = " (" + width + ", " + scheme + ")";
      const { context, page } = await open(browser, owner, { viewport: { width, height: 900 }, colorScheme: scheme, javaScriptEnabled: false });
      let response = await page.goto(BASE + AGENTS);
      await basics(page, response, "list", tag);
      check("the list has the form Add agent" + tag, (await page.locator("form[action$='/agents/enroll']").count()) === 1, "");
      await shot(page, "list." + width + "." + scheme);
      response = await page.goto(BASE + AGENTS + "/" + IDS.night);
      await basics(page, response, "agent page", tag);
      check("the agent page has Reset token and the warning" + tag, (await page.locator("form[action$='/agents/reset'] button").count()) === 1 && (await page.content()).includes("The old token stops at once."), "");
      await shot(page, "agent." + width + "." + scheme);
      response = await page.goto(BASE + INBOX + "/items/" + IDS.request);
      await basics(page, response, "request page", tag);
      check("the request page has the line Also asked by" + tag, (await page.locator(".sender__also").innerText()).includes("Report writer"), "");
      await shot(page, "request." + width + "." + scheme);

      // Add an agent with JavaScript off. The answer is the page of the token.
      await page.goto(BASE + AGENTS);
      await page.fill("form[action$='/agents/enroll'] input[name=name]", "Eye " + width + " " + scheme);
      [response] = await Promise.all([page.waitForNavigation(), page.click("form[action$='/agents/enroll'] button")]);
      await basics(page, response, "token page", tag);
      const token = await page.inputValue("#agent-token");
      tokens.push(token);
      check("the token page shows a token and the header line" + tag, /^agt_[A-Za-z0-9_-]{43}$/.test(token) && (await page.inputValue("#agent-header")) === "Agent-Token: " + token, token);
      check("the token page says one time" + tag, (await page.content()).includes("shows it one time"), "");
      await shot(page, "token." + width + "." + scheme);
      // The token is on no other page.
      await page.goto(BASE + AGENTS);
      check("the token is not on the list" + tag, !(await page.content()).includes(token.slice(4)), "");
      await context.close();
    }
  }

  // Reset with JavaScript off: the old token is gone from the page, the new one shows once.
  {
    const { context, page } = await open(browser, owner, { viewport: { width: 390, height: 900 }, javaScriptEnabled: false });
    await page.goto(BASE + AGENTS + "/" + IDS.night);
    const [response] = await Promise.all([page.waitForNavigation(), page.click("form[action$='/agents/reset'] button")]);
    await basics(page, response, "reset page", " (390, js off)");
    check("the reset page says that the old token stopped", (await page.content()).includes("The old token stopped at once"), "");
    const fresh = await page.inputValue("#agent-token");
    check("the reset page gives a token that is new", /^agt_/.test(fresh) && !tokens.includes(fresh), fresh);
    await shot(page, "reset.390.dark");
    await context.close();
  }

  // With JavaScript on, the copy button copies the token and shows the check.
  {
    const { context, page } = await open(browser, owner, { viewport: { width: 1440, height: 900 }, javaScriptEnabled: true, permissions: ["clipboard-read", "clipboard-write"] });
    await page.goto(BASE + AGENTS);
    await page.fill("form[action$='/agents/enroll'] input[name=name]", "Eye copy");
    await Promise.all([page.waitForNavigation(), page.click("form[action$='/agents/enroll'] button")]);
    const token = await page.inputValue("#agent-token");
    await page.click("button[data-copy=agent-token]");
    await page.waitForTimeout(300);
    const clip = await page.evaluate(() => navigator.clipboard.readText());
    check("the copy button copies the token", clip === token, clip);
    check("the copy button shows the check", (await page.locator("button[data-copy=agent-token].is-copied").count()) === 1, "");
    await page.click("button[data-copy=agent-header]");
    await page.waitForTimeout(300);
    check("the second copy button copies the header line", (await page.evaluate(() => navigator.clipboard.readText())) === "Agent-Token: " + token, "");
    await shot(page, "token-copy.1440.dark");
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
