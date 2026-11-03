# Hardening of the sign-in page

The sign-in page is `/auth/login`. This path is a contract with Traefik ForwardAuth and with the applications. It never changes.

Four measures protect or hide the page. Two of them are real protections. Two of them are obscurity.

| Measure | Kind | What it does |
|---|---|---|
| 1. Anti-enumeration | Real protection | An outside observer cannot find out which logins exist. |
| 2. Hide the product | Obscurity, not security | The page does not name the product or its version. |
| 3. Anti-bot checks | Real protection, limited | A simple script that posts at once, or that fills every field, fails. |
| 4. Script minification | Obscurity, not security | The served script has no comment and no long name. |

Obscurity does not stop an attacker who reads the page. It only removes the easy marks. Do not count on items 2 and 4 for security. The limiters and Argon2 stay the protection against password guessing.

None of the four measures adds a configuration key. No new key means no new default to document.

## 1. Anti-enumeration

An attacker can try to learn which logins exist. The portal gives the same answer for each cause of a refusal.

### What the observer sees

These cases give the same answer. Each case gets status 401, the text "Invalid login or password", and the same headers. The page has the same size and the same structure. Only the anti-CSRF token and the form token change.

- The login does not exist.
- The login is the superadmin, and the password is wrong.
- The login is an ordinary account, and the password is wrong.
- The login is longer than any login can be.
- The password is right, but the check of the form fails (see item 3).

Each of these requests runs exactly one Argon2 check. For an unknown login, the check uses a dummy hash that has the same cost parameters as a hash of the portal. Each of these requests adds one failure to the per-IP budget and to the global budget. Each one adds one to the metric `portal_login_attempts_total{result="failure"}`.

The portal reads the accounts of the shared database before each sign-in, for every login. The read has a limit of one for each second. In the past, only an unknown login started the read. The time then told the observer that the login was unknown. The read now starts for every login.

The tests `an_unknown_login_a_wrong_password_and_a_failed_form_check_get_the_same_refusal` and `a_failed_form_check_uses_the_budget_of_the_failed_sign_ins` check this. They compare the status, the body, the size and the headers. They do not measure time. They count the Argon2 checks, because the count is a measure that does not depend on the load of the machine.

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
- The visual design of the card, and the behavior of the page.
- The session cookie, until the site sets `session.cookie_name`.
- The settings pages (`/auth/settings/...`), the script `/auth/assets/settings.js?v=<version>` and the metrics path. They need a session, or they are on a path that the operator controls. The Inbox paths `/readyz` and `/api/version` are public when the block `inbox` exists. `/api/version` shows the version.

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

## Operation notes

- An upgrade changes the hash in the asset paths. A page that an old instance served can ask for a file that a new instance does not have. During a rolling upgrade, this gives a page without style for a short time. A reload fixes it.
- The cookie of the form has a new name. A form that was open at the upgrade gives the page "The page was open too long" once.
- The scripts that sign in with `curl` must read the field `token` of the page, wait one second, and post it. `demo/lib.sh` does this.
- `tools/dev/ui-check.js` waits 1.2 seconds before it submits the form.
