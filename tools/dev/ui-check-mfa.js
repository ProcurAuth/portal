// Checks the second factor in a real browser: the enrolment on the page Security, the second step of the sign-in with a
// code and with a recovery code, the error state, and the forced enrolment. The browser generates the codes itself, with
// the secret that the page shows. The check runs at the widths 1280 and 390, in the dark and the light scheme, with
// and without script. Run it with tools/dev/ui-check.sh and UI_CHECK_SCRIPT=ui-check-mfa.js.
//
// The check changes the second factor of the superadmin: use a portal that is yours, never a shared demo, and use two
// portals that have a new database: one with `authentication.mfa = "optional"` (UI_CHECK_BASE) and one with
// `"required"` (UI_CHECK_BASE_REQUIRED).
//
// Environment:
//   UI_CHECK_BASE           base URL of the portal with the policy optional (default http://127.0.0.1:18811)
//   UI_CHECK_BASE_REQUIRED  base URL of the portal with the policy required (default http://127.0.0.1:18812)
//   UI_CHECK_LOGIN          login of the superadmin (default admin)
//   UI_CHECK_PASSWORD       its password
//   UI_CHECK_SHOTS          a directory for the screenshots (optional)
const crypto = require("crypto");
const { chromium } = require("playwright");

const BASE = process.env.UI_CHECK_BASE || "http://127.0.0.1:18811";
const BASE_REQUIRED = process.env.UI_CHECK_BASE_REQUIRED || "http://127.0.0.1:18812";
const LOGIN = process.env.UI_CHECK_LOGIN || "admin";
const PASSWORD = process.env.UI_CHECK_PASSWORD || "";
const SHOTS = process.env.UI_CHECK_SHOTS || "";
let failures = 0;

function check(name, ok, detail) {
  console.log((ok ? "ok    " : "FAIL  ") + name + (ok ? "" : " -> " + detail));
  if (!ok) failures += 1;
}

async function shot(page, name) {
  if (SHOTS) await page.screenshot({ path: SHOTS + "/" + name + ".png", fullPage: true });
}

// RFC 6238: HMAC-SHA-1, 6 digits, a step of 30 seconds. `offset` moves the step.
function base32(text) {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let bits = "";
  for (const c of text.replace(/[ =]/g, "")) bits += alphabet.indexOf(c).toString(2).padStart(5, "0");
  const bytes = [];
  for (let i = 0; i + 8 <= bits.length; i += 8) bytes.push(parseInt(bits.slice(i, i + 8), 2));
  return Buffer.from(bytes);
}

function code(secret, offset) {
  const counter = Math.floor(Date.now() / 30000) + offset;
  const buffer = Buffer.alloc(8);
  buffer.writeBigUInt64BE(BigInt(counter));
  const mac = crypto.createHmac("sha1", base32(secret)).update(buffer).digest();
  const at = mac[mac.length - 1] & 0x0f;
  const value = (mac.readUInt32BE(at) & 0x7fffffff) % 1000000;
  return String(value).padStart(6, "0");
}

async function open(browser, base, scheme, width, script) {
  const mobile = width < 600;
  const context = await browser.newContext({
    colorScheme: scheme,
    viewport: { width, height: 900 },
    javaScriptEnabled: script,
    isMobile: mobile,
    hasTouch: mobile,
  });
  const page = await context.newPage();
  return { context, page };
}

async function passwordStep(page, base, login, password) {
  await page.goto(base + "/auth/login");
  await page.fill("input[name=login]", login || LOGIN);
  await page.fill("input[name=password]", password || PASSWORD);
  await page.waitForTimeout(1300);
  await Promise.all([page.waitForLoadState("load"), page.click("button[type=submit]")]);
}

// What the layout of the page shows: no horizontal scroll, no inline style, and the QR code inside the card.
async function layout(page) {
  return page.evaluate(() => {
    const qr = document.querySelector("svg[role=img]");
    const box = qr ? qr.getBoundingClientRect() : null;
    return {
      overflow: document.documentElement.scrollWidth > window.innerWidth,
      inlineStyle: document.querySelectorAll("[style]").length,
      qr: box ? [Math.round(box.width), Math.round(box.height), Math.round(box.left), Math.round(box.right)] : null,
      width: window.innerWidth,
    };
  });
}

async function secretOf(page) {
  const text = await page.content();
  const found = text.match(/otpauth:\/\/totp\/[^<"]*?secret=([A-Z2-7]+)/);
  return found ? found[1] : null;
}

async function main() {
  const browser = await chromium.launch();
  const tag = (scheme, width, script) => scheme + "-" + width + (script ? "-js" : "-nojs");

  // 1. The flow, with script, on the portal with the policy optional.
  {
    const { context, page } = await open(browser, BASE, "dark", 1280, true);
    await passwordStep(page, BASE);
    check("optional: the password alone opens the session", page.url() === BASE + "/", page.url());
    await page.goto(BASE + "/auth/settings/security");
    check("the page Security shows the state Off", (await page.textContent("main")).includes("Off"), "");
    await shot(page, "security-off-dark-1280");
    await page.click("text=Turn on");
    await page.waitForLoadState("load");
    const secret = await secretOf(page);
    check("the enrolment page has the link with the secret", !!secret, "");
    const l = await layout(page);
    check("the QR code is drawn inside the page", l.qr && l.qr[0] >= 150 && l.qr[3] <= l.width && !l.overflow, JSON.stringify(l));
    check("no inline style", l.inlineStyle === 0, JSON.stringify(l));
    await shot(page, "security-enrol-dark-1280");
    await page.fill("input[name=code]", "000000");
    await page.click("main button[type=submit]");
    await page.waitForLoadState("load");
    check("a wrong code keeps the enrolment page", (await page.textContent("main")).includes("not right"), "");
    await shot(page, "security-enrol-error-dark-1280");
    await page.fill("input[name=code]", code(await secretOf(page), -1));
    await page.click("main button[type=submit]");
    await page.waitForLoadState("load");
    const codes = (await page.inputValue("textarea")).split("\n");
    check("ten recovery codes are shown once", codes.length === 10 && codes.every((c) => /^[a-z2-7]{5}-[a-z2-7]{5}$/.test(c)), codes.join(","));
    await shot(page, "security-codes-dark-1280");
    await context.grantPermissions(["clipboard-read", "clipboard-write"]).catch(() => {});
    await page.click("button[data-copy]");
    await page.waitForTimeout(300);
    const copied = await page.evaluate(() => navigator.clipboard.readText()).catch(() => "");
    check("the Copy button copies the codes", copied === codes.join("\n"), copied);
    await page.click("text=Done");
    await page.waitForLoadState("load");
    check("the state is On", (await page.textContent("main")).includes("Recovery codes left"), "");
    await shot(page, "security-on-dark-1280");
    // The same page on a phone, with the form of "Turn off" open and with the enrolment page.
    await page.setViewportSize({ width: 390, height: 900 });
    await page.click("text=Turn off");
    const phone = await layout(page);
    check("Security at 390 px: no horizontal scroll", !phone.overflow, JSON.stringify(phone));
    await shot(page, "security-on-open-dark-390");
    await page.setViewportSize({ width: 1280, height: 900 });

    // Sign out, then sign in: the second step.
    await page.goto(BASE + "/");
    await Promise.all([page.waitForLoadState("load"), page.click("button:has-text('Sign out')")]);
    await passwordStep(page, BASE);
    check("the password step ends in the code card", (await page.content()).includes("one-time-code"), page.url());
    const stateOf = await layout(page);
    check("the code card: no scroll, no inline style", !stateOf.overflow && stateOf.inlineStyle === 0, JSON.stringify(stateOf));
    await shot(page, "code-dark-1280");
    await page.fill("input[autocomplete=one-time-code]", "123456");
    await page.click("button[type=submit]");
    await page.waitForLoadState("load");
    check("a wrong code shows the error line", (await page.textContent("[role=alert]")).includes("Invalid code"), "");
    await shot(page, "code-error-dark-1280");
    await page.fill("input[autocomplete=one-time-code]", code(secret, 0));
    await Promise.all([page.waitForLoadState("load"), page.click("button[type=submit]")]);
    check("a good code opens the session", page.url() === BASE + "/", page.url());

    // A recovery code, once.
    await Promise.all([page.waitForLoadState("load"), page.click("button:has-text('Sign out')")]);
    await passwordStep(page, BASE);
    await page.click("summary");
    await shot(page, "code-recovery-open-dark-1280");
    await page.fill("input[maxlength='11']", codes[0]);
    await Promise.all([page.waitForLoadState("load"), page.click("details button[type=submit]")]);
    check("a recovery code opens the session", page.url() === BASE + "/", page.url());
    await Promise.all([page.waitForLoadState("load"), page.click("button:has-text('Sign out')")]);
    await passwordStep(page, BASE);
    await page.click("summary");
    await page.fill("input[maxlength='11']", codes[0]);
    await Promise.all([page.waitForLoadState("load"), page.click("details button[type=submit]")]);
    check("the same recovery code is refused", (await page.content()).includes("Invalid code"), page.url());
    await context.close();
  }

  // 2. The cards at the widths and the schemes, with and without script.
  for (const scheme of ["dark", "light"]) {
    for (const width of [1280, 390]) {
      for (const script of [true, false]) {
        const { context, page } = await open(browser, BASE, scheme, width, script);
        await passwordStep(page, BASE);
        const l = await layout(page);
        check("code card " + tag(scheme, width, script) + ": no horizontal scroll", !l.overflow && l.inlineStyle === 0, JSON.stringify(l));
        await shot(page, "code-" + tag(scheme, width, script));
        await page.fill("input[autocomplete=one-time-code]", "123456");
        await page.click("button[type=submit]");
        await page.waitForLoadState("load");
        await shot(page, "code-error-" + tag(scheme, width, script));
        await context.close();
      }
    }
  }

  // 3. The forced enrolment on the portal with the policy required. The superadmin enrols first and makes the accounts
  // that the other rounds use, because an account enrols once.
  const rounds = [
    ["dark", 1280, true, LOGIN, PASSWORD],
    ["light", 390, true, "mfa-ua", "mfa-ua-password-1"],
    ["dark", 1280, false, "mfa-ub", "mfa-ub-password-1"],
    ["light", 390, false, "mfa-uc", "mfa-uc-password-1"],
  ];
  for (const [scheme, width, script, login, password] of rounds) {
    const { context, page } = await open(browser, BASE_REQUIRED, scheme, width, script);
    await passwordStep(page, BASE_REQUIRED, login, password);
    const name = login + " " + tag(scheme, width, script);
    check("required, " + name + ": the password step ends in the enrolment card", (await page.content()).includes("otpauth://"), page.url());
    const l = await layout(page);
    check("the enrolment card, " + name + ": QR inside, no scroll", l.qr && l.qr[3] <= l.width && !l.overflow && l.inlineStyle === 0, JSON.stringify(l));
    await shot(page, "forced-" + tag(scheme, width, script));
    const secret = await secretOf(page);
    await page.fill("input[autocomplete=one-time-code]", "000000");
    await page.click("button[type=submit]");
    await page.waitForLoadState("load");
    check("a wrong code keeps the card, " + name, (await page.textContent("[role=alert]")).includes("Invalid code"), "");
    await shot(page, "forced-error-" + tag(scheme, width, script));
    await page.fill("input[autocomplete=one-time-code]", code(secret, -1));
    await Promise.all([page.waitForLoadState("load"), page.click("button[type=submit]")]);
    check("the codes card shows ten codes, " + name, (await page.inputValue("textarea")).split("\n").length === 10, "");
    const copyVisible = await page.evaluate(() => {
      const b = document.querySelector("button[data-copy]");
      return b ? getComputedStyle(b).display !== "none" : null;
    });
    check("the Copy button shows with script only, " + name, copyVisible === script, String(copyVisible));
    await shot(page, "forced-codes-" + tag(scheme, width, script));
    await Promise.all([page.waitForLoadState("load"), page.click("text=Continue")]);
    check("Continue opens the launcher, " + name, page.url() === BASE_REQUIRED + "/", page.url());
    if (login === LOGIN) {
      // The superadmin makes the accounts of the next rounds.
      for (const [user, pw] of [["mfa-ua", "mfa-ua-password-1"], ["mfa-ub", "mfa-ub-password-1"], ["mfa-uc", "mfa-uc-password-1"]]) {
        await page.goto(BASE_REQUIRED + "/auth/settings/new-user");
        await page.fill("input[name=login]", user);
        await page.fill("input[name=password]", pw);
        await Promise.all([page.waitForLoadState("load"), page.click("#add-user [type=submit], button[form=add-user][type=submit]")]);
      }
    }
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
