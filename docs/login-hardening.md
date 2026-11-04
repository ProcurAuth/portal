# Hardening of the sign-in page

The sign-in page is `/auth/login`. This path is a contract with Traefik ForwardAuth and with the applications. It never changes.

Five measures protect or hide the page. Two of them are real protections. Three of them are obscurity.

| Measure | Kind | What it does |
|---|---|---|
| 1. Anti-enumeration | Real protection | An outside observer cannot find out which logins exist. |
| 2. Hide the product | Obscurity, not security | The page does not name the product or its version. |
| 3. Anti-bot checks | Real protection, limited | A simple script that posts at once, or that fills every field, fails. |
| 4. Script minification | Obscurity, not security | The served script has no comment and no long name. |
| 5. Random names and structure | Obscurity, not security | The classes, the identifiers and the structure of the page are not fixed. |

Obscurity does not stop an attacker who reads the page. It only removes the easy marks. Do not count on items 2, 4 and 5 for security. The limiters and Argon2 stay the protection against password guessing.

None of the five measures adds a configuration key. No new key means no new default to document.

## 1. Anti-enumeration

An attacker can try to learn which logins exist. The portal gives the same answer for each cause of a refusal.

### What the observer sees

These cases give the same answer. Each case gets status 401, the text "Invalid login or password", and the same headers. The page has the same structure. Only the anti-CSRF token, the form token and the inert elements of item 5 change. The size changes with the inert elements, and the change does not depend on the cause.

- The login does not exist.
- The login is the superadmin, and the password is wrong.
- The login is an ordinary account, and the password is wrong.
- The login is longer than any login can be.
- The password is right, but the check of the form fails (see item 3).

Each of these requests runs exactly one Argon2 check. For an unknown login, the check uses a dummy hash that has the same cost parameters as a hash of the portal. Each of these requests adds one failure to the per-IP budget and to the global budget. Each one adds one to the metric `portal_login_attempts_total{result="failure"}`.

The portal reads the accounts of the shared database before each sign-in, for every login. The read has a limit of one for each second. In the past, only an unknown login started the read. The time then told the observer that the login was unknown. The read now starts for every login.

The tests `an_unknown_login_a_wrong_password_and_a_failed_form_check_get_the_same_refusal` and `a_failed_form_check_uses_the_budget_of_the_failed_sign_ins` check this. They compare the status, the canonical form of the body (the body without the inert elements, with the attributes and the head in a fixed order) and the headers. They do not measure time. They count the Argon2 checks, because the count is a measure that does not depend on the load of the machine.

### What stays different (unavoidable)

An observer can still see these differences. None of them depends on whether the typed login exists, except item (b).

- (a) A refusal of a limiter has status 429 and the header `Retry-After`. It does not run Argon2, so it is faster. The observer learns the state of the limiter for his own address, or for all clients. The observer learns nothing about the login. A limiter that hid its refusal would let an attacker use the Argon2 budget of the server.
- (b) The superadmin login has its own budget in the global limiter. While one budget is locked out, the answer tells whether the typed login is the superadmin login. The reason is that an anonymous attack must not lock out the administrator. The superadmin login is not a secret: each identity token carries it.
- (c) A good sign-in has status 303 and a cookie. This difference is the purpose of the page.
- (d) A request with a bad body, a bad `rd` or `sig`, a wrong content type, or a password that is too long gets status 400 or 415. A form that stayed open too long gets status 403. These answers do not depend on the login.
- (e) An account of the file can have a `password_hash` that was made with other Argon2 cost parameters. The check then takes another time than the check against the dummy hash. Make these hashes with `portal hash-password`, so that all hashes have the same cost.
- (f) A busy server queues the checks behind the semaphore `max_concurrent_password_checks`. The queue time is the same for every login.

No state "disabled" or "locked" exists for an account. A deleted account is an unknown login. An account of the file that has no `password_hash` is an unknown login for the sign-in. The portal has no per-account lockout. Such a lockout would show which accounts exist.

### The `rd` and `sig` fields

The fields `rd` and `sig` are the return target and its HMAC. They do not depend on a login. A value that is not valid is replaced by `/` on the page. A value that is not valid in a POST gives status 400. An attacker cannot make a valid pair without the session secret.

## 2. Hide the product

Done:

- The title of the page is `Authentication`. It has no name.
- The page has no comment, no `generator` tag and no version. The server sends no `Server` header and no `X-Powered-By` header.
- The stylesheet and the script have neutral paths: `/auth/a/<hash>.css` and `/auth/a/<hash>.js`. The hash is the first 8 bytes of the SHA-256 of the file. The old paths `/auth/style.css` and `/auth/assets/theme.js` still work. The settings pages and old links need them.
- The assets have no comment and no name of the product. The key of the browser storage for the theme is `ui-color-mode`.
- The cookie of the sign-in form is `lc` (`__Host-lc` over HTTPS). The old name carried the name of the product.
- ForwardAuth lets the neutral asset paths pass without a session, as it does for the old paths.

Not done, on purpose:

- The default name of the session cookie is still `__Host-portal`. The key `session.cookie_name` sets it. A change of the default ends all sessions at the upgrade. A site that wants a neutral name sets the key. The session cookie shows only after a sign-in. It goes to the applications, because it has `Path=/`.

What still identifies the product:

- The path `/auth/login` and the routes `/auth/verify`, `/auth/logout` and `/auth/denied`. They are a contract.
- The query `?rd=...&sig=...` of the redirect to the page.
- The names of the form fields: `login`, `password`, `csrf`, `rd`, `sig`, `token`.
- The headers of the security policy, for example the text of the Content-Security-Policy.
- The visual design of the card, and the behavior of the page. Item 5 lists what it hides and what it keeps.
- The session cookie, until the site sets `session.cookie_name`.
- The settings pages (`/auth/settings/...`) keep the plain class names and the plain files. They need a session. Also the script `/auth/assets/settings.js?v=<version>`, and the metrics path. The metrics path is on a path that the operator controls. The Inbox paths `/readyz` and `/api/version` are public when the block `inbox` exists. `/api/version` shows the version.

## 3. Anti-bot checks

The form has three checks. All of them work without JavaScript.

### The signed token

The hidden field `token` holds the time of issue and an HMAC. The HMAC covers the time and the anti-CSRF token of the browser. The server signs with the session secret. The check has no state on the server, so any instance with the same secret can check it.

- The token is too new when the submit comes less than 1 second after the display. A person cannot type a login and a password so fast.
- The token is too old after 12 hours. This is the life of the anti-CSRF cookie. A person with an old page gets the page "The page was open too long" again, with a new token. This answer is the same as for an expired anti-CSRF cookie.
- A token that is missing, damaged, or signed for another anti-CSRF token is a failed check.
- A clock that runs behind on another instance by more than the time of the person makes a token too new. Keep the clocks of the instances in step.

The token does not stop a replay in its 12 hours. The anti-CSRF token and the token are bound to each other, but a script can read both from the page. The token forces a script to read the form and to wait one second.

### The trap field

The form has a field `website` in a block with the class `hp`. The stylesheet hides the block (`display: none`). The block has `aria-hidden="true"` and `inert`. The field has `tabindex="-1"` and `autocomplete="off"`. It has the attributes that the password managers 1Password, Bitwarden and LastPass read to ignore a field. A keyboard and a screen reader cannot reach it. A person never fills it. A script that fills every field of the form fills it, and the check fails.

### A failed check looks like a wrong password

A failed check does not give a different answer. The sign-in goes the whole way: the Argon2 check, the limiters, the metrics and the log. The end of the sign-in refuses, whatever the password. The answer is the one of item 1: status 401 and the same page. The time is the same.

The check counts as a failed sign-in in the per-IP budget, in the global budget and in `portal_login_attempts_total{result="failure"}`. The metric `portal_login_form_check_failures_total` counts the failed checks alone. It is a part of the failures. The log line `login_failed` has the field `reason`: `credentials` or `form_check`. The reason is in the log only. The page never shows it.

A password manager can submit the form in less than 1 second after the display. The answer is then "Invalid login or password". The new page has a new token, and a second submit works. This cost is accepted.

### The names of the login and password fields

The names `login` and `password` stay stable. Random names for each render were not added. The reason is that no test here can show that every password manager still works. Browsers and managers keep the name of a field with the saved credential, and they use it to find the form again. A random name risks the fill and the prompt to save. The gain is small: a script finds the fields by the type `password`. The fields keep `autocomplete="username"` and `autocomplete="current-password"`, and the password has `type="password"`.

## 4. Script minification

The script that every page loads is `web/theme.js`. The server makes a smaller copy once, at the first request. It uses a small transform in `src/ui/minify.rs`. The transform removes the comments and the layout. It gives short names to the local names. It needs no tool at build time. It adds no dependency.

The transform is a tokenizer, not a parser. It accepts only the JavaScript that the script uses. It refuses a template literal and a regular expression literal. It does not rename a script that has arrow functions, classes, `eval` or `with`. It does not rename in a declaration with a comma. A test makes sure that the shipped script is in this subset. If the transform fails, the server sends the source.

The stylesheet gets the same treatment: the comments and the layout go. Strings stay as they are.

The file has a neutral path with a hash of the content. The browser can keep it for one year (`immutable`). The CSP stays strict: the file is external, and there is no `eval` and no inline script.

Not done:

- No control flow flattening, no string encoding and no dead code. Each of them needs a parser, or a new tool in the build. None of them gives security.
- No transform of `settings.js` (2200 lines). The sign-in page does not load it.

The tests check that the served script has no comment and no name of the product. They also run `node --check` on it. The test skips the check when the image has no `node`.

## 5. Random names and structure

The sign-in page, the status pages and the launcher load only two files. The files are the shared stylesheet `web/portal.css` and the theme script `web/theme.js`. The status pages are "Page not found", "Access denied" and "Authenticated". This measure applies to these pages. The settings pages need a session and load other files. They keep the plain names.

The code is in `src/ui/mask.rs`. The renderers still write plain names. A last step, the mask, changes the page before the server sends it. This way the renderers and their tests do not change.

### The names

- Each class, each identifier (`id`, `for`, `aria-labelledby`, `href="#..."`) and each data attribute of the shared files gets a mapped name. A mapped name has 6 to 10 characters from `a-z0-9`. It starts with a letter.
- The mapped name is the HMAC-SHA-256 of the plain name. The key of the HMAC derives from the session secret, with a fixed label. The secret is the one that signs the form token.
- Each instance that has the same secret makes the same names. No instance keeps a state, so a rolling upgrade works. A restart does not change the names. A new session secret changes them.
- The same map rewrites the stylesheet and the script. In the stylesheet, only the selectors change. In the script, only the strings that name the class `js` and the data attributes change. The values do not change.
- The mapped files have their own paths `/auth/a/<hash>.css` and `/auth/a/<hash>.js`. The hash follows the mapped content. The files are the same for all visitors, so the browser keeps them for one year (`immutable`).
- The settings pages, and the old paths `/auth/style.css` and `/auth/assets/theme.js`, keep the plain files. A browser that visits both kinds of page loads both pairs of files. A page that loads the plain files and a page that loads the mapped files never mix their names.
- ForwardAuth lets the mapped paths pass without a session, as it does for the plain paths.
- A launcher of an account that receives notifications loads a third file, the script of the notifications (`web/notify.js`). It has a neutral path `/auth/a/<hash>.js`. The pass changes no string of this script: it finds its elements by data attributes, and the page has the classes of its parts in the shared stylesheet. The markup of the inbox icon, of the popover and of the toasts goes through the same pass as the rest of the launcher. The settings pages load the file at `/auth/assets/notify.js?v=<version>`.

### The structure

At each render, the mask makes these changes. A change has no effect on the look or on the behavior.

- It inserts 0 to 3 inert elements, at random places in the body. An inert element is a wrapper (`div` or `span`) around an element of the page, with the rule `display: contents`. It can also be an empty `div` with `display: none` and `aria-hidden="true"`. An inert element has a mapped class and no other attribute. It has no content of its own, no tab stop and no role.
- A wrapper never encloses a field, a button or an element of the sprite. A `span` never encloses a block. The mask puts a `div` only where the content of the parent can be a block.
- It changes the order of the attributes of each tag.
- It changes the order of the elements of the head. `<meta charset>` stays first. Order has no effect for the stylesheet and the script: the browser loads both before the first paint.

The stylesheet of these pages has no rule that depends on the structure. It has no child combinator (`>`), no sibling combinator (`+`, `~`) and no `:first-child` or `:first-of-type`. A test reads the stylesheet and fails if such a rule comes back. A rule for a case that needs the structure uses a class.

The order of the login field and the password field does not change. The trap field stays after them. The form fields and their attributes stay in the form. Password managers find them as before.

### What still identifies the product

This is an honest list. The measure is obscurity.

- The names of the form fields: `login`, `password`, `csrf`, `rd`, `sig`, `token` and the trap `website`. The attributes `autocomplete`, `data-lpignore`, `data-1p-ignore` and `data-bwignore`. Password managers and scripts need them.
- The text, the number and the kind of the elements, and their order. The form has two fields, a trap and a button.
- The names of the CSS custom properties (`--bg`, `--accent-fill`, and others), the name of the keyframes `pulse-up`, the values of the rules and the media queries. The mask maps classes, identifiers and data attributes only.
- The values of the theme (`dark`, `light`, `system`), the `aria-*` attributes, `role`, `type` and the key `ui-color-mode` of the browser storage.
- The mapped names are fixed for one deployment. A reader that learns them once finds them on every request. They differ from one deployment to another. They change when the session secret changes.
- The mapped files are served from `/auth/a/`, and both have a hash in the name.
- The cost of the page: size, number of requests, and the two files.
- The behavior: the theme switch, the form flow, the cookie `lc`, and the headers.

### Tests

- Two instances with the same secret make the same names and the same files. Two secrets make other names and other files.
- A name has 6 to 10 characters, it starts with a letter, and no two plain names give the same mapped name.
- The login page, the denied page, the error page and the launcher have none of the plain class names of `portal.css`. Each class of these pages has a rule in the mapped stylesheet. Each `for` and `href="#..."` points to an identifier of the page.
- The mapped stylesheet and the mapped script have none of the plain names. `node --check` accepts the mapped script, when the image has `node`.
- Over 200 seeds, each page has the same canonical form. The mask inserts at most 3 elements, and the counts 0, 1, 2 and 3 all occur.
- The tests of the sign-in page read the mapped names from the mask. The tests of the anti-enumeration compare the canonical form.

The look is checked in a browser, not in a unit test. `tools/dev/ui-check.sh` runs a Playwright check against a portal that is yours. A check of this measure compares screenshots of a portal with the mask and of a portal without it. The viewports are 1280 and 390 pixels, in the dark and the light scheme, with and without script. A difference of one pixel is a failure.

Not done:

- No random names for the fields of the form. Item 3 gives the reason.
- No per-render change of the class names. A name that changes at each render cannot go into a file that the browser keeps. The structure changes at each render. The names do not.
- No change to the pages of the settings.

## Operation notes

- A new session secret changes the mapped names and the mapped paths. It also ends the sessions, so the effect is the same as the effect of an upgrade.
- An upgrade changes the hash in the asset paths. A page that an old instance served can ask for a file that a new instance does not have. During a rolling upgrade, this gives a page without style for a short time. A reload fixes it.
- The cookie of the form has a new name. A form that was open at the upgrade gives the page "The page was open too long" once.
- The scripts that sign in with `curl` must read the field `token` of the page, wait one second, and post it. `demo/lib.sh` does this.
- The page changes the order of the attributes at each render. A script must read the attributes of a tag in any order, and it must not use the plain class names or the identifiers of the page. Use the field names. `demo/lib.sh` and `tools/dev/ui-check.js` do this.
- `tools/dev/ui-check.js` waits 1.2 seconds before it submits the form.
