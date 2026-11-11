// Checks the external services in a real browser: the tile with its own icon and its description, the route of the
// icon (headers, and the answer to a visitor), the form "Add external service" with its file input that fills the
// data URI, a file that is too large, and the service of the file (badge `file`, no trash icon).
// Run it with: UI_CHECK_SCRIPT=ui-check-external.js tools/dev/ui-check.sh
//
// The check WRITES to the portal: it adds one external service and deletes it again. Run it against a portal that is
// yours, never against the shared demo. The portal needs a block `external_service "Team wiki"` in its file, with a
// description and an icon (`icon` or `icon_path`), and the mode `access.grants_mode` of `dynamic` or `hybrid`.
//
// Environment:
//   UI_CHECK_BASE            base URL of the portal (default http://localhost:8088)
//   UI_CHECK_LOGIN           login of the superadmin (default admin)
//   UI_CHECK_PASSWORD        password of the superadmin (default test)
//   UI_CHECK_EXT_NAME        name of the service of the file (default Team wiki)
//   UI_CHECK_EXT_DESCRIPTION its description (default The wiki of the team)
//   UI_CHECK_SHOTS           a directory for the screenshots (optional)

const { chromium } = require("playwright");

const BASE = process.env.UI_CHECK_BASE || "http://localhost:8088";
const SHOTS = process.env.UI_CHECK_SHOTS || "";
const ADMIN = process.env.UI_CHECK_LOGIN || "admin";
const ADMIN_PASSWORD = process.env.UI_CHECK_PASSWORD || "test";
const FILE_NAME = process.env.UI_CHECK_EXT_NAME || "Team wiki";
const FILE_DESCRIPTION = process.env.UI_CHECK_EXT_DESCRIPTION || "The wiki of the team";
// A name that no earlier run used.
const ADDED_NAME = "Icon check " + Date.now().toString(36);
const ADDED_DESCRIPTION = "Added by the browser check";
const SVG = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><rect width="8" height="8" fill="#336"/></svg>';

let failures = 0;
function check(name, ok, detail) {
  console.log((ok ? "ok    " : "FAIL  ") + name + (ok ? "" : " -> " + detail));
  if (!ok) failures += 1;
}

async function signIn(browser, viewport, scheme) {
  const context = await browser.newContext({ viewport, colorScheme: scheme });
  const page = await context.newPage();
  const problems = [];
  page.on("console", (message) => {
    if (message.type() === "error") problems.push(message.text());
  });
  await page.goto(BASE + "/auth/login");
  await page.fill("input[name=login]", ADMIN);
  await page.fill("input[name=password]", ADMIN_PASSWORD);
  // The sign-in form has a minimum time between its display and its submit.
  await page.waitForTimeout(1200);
  await page.click("button[type=submit]");
  await page.waitForLoadState("load");
  return { context, page, problems };
}

async function shot(page, name) {
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/${name}.png` });
}

// The launcher gives each class a random name, so the check finds a tile by its structure: a link in a list that
// holds a span with the exact name.
function tile(page, name) {
  return page.locator("ul li a[href]", { has: page.locator(`span:text-is("${name}")`) });
}

async function tileChecks(page, problems, name, description, label) {
  const t = tile(page, name);
  check(`${label}: the tile exists`, (await t.count()) === 1, await t.count());
  if ((await t.count()) !== 1) return null;
  check(
    `${label}: the description is under the name`,
    (await t.locator(`span:text-is("${description}")`).count()) === 1,
    await t.innerText(),
  );
  const img = t.locator('img[src^="/auth/ext-icon/"]');
  check(`${label}: the tile has its own icon`, (await img.count()) === 1, await img.count());
  if ((await img.count()) !== 1) return null;
  const src = await img.getAttribute("src");
  check(`${label}: the icon is on the same origin`, src.startsWith("/auth/ext-icon/"), src);
  await img.scrollIntoViewIfNeeded();
  await page.waitForFunction((el) => el.complete, await img.elementHandle(), { timeout: 5000 });
  const drawn = await img.evaluate((el) => el.complete && el.naturalWidth > 0);
  check(`${label}: the browser draws the icon`, drawn, "naturalWidth is 0");
  const marks = problems.filter((p) => /Refused|Content Security Policy|violates/i.test(p));
  check(`${label}: no policy error in the console`, marks.length === 0, marks.join(" | "));
  return src;
}

(async () => {
  const browser = await chromium.launch();
  const { context, page, problems } = await signIn(browser, { width: 1280, height: 720 }, "light");
  await page.goto(BASE + "/");
  await page.waitForLoadState("load");
  const src = await tileChecks(page, problems, FILE_NAME, FILE_DESCRIPTION, "launcher (file)");
  await shot(page, "external-launcher");

  // The route of the icon: headers for the session, and the answer for a visitor.
  if (src) {
    const answer = await context.request.get(BASE + src);
    const h = answer.headers();
    check("route: 200 for the superadmin", answer.status() === 200, answer.status());
    check("route: an image type", /^image\//.test(h["content-type"] || ""), h["content-type"]);
    check("route: nosniff", h["x-content-type-options"] === "nosniff", h["x-content-type-options"]);
    check(
      "route: a policy that allows no source, no script and no embedding",
      h["content-security-policy"] === "default-src 'none'; style-src 'unsafe-inline'; sandbox",
      h["content-security-policy"],
    );
    check("route: a strong validator", /^"[0-9a-f]{32}"$/.test(h["etag"] || ""), h["etag"]);
    check("route: a private cache", h["cache-control"] === "private, no-cache", h["cache-control"]);
    const again = await context.request.get(BASE + src, { headers: { "If-None-Match": h["etag"] } });
    check("route: 304 with the validator", again.status() === 304, again.status());
    const visitor = await browser.newContext();
    const seen = await visitor.request.get(BASE + src, { maxRedirects: 0 });
    check(
      "route: a visitor gets no image",
      seen.status() !== 200 && !/^image\//.test(seen.headers()["content-type"] || ""),
      seen.status() + " " + seen.headers()["content-type"],
    );
    await visitor.close();
  }

  // The Services page: the service of the file is read-only.
  await page.goto(BASE + "/auth/settings/services");
  await page.waitForLoadState("load");
  const fileRow = page.locator("tr", { has: page.locator(`a.rowLink:text-is("${FILE_NAME}")`) });
  await fileRow.locator("a.rowLink").click();
  await page.waitForLoadState("load");
  const panelFile = page.locator("section[aria-labelledby=detail-title]");
  check(
    "services: the badge file on the service of the file",
    (await page.locator(`tr:has(a.rowLink:text-is("${FILE_NAME}")) .badge:text-is("file")`).count()) === 1,
    "no badge",
  );
  check(
    "services: the panel shows the description",
    (await panelFile.locator(".extDesc").innerText()) === FILE_DESCRIPTION,
    await panelFile.locator(".extDesc").innerText().catch(() => "none"),
  );
  check(
    "services: no trash icon for the service of the file",
    (await panelFile.locator('form[action="/auth/services/delete"]').count()) === 0,
    "a delete form exists",
  );
  await shot(page, "external-services-file");

  // The form: the file input fills the field of the icon.
  await page.locator("summary", { hasText: "Add external service" }).click();
  const form = page.locator("form.extForm");
  check("form: the file input is visible with script", await form.locator("#ext-icon-file").isVisible(), "hidden");
  await form.locator("#ext-name").fill(ADDED_NAME);
  await form.locator("#ext-href").fill("https://wiki.example.org/");
  await form.locator("#ext-desc").fill(ADDED_DESCRIPTION);
  // A file that is too large: the script refuses it and says why. The field stays empty.
  await form.locator("#ext-icon-file").setInputFiles({
    name: "big.png",
    mimeType: "image/png",
    buffer: Buffer.alloc(32 * 1024 + 1, 1),
  });
  check(
    "form: a file above 32 KiB is refused with a message",
    (await form.locator("[data-icon-msg]").innerText()).includes("32768") &&
      (await form.locator("#ext-icon").inputValue()) === "",
    await form.locator("[data-icon-msg]").innerText(),
  );
  await form.locator("#ext-icon-file").setInputFiles({
    name: "icon.svg",
    mimeType: "image/svg+xml",
    buffer: Buffer.from(SVG),
  });
  await page.waitForFunction(() => document.getElementById("ext-icon").value.startsWith("data:"));
  const value = await form.locator("#ext-icon").inputValue();
  check("form: the field holds a data URI", value.startsWith("data:image/svg+xml;base64,"), value.slice(0, 40));
  await shot(page, "external-form");
  await form.locator("button[type=submit]").click();
  await page.waitForLoadState("load");
  check("form: the notice says that the service is added", (await page.locator("body").innerText()).includes("External service added."), "no notice");

  // The new tile shows its icon and its description.
  await page.goto(BASE + "/");
  await page.waitForLoadState("load");
  await tileChecks(page, problems, ADDED_NAME, ADDED_DESCRIPTION, "launcher (form)");
  await shot(page, "external-launcher-added");

  // A refused form: a bad link gives a notice and stores nothing.
  await page.goto(BASE + "/auth/settings/services");
  await page.locator("summary", { hasText: "Add external service" }).click();
  const bad = page.locator("form.extForm");
  await bad.locator("#ext-name").fill(ADDED_NAME + " bad");
  await bad.locator("#ext-href").evaluate((el) => {
    el.type = "text";
  });
  await bad.locator("#ext-href").fill("http://1.2.3/");
  await bad.locator("button[type=submit]").click();
  await page.waitForLoadState("load");
  check(
    "form: a link with a bad IPv4 address is refused with a notice",
    (await page.locator("body").innerText()).includes("valid host"),
    "no notice",
  );

  // Cleanup: delete the service that this run added.
  await page.goto(BASE + "/auth/settings/services");
  await page.locator(`a.rowLink:text-is("${ADDED_NAME}")`).click();
  await page.waitForLoadState("load");
  await page.locator("section[aria-labelledby=detail-title] details.ask > summary").first().click();
  await page.locator('section[aria-labelledby=detail-title] form[action="/auth/services/delete"] button[type=submit]').click();
  await page.waitForLoadState("load");
  check("cleanup: the service is deleted", (await page.locator("body").innerText()).includes("External service deleted"), "not deleted");

  await browser.close();
  console.log(failures === 0 ? "all checks passed" : `${failures} check(s) failed`);
  process.exit(failures === 0 ? 0 : 1);
})().catch((error) => {
  console.error(error);
  process.exit(2);
});
