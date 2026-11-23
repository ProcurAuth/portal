// Take the screenshots of the portal pages in a real browser (Chrome) and write PNG files.
// Usage: node capture.js PLAN.json OUT_DIR
// The plan is a JSON file:
//   { "base": "http://127.0.0.1:8088",
//     "users": { "alice": "password" },
//     "shots": [ { "name": "launcher", "user": "alice", "path": "/", "height": 760, "wait": "css selector" } ] }
// A shot with "user": null is the page of a visitor with no session. A shot writes NAME-light.png and NAME-dark.png.
// The driver changes nothing in the portal: it only signs in and reads pages. The optional keys of a shot are
//   "height"  the height of the window (default 800)
//   "width"   the width of the window (default 1280)
//   "click"   a CSS selector to click before the picture
//   "hide"    a list of CSS selectors to hide before the picture (use it for a value that is not a demo value)
//   "scrollTo" a CSS selector to scroll into view before the picture
//   "fullPage" true to take the whole page
// The module playwright-core must be found by node (see run.sh).
const { chromium } = require("playwright-core");
const fs = require("fs");

const [planFile, outDir] = process.argv.slice(2);
const plan = JSON.parse(fs.readFileSync(planFile, "utf8"));
const chrome = process.env.CHROME || "/usr/bin/google-chrome";
fs.mkdirSync(outDir, { recursive: true });

const states = {};

// Sign in one time for each account. The portal limits the sign-ins of one address, so the driver reuses the session
// for the second theme.
async function signIn(browser, user, password) {
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1 });
  const page = await context.newPage();
  await page.goto(plan.base + "/auth/login");
  await page.fill("input[name=login]", user);
  await page.fill("input[name=password]", password);
  // The form has a minimum time between its display and its submit.
  await page.waitForTimeout(1500);
  await Promise.all([page.waitForNavigation(), page.click("button[type=submit]")]);
  if (page.url().includes("/auth/login")) throw new Error("the sign-in of " + user + " failed");
  const state = await context.storageState();
  await context.close();
  return state;
}

(async () => {
  const browser = await chromium.launch({ executablePath: chrome, args: ["--no-sandbox", "--hide-scrollbars"] });
  for (const shot of plan.shots) {
    if (shot.user !== null && !(shot.user in states)) states[shot.user] = await signIn(browser, shot.user, plan.users[shot.user]);
  }
  for (const scheme of ["light", "dark"]) {
    for (const shot of plan.shots) {
      const context = await browser.newContext({
        viewport: { width: shot.width || 1280, height: shot.height || 800 },
        colorScheme: scheme,
        deviceScaleFactor: 1,
        ...(shot.user === null ? {} : { storageState: states[shot.user] }),
      });
      const page = await context.newPage();
      await page.goto(plan.base + shot.path, { waitUntil: "load" });
      if (shot.click) await page.click(shot.click);
      for (const sel of shot.hide || []) await page.addStyleTag({ content: sel + " { visibility: hidden !important; }" });
      if (shot.scrollTo) await page.locator(shot.scrollTo).first().scrollIntoViewIfNeeded();
      await page.waitForTimeout(400);
      const file = outDir + "/" + shot.name + "-" + scheme + ".png";
      await page.screenshot({ path: file, fullPage: !!shot.fullPage });
      console.log("wrote " + file);
      await context.close();
    }
  }
  await browser.close();
})().catch((e) => { console.error(e); process.exit(1); });
