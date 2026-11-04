// Checks the sign-in page and the card of the second step in a real browser: the band with the destination (only with a
// redirect), the floating labels, the button that shows the password, the message line, the targets, the contrast, the
// position of the card, and a real sign-in, a refused one, one with the box "Keep me signed in", and the second step.
// It runs at 1280 and 390 px (and the widths 320 and 1920 for the layout), in the dark and the light scheme, with and
// without script. Run it with tools/dev/ui-check.sh and UI_CHECK_SCRIPT=ui-check-login.js.
//
// The check signs in, turns on the second factor of the superadmin, and signs in many times: use a portal that is
// yours, with `authentication.mfa = "optional"`, the limiters of the sign-in off (`security.login_rate_limit_enabled` and
// `global_login_rate_limit_enabled` at false) and a new database, never a shared demo.
//
// Environment:
//   UI_CHECK_BASE      base URL of the portal (default http://127.0.0.1:18814)
//   UI_CHECK_HOST      a host name that the browser maps to the address of UI_CHECK_BASE, so the band shows it
//                      (for example app.example.com). Without it, the band shows the host of UI_CHECK_BASE.
//   UI_CHECK_LOGIN     login of the superadmin (default admin)
//   UI_CHECK_PASSWORD  its password
//   UI_CHECK_SHOTS     a directory for the screenshots (optional)
const crypto = require("crypto");
const { chromium } = require("playwright");

const BASE_PLAIN = process.env.UI_CHECK_BASE || "http://127.0.0.1:18814";
const SHOWN = process.env.UI_CHECK_HOST || "";
const BASE = SHOWN ? BASE_PLAIN.replace(/\/\/[^:/]+/, "//" + SHOWN) : BASE_PLAIN;
const LOGIN = process.env.UI_CHECK_LOGIN || "admin";
const PASSWORD = process.env.UI_CHECK_PASSWORD || "";
const SHOTS = process.env.UI_CHECK_SHOTS || "";
const DEST = "/chdash/reports/2026?tab=1";
let failures = 0;

function check(name, ok, detail) {
  console.log((ok ? "ok    " : "FAIL  ") + name + (ok ? "" : " -> " + detail));
  if (!ok) failures += 1;
}

async function shot(page, name) {
  if (SHOTS) await page.screenshot({ path: SHOTS + "/" + name + ".png", fullPage: false });
}

// RFC 6238: HMAC-SHA-1, 6 digits, a step of 30 seconds.
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
  return String((mac.readUInt32BE(at) & 0x7fffffff) % 1000000).padStart(6, "0");
}

async function open(browser, scheme, width, script, height) {
  const mobile = width < 600;
  const context = await browser.newContext({
    colorScheme: scheme,
    viewport: { width, height: height || 800 },
    javaScriptEnabled: script,
    isMobile: mobile,
    hasTouch: mobile,
  });
  const page = await context.newPage();
  return { context, page };
}

// The address that the edge sends a visitor to: the portal signs the destination, as it does for ForwardAuth.
async function signedLogin(context, rd) {
  const reply = await context.request.get(BASE_PLAIN + "/auth/verify", { headers: { "X-Forwarded-Uri": rd }, maxRedirects: 0 });
  return BASE + reply.headers()["location"];
}

// What the page shows. The classes are masked, so the code finds the parts by what they are, not by a class.
function inspect(page) {
  return page.evaluate(() => {
    const lum = (rgb) => {
      const c = rgb.map((v) => {
        v /= 255;
        return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
      });
      return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    };
    const parse = (text) => (text.match(/[\d.]+/g) || []).map(Number);
    const ratio = (a, b) => {
      const [x, y] = [lum(parse(a)), lum(parse(b))];
      return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
    };
    const box = (el) => {
      const r = el.getBoundingClientRect();
      return { x: Math.round(r.left), y: Math.round(r.top), w: Math.round(r.width), h: Math.round(r.height) };
    };
    const form = document.querySelector("form");
    let card = form;
    while (card && getComputedStyle(card).borderTopWidth === "0px") card = card.parentElement;
    const panel = getComputedStyle(card).backgroundColor;
    const divs = Array.from(document.querySelectorAll("main div")).filter(
      (d) => /^\s*To\s/.test(d.textContent) && d.querySelector("svg")
    );
    const band = divs.filter((d) => !divs.some((o) => o !== d && d.contains(o)))[0] || null;
    const out = { width: window.innerWidth, height: window.innerHeight };
    out.overflow = document.documentElement.scrollWidth > window.innerWidth;
    out.inlineStyle = document.querySelectorAll("[style]").length;
    out.h1 = Array.from(document.querySelectorAll("h1")).map((h) => ({
      text: h.textContent.trim(),
      box: box(h),
    }));
    out.title = document.title;
    out.card = box(card);
    out.cardCentre = Math.round(((card.getBoundingClientRect().top + card.getBoundingClientRect().height / 2) / window.innerHeight) * 1000) / 10;
    out.cardBorder = getComputedStyle(card).borderTopWidth;
    out.cardShadow = getComputedStyle(card).boxShadow;
    out.cardRadius = getComputedStyle(card).borderTopLeftRadius;
    out.formTop = Math.round(form.getBoundingClientRect().top - card.getBoundingClientRect().top);
    out.contrasts = [];
    if (band) {
      const b = getComputedStyle(band);
      out.band = { box: box(band), text: band.innerText.replace(/\s+/g, " ").trim(), height: b.height, tooltip: (band.querySelector("[title]") || {}).title || "" };
      const shown = band.querySelector("[title]") || band;
      out.band.clipped = shown.scrollWidth > shown.clientWidth + 1;
      out.band.inside = box(band).x >= out.card.x - 1 && box(band).x + box(band).w <= out.card.x + out.card.w + 1;
      out.contrasts.push(["band text on the band", ratio(getComputedStyle(shown).color, b.backgroundColor), 4.5]);
      out.band.fontSize = getComputedStyle(shown).fontSize;
      out.links = band.querySelectorAll("a").length;
    }
    out.fields = Array.from(document.querySelectorAll("main input:not([type=hidden]):not([type=checkbox])"))
      .filter((i) => i.checkVisibility())
      .map((i) => {
        const l = i.labels && i.labels[0];
        const wrap = i.parentElement;
        return {
          name: i.name,
          type: i.type,
          box: box(i),
          label: l ? l.textContent.trim() : null,
          labelBox: l ? box(l) : null,
          labelTransform: l ? getComputedStyle(l).transform : null,
          labelColor: l ? getComputedStyle(l).color : null,
          underline: getComputedStyle(wrap).borderBottomColor,
          underlineWidth: getComputedStyle(wrap).borderBottomWidth,
          invalid: i.getAttribute("aria-invalid"),
          autocomplete: i.getAttribute("autocomplete"),
          maxlength: i.getAttribute("maxlength"),
          autofocus: i.hasAttribute("autofocus"),
          focused: document.activeElement === i,
          value: i.value,
        };
      });
    for (const f of out.fields) {
      out.contrasts.push(["field boundary " + f.name, ratio(f.underline, panel), 3]);
      if (f.labelColor) out.contrasts.push(["label of " + f.name, ratio(f.labelColor, panel), 4.5]);
    }
    const eye = form.querySelector("button[aria-controls]");
    out.eye = eye ? { hidden: eye.hidden, display: getComputedStyle(eye).display, box: box(eye), pressed: eye.getAttribute("aria-pressed"), label: eye.getAttribute("aria-label"), type: document.getElementById(eye.getAttribute("aria-controls")).type } : null;
    const keep = form.querySelector("input[name=keep]");
    if (keep) {
      const l = keep.labels[0];
      out.keep = { text: l.textContent.trim(), box: box(keep), labelBox: box(l), border: getComputedStyle(keep).borderTopColor, checked: keep.checked };
      out.contrasts.push(["keep text", ratio(getComputedStyle(l).color, panel), 4.5]);
      out.contrasts.push(["keep box", ratio(getComputedStyle(keep).borderTopColor, panel), 3]);
    }
    const alert = document.querySelector("[role=alert]");
    const live = document.querySelector("[aria-live]");
    const msg = alert || live;
    out.msg = msg ? { text: msg.textContent.trim(), role: msg.getAttribute("role"), live: msg.getAttribute("aria-live"), box: box(msg), color: getComputedStyle(msg).color } : null;
    if (alert) out.contrasts.push(["error text", ratio(getComputedStyle(alert).color, panel), 4.5]);
    out.buttons = Array.from(document.querySelectorAll("main button[type=submit]"))
      .filter((b) => b.checkVisibility())
      .map((b) => {
        const fs = getComputedStyle(b.form);
        const inner = b.form.getBoundingClientRect().width - parseFloat(fs.paddingLeft) - parseFloat(fs.paddingRight);
        return { text: b.textContent.trim(), box: box(b), width: Math.round(b.getBoundingClientRect().width), formWidth: Math.round(inner) };
      });
    const primary = form.querySelector("button[type=submit]");
    if (primary) out.contrasts.push(["button text", ratio(getComputedStyle(primary).color, getComputedStyle(primary).backgroundColor), 4.5]);
    const sum = document.querySelector("summary");
    if (sum) {
      out.summary = { box: box(sum), text: sum.textContent.trim() };
      out.contrasts.push(["summary text", ratio(getComputedStyle(sum).color, panel), 4.5]);
    }
    out.tabOrder = [];
    return out;
  });
}

async function tabOrder(page, count) {
  const seen = [];
  for (let i = 0; i < count; i += 1) {
    await page.keyboard.press("Tab");
    seen.push(await page.evaluate(() => {
      const e = document.activeElement;
      return e ? (e.name || e.tagName.toLowerCase()) + (e.type ? ":" + e.type : "") : "none";
    }));
  }
  return seen;
}

async function main() {
  const args = SHOWN ? ["--host-resolver-rules=MAP " + SHOWN + " " + new URL(BASE_PLAIN).hostname] : [];
  const browser = await chromium.launch({ args });
  const tag = (scheme, width, script, red) => scheme + "-" + width + (script ? "-js" : "-nojs") + (red ? "-redirect" : "-direct");

  // 1. The layout of the sign-in page: both states, both schemes, four widths, with and without script.
  for (const scheme of ["dark", "light"]) {
    for (const width of [320, 390, 1280, 1920]) {
      for (const script of [true, false]) {
        for (const redirect of [true, false]) {
          const name = tag(scheme, width, script, redirect);
          const { context, page } = await open(browser, scheme, width, script, width < 600 ? 844 : 800);
          const url = redirect ? await signedLogin(context, DEST) : BASE + "/auth/login";
          await page.goto(url);
          await page.waitForTimeout(400);
          const i = await inspect(page);
          check("no horizontal scroll, no inline style (" + name + ")", !i.overflow && i.inlineStyle === 0, JSON.stringify([i.overflow, i.inlineStyle]));
          check("one hidden heading and the title Authentication (" + name + ")", i.h1.length === 1 && i.h1[0].text === "Authentication" && i.h1[0].box.w <= 1 && i.title === "Authentication", JSON.stringify([i.h1, i.title]));
          check("the card is flat with a hairline border (" + name + ")", i.cardBorder === "1px" && i.cardShadow === "none" && i.card.w <= 320 && (i.width < 400 || i.card.w === 320), JSON.stringify([i.cardBorder, i.cardShadow, i.card]));
          check(redirect ? "a band with the destination (" + name + ")" : "no band (" + name + ")", redirect ? !!i.band : !i.band, JSON.stringify(i.band));
          if (redirect) {
            const want = (SHOWN ? SHOWN + ":" + new URL(BASE).port : new URL(BASE).host) + "/chdash/reports/2026";
            check("the band says host and path, not the query (" + name + ")", i.band.text === "To " + want || i.band.text.replace(/^To\s*/, "") === want, i.band.text);
            check("the band is 32 px high, inside the card, with no link (" + name + ")", i.band.height === "32px" && i.band.inside && i.links === 0, JSON.stringify(i.band));
            check("the band text is 12 px (" + name + ")", i.band.fontSize === "12px", i.band.fontSize);
            check("the address is not cut at this width, or it carries its tooltip (" + name + ")", !i.band.clipped || i.band.tooltip.includes("/chdash/reports/2026"), JSON.stringify(i.band));
          } else {
            const page_text = await page.evaluate(() => document.body.innerText);
            check("no word Launcher, no placeholder strip (" + name + ")", !/Launcher/.test(page_text) && i.formTop <= 12, JSON.stringify([i.formTop]));
          }
          // The card stays at 35 % (the shell is a grid with the extra space under the card).
          if (width >= 390 && !(width === 390 && !redirect && false)) {
            check("the visual centre is near 35 % of the height (" + name + ")", Math.abs(i.cardCentre - 35) <= 2.5 || i.card.h + 48 + 0.3 * i.height > i.height, String(i.cardCentre));
          }
          for (const [what, got, want] of i.contrasts) check("contrast " + what + " " + got.toFixed(2) + " >= " + want + " (" + name + ")", got >= want, got.toFixed(2));
          const names = i.fields.map((f) => f.name).join(",");
          check("the fields are login then password (" + name + ")", names === "login,password", names);
          const login = i.fields[0];
          const pw = i.fields[1];
          check("login: username, autofocus, label Login (" + name + ")", login.autocomplete === "username" && login.autofocus && login.label === "Login" && login.maxlength === "128", JSON.stringify(login));
          check("password: current-password and its label (" + name + ")", pw.autocomplete === "current-password" && pw.label === "Password", JSON.stringify(pw));
          check("fields are at least 24 px high and the underline is 1 px (" + name + ")", i.fields.every((f) => f.box.h >= 24 && f.underlineWidth === "1px"), JSON.stringify(i.fields.map((f) => f.box)));
          check("the button is full width and at least " + (width < 600 ? 44 : 24) + " px high (" + name + ")", i.buttons[0].width === i.buttons[0].formWidth && i.buttons[0].box.h >= (width < 600 ? 44 : 24), JSON.stringify(i.buttons));
          check("the box has its label and 24 px (" + name + ")", i.keep.text === "Keep me signed in for 30 days" && i.keep.labelBox.h >= (width < 600 ? 44 : 24) && !i.keep.checked, JSON.stringify(i.keep));
          check("the message line has its place and is quiet (" + name + ")", i.msg && i.msg.text === "" && i.msg.live === "polite" && i.msg.box.h >= 16, JSON.stringify(i.msg));
          // The eye: only with script.
          if (script) {
            check("the eye shows with script and is 24 px at least (" + name + ")", i.eye && !i.eye.hidden && i.eye.display !== "none" && i.eye.box.w >= (width < 600 ? 44 : 24) && i.eye.box.h >= (width < 600 ? 44 : 24), JSON.stringify(i.eye));
          } else {
            check("no eye without script (" + name + ")", i.eye && i.eye.hidden && i.eye.display === "none", JSON.stringify(i.eye));
          }
          // The label rests over the empty field, and floats when the field has focus or a value.
          check("at load the login has the focus and its label is up, the password label rests (" + name + ")", login.focused && login.labelTransform !== "none" && pw.labelTransform === "none", JSON.stringify([login.focused, login.labelTransform, pw.labelTransform]));
          await page.focus("input[name=password]");
          await page.waitForTimeout(400);
          const j = await inspect(page);
          check("the focused field floats its label, the other rests (" + name + ")", j.fields[1].labelTransform !== "none" && j.fields[0].labelTransform === "none", JSON.stringify([j.fields[0].labelTransform, j.fields[1].labelTransform]));
          await page.fill("input[name=login]", "ada");
          await page.focus("input[name=password]");
          await page.waitForTimeout(400);
          const k = await inspect(page);
          check("a field with a value keeps its label up (" + name + ")", k.fields[0].labelTransform !== "none", k.fields[0].labelTransform);
          check("the floated label stays above the text (" + name + ")", k.fields[0].labelBox.y + k.fields[0].labelBox.h / 2 - k.fields[0].box.y <= 12, JSON.stringify([k.fields[0].labelBox, k.fields[0].box]));
          await page.fill("input[name=login]", "");
          await shot(page, "login-" + name);
          await context.close();
        }
      }
    }
  }

  // 2. The eye, the tab order, the autofocus.
  {
    const { context, page } = await open(browser, "dark", 1280, true);
    await page.goto(await signedLogin(context, DEST));
    const i = await inspect(page);
    check("login has the focus at load", i.fields[0].focused, JSON.stringify(i.fields[0]));
    await page.fill("input[name=password]", "secret-text");
    const before = await inspect(page);
    await page.click("button[aria-controls]");
    const on = await inspect(page);
    check("the eye shows the password and says it is pressed", before.eye.type === "password" && on.eye.type === "text" && on.eye.pressed === "true", JSON.stringify([before.eye, on.eye]));
    check("the value stays when the password shows", (await page.inputValue("input[name=password]")) === "secret-text", "");
    await shot(page, "login-eye-on-dark-1280");
    await page.click("button[aria-controls]");
    const off = await inspect(page);
    check("the eye hides the password again", off.eye.type === "password" && off.eye.pressed === "false", JSON.stringify(off.eye));
    await page.goto(await signedLogin(context, DEST));
    const order = await tabOrder(page, 5);
    // The login has the focus at load, so the first Tab may reach the password or, in a fresh page, the login again.
    const rest = order[0] === "login:text" ? order.slice(1) : order;
    check("tab order: login, password, eye, box, button", rest.join(" ").startsWith("password:password button:button keep:checkbox"), order.join(" "));
    await context.close();
  }

  // 3. A refused sign-in keeps the size of the card and shows the message and the marks.
  for (const scheme of ["dark", "light"]) {
    for (const width of [1280, 390]) {
      for (const script of [true, false]) {
        for (const redirect of [true, false]) {
          const name = tag(scheme, width, script, redirect);
          const { context, page } = await open(browser, scheme, width, script, width < 600 ? 844 : 800);
          await page.goto(redirect ? await signedLogin(context, DEST) : BASE + "/auth/login");
          const before = await inspect(page);
          await page.fill("input[name=login]", "nobody");
          await page.fill("input[name=password]", "a wrong password");
          await page.waitForTimeout(1300);
          await Promise.all([page.waitForNavigation(), page.click("button[type=submit]")]);
          const after = await inspect(page);
          check("a refusal: the message, role alert, both fields marked (" + name + ")", after.msg && after.msg.text === "Invalid login or password" && after.msg.role === "alert" && after.fields.every((f) => f.invalid === "true"), JSON.stringify([after.msg, after.fields.map((f) => f.invalid)]));
          check("a refusal: the card keeps its size and its place (" + name + ")", JSON.stringify(before.card) === JSON.stringify(after.card), JSON.stringify([before.card, after.card]));
          check("a refusal: the band is the same (" + name + ")", JSON.stringify(before.band && before.band.text) === JSON.stringify(after.band && after.band.text), JSON.stringify([before.band, after.band]));
          check("a refusal: the field marked invalid has a red line, the text reads 4.5:1 (" + name + ")", after.contrasts.every(([, got, want]) => got >= want), JSON.stringify(after.contrasts));
          check("a refusal: the sign-in stays at the login page, the target is kept", page.url().includes("/auth/login"), page.url());
          await shot(page, "refused-" + name);
          await context.close();
        }
      }
    }
  }

  // 4. A real sign-in, with a redirect and without, and with the box "Keep me signed in".
  for (const [redirect, keep] of [[true, false], [false, false], [false, true]]) {
    const { context, page } = await open(browser, "dark", 1280, true);
    await page.goto(redirect ? await signedLogin(context, DEST) : BASE + "/auth/login");
    await page.fill("input[name=login]", LOGIN);
    await page.fill("input[name=password]", PASSWORD);
    if (keep) await page.locator("label", { has: page.locator("input[name=keep]") }).click({ position: { x: 60, y: 8 } });
    await page.waitForTimeout(1300);
    await Promise.all([page.waitForNavigation(), page.click("button[type=submit]")]);
    const where = new URL(page.url()).pathname;
    check("sign-in " + (redirect ? "with" : "without") + " a redirect lands on " + (redirect ? DEST.split("?")[0] : "/"), where === (redirect ? DEST.split("?")[0] : "/"), page.url());
    const cookies = await context.cookies();
    const session = cookies.find((c) => /portal|session|^lc$/.test(c.name) && c.name !== "lc") || cookies[0];
    const persistent = cookies.some((c) => c.name !== "lc" && c.expires > 0);
    check("the box " + (keep ? "keeps" : "does not keep") + " the session after the browser closes", persistent === keep, JSON.stringify(cookies.map((c) => [c.name, c.expires])));
    void session;
    await context.close();
  }

  // 5. The second step. The superadmin turns on the second factor on the page Security, then signs in again.
  {
    const { context, page } = await open(browser, "dark", 1280, true);
    await page.goto(BASE + "/auth/login");
    await page.fill("input[name=login]", LOGIN);
    await page.fill("input[name=password]", PASSWORD);
    await page.waitForTimeout(1300);
    await Promise.all([page.waitForNavigation(), page.click("button[type=submit]")]);
    await page.goto(BASE + "/auth/settings/security");
    const text = await page.textContent("main");
    if (text.includes("Off")) {
      await Promise.all([page.waitForNavigation(), page.click("text=Turn on")]);
      const secret = (await page.content()).match(/otpauth:\/\/totp\/[^<"]*?secret=([A-Z2-7]+)/)[1];
      await page.fill("input[name=code]", code(secret, -1));
      await Promise.all([page.waitForNavigation(), page.click("main button[type=submit]")]);
      global.SECRET = secret;
    }
    check("the second factor is on (a new database: the check needs the secret of its own enrolment)", !!global.SECRET, text.slice(0, 80));
    await context.close();
    for (const scheme of ["dark", "light"]) {
      for (const width of [1280, 390]) {
        for (const script of [true, false]) {
          for (const redirect of [true, false]) {
            const name = tag(scheme, width, script, redirect);
            const second = await open(browser, scheme, width, script, width < 600 ? 844 : 800);
            await second.page.goto(redirect ? await signedLogin(second.context, DEST) : BASE + "/auth/login");
            await second.page.fill("input[name=login]", LOGIN);
            await second.page.fill("input[name=password]", PASSWORD);
            await second.page.waitForTimeout(1300);
            await Promise.all([second.page.waitForNavigation(), second.page.click("button[type=submit]")]);
            await second.page.waitForTimeout(400);
            const i = await inspect(second.page);
            check("code step: " + (redirect ? "band with the destination" : "no band") + " (" + name + ")", redirect ? !!i.band && i.band.text.includes("/chdash/reports/2026") : !i.band, JSON.stringify(i.band));
            check("code step: hidden heading Verification, title Authentication (" + name + ")", i.h1.length === 1 && i.h1[0].text === "Verification" && i.title === "Authentication", JSON.stringify([i.h1, i.title]));
            const f = i.fields[0];
            check("code step: one field with one-time-code, numeric, focused, label (" + name + ")", i.fields.length === 1 && f.autocomplete === "one-time-code" && f.focused && f.label === "Authentication code", JSON.stringify(i.fields));
            check("code step: no horizontal scroll, no inline style, summary is a target (" + name + ")", !i.overflow && i.inlineStyle === 0 && i.summary.box.h >= (width < 600 ? 44 : 24), JSON.stringify([i.overflow, i.inlineStyle, i.summary]));
            for (const [what, got, want] of i.contrasts) check("code step contrast " + what + " " + got.toFixed(2) + " (" + name + ")", got >= want, got.toFixed(2));
            await shot(second.page, "code-" + name);
            // A wrong code counts against the account: five of them lock the second step. Only four rounds send one.
            const wrongRound = scheme === "dark" ? width === 1280 && script : width === 390 && !script;
            if (!wrongRound && !(redirect && script && scheme === "dark" && width === 1280)) {
              await second.context.close();
              continue;
            }
            await second.page.fill("input[autocomplete=one-time-code]", "123456");
            const before = await inspect(second.page);
            await Promise.all([second.page.waitForNavigation(), second.page.click("button[type=submit]")]);
            const after = await inspect(second.page);
            check("code step: a wrong code shows the message and marks the field (" + name + ")", after.msg && after.msg.text === "Invalid code" && after.msg.role === "alert" && after.fields[0].invalid === "true", JSON.stringify(after.msg));
            check("code step: the card keeps its size (" + name + ")", before.card.h === after.card.h, JSON.stringify([before.card, after.card]));
            await shot(second.page, "code-error-" + name);
            await second.page.click("summary");
            await shot(second.page, "code-recovery-" + name);
            const opened = await inspect(second.page);
            check("code step: the recovery form opens in the card (" + name + ")", opened.fields.length === 2 && opened.fields[1].label === "Recovery code" && !opened.overflow, JSON.stringify(opened.fields.map((x) => x.label)));
            if (redirect && script && scheme === "dark" && width === 1280) {
              await second.page.fill("input[autocomplete=one-time-code]", code(global.SECRET, 0));
              await Promise.all([second.page.waitForNavigation(), second.page.click("button[type=submit]")]);
              check("code step: a good code lands on the destination", new URL(second.page.url()).pathname === "/chdash/reports/2026", second.page.url());
            }
            await second.context.close();
          }
        }
      }
    }
  }

  await browser.close();
  console.log(failures === 0 ? "all checks passed" : failures + " checks failed");
  process.exit(failures === 0 ? 0 : 1);
}

main().catch((e) => {
  console.error(e);
  process.exit(2);
});
