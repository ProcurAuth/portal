// Checks the rule "any right on a service opens the service" in the editor of grants, in a real
// browser. Run it with tools/dev/ui-check.sh. The portal is the demo (demo/up.sh): the check needs
// the services ClickHouse Dash, Notes, Wiki and Big database, and the superadmin.
//
// Environment:
//   UI_CHECK_BASE        base URL of the portal (default http://localhost:8088)
//   UI_CHECK_LOGIN       login of the superadmin (default admin)
//   UI_CHECK_PASSWORD    password of the superadmin (default test)
//   UI_CHECK_KEY_LOGIN   login of an account that can make keys (optional, for the form of a key)
//   UI_CHECK_KEY_PASSWORD  its password
//   UI_CHECK_SHOTS       a directory for the screenshots (optional)
//
// The check writes nothing to the portal: it never clicks Save.

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

  // The switch off takes the rights with it, and says how many.
  await row(page, CH).locator("input[data-switch]").uncheck();
  s = await state(page, CH);
  check("switch off: the row says Access off and the count", !s.on && s.info.includes("Access off") && s.info.includes("5 permissions removed"), JSON.stringify(s));
  check("switch off: no field is sent", (await fields(page)).length === 0, (await fields(page)).join());
  await shot(page, "switch-off");

  // The switch on again brings nothing back.
  await row(page, CH).locator("input[data-switch]").check();
  s = await state(page, CH);
  check("switch on again: only access is back", s.on && (await fields(page)).join() === `access on service:${CH_ID}`, JSON.stringify(s));

  // The row `access` of the pane works as the switch.
  await tick(page, "query.export");
  await tick(page, "access", false);
  s = await state(page, CH);
  check("row access unticked: the switch is off and the rights are gone", !s.on && (await fields(page)).length === 0, JSON.stringify(s) + (await fields(page)).join());

  // A table, four levels below the service (Big database).
  await expand(page, "Big database");
  for (const path of ["cluster:eu1", "cluster:eu1/host:ch-1-1", "cluster:eu1/host:ch-1-1/database:db1"]) await node(page, path);
  await node(page, "cluster:eu1/host:ch-1-1/database:db1/table:t1");
  // A long list starts with its groups closed: open the first one.
  await page.locator(".ge__svc.is-expanded .ge__group").first().click();
  const first = await page.locator(".ge__svc.is-expanded .ge__act input[data-action]:not([disabled])").first().getAttribute("data-action");
  await tick(page, first);
  await expectOpen(page, "Big database", "bigdb", "1 permission", "table");

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

(async () => {
  const browser = await chromium.launch();
  try {
    await newUser(browser);
    await keyForm(browser);
  } finally {
    await browser.close();
  }
  console.log(failures ? `${failures} check(s) failed` : "all checks passed");
  process.exit(failures ? 1 : 0);
})().catch((error) => {
  console.error(error);
  process.exit(2);
});
