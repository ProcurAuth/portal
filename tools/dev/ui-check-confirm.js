// Checks the approval controls of the grant editor and the key form in a real browser: the lock
// button on a ticked permission of a service (every action has one, but `access`), the lock that the owner side
// imposes, the chip `confirm` of the list of the keys, and the choice of the interaction of a key.
// Run it with: UI_CHECK_SCRIPT=ui-check-confirm.js tools/dev/ui-check.sh
//
// The check WRITES to the portal (it saves a grant of the account `alice` and makes a key). Run it
// against a portal that is yours, never against the shared demo. The portal needs the service
// ClickHouse Dash of the demo, and the account alice with: access, query.run.read and query.run.write on
// ClickHouse Dash, and the rights to read, edit, make and use the MCP keys and to read the keys.
//
// Environment:
//   UI_CHECK_BASE            base URL of the portal (default http://localhost:8088)
//   UI_CHECK_LOGIN           login of the superadmin (default admin)
//   UI_CHECK_PASSWORD        password of the superadmin (default test)
//   UI_CHECK_USER            login of the account alice (default alice)
//   UI_CHECK_USER_PASSWORD   its password (mandatory)
//   UI_CHECK_SHOTS           a directory for the screenshots (optional)

const { chromium } = require("playwright");

const BASE = process.env.UI_CHECK_BASE || "http://localhost:8088";
const SHOTS = process.env.UI_CHECK_SHOTS || "";
const ADMIN = process.env.UI_CHECK_LOGIN || "admin";
const ADMIN_PASSWORD = process.env.UI_CHECK_PASSWORD || "test";
const USER = process.env.UI_CHECK_USER || "alice";
const USER_PASSWORD = process.env.UI_CHECK_USER_PASSWORD || "";
const CH = "ClickHouse Dash";
// A name that no earlier run used, so that a key left by a failed run does not hide the key of this run.
const KEY_NAME = "confirm-check-" + Date.now().toString(36);

let failures = 0;
function check(name, ok, detail) {
  console.log((ok ? "ok    " : "FAIL  ") + name + (ok ? "" : " -> " + detail));
  if (!ok) failures += 1;
}

async function signIn(browser, login, password, viewport, scheme) {
  const context = await browser.newContext({ viewport, colorScheme: scheme });
  const page = await context.newPage();
  await page.goto(BASE + "/auth/login");
  await page.fill("input[name=login]", login);
  await page.fill("input[name=password]", password);
  // The sign-in form has a minimum time between its display and its submit.
  await page.waitForTimeout(1200);
  await page.click("button[type=submit]");
  await page.waitForLoadState("load");
  return { context, page };
}

async function shot(page, name) {
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/${name}.png` });
}

function row(page, name) {
  return page.locator(".ge__svc", { has: page.locator(`.ge__svcName:text-is("${name}")`) });
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

function actionRow(page, action) {
  return page.locator(".ge__svc.is-expanded .ge__act", { has: page.locator(`input[data-action="${action}"]`) });
}

function lock(page, action) {
  return actionRow(page, action).locator(".ge__confirm");
}

const sideways = (page) =>
  page.evaluate(() => {
    const main = document.querySelector("main");
    return Math.max(document.documentElement.scrollWidth - window.innerWidth, main ? main.scrollWidth - main.clientWidth : 0);
  });

// The names of the hidden fields that the editor wrote, with their values.
const fields = (page) =>
  page.evaluate(() =>
    Array.from(document.querySelectorAll(".grantEditor__fields input[type=hidden]")).map((i) => i.name + "=" + i.value)
  );

// The state of the lock of a row, or null when the row has none.
async function lockState(page, action) {
  const button = lock(page, action);
  if ((await button.count()) === 0) return null;
  return {
    pressed: await button.getAttribute("aria-pressed"),
    disabled: await button.isDisabled(),
    title: await button.getAttribute("title"),
    label: await button.getAttribute("aria-label"),
    icon: await button.locator("use").getAttribute("href"),
    width: Math.round((await button.boundingBox()).width),
  };
}

// Phase A: the superadmin sets the approval on the grant of alice and saves it.
async function setApproval(browser) {
  const { context, page } = await signIn(browser, ADMIN, ADMIN_PASSWORD, { width: 1440, height: 900 }, "dark");
  await page.goto(`${BASE}/auth/settings/users/${USER}`);
  await page.waitForSelector(".ge__svc");
  await expand(page, CH);
  await node(page, "");
  await tick(page, "query.run.read");
  await tick(page, "query.run.write");
  const readLock = await lockState(page, "query.run.read");
  check("account page: every action has a lock, and it starts open", readLock && readLock.pressed === "false", JSON.stringify(readLock));
  let s = await lockState(page, "query.run.write");
  check("account page: a ticked action has a lock", s !== null, "no lock");
  check("account page: the lock starts open", s && s.pressed === "false" && s.icon === "#i-lock-open", JSON.stringify(s));
  check("account page: the tooltip says that the lock asks for the keys of the account", s && /^Asks for an approval when a key of this account uses the action\. The account itself never asks\./.test(s.title || "") && /approval_cannot_wait/.test(s.title || ""), JSON.stringify(s));
  check("account page: the lock is a flat icon button of 24 px", s && s.width === 24, JSON.stringify(s));
  // An action that is not ticked has no lock.
  await tick(page, "query.run.write", false);
  check("account page: an action that is not ticked has no lock", (await lockState(page, "query.run.write")) === null, "lock on an unticked row");
  await tick(page, "query.run.write");
  await shot(page, "1a-account-lock-open-1440-dark");
  await lock(page, "query.run.write").click();
  s = await lockState(page, "query.run.write");
  check("account page: a click closes the lock", s && s.pressed === "true" && s.icon === "#i-lock", JSON.stringify(s));
  const sent = await fields(page);
  const at = sent.indexOf("grant" + sent.findIndex((f) => f.startsWith("grant") && f.includes("query.run.write")) + "=query.run.write on service:data-clickhouse-dash");
  check("account page: the form sends confirm for this grant only", sent.filter((f) => f.startsWith("confirm")).length === 1, JSON.stringify(sent));
  void at;
  await shot(page, "1-account-lock-closed-1440-dark");
  await page.locator("[data-save]").first().click();
  await page.waitForLoadState("load");
  await page.goto(`${BASE}/auth/settings/users/${USER}`);
  await page.waitForSelector(".ge__svc");
  await expand(page, CH);
  await node(page, "");
  s = await lockState(page, "query.run.write");
  check("account page: the lock stays closed after Save", s && s.pressed === "true", JSON.stringify(s));
  const readAfter = await lockState(page, "query.run.read");
  check("account page: the other action keeps its open lock", readAfter && readAfter.pressed === "false", JSON.stringify(readAfter));
  await context.close();
}

// The rows of the account page with the lock closed, in one size and one theme.
async function accountShots(browser, width, height, scheme) {
  const label = `${width}-${scheme}`;
  const { context, page } = await signIn(browser, ADMIN, ADMIN_PASSWORD, { width, height }, scheme);
  await page.goto(`${BASE}/auth/settings/users/${USER}`);
  await page.waitForSelector(".ge__svc");
  await expand(page, CH);
  await node(page, "");
  const s = await lockState(page, "query.run.write");
  check(`${label} account page: the saved lock is closed`, s && s.pressed === "true", JSON.stringify(s));
  await actionRow(page, "query.run.write").scrollIntoViewIfNeeded();
  check(`${label} account page: the page does not scroll sideways`, (await sideways(page)) <= 0, String(await sideways(page)));
  await shot(page, `1b-account-rows-${label}`);
  await context.close();
}

// Phase B: alice makes a key. The owner side imposes the approval on the key.
async function keyForm(browser, width, height, scheme) {
  const label = `${width}-${scheme}`;
  const { context, page } = await signIn(browser, USER, USER_PASSWORD, { width, height }, scheme);
  await page.goto(BASE + "/auth/keys/new?kind=mcp");
  await page.waitForSelector(".ge__svc");
  await expand(page, CH);
  await node(page, "");
  await tick(page, "query.run.read");
  await tick(page, "query.run.write");
  const read = await lockState(page, "query.run.read");
  const write = await lockState(page, "query.run.write");
  check(`${label} key form: an action that the owner does not ask for has an open lock`, read && read.pressed === "false" && !read.disabled, JSON.stringify(read));
  check(
    `${label} key form: the lock is closed and locked when the owner asks`,
    write && write.pressed === "true" && write.disabled && write.icon === "#i-lock",
    JSON.stringify(write)
  );
  check(`${label} key form: the tooltip names the owner`, write && /Your own grant asks/.test(write.title || ""), JSON.stringify(write));
  // A click on the locked lock changes nothing.
  await lock(page, "query.run.write").click({ force: true }).catch(() => {});
  const again = await lockState(page, "query.run.write");
  check(`${label} key form: a click on the locked lock changes nothing`, again && again.pressed === "true", JSON.stringify(again));
  // The same grant on a resource below: with the whole service ticked, the row says "via the whole service" and
  // has no lock of its own. With the whole service unticked, the grant on the resource has the locked lock,
  // because the owner holds the approval on the whole service, which covers the resource.
  await node(page, "host:ch-eu1");
  const covered = await lockState(page, "query.run.write");
  check(`${label} key form: a node below a ticked grant has no lock of its own`, covered === null, JSON.stringify(covered));
  await node(page, "");
  await tick(page, "query.run.write", false);
  await node(page, "host:ch-eu1");
  await tick(page, "query.run.write");
  const below = await lockState(page, "query.run.write");
  check(`${label} key form: a node below the owner grant is locked closed`, below && below.pressed === "true" && below.disabled, JSON.stringify(below));
  // Back to the whole service, so that the key made below holds the grant that the owner imposes.
  await tick(page, "query.run.write", false);
  await node(page, "");
  await tick(page, "query.run.write");
  // The choice of the interaction: the default of an MCP key is conversational.
  const radios = await page.locator('input[name="interaction"]').evaluateAll((list) => list.map((r) => r.value + ":" + r.checked));
  check(`${label} key form: interaction defaults to conversational`, radios.join() === "conversational:true,one_shot:false", radios.join());
  await page.fill("#key-name", `${KEY_NAME}-${label}`);
  await shot(page, `2-key-form-${label}`);
  await actionRow(page, "query.run.write").scrollIntoViewIfNeeded();
  await shot(page, `2b-key-form-rows-${label}`);
  check(`${label} key form: the page does not scroll sideways`, (await sideways(page)) <= 0, String(await sideways(page)));
  // Choose one_shot with the keyboard on the radio group, then make the key.
  await page.locator('label.choiceGroup__option', { hasText: "One shot" }).click();
  await page.waitForTimeout(100);
  await shot(page, `3-key-form-one-shot-${label}`);
  await page.locator('button[type="submit"][form="key-form"]').click();
  await page.waitForLoadState("load");
  const made = await page.locator("#key-text").inputValue();
  check(`${label} key made: the page shows the key text`, /^mcp_/.test(made), made.slice(0, 8));
  await shot(page, `4-key-made-${label}`);
  // The list of the keys shows the chip and the interaction.
  await page.goto(BASE + "/auth/settings");
  await page.waitForLoadState("load");
  const rowOfKey = page.locator("tr", { hasText: `${KEY_NAME}-${label}` });
  check(`${label} key list: the key shows the chip confirm`, (await rowOfKey.locator(".badge--confirm").count()) === 1, await rowOfKey.innerText());
  const chip = rowOfKey.locator(".badge--confirm");
  check(`${label} key list: the chip has a tooltip`, /ask for an approval|asks for an approval/.test((await chip.getAttribute("title")) || ""), String(await chip.getAttribute("title")));
  check(`${label} key list: the page does not scroll sideways`, (await sideways(page)) <= 0, String(await sideways(page)));
  await shot(page, `5-key-list-${label}`);
  // The page of the change shows the value that was chosen.
  await rowOfKey.locator('a[aria-label^="Edit"]').click();
  await page.waitForSelector('input[name="interaction"]');
  const edited = await page.locator('input[name="interaction"]').evaluateAll((list) => list.map((r) => r.value + ":" + r.checked));
  check(`${label} key edit: the chosen interaction shows`, edited.join() === "conversational:false,one_shot:true", edited.join());
  await page.waitForSelector(".ge__svc");
  await expand(page, CH);
  await node(page, "");
  const editLock = await lockState(page, "query.run.write");
  check(`${label} key edit: the lock is closed and locked`, editLock && editLock.pressed === "true" && editLock.disabled, JSON.stringify(editLock));
  await actionRow(page, "query.run.write").scrollIntoViewIfNeeded();
  await shot(page, `6-key-edit-${label}`);
  // Revoke the key so that the next run starts clean.
  await page.locator('summary[aria-label^="Revoke"]').first().click();
  await page.locator('.ask__panel button[type="submit"]').first().click();
  await page.waitForLoadState("load");
  await context.close();
}

(async () => {
  if (!USER_PASSWORD) {
    console.error("UI_CHECK_USER_PASSWORD is mandatory");
    process.exit(2);
  }
  const browser = await chromium.launch();
  try {
    await setApproval(browser);
    for (const [width, height] of [[1440, 900], [390, 844]]) {
      for (const scheme of ["dark", "light"]) await accountShots(browser, width, height, scheme);
    }
    for (const [width, height] of [[1440, 900], [390, 844]]) {
      for (const scheme of ["dark", "light"]) await keyForm(browser, width, height, scheme);
    }
  } finally {
    await browser.close();
  }
  console.log(failures ? `${failures} check(s) failed` : "all checks passed");
  process.exit(failures ? 1 : 0);
})().catch((error) => {
  console.error(error);
  process.exit(2);
});
