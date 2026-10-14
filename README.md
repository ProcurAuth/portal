# portal

Authentication service for Traefik `ForwardAuth`, and the launcher page of the lab, written in Rust: a superadmin defined by the environment, users with roles added from the settings pages, a role gate on every service, and a history of who signed in and what they used. The launcher (`/`) lists the services that declare themselves by label (Docker Compose) or annotation (Kubernetes IngressRoute), grouped in full-width rows, with their health and an offline icon library; see the repository README for the labels.

The runtime image is `FROM scratch`. The HTTP stack is Hyper 1.x on Tokio; discovery uses `reqwest` (rustls) for the Kubernetes API and health probes. Pages are rendered in Rust; the only script is the theme selector (the settings page of the users adds one small script for its password generator). Accounts and history live in one SQLite file (bundled in the static binary) or, to run several active instances, in a shared PostgreSQL database (see [Several instances](#several-instances-postgresql)). There is no Redis, JavaScript framework, CDN, or runtime shell, and nothing is fetched from the internet at runtime.

## Authentication model

The login page asks for a **login** and a **password**.

The **superadmin** is the one account that is not in the database. Its login and password are read from files that the environment fills in (`PORTAL_SUPERADMIN_LOGIN`, `PORTAL_SUPERADMIN_PASSWORD`):

```hcl
authentication {
  superadmin_login_file    = "/run/secrets/portal_superadmin_login"
  superadmin_password_file = "/run/secrets/portal_superadmin_password"
}
```

At startup the plaintext password is read with a strict size bound, hashed once with Argon2id, and the plaintext buffer is zeroized. The superadmin sees and may open everything, and is the only account that can manage the others.

**Users** are added and deleted by the superadmin from the settings pages (the gear of the launcher). Their Argon2id hashes are stored in the database (the SQLite file `storage.database`, or PostgreSQL); the request path reads an in-memory copy and never waits on the database. A user holds any number of roles. Exactly one Argon2 check runs per login attempt whoever the login names (the superadmin's hash, the account's, or a dummy one), and the failure message is the same, so neither time nor wording tells whether a login exists.

A successful login creates a 256-bit random **opaque session ID**. The browser receives only that random ID. With SQLite the server keeps the active-session set in bounded RAM with an expiration time; logout deletes the session immediately, and restarting the process clears all sessions. With PostgreSQL the sessions live in the shared database (only the SHA-256 of the cookie is stored), so they are valid on every instance and survive a restart.

## Roles and the gate

The roles that exist are listed in the configuration:

```hcl
roles {
  names = "ops, dev, viewer"
}
```

A service binds to roles with its `portal.roles` label (an annotation in K3s): `portal.roles: ops, dev`. Its rules:

- no `portal.roles`: open to the superadmin only. Access is granted by naming a role, never by omission, so a new service is closed to every user until it is bound to a role;
- some roles: open to the superadmin and to users holding at least one of them;
- a role that is not in `roles.names` exists nowhere: it opens the service to nobody but the superadmin, and the Services settings page flags the service with a warning (without naming the role).

`/auth/verify` enforces this. It matches the path asked for (`X-Forwarded-Uri`) to the service that owns the longest path prefix of its `href`, on whole segments. Traefik routes on the cleaned path but forwards the raw URI, so the portal reads the path every way a proxy or a backend might (`%2e%2e`, `//`, `%2f` as a character and as a separator, `\`) and allows the request only if every reading is allowed: `/grafana/../clickhouse/` is judged as a ClickHouse request. A refused browser navigation is redirected to `/auth/denied` (the answer to ForwardAuth cannot carry a page: Traefik caps its body); anything else gets a plain 403. `/auth/*` is never subject to roles, and a path that no service owns (an absolute asset path) stays open to every signed-in user. The launcher shows only the services a user may open.

### Paths open without a session (`portal.exclude`)

A caller that cannot sign in (a webhook, a probe) needs a path the portal does not authenticate. The service declares it with `portal.exclude` (an annotation in K3s), **one rule per line** (blank lines and `#` comments are ignored):

```yaml
portal.exclude: |
  POST /dagu/api/v1/webhooks/*
  GET  /dagu/api/v1/health
  PUT,POST /dagu/hooks/**
```

A rule is `[METHOD[,METHOD]] /path`. A segment is written out (`[A-Za-z0-9._~-]`, up to 64 characters), `*` stands for exactly one segment, and `**`, last only, for one or more. There is no regular expression and no partial wildcard (`fo*`): a rule that skips authentication has to be readable at a glance, and a pattern engine is one more thing to get wrong. Without a method the rule matches any; with methods, only those (a request with no method header matches none of them).

The rules are strict on purpose:

- a service can only open paths under **its own `href`**, and only while it is the service that owns the path (the longest prefix wins, as for roles): `/dagu/admin` declared by another service stays closed to Dagu's rules;
- the path is read every way a proxy or a backend might read it (`%2e%2e`, `//`, `%2f` as a character and as a separator, `\`), and the request is let through only if **every** reading belongs to the same service and matches one of its rules: `/dagu/api/v1/webhooks/..%2f..%2fadmin` is not a webhook;
- a wildcard never stands for a segment that hides a separator, and `/auth/*` is never open;
- a rule that cannot be read (a regular expression, a bad method, a path outside the `href`, more than 8 rules) makes the **whole label** unusable: nothing is opened, the service is reported in the catalogue problems, and it is not listed until it is fixed;
- the query string is ignored, and nothing is recorded in a session (there is none): the request is counted in `portal_forward_auth_requests_total{result="excluded"}`.

The Services settings page lists the rules of every service, so the whole exposure can be audited in one place. What a rule opens is protected only by what the service does itself (a webhook token): the portal no longer asks anything.

## Settings pages and history

The superadmin gets a gear on the launcher. It opens the settings, which are **one HTML page per tab**, as the pages of clickhouse-dash are: a header with **Back to panel**, a row of underlined tab links (no fill, only a line under the current one), then the page. They live under `/auth/settings` (Traefik already routes `/auth/` to the portal), work without script, and answer 404 to anyone who is not the superadmin in its own right (a user, or the superadmin browsing as a user). A page carries only what its tab shows, and the launcher carries none of it, so the browser builds a small DOM wherever it goes (a twelve-service launcher is about 150 elements; the settings used to add 700 more to it). Four tabs:

1. **Users** (`/auth/settings`): the accounts first, with the number of failed sign-ins of the last 30 days as a red counter; **Add a user** is one button that opens the form in place (it also opens after a refused attempt). One card per account: last sign-in, a graph of its connections, its latest sessions (time, address, browser, duration, how each ended), the services it used, the services it may open, **Log in as**, **Edit** and a two-step **Delete**. **Edit** changes the roles and, when a new password is typed, the password (leave it empty to keep it); a new password ends that account's sessions at once, a change of roles applies from the next request. The **Generate** button of both forms fills the password field with a random 20-character password drawn in the browser, never sent anywhere before the form is submitted. Below the accounts: connections per day over 30 days (neutral bars, today's in the accent, with the peak day written out; a graph is one SVG of three paths) and the recent events. The password generator is the only script of these pages and loads on this page only.
2. **Roles** (`/auth/settings/roles`): each role of the configuration, with its users and the services it opens.
3. **Services** (`/auth/settings/services`): each service with the roles it accepts (`Superadmin only` when it names no existing role, or the existing roles) and a warning when a role it names does not exist.
4. **Logins** (`/auth/settings/logins`): the sign-in log, below.

### Sign-in log

Every sign-in attempt that reaches the password check is recorded: when (UTC, to the second), whether it succeeded, the **login as typed**, whether that login names an account, the client address and the browser. The page shows the figures of the last 24 hours (attempts, refused, distinct addresses refused), the log newest first, 50 rows a page, with a *Refused* filter (`?r=failed`, `?p=N`).

The password is never recorded, in the database, on the page or in the logs. What a visitor types is hostile data, so it never travels raw: control characters (newline, NUL, escape sequences) and characters that hide or reorder text (zero-width characters, right-to-left overrides, line separators) are kept as a visible `\u{..}` escape, the text is cut at 64 characters with an ellipsis, it is only ever a bound SQL parameter, it is HTML-escaped where the page prints it (and the CSP forbids inline script and style anyway), and in the JSON logs (`login_failed`, `login_success` carry `login`) it stays inside one escaped string, so it cannot forge a line. An attempt blocked by the rate limiter is counted in the metrics but not listed one by one, and the log keeps at most 20,000 attempts (newest first), so a siege cannot fill the volume.

A browser refused by the role gate lands on a 403 page that names the service (only a service of the catalogue is ever named, from its id), says to ask the administrator for a role, and offers **Back to panel** and **Sign out**.

**Log in as** gives the superadmin a session as the user (flagged *impersonated*: it is not counted as a connection of that user, and is tagged *via superadmin* in the history). A banner offers **Back to superadmin**, which needs no password because the impersonated session itself proves it was opened by the superadmin. Account management is refused while impersonating.

History goes to the database (SQLite, or PostgreSQL) and is kept `storage.retention_days` days (90 by default): sessions (login, address, user agent, start, last seen, end and why), the services each session used (counts per service, never a path or query) and administrative events (users added or deleted, impersonation, failed sign-ins) and the sign-in attempts above. The per-account failed counters count only logins that name an account; a login that does not exist is counted apart and appears, as typed, in the sign-in log (a person sometimes types a password in the login field: that is the one thing this log can hold that it should not, which is why it is short-lived and visible only to the superadmin). Requests only touch memory; one writer thread flushes every 15 seconds in a single transaction, and an event is dropped (and counted in `portal_tracking_dropped_total`) rather than slowing a request down. With SQLite, sessions a previous run left open end with the reason *restart*; with PostgreSQL sessions outlive a restart, and only a session whose instance died before recording its end is closed that way (once it has been idle for longer than `session.idle_timeout`). Times are UTC.

```hcl
users {
  max_users = 64
}

storage {
  database       = "/var/lib/portal/portal.db"
  retention_days = 90
}
```

## Session signing secret

The session block supports exactly two modes:

```hcl
session {
  secret_mode = "ephemeral"
}
```

`ephemeral` generates a new 32-byte secret from the OS CSPRNG at process startup.

Or:

```hcl
session {
  secret_mode = "file"
  secret_file = "/run/secrets/portal_session_secret"
}
```

`file` reads at least 32 bytes from an absolute secret-file path. Inline HCL secrets are rejected.

The secret signs authentication return targets and the anti-CSRF tokens of the forms. With SQLite, opaque sessions remain process-local RAM state in both modes, so a restart always logs existing sessions out. With PostgreSQL the sessions are in the database, and `file` is **required**: every instance must sign with the same key, or a form served by one instance would be refused by the other (`ephemeral` is rejected with the `postgres` block).

## Several instances (PostgreSQL)

By default one process owns a SQLite file and keeps the sessions in its memory, so there can be one active instance. The optional `postgres` block moves everything to a shared PostgreSQL database instead; any number of instances then run at once, and losing one costs nothing but the requests it was serving.

```hcl
session {
  secret_mode = "file"                       # required: every instance signs with the same key
  secret_file = "/run/secrets/portal_session_secret"
}

storage {
  retention_days = 90                        # storage.database must be absent with the postgres block
}

postgres {
  host            = "postgresql-rw.data.svc.cluster.local"
  port            = 5432                     # default
  database        = "portal"
  user            = "portal"
  password_file   = "/run/secrets/portal_db_password"
  pool_size       = 8                        # default 4, 1 to 64
  connect_timeout = "5s"                     # default, 1s to 60s
}
```

The client is a blocking PostgreSQL client over plain TCP: no TLS stack and no C dependency (the connection stays inside the cluster; the server must allow `scram-sha-256` over a non-TLS connection from the pods). At startup the first connection is retried for about a minute, then the schema is created or brought up to date under an advisory lock, so several instances starting together are safe. A statement that runs for more than 15 s is cancelled, and a broken connection is replaced and the operation retried once, which is what a failover of the database looks like.

What is shared and how:

- **Sessions** are rows of a `sessions` table (digest of the cookie, client binding, identity, expiry, last use). A login writes the session and its line of the history in one transaction. Each instance keeps a copy of what it checked, for **5 seconds**: ForwardAuth asks the database about one session once per 5 s, not once per request. The binding (address and User-Agent), the idle timeout (renewed at most every 5 s of activity) and the absolute lifetime are enforced exactly as with SQLite. A logout, a deleted account or a password change ends the session in the database at once and **stops working on the other instances within 5 seconds** (the copy). If the database cannot be reached, a session checked less than 2 minutes ago is still accepted, so a failover does not sign everybody out; new sign-ins fail with `503` meanwhile.
- **Accounts** are read from the database every 5 seconds by every instance, so an account created, edited or deleted on one instance reaches the others within 5 seconds. A login the instance does not know yet makes it read the accounts again at once (at most once per second), so an account that was just created can sign in on any instance, and a session opened for it elsewhere is never ended for that reason.
- **History** is written by every instance (batches every 15 s, as with SQLite). A session ended by logout, revocation, expiry or idleness is recorded once, by the instance whose call ended it: the database hands each ended session to exactly one caller. Starting or stopping an instance never closes the sessions of the others.
- **Rate limiting stays per instance**: the failed-login limiters (per IP and global) are in memory, so with two instances an attacker gets up to twice the budget, and a lockout on one instance does not lock the other. `session.max_active_sessions` is checked against the database, but two instances racing at the limit can overshoot it by one each.
- Liveness and readiness are unchanged (`/healthz` does not touch the database). `/metrics` adds `portal_shared_store_errors_total{use="sessions|directory"}`.

`users.max_users` is checked by the instance that adds the account, against its own copy, so two instances adding at the same moment can exceed it by one.

### Moving from SQLite to PostgreSQL

`portal import-sqlite --config FILE --from SQLITE_FILE` copies the accounts (with their roles), the sessions of the history, the service accesses, the audit events and the sign-in log into the empty PostgreSQL database that the configuration names, in one transaction, and prints what it copied. It refuses a target that already holds any of that and then changes nothing. The SQLite file is only read (it may be the live one). Sessions do not move (they were in the memory of the old process): the ones the file still shows as open end where they were last seen, with the reason *restart*, and everybody signs in again after the switch. Changes made to the SQLite file after the import are not carried over, so stop changing accounts between the import and the switch.

## Local test

The Compose file contains the HCL directly via `configs.content`. The superadmin login and password are sourced from the host environment and materialized as Compose secret files; the database lives in a named volume.

```bash
export PORTAL_SUPERADMIN_LOGIN='admin'
export PORTAL_SUPERADMIN_PASSWORD='test1234'
docker compose up -d --build
```

Open:

```text
http://localhost:4181/auth/login
```

Sign in as `admin` / `test1234`; the gear opens the settings pages.

Stop with:

```bash
docker compose down
```

The local Compose intentionally publishes port 4181 only on loopback. Production deployment is expected to consume the same binary behind Traefik without publishing that port publicly.

## HCL example

```hcl
server {
  listen_address     = "0.0.0.0:4181"
  workers            = 4
  max_body_bytes     = "8KiB"
  max_connections    = 1024
  max_headers        = 64
  max_header_bytes   = "32KiB"
  header_read_timeout = "10s"
  body_read_timeout   = "10s"
}

authentication {
  superadmin_login_file    = "/run/secrets/portal_superadmin_login"
  superadmin_password_file = "/run/secrets/portal_superadmin_password"
}

roles {
  names = "ops, dev, viewer"
}

users {
  max_users = 64
}

storage {
  database       = "/var/lib/portal/portal.db"
  retention_days = 90
}

session {
  secret_mode         = "ephemeral"
  cookie_name         = "__Host-portal"
  ttl                 = "8h"
  same_site           = "Strict"
  max_active_sessions = 4096
}

security {
  max_password_bytes      = 1024
  max_forwarded_uri_bytes = 4096
  max_cookie_header_bytes = 8192

  login_rate_limit_enabled = true
  login_attempts_per_ip    = 5
  login_window             = "1m"
  login_lockout            = "5m"

  global_login_rate_limit_enabled = true
  global_login_attempts            = 100
  global_login_window              = "1m"
  global_login_lockout             = "30s"

  max_rate_limit_entries         = 4096
  max_concurrent_password_checks = 1
}

metrics {
  enabled = true
  path    = "/metrics"
}

logging {
  level = "info"
}
```

The HCL parser is intentionally a small strict subset parser. Unknown blocks, unknown keys, duplicate blocks, duplicate keys, nested blocks, invalid scalar syntax, and invalid values are rejected at startup.

## HTTP hardening

Hyper handles HTTP/1 parsing. The application configures bounded HTTP behavior:

- maximum simultaneous TCP connections;
- maximum number of request headers;
- maximum Hyper header buffer;
- header-read timeout;
- application body-size limit;
- body-read timeout;
- bounded concurrent Argon2 checks.

Dynamic `Location`, `Set-Cookie`, and `Retry-After` values are converted through typed HTTP header values. Invalid values fail closed rather than being emitted.

The crate uses `#![forbid(unsafe_code)]` for project code.

## CSRF and Origin/Referer verification

`POST /auth/login`, `POST /auth/logout` and the account routes (`POST /auth/users/add`, `/delete`, `/impersonate`, `/impersonate-end`) require both:

1. a CSRF token: for login the double-submit cookie; for the others an HMAC of the session ID and of the purpose (`logout`, `users`) carried by the button's form (no cookie to expire under a page left open, and a token for one action cannot be replayed on another); and
2. a same-origin `Origin` check, falling back to `Referer` only when `Origin` is absent.

The account routes also check on the server that the session is the superadmin's own (not an impersonated one): in K3s those paths do not go through ForwardAuth, so the portal cannot rely on the gate.

Signing out is the Sign out button's POST and ends the session at once; `GET /auth/logout` only redirects to `/`.

When requests arrive through Traefik, the expected public origin is built from the unique `X-Forwarded-Proto` and `X-Forwarded-Host` values. For direct local access, `http` plus the unique `Host` header is used. Partial forwarded-origin metadata, duplicate headers, `Origin: null`, foreign origins, userinfo, and invalid schemes are rejected.

Authentication pages return `Referrer-Policy: same-origin` so the Referer fallback remains available without leaking paths cross-origin.

## Opaque session behavior

Session cookies use:

```text
__Host-portal=<opaque random token>; Path=/; Max-Age=<ttl>; HttpOnly; SameSite=Strict; Secure   (HTTPS)
portal=<opaque random token>; Path=/; Max-Age=<ttl>; HttpOnly; SameSite=Strict                       (plain HTTP)
```

The scheme is read per request from `X-Forwarded-Proto`. A session is bound to the client address and User-Agent of the login (`session.bind = "ip_user_agent"`, also `user_agent` or `none`) and ends after `session.idle_timeout` (default `30m`) without use.

The token contains no identity, expiration timestamp, claims, or signed application data. The server stores only a SHA-256 digest of each token and its expiration time (in memory with SQLite, in the shared database with PostgreSQL).

The session store is bounded by `session.max_active_sessions`. If full, a successful password check cannot create an additional session and returns `503` rather than evicting an arbitrary live session.

## Anti-bruteforce

Two independent process-local controls run before Argon2 (with several instances each one has its own counters, see above):

- per-IP failed-password limiter;
- global failed-password limiter for distributed attacks.

Only failed password checks consume the failure budgets. Successful authentication clears the per-IP state but intentionally does not clear the global failure history.

The number of concurrent Argon2 operations is separately bounded by `max_concurrent_password_checks`.

## Prometheus

When enabled, `/metrics` exports bounded-cardinality Prometheus metrics for HTTP status/route, login success/failure, CSRF/origin failures, IP/global rate limits, session checks, active sessions, session-capacity rejection, Argon2 concurrency, HTTP connections, and uptime.

Do not put client IPs, raw URIs, passwords, cookies, or return targets into metric labels.

See `examples/prometheus.yml`.

## Global Traefik ForwardAuth

The project is designed to be installed as an entryPoint-level middleware. A request to a protected application first hits `/auth/verify`. Without a valid session, the service returns a redirect to `/auth/login` containing a validated HMAC-signed path/query return target. After successful login it returns the browser to that target.

The public auth paths `/auth/login`, `/auth/logout`, and `/auth/style.css` are explicitly allowed by `/auth/verify`, preventing a loop when the ForwardAuth middleware is global. `/auth/success` still requires a valid session.

Any other path the portal receives (`/auth/…` or the catch-all route) redirects to the login page when there is no session, whether or not the path exists, and answers 404 once signed in. In the Traefik routes this catch-all runs ForwardAuth first, then rewrites the path to `/__portal-fallback-404` so the portal's own endpoints are not exposed on arbitrary paths.

See:

- `examples/traefik-entrypoint-snippet.yaml`
- `examples/traefik-dynamic.yaml`

## Absolute JS/CSS paths

The authentication cookie has `Path=/`, therefore same-origin resources such as `/app.js`, `/css/app.css`, `/api/...`, and `/images/...` carry the same session cookie and pass the global ForwardAuth check after login.

Authentication does not rewrite application paths. If an application mounted under `/testservice` emits `/app.js`, Traefik still needs a routing rule that maps `/app.js` to the intended backend.

## Reproducible dependency resolution

After this Hyper migration, bootstrap `Cargo.lock` once on a Docker-capable development machine, then commit it:

```bash
./scripts/update-lockfile.sh
```

From that point onward, `Cargo.lock` is part of the source tree and must be reviewed/committed whenever dependencies change. Normal build paths never regenerate it:

```bash
cargo fetch --locked
cargo test --frozen
cargo build --release --frozen
```

`--frozen` is used for the actual compilation/test steps after dependencies have been fetched. The Docker build follows the same pattern.

When `Cargo.toml` is intentionally changed later, run the same maintenance script explicitly, review the resulting lockfile diff, and commit it before CI/release.

## Native commands

```text
portal run --config FILE
portal check-config --config FILE
portal healthcheck --config FILE
portal import-sqlite --config FILE --from SQLITE_FILE
portal hash-password < password.txt
portal version
```

## Release

The GitHub `Release` workflow is manually launched from the GitHub Actions UI. Its version input is independent of the version in `Cargo.toml`; for example `2.0.0` builds binaries reporting `portal 2.0.0` and publishes `v2.0.0` assets.

Supported release targets:

```text
linux/amd64  -> x86_64-unknown-linux-musl
linux/arm64  -> aarch64-unknown-linux-musl
linux/arm/v7 -> armv7-unknown-linux-musleabihf
```
