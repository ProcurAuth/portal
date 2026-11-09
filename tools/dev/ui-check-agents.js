// Checks the web pages of the agents in a real browser: the groups of the panel "Open items", the notice of a new agent, the
// page and the list of the agents, the box "From agent" on the page of a request, the History filter and the forms. Run it with
// tools/dev/ui-check.sh and UI_CHECK_SCRIPT=ui-check-agents.js. The checks WRITE: they name, allow, block, unblock and merge
// agents. Use a server that the test `agents_demo_for_the_eyes` of tests/agents_web_e2e.rs starts (it writes demo.json with the
// values below). Start it twice to check both modes: INBOX_DEMO_LOCK=1 sets `inbox.agents.lock_new = true`. The script reads the
// mode from the page, and each check holds for the mode that it finds.
//
// For each of the widths 390, 1440 and 1920, and for the dark and the light theme, each page must load, have no sideways scroll
// and show its parts in the right order. The forms must work with JavaScript off. Never run it against a shared demo.
//
// Environment:
//   UI_CHECK_BASE        base URL of the server, for example http://127.0.0.1:41000
//   UI_CHECK_USER        the owner of the agents (default dana)
//   UI_CHECK_USER_PASSWORD, UI_CHECK_ROOT_PASSWORD  the passwords (from demo.json)
//   UI_CHECK_ADMIN       an account with approval.read (default ann)
//   UI_CHECK_IDS         the JSON object `ids` of demo.json: the requests and the agents
//   UI_CHECK_SHOTS       a directory for the screenshots (optional)
const { chromium } = require("playwright");

const BASE = process.env.UI_CHECK_BASE;
const OWNER = process.env.UI_CHECK_USER || "dana";
const PW = process.env.UI_CHECK_USER_PASSWORD;
const ROOT_PW = process.env.UI_CHECK_ROOT_PASSWORD;
const ADMIN = process.env.UI_CHECK_ADMIN || "ann";
const IDS = JSON.parse(process.env.UI_CHECK_IDS || "{}");
const SHOTS = process.env.UI_CHECK_SHOTS || "";
const INBOX = "/auth/settings/inbox";
const AGENTS = INBOX + "/agents";
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

// The page scrolls inside `main`: a tall window shows all of it in one picture.
async function shot(page, name) {
  if (!SHOTS) return;
  const size = page.viewportSize();
  await page.setViewportSize({ width: size.width, height: 2600 });
  await page.screenshot({ path: SHOTS + "/" + name + ".png", fullPage: true });
  await page.setViewportSize(size);
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

// The names of the groups of the panel, in the order of the page.
const groupNames = (page) => page.$$eval(".agrp__name", (els) => els.map((e) => e.textContent));

async function main() {
  const browser = await chromium.launch();
  const owner = await signIn(browser, OWNER, PW);
  const root = await signIn(browser, "root", ROOT_PW);
  const admin = await signIn(browser, ADMIN, "e2e-account-password-0123");

  // The mode of the server: the page of the request of the new agent says it.
  let locked;
  {
    const { context, page } = await open(browser, owner, { viewport: { width: 1440, height: 900 }, javaScriptEnabled: false });
    await page.goto(BASE + INBOX + "/items/" + IDS.new);
    locked = (await page.locator("button[value=accept]").count()) === 0;
    console.log("mode: lock_new is " + locked);
    await context.close();
  }
  const mode = locked ? "lock" : "free";

  // ---- the pages at each size and in each theme, with JavaScript off
  for (const scheme of ["dark", "light"]) {
    for (const width of [390, 1440, 1920]) {
      const tag = " (" + width + ", " + scheme + ")";
      const { context, page } = await open(browser, owner, { viewport: { width, height: 900 }, colorScheme: scheme, javaScriptEnabled: false });
      const pages = [
        ["inbox", INBOX],
        ["agents", AGENTS],
        ["agent-known", AGENTS + "/" + IDS.agent_billing],
        ["agent-new", AGENTS + "/" + IDS.agent_new],
        ["agent-blocked", AGENTS + "/" + IDS.agent_blocked],
        ["approval-known", INBOX + "/items/" + IDS.billing1],
        ["approval-new", INBOX + "/items/" + IDS.new],
        ["approval-copy", INBOX + "/items/" + IDS.copy],
        ["approval-bucket", INBOX + "/items/" + IDS.bucket],
        ["history", INBOX + "/items"],
        ["history-agent", INBOX + "/items?agent=" + IDS.agent_billing],
      ];
      for (const [name, path] of pages) {
        const response = await page.goto(BASE + path);
        check(name + " loads" + tag, response.status() === 200, response.status());
        const [doc, mainWidth] = await noSideways(page);
        check(name + " has no sideways scroll" + tag, doc <= 0 && mainWidth <= 0, JSON.stringify([doc, mainWidth]));
        check(name + " has Cache-Control no-store" + tag, (response.headers()["cache-control"] || "").includes("no-store"), response.headers()["cache-control"]);
        check(name + " has no script element in the body" + tag, (await page.locator("main script").count()) === 0, "");
        await shot(page, name + "." + mode + "." + width + "." + scheme);
      }

      // The page Inbox: the alerts, then the notices, then the groups, then the phones.
      await page.goto(BASE + INBOX);
      const sent = await top(page, "#sent-title");
      const notice = await top(page, ".firstuse");
      const items = await top(page, "#oi-t");
      const phones = await top(page, "#phones-title");
      check("alerts, notice, groups, phones in this order" + tag, [sent, notice, items, phones].every((v) => v !== null) && sent < notice && notice < items && items < phones, JSON.stringify([sent, notice, items, phones]));
      const names = await groupNames(page);
      const first = names[0];
      check("the new agents come first and the bucket last" + tag, /#\d+$/.test(first) && /^Unattributed/.test(names[names.length - 1]), JSON.stringify(names));
      check("there are 2 notices, one for each new agent" + tag, (await page.locator(".firstuse").count()) === 2, await page.locator(".firstuse").count());
      check("each notice has the four forms" + tag, (await page.locator(".firstuse form").count()) === 8, await page.locator(".firstuse form").count());
      check("the groups are details that open and close without script" + tag, (await page.locator("details.agrp[open]").count()) === names.length, "");
      check("a group has a Manage link, the bucket has none" + tag, (await page.locator("details.agrp a.button:has-text('Manage')").count()) >= 3 && (await page.locator("details.agrp--bucket > summary a.button").count()) === 0, "");
      const lockline = await page.locator(".oi .lockline").count();
      check("the locked rows show only with lock_new" + tag, locked ? lockline >= 1 : lockline === 0, lockline);
      check("a warning shows for a text that copies a name" + tag, (await page.locator(".firstuse .warnline").count()) === 1, await page.locator(".firstuse .warnline").count());
      check("the hidden characters of the client have marks" + tag, (await page.locator(".firstuse .md-ctl").count()) >= 3, await page.locator(".firstuse .md-ctl").count());
      const raw = await page.evaluate(() => /[‮​]/.test(document.body.innerText));
      check("no raw hidden character is in the page" + tag, !raw, "");

      // The approval of the new agent.
      await page.goto(BASE + INBOX + "/items/" + IDS.new);
      const accept = await page.locator("button[value=accept]").count();
      check("Approve is off only with lock_new" + tag, locked ? accept === 0 && (await page.locator("button.button--off[disabled]").count()) === 1 : accept === 1, accept);
      check("Decline stays on" + tag, (await page.locator("button[value=decline]").count()) === 1, "");
      const sender = await top(page, ".sender");
      const cmd = await top(page, "#cmd-t");
      check("the box From agent is above the command" + tag, sender !== null && cmd !== null && sender < cmd, JSON.stringify([sender, cmd]));
      check("the name in the box is the number of the key" + tag, /#\d+$/.test(await page.locator(".sender__name").innerText()), await page.locator(".sender__name").innerText());
      check("the dashed box says Not verified" + tag, (await page.locator(".says").textContent()).includes("Not verified"), "");
      await page.goto(BASE + INBOX + "/items/" + IDS.copy);
      check("the injected text is shown as text" + tag, (await page.locator(".says .md-ctl").count()) >= 3 && (await page.locator(".says i, .says b, .says img, .says script").count()) === 0, "");
      await context.close();
    }
  }

  // ---- the owner sees nothing of another owner, and an administrator reads no agent
  {
    const { context, page } = await open(browser, admin, { viewport: { width: 1440, height: 900 }, javaScriptEnabled: false });
    const response = await page.goto(BASE + AGENTS + "/" + IDS.agent_billing);
    check("an administrator gets 404 on the page of an agent of another owner", response.status() === 404, response.status());
    await page.goto(BASE + INBOX + "/items/" + IDS.billing1);
    const text = await page.content();
    check("an administrator reads the name of the agent and nothing that it said", text.includes("Billing bot") && !text.includes("billing-bot") && !text.includes("claude-code 2.1"), "");
    await shot(page, "approval-admin.1440.dark");
    await context.close();
    const su = await open(browser, root, { viewport: { width: 1440, height: 900 }, javaScriptEnabled: false });
    const r = await su.page.goto(BASE + AGENTS + "/" + IDS.agent_billing);
    check("the superadmin gets 404 on the page of an agent of another owner", r.status() === 404, r.status());
    await su.context.close();
  }

  // ---- the forms with JavaScript off, at 390 px
  {
    const { context, page } = await open(browser, owner, { viewport: { width: 390, height: 900 }, javaScriptEnabled: false });
    // A group closes and opens with the keyboard of the browser, and the Manage link navigates.
    await page.goto(BASE + INBOX);
    await page.locator("details.agrp > summary").first().click();
    check("a click on the header closes the group", (await page.locator("details.agrp[open]").count()) === (await groupNames(page)).length - 1, "");
    await page.locator("details.agrp > summary").first().click();
    await Promise.all([page.waitForNavigation(), page.locator("details.agrp a.button:has-text('Manage')").first().click()]);
    check("the Manage link opens the page of the agent", page.url().includes(AGENTS + "/"), page.url());

    // A bad name is refused with the page of the agent and what was typed.
    await page.goto(BASE + AGENTS + "/" + IDS.agent_night);
    await page.fill("form[action$='/agents/rename'] input[name=name]", " bad");
    await Promise.all([page.waitForNavigation(), page.click("form[action$='/agents/rename'] button")]);
    check("a name with a space at its end is refused", (await page.content()).includes("A name has 1 to 32 characters") && (await page.locator("form[action$='/agents/rename'] input[name=name]").inputValue()) === " bad", page.url());
    await shot(page, "rename-refused.390.dark");
    // A name that another agent has, after the fold, is refused.
    await page.fill("form[action$='/agents/rename'] input[name=name]", "BILLING BOT");
    await Promise.all([page.waitForNavigation(), page.click("form[action$='/agents/rename'] button")]);
    check("a name of another agent of the key is refused", (await page.content()).includes("Another agent of this key has this name."), "");
    await page.fill("form[action$='/agents/rename'] input[name=name]", "Night ETL 2");
    await Promise.all([page.waitForNavigation(), page.click("form[action$='/agents/rename'] button")]);
    check("a rename is saved", (await page.content()).includes("The name is saved.") && (await page.locator("h2").first().innerText()).includes("Night ETL 2"), page.url());
    await shot(page, "renamed.390.dark");

    // The request of the new agent: with lock_new the server refuses a forged accept and a decline works. Without it the
    // approval works.
    await page.goto(BASE + INBOX + "/items/" + IDS.new);
    if (locked) {
      check("the button Approve is off and the lock shows", (await page.locator("button.button--off[disabled]").count()) === 1 && (await page.content()).includes("Approve is locked."), "");
      await Promise.all([page.waitForNavigation(), page.click("button[value=decline]")]);
      check("a decline works for a locked agent", (await page.content()).includes("You declined the request"), page.url());
    } else {
      await Promise.all([page.waitForNavigation(), page.click("button[value=accept]")]);
      check("an approval works for a new agent without lock_new", (await page.content()).includes("You approved the request"), page.url());
    }
    await shot(page, "new-agent-answered.390.dark");

    // The first-use form of the agent that copies a name: name it, then allow it.
    await page.goto(BASE + INBOX);
    const form = page.locator(".firstuse").filter({ has: page.locator("q", { hasText: /^Billing Bot$/ }) });
    await form.locator("input[name=name]").fill("Refund cleaner");
    await Promise.all([page.waitForNavigation(), form.locator("button:has-text('Name and allow')").click()]);
    check("Name and allow gives the notice and removes the new agent", page.url().includes("s=agent_allowed") && (await page.content()).includes("The agent is allowed.") && (await page.locator(".firstuse").count()) === 1, page.url());
    await shot(page, "allowed.390.dark");
    await page.goto(BASE + AGENTS);
    check("the list shows the new name, and the agent is known", (await page.locator("table.agTbl tr", { hasText: "Refund cleaner" }).innerText()).includes("Known"), "");
    await page.goto(BASE + INBOX);

    // A merge of the last new agent into a known one: its request moves.
    const before = await groupNames(page);
    const merge = page.locator(".firstuse form[action$='/agents/merge']");
    await merge.locator("select[name=into]").selectOption({ label: "Billing bot" });
    await Promise.all([page.waitForNavigation(), merge.locator("button").click()]);
    check("a merge opens the page of the target with its notice", page.url().includes(AGENTS + "/" + IDS.agent_billing) && (await page.content()).includes("The agents are merged."), page.url());
    await page.goto(BASE + INBOX);
    const after = await groupNames(page);
    check("the merged agent is gone and its notice too", after.length === before.length - 1 && (await page.locator(".firstuse").count()) === 0, JSON.stringify([before, after]));
    await shot(page, "merged.390.dark");

    // A block cancels the requests of the agent.
    await page.goto(BASE + AGENTS + "/" + IDS.agent_report);
    check("the block box says how many requests it cancels", (await page.locator(".danger-zone").innerText()).includes("The 1 open request is cancelled."), "");
    await Promise.all([page.waitForNavigation(), page.click(".danger-zone button")]);
    check("a block shows its notice on the page Inbox", page.url().includes("s=agent_blocked") && (await page.content()).includes("Its open requests are cancelled."), page.url());
    check("the blocked agent has no group now", !(await groupNames(page)).includes("Report writer"), JSON.stringify(await groupNames(page)));
    await shot(page, "blocked.390.dark");
    await page.goto(BASE + INBOX + "/items");
    check("the History says that the owner blocked the agent", (await page.content()).includes("Cancelled: the owner blocked the agent."), "");

    // The History: an agent name filters, the chip clears the filter.
    await page.goto(BASE + INBOX + "/items");
    await Promise.all([page.waitForNavigation(), page.locator("table.hist a:has-text('Billing bot')").first().click()]);
    check("a name in the History filters by the agent", page.url().includes("agent=" + IDS.agent_billing) && (await page.locator(".histAgent").count()) === 1, page.url());
    await shot(page, "history-filtered.390.dark");
    await Promise.all([page.waitForNavigation(), page.click(".histAgent")]);
    check("the chip clears the filter", !page.url().includes("agent=") && (await page.locator(".histAgent").count()) === 0, page.url());

    // An agent that was blocked can be unblocked.
    await page.goto(BASE + AGENTS + "/" + IDS.agent_blocked);
    await Promise.all([page.waitForNavigation(), page.click("form[action$='/agents/unblock'] button")]);
    check("an unblock shows its notice and the block box again", (await page.content()).includes("The agent is not blocked now.") && (await page.locator(".danger-zone").count()) === 1, page.url());
    await page.goto(BASE + AGENTS);
    check("the list shows every agent", (await page.locator("table.agTbl tbody tr").count()) >= 5, await page.locator("table.agTbl tbody tr").count());
    await shot(page, "agents-list.390.dark");

    // The bucket is never locked: the approval works in both modes.
    await page.goto(BASE + INBOX + "/items/" + IDS.bucket);
    await Promise.all([page.waitForNavigation(), page.click("button[value=accept]")]);
    check("an approval works for the bucket in both modes", (await page.content()).includes("You approved the request"), page.url());
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
