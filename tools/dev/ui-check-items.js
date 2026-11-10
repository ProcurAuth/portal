// Checks the web pages of the items in a real browser: the panel "Open items" on the page Inbox, the History, the page of
// one request, the forms and the page of the limits. Run it with tools/dev/ui-check.sh and
// UI_CHECK_SCRIPT=ui-check-items.js. The checks WRITE: they answer, reply, cancel and read requests. Use a server that
// the test `items_demo_for_the_eyes` of tests/items_web_e2e.rs starts (it writes demo.json with the values below).
//
// For each of the widths 390, 1440 and 1920, and for the dark and the light theme, each page must load, have no
// sideways scroll and show its zones in the right order. The forms must work with JavaScript off.
//
// Environment:
//   UI_CHECK_BASE        base URL of the server, for example http://127.0.0.1:41000
//   UI_CHECK_USER        the owner of the requests (default dana)
//   UI_CHECK_USER_PASSWORD, UI_CHECK_ROOT_PASSWORD  the passwords (from demo.json)
//   UI_CHECK_ADMIN       an account with approval.read and approval.cancel (default ann)
//   UI_CHECK_IDS         the JSON object `ids` of demo.json
//   UI_CHECK_SHOTS       a directory for the screenshots (optional)
//
// The questions (lot L14): when UI_CHECK_IDS holds the key `both`, the script checks the web pages of the questions
// instead. Start the server with the test `questions_demo_for_the_eyes` of tests/questions_web_e2e.rs (it writes
// demo-questions.json). The pages are checked at 390 and 1440 px, dark and light, with JavaScript off.
const { chromium } = require("playwright");

const BASE = process.env.UI_CHECK_BASE;
const OWNER = process.env.UI_CHECK_USER || "dana";
const PW = process.env.UI_CHECK_USER_PASSWORD;
const ROOT_PW = process.env.UI_CHECK_ROOT_PASSWORD;
const ADMIN = process.env.UI_CHECK_ADMIN || "ann";
const IDS = JSON.parse(process.env.UI_CHECK_IDS || "{}");
const SHOTS = process.env.UI_CHECK_SHOTS || "";
const INBOX = "/auth/settings/inbox";
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

async function open(browser, state, opts) {
  const context = await browser.newContext({ storageState: state, ...opts });
  return { context, page: await context.newPage() };
}

async function shot(page, name) {
  if (SHOTS) await page.screenshot({ path: SHOTS + "/" + name + ".png", fullPage: true });
}

// The top edge of an element, or null.
const top = (page, selector) =>
  page.evaluate((s) => {
    const el = document.querySelector(s);
    return el ? el.getBoundingClientRect().top + window.scrollY : null;
  }, selector);

const noSideways = (page) =>
  page.evaluate(() => {
    const main = document.querySelector("main");
    return [document.documentElement.scrollWidth - document.documentElement.clientWidth, main ? main.scrollWidth - main.clientWidth : 0];
  });

// The pages of the questions (lot L14).
async function questions(browser, owner, root) {
  const Q = INBOX + "/items/";
  const pages = [
    ["inbox", INBOX],
    ["history", INBOX + "/items"],
    ["q-both", Q + IDS.both],
    ["q-old", Q + IDS.old],
    ["q-closed", Q + IDS.closed],
    ["q-multiple", Q + IDS.multiple],
    ["q-text", Q + IDS.text],
    ["q-confirm", Q + IDS.confirm],
    ["q-none", Q + IDS.none],
    ["q-discussion", Q + IDS.discussion],
    ["q-form", Q + IDS.form],
    ["q-form-done", Q + IDS.form_done],
    ["q-automatic", Q + IDS.jev],
    ["q-marks", Q + IDS.marks],
    ["q-answered", Q + IDS.answered],
    ["q-declined", Q + IDS.declined],
    ["q-cancelled", Q + IDS.cancelled],
  ];
  for (const scheme of ["dark", "light"]) {
    for (const width of [390, 1440]) {
      const tag = " (" + width + ", " + scheme + ")";
      const { context, page } = await open(browser, owner, { viewport: { width, height: 900 }, colorScheme: scheme, javaScriptEnabled: false });
      for (const [name, path] of pages) {
        const response = await page.goto(BASE + path);
        check(name + " loads" + tag, response.status() === 200, response.status());
        const [doc, mainWidth] = await noSideways(page);
        check(name + " has no sideways scroll" + tag, doc <= 0 && mainWidth <= 0, JSON.stringify([doc, mainWidth]));
        check(name + " has Cache-Control no-store" + tag, (response.headers()["cache-control"] || "").includes("no-store"), response.headers()["cache-control"]);
        await shot(page, name + "." + width + "." + scheme);
      }
      // The panel: question rows, grouped by agent, with the alerts above.
      await page.goto(BASE + INBOX);
      check("the panel lists the questions" + tag, (await page.locator(".oi .kind").count()) >= 8, await page.locator(".oi .kind").count());
      check("the panel has one group for each agent" + tag, (await page.locator("details.agrp").count()) >= 1, "");
      // The rows under the options follow the flags.
      await page.goto(BASE + Q + IDS.both);
      check("both rows show" + tag, (await page.locator(".opt.is-other").count()) === 1 && (await page.locator("textarea[form=qask]").count()) === 1, "");
      await page.goto(BASE + Q + IDS.closed);
      check("a closed question has no row" + tag, (await page.locator(".opt.is-other").count()) === 0 && (await page.locator("textarea").count()) === 0, "");
      await page.goto(BASE + Q + IDS.jev);
      check("an automatic system has no chat control" + tag, (await page.locator("textarea, form#qask, form[action$='/items/reply']").count()) === 0 && (await page.content()).includes("Automatic system (no conversation)"), "");
      const hint = await top(page, ".auto");
      const first = await top(page, ".opt");
      check("the hint is above the options" + tag, hint !== null && first !== null && hint < first, JSON.stringify([hint, first]));
      check("no option is ticked" + tag, (await page.locator("input:checked").count()) === 0, "");
      await page.goto(BASE + Q + IDS.none);
      check("a message with no answer has no Send" + tag, (await page.locator("button[value=accept]").count()) === 0 && (await page.locator("button[value=decline]").count()) === 1, "");
      await page.goto(BASE + Q + IDS.form);
      check("a form has a block for each question" + tag, (await page.locator(".q").count()) === 3, await page.locator(".q").count());
      await page.goto(BASE + Q + IDS.marks);
      check("the marks are visible and the banner shows" + tag, (await page.locator(".vis, .md-ctl").count()) >= 2 && (await page.locator(".banner--bad").count()) === 1, "");
      await page.goto(BASE + Q + IDS.old);
      check("the old question has the banner and no box" + tag, (await page.locator(".banner--warn").count()) >= 1 && (await page.locator("input[type=checkbox][name=stale_ack]").count()) === 0, "");
      await context.close();
    }
  }

  // ---- the copy button, with JavaScript on
  {
    const context = await browser.newContext({ storageState: owner, viewport: { width: 1440, height: 900 }, permissions: ["clipboard-read", "clipboard-write"] });
    const page = await context.newPage();
    await page.goto(BASE + INBOX + "/items/" + IDS.both);
    const button = page.locator("[data-copy-text]").first();
    check("the copy button shows with script", await button.isVisible(), "");
    await button.click();
    const copied = await page.evaluate(() => navigator.clipboard.readText());
    check("the copied command is the exact text", copied === "kubectl -n prod set image deploy/api \\\n  api=registry.local/api:v5.1", JSON.stringify(copied));
    await context.close();
  }

  // ---- the forms, with JavaScript off
  {
    const { context, page } = await open(browser, owner, { viewport: { width: 390, height: 900 }, javaScriptEnabled: false });
    const fresh = (id) => page.goto(BASE + INBOX + "/items/" + id);
    // Ask the agent: the button of the second form sends its text, the question stays open.
    await fresh(IDS.both);
    await page.fill("textarea[form=qask]", "How long does it take?");
    await Promise.all([page.waitForNavigation(), page.click("button[form=qask]")]);
    check("a question to the agent makes a turn", (await page.content()).includes("How long does it take?") && page.url().includes("s=replied") && (await page.locator("button[value=accept]").count()) === 1, page.url());
    check("the field is closed while the agent writes", (await page.locator("textarea[form=qask][disabled]").count()) === 1, "");
    await shot(page, "q-asked.390.dark");
    // A choice with no tick: the page shows the mistake.
    await fresh(IDS.closed);
    await Promise.all([page.waitForNavigation(), page.click("button[value=accept]")]);
    check("an answer with no tick is refused with a mistake", (await page.locator(".miss").count()) === 1 && (await page.content()).includes("Choose one."), "");
    await shot(page, "q-mistake.390.dark");
    await page.check("input[value='o:wait']");
    await Promise.all([page.waitForNavigation(), page.click("button[value=accept]")]);
    check("a choice goes through", page.url().includes("s=q_answered") && (await page.content()).includes("Your answer is sent to the agent."), page.url());
    check("the answered page shows the choice and no form", (await page.locator("button[value=accept]").count()) === 0 && (await page.locator(".ans__list li").count()) === 1, "");
    await shot(page, "q-answered-web.390.dark");
    // The field stops at 2000 characters: the browser cuts a longer text, and the server refuses a longer one (tests/questions_web_e2e.rs).
    await fresh(IDS.text);
    check("the text field stops at 2000 characters", (await page.getAttribute("textarea[name=b0t]", "maxlength")) === "2000", "");
    check("the secret warning is under the field", (await page.content()).includes("Do not enter a password or a secret"), "");
    await page.fill("textarea[name=b0t]", "Every night, 02:00 to 05:00.");
    await Promise.all([page.waitForNavigation(), page.click("button[value=accept]")]);
    check("a text answer goes through", (await page.content()).includes("Every night, 02:00 to 05:00."), page.url());
    // A multiple choice with the row Other.
    await fresh(IDS.multiple);
    await page.check("input[name=b0o_go]");
    await page.check("input[name=b0o_stage]");
    await page.check("input[name=b0x]");
    await page.fill("textarea[name=b0t]", "mine");
    await Promise.all([page.waitForNavigation(), page.click("button[value=accept]")]);
    check("the ticks and a text together are refused", (await page.content()).includes("replaces the choices") && (await page.inputValue("textarea[name=b0t]")) === "mine" && (await page.locator("input[name=b0o_go]:checked").count()) === 1, "");
    await shot(page, "q-exclusive.390.dark");
    await page.uncheck("input[name=b0o_go]");
    await page.uncheck("input[name=b0o_stage]");
    await Promise.all([page.waitForNavigation(), page.click("button[value=accept]")]);
    check("the text alone is the free answer", (await page.content()).includes("Your own answer (Other)."), page.url());
    // A form: the blocks without an answer are named, then it goes through.
    await fresh(IDS.form);
    await page.check("input[name=b0o_ev]");
    await Promise.all([page.waitForNavigation(), page.click("button[value=accept]")]);
    check("an incomplete form names the blocks", (await page.locator(".miss").count()) >= 2 && (await page.locator("input[name=b0o_ev]:checked").count()) === 1, await page.locator(".miss").count());
    await shot(page, "q-form-mistake.390.dark");
    await page.check("input[name=b1][value='o:d90']");
    await page.fill("textarea[name=b2t]", "Every night");
    await Promise.all([page.waitForNavigation(), page.click("button[value=accept]")]);
    check("a form goes through", (await page.locator(".ans").count()) === 3, await page.locator(".ans").count());
    await shot(page, "q-form-answered.390.dark");
    // A decline and a message with no answer.
    await fresh(IDS.confirm);
    await Promise.all([page.waitForNavigation(), page.click("button[value=decline]")]);
    check("a decline ends the question", (await page.content()).includes("You declined to answer."), page.url());
    await fresh(IDS.none);
    await Promise.all([page.waitForNavigation(), page.click("button[value=decline]")]);
    check("a message with no answer is closed with Decline", (await page.locator("button[value=decline]").count()) === 0, page.url());
    // The automatic system: choices only.
    await fresh(IDS.jev);
    await page.check("input[value='o:billing']");
    await Promise.all([page.waitForNavigation(), page.click("button[value=accept]")]);
    check("an automatic decision goes through", (await page.content()).includes("billing") && page.url().includes("s=q_answered"), page.url());
    await context.close();
  }

  // ---- the superadmin reads the content behind the switch of the file
  {
    const su = await open(browser, root, { viewport: { width: 1440, height: 900 }, javaScriptEnabled: false });
    await su.page.goto(BASE + INBOX + "/items/" + IDS.old + "/body");
    check("the superadmin reads the content of a question", (await su.page.content()).includes("You read this content as the administrator"), su.page.url());
    await shot(su.page, "q-admin-body.1440.dark");
    await su.context.close();
  }
}

async function main() {
  const browser = await chromium.launch();
  const owner = await signIn(browser, OWNER, PW);
  const root = await signIn(browser, "root", ROOT_PW);
  if (IDS.both) {
    await questions(browser, owner, root);
    await browser.close();
    console.log(failures === 0 ? "all checks passed" : failures + " checks failed");
    process.exit(failures === 0 ? 0 : 1);
  }
  const admin = await signIn(browser, ADMIN, "e2e-account-password-0123");

  // ---- the pages at each size and in each theme, with JavaScript off
  for (const scheme of ["dark", "light"]) {
    for (const width of [390, 1440, 1920]) {
      const tag = " (" + width + ", " + scheme + ")";
      const { context, page } = await open(browser, owner, { viewport: { width, height: 900 }, colorScheme: scheme, javaScriptEnabled: false });
      const pages = [
        ["inbox", INBOX],
        ["history", INBOX + "/items"],
        ["approval-old", INBOX + "/items/" + IDS.old],
        ["approval-markdown", INBOX + "/items/" + IDS.markdown],
        ["approval-marks", INBOX + "/items/" + IDS.marks],
        ["approval-long", INBOX + "/items/" + IDS.long + "?full=1"],
      ];
      for (const [name, path] of pages) {
        const response = await page.goto(BASE + path);
        check(name + " loads" + tag, response.status() === 200, response.status());
        const [doc, mainWidth] = await noSideways(page);
        check(name + " has no sideways scroll" + tag, doc <= 0 && mainWidth <= 0, JSON.stringify([doc, mainWidth]));
        check(name + " has Cache-Control no-store" + tag, (response.headers()["cache-control"] || "").includes("no-store"), response.headers()["cache-control"]);
        await shot(page, name + "." + width + "." + scheme);
      }
      await page.goto(BASE + INBOX);
      const sent = await top(page, "#sent-title");
      const items = await top(page, "#oi-t");
      const phones = await top(page, "#phones-title");
      check("alerts above the open items, the phones below" + tag, sent !== null && items !== null && phones !== null && sent < items && items < phones, JSON.stringify([sent, items, phones]));
      check("the open items have rows with a link" + tag, (await page.locator(".oi__title").count()) >= 4, await page.locator(".oi__title").count());
      await page.goto(BASE + INBOX + "/items/" + IDS.markdown);
      const cmd = await top(page, "#cmd-t");
      const exp = await top(page, "#exp-t");
      check("the command is above the explanation" + tag, cmd !== null && exp !== null && cmd < exp, JSON.stringify([cmd, exp]));
      check("the copy button is hidden without script" + tag, !(await page.locator("[data-copy-text]").first().isVisible()), "");
      const colors = await page.evaluate(() => {
        const a = document.querySelector(".md-cmd .tok-keyword");
        const b = document.querySelector(".sec2--exp .md-code .tok-keyword");
        return [a && getComputedStyle(a).color, b && getComputedStyle(b).color];
      });
      check("the code of the agent has other colors than the command" + tag, colors[0] && colors[1] && colors[0] !== colors[1], JSON.stringify(colors));
      await page.goto(BASE + INBOX + "/items/" + IDS.marks);
      check("the marks are visible and the banner shows" + tag, (await page.locator(".md-ctl").count()) >= 3 && (await page.locator(".banner--bad").count()) === 1, "");
      await page.goto(BASE + INBOX + "/items/" + IDS.old);
      check("the old request has the box and the banner" + tag, (await page.locator("input[name=stale_ack][required]").count()) === 1 && (await page.locator(".banner--warn").count()) === 1, "");
      await context.close();
    }
  }

  // ---- the limits page, with the superadmin, at the three sizes
  for (const width of [390, 1440, 1920]) {
    const { context, page } = await open(browser, root, { viewport: { width, height: 900 }, javaScriptEnabled: false });
    const response = await page.goto(BASE + INBOX + "/limits");
    check("limits load (" + width + ")", response.status() === 200, response.status());
    const [doc, mainWidth] = await noSideways(page);
    check("limits have no sideways scroll (" + width + ")", doc <= 0 && mainWidth <= 0, JSON.stringify([doc, mainWidth]));
    check("the secret key shows no value (" + width + ")", (await page.locator(".lim__v.is-mask").count()) >= 1, "");
    await shot(page, "limits." + width + ".dark");
    await context.close();
  }

  // ---- the copy buttons, with JavaScript on
  {
    const context = await browser.newContext({ storageState: owner, viewport: { width: 1440, height: 900 }, permissions: ["clipboard-read", "clipboard-write"] });
    const page = await context.newPage();
    await page.goto(BASE + INBOX + "/items/" + IDS.markdown);
    const button = page.locator("[data-copy-text]").first();
    check("the copy button shows with script", await button.isVisible(), "");
    const expected = await page.evaluate(() => {
      const raw = atob(document.querySelector("[data-copy-text]").getAttribute("data-copy-text"));
      return new TextDecoder().decode(Uint8Array.from(raw, (c) => c.charCodeAt(0)));
    });
    await button.click();
    const copied = await page.evaluate(() => navigator.clipboard.readText());
    check("the copied command is the exact text", copied === expected && copied.length > 0, JSON.stringify([copied, expected]));
    const blocks = await page.locator(".sec2--exp .md-copy").count();
    if (blocks > 0) {
      await page.locator(".sec2--exp .md-copy").first().click();
      const code = await page.evaluate(() => navigator.clipboard.readText());
      const shown = await page.locator(".sec2--exp .md-code").first().innerText();
      check("a code block of the agent has a copy button", code.trim() === shown.trim() && code.length > 0, JSON.stringify([code, shown]));
    }
    check("the explanation text is not selectable", (await page.evaluate(() => getComputedStyle(document.querySelector(".exp__text")).userSelect)) !== "text", "");
    await context.close();
  }

  // ---- the forms with JavaScript off
  {
    const { context, page } = await open(browser, owner, { viewport: { width: 390, height: 900 }, javaScriptEnabled: false });
    // A long explanation folds, and a link shows all of it.
    await page.goto(BASE + INBOX + "/items/" + IDS.long);
    check("the long explanation folds", (await page.locator(".exp__more a").count()) === 1, "");
    await page.click(".exp__more a");
    check("Show all gives the whole text", (await page.locator(".exp__more").count()) === 0 && page.url().includes("full=1"), page.url());

    // The old request: the browser needs the box, then the approval goes through.
    await page.goto(BASE + INBOX + "/items/" + IDS.old);
    await page.click("button[value=accept]");
    check("an old request is not sent without the box", page.url().endsWith("/items/" + IDS.old), page.url());
    await page.check("input[name=stale_ack]");
    await Promise.all([page.waitForNavigation(), page.click("button[value=accept]")]);
    check("the approval of an old request goes through", page.url().includes("s=approved") && (await page.locator(".notice, [role=status]").count()) >= 0 && (await page.content()).includes("You approved the request"), page.url());
    check("the outcome replaces the form", (await page.locator("button[value=accept]").count()) === 0 && (await page.content()).includes("Approved by"), "");
    await shot(page, "approved.390.dark");

    // A question and a refusal with a comment, on the request that allows both.
    await page.goto(BASE + INBOX + "/items/" + IDS.markdown);
    await page.click("summary:has-text('Ask the agent a question')");
    await page.fill("textarea[name=text][required]", "Is the count the same on both sides?");
    await Promise.all([page.waitForNavigation(), page.click("form[action$='/items/reply'] button[type=submit]")]);
    check("a question makes a turn in the thread", (await page.content()).includes("Is the count the same on both sides?") && (await page.locator(".turn--agent .md-code").count()) >= 1, page.url());
    await shot(page, "thread.390.dark");
    await page.click("summary:has-text('Decline with a comment')");
    await page.fill("textarea#cm", "Use staging_b, not prod");
    await Promise.all([page.waitForNavigation(), page.click("button:has-text('Decline with comment')")]);
    check("a refusal with a comment ends the request", (await page.content()).includes("You declined the request") && (await page.content()).includes("Declined by"), page.url());
    await context.close();
  }

  // ---- the History after the answers, the filters and the chain
  {
    const { context, page } = await open(browser, owner, { viewport: { width: 1440, height: 900 }, javaScriptEnabled: false });
    await page.goto(BASE + INBOX + "/items");
    check("the History lists the final requests", (await page.locator("table.hist tbody tr").count()) >= 6, await page.locator("table.hist tbody tr").count());
    check("a chained request has its row", (await page.locator("table.hist tr.chain").count()) >= 1, "");
    await page.click(".chips a:has-text('Declined')");
    check("a chip filters the rows", (await page.locator("table.hist tbody tr").count()) >= 1 && page.url().includes("st=declined"), page.url());
    await page.fill("input[name=k]", "nobody");
    await Promise.all([page.waitForNavigation(), page.press("input[name=k]", "Enter")]);
    check("the key filter works with the Enter key", (await page.content()).includes("No request matches."), page.url());
    await shot(page, "history-filtered.1440.dark");
    await context.close();
  }

  // ---- an administrator: metadata only, the cancel button, and the content only for the superadmin
  {
    const { context, page } = await open(browser, admin, { viewport: { width: 1440, height: 900 }, javaScriptEnabled: false });
    await page.goto(BASE + INBOX + "/items/" + IDS.marks);
    const text = await page.content();
    check("the administrator reads no command", !text.includes("swor wen peek") && text.includes("metadata only"), "");
    await shot(page, "admin-metadata.1440.dark");
    await Promise.all([page.waitForNavigation(), page.click("form[action$='/items/cancel'] button")]);
    check("the administrator cancels an open request", (await page.content()).includes("Cancelled by an administrator."), page.url());
    await context.close();
    const su = await open(browser, root, { viewport: { width: 1440, height: 900 }, javaScriptEnabled: false });
    await su.page.goto(BASE + INBOX + "/items/" + IDS.declined + "/body");
    check("the superadmin reads the content", (await su.page.content()).includes("You read this content as the administrator"), su.page.url());
    await shot(su.page, "admin-body.1440.dark");
    await su.context.close();
  }

  await browser.close();
  console.log(failures === 0 ? "all checks passed" : failures + " checks failed");
  process.exit(failures === 0 ? 0 : 1);
}

main().catch((e) => {
  console.error(e);
  process.exit(2);
});
