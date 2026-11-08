<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/logo-lockup-onblue.svg">
    <source media="(prefers-color-scheme: light)" srcset="assets/brand/logo-lockup.svg">
    <img alt="portal" src="assets/brand/logo-lockup.svg" width="280">
  </picture>
</p>

# portal

portal is one Rust program. It has one binary, one configuration file and one database. It does these functions:

- **Launcher.** It lists the services of a lab. A service declares itself by label (Docker Compose) or by annotation (Kubernetes IngressRoute).
- **Gate.** It is a Traefik `ForwardAuth` server. It protects each service with the session of a person.
- **Accounts.** A superadmin is defined by the environment. The superadmin makes users. A user gets permissions from the settings pages or from the configuration.
- **Keys.** A user makes keys. A key lets a program act for its owner, with limits. A key has one of two kinds: `mcp_` or `api_`.
- **Decision point.** portal gives an identity token to the services. It is also an AuthZEN PDP for them. It is the access gateway and the PDP of section 11 of the MCP server contract.
- **History.** portal keeps a record of who signed in and what they used.
- **Inbox.** The Inbox sends alerts to Android phones. It makes sure that each phone shows each alert one time. The Android app has the name Portal Inbox.

Each part except the launcher, the gate and the accounts is optional. A file with no block `keys` and no block `inbox` runs the portal as a panel of services only. See "Settings pages and history" for the switches.

The runtime image is `FROM scratch`. The HTTP stack is Hyper 1.x on Tokio. Discovery uses `reqwest` (rustls) for the Kubernetes API and for health probes. Pages are rendered in Rust. The only scripts are the theme selector, the script of the settings pages and the script of the notifications. The script of the settings pages draws the editor of grants and the matrices. It also gives the Copy buttons. The script of the notifications asks for new alerts, shows the toasts and updates the badge. There is no JavaScript framework, no CDN and no runtime shell. Nothing is fetched from the internet at runtime.

The data lives in one database. By default it is one SQLite file. The SQLite library is bundled in the static binary. To run several active instances, use a shared PostgreSQL database instead (see [Several instances](#several-instances-postgresql)). There is no Redis.

## Architecture

```text
 Browser ── Traefik ──ForwardAuth──▶ portal  listen_address (4181)      pages, launcher, /auth/verify, /metrics
 Key callers ── Traefik ──ForwardAuth (/auth/verify-key)──▶ portal  same port   MCP keys
 Services ──────────AuthZEN────────▶ portal  pdp.listen_address (4182)  decisions
 Senders ── Traefik, no ForwardAuth ──▶ portal  listen_address (4181)      /api/v1/*: alerts, webhooks (same port as the pages)
 Phones ── Traefik, no ForwardAuth ───▶ portal  listen_address (4181)      /v1/*: enrollment, sessions (WebSocket, Noise)
                                     portal  inbox.cluster.listen       other instances (optional)
                                       │
                                       └── one database: SQLite file, or PostgreSQL
```

| Listener | Key | Use | Who reaches it |
|---|---|---|---|
| Portal | `server.listen_address` | The pages of the portal, the settings, `/auth/verify`, `/auth/verify-key`, `/healthz` and `/metrics`. With the block `inbox`, also the Inbox. The Inbox has the API for the senders and the webhooks (`/api/v1/*`, `/api/version`). It also has the routes of the phones (`/v1/enroll`, `/v1/session`) and `/readyz`. | Traefik and Prometheus. Traefik sends `/v1/`, `/api/v1/` and `/api/version` to it without ForwardAuth. |
| PDP | `pdp.listen_address` | The AuthZEN decisions. Optional. | The services only. Do not route it through Traefik. |
| Cluster | `inbox.cluster.listen` | Copies of alerts between instances. Optional. | The pods of the service only. |

The Inbox is a feature of the portal. The pages, the senders, the webhooks and the phones use one port. The two routes of the phones are HTTP upgrade requests. The content is Noise-encrypted inside WebSocket frames. A reverse proxy carries them with path routing, so a separate exposure, connection budget and retry need no second port. The metrics of the portal and of the Inbox are in one Prometheus text at `metrics.path` of the portal listener.

portal has no TLS code on any listener. Traefik terminates TLS. The portal reads the scheme of each request from `X-Forwarded-Proto`. See "Security model".

The Android app is a client of the Inbox. It holds one encrypted connection to the portal for each server. It receives alerts only. It does not use a push service of a third party.

The repository has these parts:

| Path | Content |
|---|---|
| `src/` | The binary `portal`. The code of the Inbox is in `src/inbox/`. |
| `crates/inbox-core` | The protocol and the cryptography of the phones, with no I/O. The server and the app share it. |
| `crates/inbox-android`, `android/` | The JNI bridge and the Android app (name Portal Inbox, application id `app.portalinbox`). |
| `web/` | The style sheets, the icons and the scripts of the pages. |
| `deploy/`, `docker-compose.yml`, `docker/` | The Compose stack, the K3s manifests and the Dockerfile. |
| `demo/` | A local demo with Traefik and three example services. |
| `tools/` | The scripts for the build, the tests, the emulator and the release. |
| `docs/` | The design document (`docs/design-inbox.md`) and the chapter of the Inbox (`docs/inbox/`). |
| `examples/` | Examples of Traefik and Prometheus files. |

## Quick start

Docker is the only tool that you need.

1. Build the image: `tools/build.sh image`.
2. Make the secrets: `deploy/compose/init-secrets.sh`.
3. Start the stack: `docker compose up -d`.
4. Open `http://127.0.0.1:4181/auth/login`. Sign in with the files `superadmin_login` and `superadmin_password` in `deploy/compose/secrets/`.
5. Stop the stack and delete its database: `docker compose down -v`.

The stack has the portal with the Inbox, and PostgreSQL. The portal on port 4181 serves the pages, the API of the senders and the phones. No proxy is in front of it, so no ForwardAuth applies. [`deploy/README.md`](deploy/README.md) describes the stack, the variant with two instances and the K3s manifests. [`demo/README.md`](demo/README.md) describes the demo with Traefik, services and keys. [`docs/inbox/README.md`](docs/inbox/README.md) describes the Inbox and the test with the Android emulator.

## Security model

- **One identity.** An account has one login and one set of permissions for the launcher, the keys and the Inbox. The permissions for the keys and the Inbox are on one internal scope, the service `portal`.
- **Passwords.** The database keeps only Argon2id hashes. Each sign-in costs one Argon2 check, also for an unknown login. The failure message is always the same.
- **Sessions.** A session is an opaque random token. The server keeps only its SHA-256 digest. The session is bound to the client address and the browser.
- **Gate.** A new user has no permission. The gate checks the forwarded path in every way that a proxy can interpret it. It lets a request pass only if every interpretation is allowed.
- **Keys.** A key never gets more than its owner. The portal keeps only the digest of the key text. A verifier checks the form and the kind of a key before any lookup.
- **Inbox.** A phone enrolls with a one-time code of an account. The session is Noise IK to a key that the phone pinned. A sender proves itself with an `api_` key. The permissions of the portal features decide who sends, reads and revokes.
- **Configuration.** The file is the only source. A secret comes only from `_env` or `_file`. An unknown key stops the program at start.
- **Logs.** The log never holds a password, a key text or a secret. The effective configuration shows `<hidden>` for each secret.
- **Code.** The project code has no `unsafe` Rust. The binary is static.

### TLS and Traefik

The portal knows nothing about TLS. Its listeners speak plain HTTP. Traefik terminates TLS at the entry point and sends plain HTTP to the portal on a private network. Run the portal in this way:

1. Publish only Traefik. Do not publish the ports of the portal. The PDP port is for the services only.
2. Make the ForwardAuth middleware `portal-user` for `/auth/verify`. It copies `Cookie` and `User-Agent` to the portal and the identity token back. Set it on the entry point, or on each service router.
3. Route `/auth/` and `/` to the portal. Do not apply ForwardAuth to `/auth/`: the portal checks the session itself.
4. Route `/v1/`, `/api/v1/` and `/api/version` to the portal without ForwardAuth. Add the middleware `retry`. A phone and a sender have no session.
5. For key routes of agents, use a second ForwardAuth middleware that points to `/auth/verify-key`. It copies `Authorization`.
6. Set `server.trusted_proxies` to the network of Traefik. The portal then reads `X-Forwarded-For`, `X-Forwarded-Proto` and `X-Forwarded-Host` from Traefik only.
7. Keep `trustForwardHeader: false` on each ForwardAuth middleware. Traefik then writes `X-Forwarded-Uri` and `X-Forwarded-Method` from the real request.

Set `trustForwardHeader: true` only when each hop in front of Traefik overwrites or removes the `X-Forwarded-Uri` and `X-Forwarded-Method` headers of the client. Otherwise the client chooses the request that `/auth/verify` checks. For example, a request with `X-Forwarded-Uri: /auth/login` passes the gate without a session. Behind a TCP load balancer, give the client address with the PROXY protocol of the entry point. Behind an HTTP proxy, keep `false` and set `session.bind = "user_agent"`.

HTTPS gives the session cookie its `__Host-` name and its `Secure` attribute. A phone and a sender send secrets in clear text on plain HTTP. Use an HTTPS `inbox.devices.public_address` and an HTTPS URL for the senders. `demo/` and `examples/` have Traefik files that you can use. See also "Global Traefik ForwardAuth".

The file [`SECURITY.md`](SECURITY.md) gives the invariants. The page [`docs/inbox/SECURITY.md`](docs/inbox/SECURITY.md) gives the model of the Inbox.

## Authentication model

The login page asks for a **login** and a **password**. When the visitor comes from a protected address, a band at the top of the card shows the page that the visitor asked for. The band shows the path, for every page. Without a protected address, the card has no band. See `docs/login-hardening.md`.

The **superadmin** is the one account that is not in the database. Its login and its password come from an environment variable or from a file. The configuration names the source (see [Secrets](#secrets-_env-and-_file)):

```hcl
authentication {
  superadmin_login_file    = "/run/secrets/portal_superadmin_login"
  superadmin_password_file = "/run/secrets/portal_superadmin_password"
  # The key of the TOTP secrets (64 hexadecimal characters). The policy `authentication.mfa` is "optional" by default.
  totp_key_file            = "/run/secrets/portal_totp_key"
}
```

The superadmin login has no upper-case letter and no space. The program refuses a login that breaks this rule at start. The sign-in form compares logins in lower case. The program does not change the login.

At start, the program reads the password with a size limit. The program hashes the password once with Argon2id. The program then erases the plaintext from memory. The superadmin can see and open everything. The superadmin is the only account that can manage the other accounts.

The superadmin adds and deletes **users** from the settings pages (the gear of the launcher). The database stores their Argon2id hashes. The database is the SQLite file `storage.database`, or PostgreSQL. The request path reads an in-memory copy. The request path never waits on the database. A user has a list of grants (see [Grants and the gate](#grants-and-the-gate)). Each login attempt runs exactly one Argon2 check. This applies to any login name. The check uses the superadmin hash, the hash of the account, or a dummy hash. The failure message is the same in all cases. Neither the time nor the text shows whether a login exists.

A successful login creates a 256-bit random **opaque session ID**. The browser receives only that random ID. With SQLite, the server keeps the active-session set in bounded RAM. Each session has an expiration time. Logout deletes the session at once. A restart of the process clears all sessions. With PostgreSQL, the sessions are in the shared database. Only the SHA-256 of the cookie is stored. The sessions are valid on every instance and survive a restart. A session of the superadmin is valid only with the superadmin credentials that opened it. A session that the superadmin opened with **Log in as** follows the same rule. A new superadmin login, password or session secret ends these sessions on every instance (see [Several instances](#several-instances-postgresql)).

### Remember me

The sign-in form has a box "Stay signed in for 30 days", at the left edge of the card under the password field. The box is not ticked at first. The label is the only text of the box. The number of days comes from `session.remember_ttl`. The box needs no script, and the label is a part of the box: a click on the words ticks it.

| | Box not ticked | Box ticked |
|---|---|---|
| Life of the session from the sign-in | `session.ttl`, default 24 hours | `session.remember_ttl`, default 30 days |
| Idle time | `session.idle_timeout`, default 30 minutes | `session.remember_idle_timeout`, default 7 days |
| Cookie | No `Max-Age`: the browser deletes it when it ends its session | `Max-Age` is the life of the session |

Each life is absolute. A session that is in use still ends at the end of its life. The idle time of a remembered session is separate. Without it, the idle time of 30 minutes would end a remembered session at each pause. The value of `session.remember_idle_timeout` is at least `session.idle_timeout` and at most `session.remember_ttl`. The server refuses the file at start when these keys contradict each other.

- Set `session.remember = false` to remove the box. The form then has no box, and the server ignores the field.
- A remembered session is in the shared store like any other session. With PostgreSQL, it is valid on every instance and survives a restart. With SQLite, the sessions are in memory. A restart ends them, so the cookie of a remembered session does not outlive the process.
- A logout, a deleted account, a new password, a new superadmin login and **End all sessions** end a remembered session, as they end any other session.
- **Log in as** never gives the remembered rule. The impersonated session has the life `session.ttl`, and it ends at the latest when the session of the superadmin ends. **Back to superadmin** opens a session that is not remembered. The superadmin ticks the box again at the next sign-in.
- The page after a refused sign-in never shows the box ticked. This way the answer to a refusal does not depend on what the client sent (see `docs/login-hardening.md`). The Activity list marks a remembered session with the badge "kept".
- `session.bind` stays as it is. With the default `ip_user_agent`, the session works only from the address that signed in. A change of address, for example on a mobile network, ends the use of a remembered session: the person signs in again. `user_agent` is the usual choice for sites that use remembered sessions on phones. It is less strict: the session then works from any address with the same User-Agent. The default does not change.

A persistent cookie is the usual risk of this option (OWASP Session Management Cheat Sheet). Use these measures:

- Keep `session.remember_ttl` as short as the site accepts. The maximum is 90 days.
- Use HTTPS, so that the cookie has the `Secure` attribute.
- Recommendation, not done in this version: ask for a new sign-in before a sensitive action in a remembered session. Examples are to create a key, to use **Log in as** and to change a password.

### Second factor (TOTP)

An account can have a second factor: a code of 6 digits from an authenticator app. The code follows RFC 6238 (HMAC-SHA-1, a step of 30 seconds). Google Authenticator, Aegis, 1Password and the other usual apps read it. The portal accepts the code of the current step and of the step before and after it. Passkeys (WebAuthn) are the next step: they stop phishing, and TOTP does not.

The key `authentication.mfa` sets the policy:

| Value | Effect |
|---|---|
| `off` | The portal shows no page of the second factor and never asks for a code. The key `authentication.totp_key` must not be set. |
| `optional` (default) | Each account chooses to turn it on in the page **Security**. An account that did not choose signs in with the password only. |
| `required` | Each account, the superadmin too, must enrol after the password step. The session starts only after the enrolment. The account cannot turn it off. |

Any value other than `off` needs the key `authentication.totp_key_env` or `authentication.totp_key_file`. It has 64 hexadecimal characters: make it with `openssl rand -hex 32`. All instances use the same key. The program stops at start when the key is absent, when its format is wrong, or when it is not the key that sealed the secrets of the database. The default `optional` therefore needs a key in each configuration. No separate key sets the policy of the superadmin. Under `optional`, the sign-in of the superadmin stays a password sign-in, so that scripts that sign in keep working. Use `required` to force it.

**Sign-in.** The first step is the sign-in form. A wrong password, an unknown login and a failed check of the form give the same answer as before (see `docs/login-hardening.md`). When the password is right and the account has a second factor, the portal opens no session. It shows a card with one field for the code. The field takes a code of the app. The card has a section "Use a recovery code". The state between the two steps is a signed token in a hidden field. It lives 5 minutes. It is bound to the account, to the client (the session binding), to the page, and to the box "Remember me". It is also bound to the password and to the second factor of the account at the password step. A new password, a reset of the second factor or a new superadmin credential thus sends the person back to the password step. No instance keeps a state, so the second step can go to another instance. Only a person who knows the password reaches the second step. This is accepted.

**Enrolment.** The page **Security** (the gear, for each account) shows the state. **Turn on** shows a QR code as inline SVG markup. The page also shows the secret in groups of four characters. The QR code holds the `otpauth://` link. The page does not print the link, because the link holds the name of the instance. The person types the first code. The portal then shows 10 recovery codes one time (`xxxxx-xxxxx`) with a **Copy** button (script only). The portal stores the secret sealed with AES-256-GCM, and the recovery codes as keyed hashes. A recovery code works one time. **New recovery codes** and **Turn off** ask the password and a code. The page works without script.

**Lost phone.** Use a recovery code. When the person has no code left, the superadmin resets the second factor of the account: the page of the account in the tab Access has a button **Reset the two-factor** with a confirmation in place. The reset ends the sessions of the account, as a new password does. Activity shows the event. The superadmin has no account above it: use the command `portal reset-two-factor --config FILE --user LOGIN`. The command works with a stopped SQLite portal, or with PostgreSQL. `--all` removes every second factor and the check value of the key, so that a new `authentication.totp_key` can serve.

**Brute force.** A wrong code counts against the account in the database, so every instance adds to the count. Five wrong codes in 10 minutes lock the second step of the account for 5 minutes. The answer is status 429 with `Retry-After`. No lock is permanent. One pending token takes 5 wrong codes. The address has its own budget: the limiter of the sign-in, with the keys `security.login_attempts_per_ip`, `security.login_window` and `security.login_lockout`. The counter `portal_login_attempts_total{result="mfa_failure"}` counts the wrong codes. The log line `login_failed` has `reason=mfa`. The portal records the last accepted time step of each account in the database: a code of the same step or of an older step is refused. A code is good one time.

**Sessions.** A session records `mfa` like `remembered`. Activity marks a session of the second factor with the badge "2fa". A session of **Log in as** needs no code of the account. It has no `mfa` flag, and it has no page **Security**: the session of the superadmin is the one that counts. Keys (`mcp_`, `api_`), the enrollment codes of the phones and the ForwardAuth path do not change: a session exists only after the full sign-in. Recommendation, not done in this version: ask for a fresh second factor before a sensitive action.

**Dependency.** The QR code needs the crate `qrcode` (pure Rust, no dependency of its own, features `svg` only). HMAC-SHA-1 and AES-256-GCM come from `aws-lc-rs`, which the portal already uses.

**Schema.** The portal has no deployment with users. The second factor changed the schema version 8 in place: it did not add a version, and it has no migration. A database that the earlier version 8 made lacks the tables `totp_credentials`, `totp_recovery` and `totp_key_check` and the column `mfa`. The portal stops at start and says so. Make a new database.

## Grants and the gate

The settings pages say "permission" where this document and the protocol say "grant". The two words name the same thing.

### Grants

A grant is an action on a node of the resource tree of a service (contract 11.8). A grant has this form:

```text
<action> on service:<id>[/<type>:<resource id>]...
```

- `access on service:grafana`: the grant applies to the whole service.
- `query.run.read on service:clickhouse-dash/host:local`: the grant applies to the host `local` and to each node below it.
- `query.run.write on service:clickhouse-dash/host:local/database:analytics`: the grant applies to the database `analytics` of the host `local`.

The `<id>` is the id of the service in the catalogue. The id is the label `portal.id`, or the slug of the group and the name. The part after the service has 0 to 4 segments.

- `access` lets the user open the service. portal decides `access` in `/auth/verify`.
- The other actions are the actions that the service publishes at `/.well-known/procurauth/actions` (protocol 5.1). The service decides these actions. The service asks the PDP of portal.
- A grant covers the node that it names and each node below it. portal compares the segments one by one (prefix match). For example, `query.run.read on service:db` covers each host and each database of `db`.
- A grant below the service does not cover its parent, a node of another branch, or the whole service.
- The service gives its tree of resource types. The top-level list `resource_types` of its document declares each type and its parent: `service` or another declared type. A document without this list has one level: each type of an action has the parent `service`.
- Each action of the document has a list `resource_types`: the types on which a grant for this action is valid. The type `service` is the whole service: it is never a segment.
- The types of the segments must follow the parents. With `host` below `service` and `database` below `host`, write `host:<id>/database:<id>`. A grant on `database:<id>` alone does not follow the tree.
- A grant below the service applies only when the last known document of the service follows it. The action must list the type of the last segment, and the segments must follow the parents. Else, the PDP does not apply the grant. If portal never read the document of the service, the PDP does not apply any grant below this service. A grant on the whole service does not need a declaration.
- A type starts with `a-z`, then has `a-z`, `0-9` and `_`, 1 to 32 characters.
- Each resource id is percent-encoded (RFC 3986). Each byte outside `A-Z a-z 0-9 - . _ ~` is `%XX`, with upper-case hex digits. For example, the id `a/b` is `a%2Fb`. Thus a `/` or a `:` in an id never divides a segment.
- The encoding is canonical. portal refuses a different encoding of the same id, for example `%2f` or `%61`.
- A decoded id has 1 to 256 bytes, without a control character. portal uses the id as it is. portal does not change its case. The settings pages show the decoded ids.
- `access` has no grant below the service. `access` opens the whole service.
- A grant can have the property `confirm` (see "Approval of an action"). It is not a limit: it has no value and no counter, and the text form of the grant does not show it.
- A new user has no grant. The user sees no service until the superadmin gives a grant.
- The superadmin has all grants. The superadmin has no `user` block.

portal has no groups of users. Each user has their own grants.

### Approval of an action

A grant can ask for the approval of a person before each use of its action. The grant then has the property `confirm`. This release stores and shows the property. The PDP does not use it yet: a later release makes the PDP answer with a reason that asks for an approval.

- A service marks the actions that can ask. The document of the service has `"approval": "optional"` on the action. This is the only value of the member. Any other value, or a member of another type, makes the whole document bad.
- The grant editor shows a lock button on the row of a ticked permission whose action has `"approval": "optional"`. The tooltip says "Asks for an approval". An open lock means no approval. A closed lock means that the permission asks for an approval. A grant of another action has no lock.
- The server refuses `confirm` on a grant of the service `portal` and on the action `access`. A form that asks for it gets a bad grant. A file that asks for it stops at start.
- The decision for a person asks for an approval when a grant that covers the action has `confirm`. The decision for a key asks when a grant of the key has `confirm`. It also asks when a grant of the owner has `confirm`. The two sides combine with OR, as the limits do. When the owner is the superadmin, only the grants of the key count.
- A key shows the chip `confirm` in the list of the keys. The editor of a key shows the lock closed and locked when a grant of the owner asks for the approval. The person cannot open it, because the key cannot go beyond its owner.
- Two grants that differ only by `confirm` count as one grant with `confirm`.
- In the file, an item of `grants` can be an object: `{ grant = "query.run.write on service:clickhouse-dash/host:prod", confirm = true }`. The value is `true` or `false`, without quotes. The default is `false`.

### Where the grants come from

The key `access.grants_mode` gives the source of the grants. The key uses the same modes as the keys in contract 3.5.

| Mode | Grants of the `user` blocks | Grants of the settings pages |
|---|---|---|
| `static` | The server uses them. | The server does not use them. The settings pages show the grants. They do not change them. If the database holds grants, the server writes a warning at start. |
| `dynamic` (default) | A `user` block stops the server at start. | The superadmin gives and removes them. The database keeps them. |
| `hybrid` | The server uses them. They are read-only in the settings pages. | The superadmin gives and removes them. |

```hcl
access {
  grants_mode = "hybrid"
}

user "alice" {
  grants = ["access on service:grafana", "query.run.read on service:db"]
}
```

The server reads the `user` blocks at each start. When you remove a grant from the file, the grant stops at the next start. The server does not write the grants of the configuration to the database.

A `user` block can give a grant below the service, for example `"query.run.write on service:db/host:local/database:analytics"`. A grant with a bad form stops the start: a bad type, a bad encoding, or more than 4 segments. At start, portal does not know the services: the catalogue and the actions documents come at run time. Thus a grant that does not follow the tree of its service does not stop the start. The Services settings page shows this grant under **Permissions to check**. At run time, the PDP does not apply this grant (see "AuthZEN PDP").

### The gate

`/auth/verify` finds the service from the path in `X-Forwarded-Uri`. The service owns the longest path prefix of its `href`. portal reads the `href` with the same percent-decode as the request path, so `/my%20app/` owns `/my%20app/x`. portal then decides `access` in its own process. portal uses the grants in memory.

Traefik reads ``PathPrefix(`/files`)`` as a text prefix: it also routes `/filesX` and `/files.json` to that service. The gate therefore gives these paths to the service of `/files/` too. Only a user with `access` on the service passes, and the request gets no identity token. The longest prefix wins, as in Traefik: `/grafana/administrator` belongs to the service of `/grafana/admin/`. The paths below `/auth/` stay with the portal.

The router rule of a service must not match a path outside the text prefix of its `href`. For example, do not give the `href` `/pgadmin/browser/` to a service with the rule ``PathPrefix(`/pgadmin`)``: the gate does not own `/pgadmin/misc/`.

Traefik routes on the clean path, but sends the raw URI. portal therefore reads the path in every way that a proxy or a backend can read it. These ways include `%2e%2e`, `//`, `%2f` as a character and as a separator, and `\`. portal lets the request through only if every way is permitted. For example, portal judges `/grafana/../clickhouse/` as a request for ClickHouse.

- A refused browser navigation goes to `/auth/denied`. Traefik limits the body of a ForwardAuth answer. The answer cannot hold the page. Other requests get a plain `403`.
- The page `/auth/denied` does not give the name of a service. A user does not see a service that is not open to that user.
- `/auth/*` is always open to a signed-in user. A path that no service owns (an absolute asset path) is open to each signed-in user.
- The previous rule needs a complete catalogue. The catalogue is complete when each configured discovery source gave its full list. The instance opens its ports only after this (see "Start and stop"), so a user never reaches the gate with an incomplete catalogue. A source that failed at setup never makes the catalogue complete. The metric `portal_catalog_complete` shows the state. After the first full list, a failed listing keeps the last good list, and the catalogue stays complete.
- A workload with unusable labels is not listed. The gate keeps the path of its `href` closed: only the superadmin passes it. The same rule applies to a workload whose id another workload already uses with another `href`. The catalogue problems name both cases.
- A list that misses a workload does not open its path. Examples are a container that restarts and a container that Compose recreates. The path keeps its last owner for 20 refreshes (5 minutes at the default `discovery.interval`). A new workload has no owner until the first refresh that lists it.
- A workload that does not ask for a listing also keeps the path of its `href` closed. Examples are `portal.enabled=false`, `homepage.*` labels without `homepage.name`, and `gethomepage.dev/*` annotations without `gethomepage.dev/enabled=true`. Traefik still routes such a workload. To open a path to each signed-in user, do not give it as the `href` of a hidden workload.
- The launcher shows only the services for which the user has `access`. The discovery problems name workloads. Only the superadmin sees these problems.
- An external service owns no path of the gate. See "External services".

portal stays a ForwardAuth server. Traefik is the only proxy. portal receives only the headers of a request. portal never receives the body of a request or of a response.

### Identity token

With the `identity` block, `/auth/verify` gives an identity token (protocol section 9) to each service that a user can open. The token is in the response header `identity.header`. The default is `X-Identity-Token`. Traefik copies the token to the request. Add the header to `authResponseHeaders` of the ForwardAuth middleware. Traefik then removes a header of the same name that the client sends.

```hcl
identity {
  issuer           = "https://portal.home.internal"
  header           = "X-Identity-Token"
  token_ttl        = "5m"
  signing_key_file = "/run/secrets/portal_identity_key"
}
```

The token is a JWT. Its algorithm is `Ed25519`. The header has `kid`. `kid` is the RFC 7638 thumbprint of the public key. The claims are:

| Claim | Value |
|---|---|
| `iss` | `identity.issuer` |
| `sub` | the login of the user (the superadmin has its own login) |
| `aud` | the id of the service |
| `iat`, `exp` | the time of issue, and `iat + identity.token_ttl` |

The token contains no permission. The service asks the PDP. portal keeps one token for each session and each service. portal gives the same token again until half of its life is gone. A path that no service owns gets no token.

Make the key with OpenSSL. Give the public key to each service as a JWK Set (protocol 9.4):

```bash
openssl genpkey -algorithm ed25519 -out portal_identity_key
portal identity-public-key --config /etc/portal/config.hcl > portal_identity_public.jwks
```

The key file holds a PKCS#8 key in PEM format. The file can also hold the base64 text of the PEM on one line. `portal check-config` reads the key. It then prints the `kid`.

### AuthZEN PDP

With the `pdp` block, portal is an AuthZEN PDP (contract 11.5) on a second listener. Do not expose this listener through Traefik. Only the services use it.

```hcl
pdp {
  listen_address = "0.0.0.0:4182"
  url            = "http://portal-pdp.edge.svc.cluster.local:4182"
}

pdp_client "db" {
  token_file = "/run/secrets/portal_pdp_token_db"
}
```

| Endpoint | Use |
|---|---|
| `POST /access/v1/evaluation` | One decision: `subject`, `action`, `resource`, `context`. The answer is `{"decision": true}` or `{"decision": false}`. If the subject does not exist or the resource is not valid, the answer has a reason (see the rules below). |
| `POST /access/v1/evaluations` | A batch. The top-level members are the defaults of each item. `options.evaluations_semantic` is `execute_all` (default), `deny_on_first_deny` or `permit_on_first_permit`. The batch has at most `pdp.max_evaluations` items. |
| `POST /access/v1/search/action` | The actions that the subject can do on the resource. The resource has the same form as in an evaluation, with its ancestors. The answer is one page. |
| `GET /.well-known/authzen-configuration` | The endpoints under `pdp.url`. This endpoint needs no token. |

Rules:

- Each server has its own `pdp_client` block and its own token. The label is the name of the server. The label is the id of its service. This id is also the audience of its identity tokens. The server sends `Authorization: Bearer <token>`. A request without a valid token gets status `401`.
- The PDP answers a server only for its own resources. The resource `{"type": "service", "id": "<name of the server>"}` is the whole service. It has no `ancestors`, or an empty list.
- A resource of another type is a node below the service, for example a database. `resource.properties.ancestors` gives the chain from the service to the parent of the node: `[{"type": "service", "id": "db"}, {"type": "host", "id": "local"}]`. The ids are not encoded in a request.
- A node below the service must have this chain. A node of depth 1, for example the host `local`, has the chain `[{"type": "service", "id": "db"}]`. A node without `ancestors`, or with an empty list, gets `invalid_resource`.
- The first ancestor must be the service of the server that asks: its token tells which server it is. The chain (the ancestors, then the resource) must follow the parents of the last known document of the service. The chain has at most 4 nodes below the service.
- A resource that does not obey these rules gets `{"decision": false, "context": {"reason": "invalid_resource"}}`. The service of another server gets the same answer. This check comes before the search of the login or the key: the superadmin and an unknown user get the same answer.
- An `invalid_resource` answer also starts an early read of the document of the server that asks (see "Actions of the services"). The server can have a new tree that portal does not know yet.
- If portal has no good document of the service, the decision is `false`, without a reason. This applies to each node and to the whole service. Only a request for `access` on the whole service is not affected. Without the document, the PDP cannot apply the ceilings and the budgets of a grant. The reason in the log is `the document of the service is not known`. After a change of the `portal.actions` URL of a service, a failed read leaves no document.
- Without a good document, the PDP still checks the start of the chain. A node without ancestors, or with a first ancestor that is not this service, gets `invalid_resource`.
- A grant covers the node that it names and each node below it. The PDP compares the types and the decoded ids of the chain segment by segment, as exact text. The three endpoints use the same rule.
- `context.match` sets how a grant matches the resource. The default is `"node"`: the rule above. With `"subtree"`, a grant on a node below the resource also gives `true`. Example: a user with a grant on one database of the host `local` gets `true` for the host `local`, so that the service can show this host.
- A service uses a `"subtree"` decision only to show or hide a navigation element. Each operation asks with `"node"` on its exact resource (contract 11.5).
- In a batch item, another value of `context.match`, or a value that is not a text, gives `{"decision": false, "context": {"reason": "invalid_request"}}`. This check comes first. A `resource.properties.ancestors` that is not an array of objects with `type` and `id` gives the same answer. In a request with one evaluation and in the action search, these values get status `400` (protocol 11.1).
- In a batch, the `context` of an item replaces the `context` at the top of the request (AuthZEN 1.0). An item without `context` uses the top-level one. The action search reads `context.match` too.
- A grant below the service applies only when the last known document of the service follows it (see "Grants"). The PDP keeps the last document that it read for each service. A failed read does not remove it. A new document that changes the tree removes the effect of the grants that do not follow it, at once. The PDP ignores such a grant: the reason in the log is `no grant` when no other grant applies. A grant on the whole service is not affected.
- The subject type must be `user`. The subject must be an account, the superadmin or a key. If not, the decision is `false`.
- A subject that the PDP does not know gets a reason: `{"decision": false, "context": {"reason": "unknown_subject"}}`. This applies to a login that no account has, and to a subject type that is not `user`. The batch gives the same answer in each item of this kind. A service can use it to tell an unknown user from a user without a grant.
- An id with a colon is not a login: the id `key:<id>` names a key (protocol 3.2, see "Keys and limits"). Without the block `keys`, each key gets `unknown_subject`. A key that does not exist and an expired key get it too. A key older than the `max_key_lifetime` of its owner, or whose owner cannot use keys, also gets it. An id with another prefix gets the same answer.
- The reasons `invalid_resource` and `invalid_request` tell that the request of the service is not correct. They do not show the grants of a user.
- No other denial has a reason, except `budget_exceeded` (see "Keys and limits"). The answer is `{"decision": false}`, so that it does not show the grants of a user.
- The PDP fails closed. A request that the PDP cannot read gets status `400` and no decision.
- The PDP returns the header `X-Request-ID` of the request.
- The log records each decision (`pdp_decision`). The record has the server, the subject, the action, the resource, the decision and the reason. The resource is the chain in the text form of a grant path, with the ids encoded, for example `service:db/host:local/database:analytics`. The record also has the match: `node`, `subtree` or `invalid`.
- The PDP has no TLS. The deployment must limit the network of the PDP to the services and portal. This differs from AuthZEN, which requires TLS.
- In K3s, the NetworkPolicy `allow-portal-pdp` (`deploy/k3s/network-policy.yaml`) admits to the port 4182 only the pods with the label `portal-pdp-client: "true"`. These pods must be in the namespace `apps`, `observability` or `edge`. Give this label to the pod template of each server that asks the PDP. The policy `allow-portal-http` opens only the port 4181 of portal, and only to the Traefik pods and the Prometheus pod. portal trusts `X-Forwarded-For` from the pod network (`server.trusted_proxies`). No other pod may reach the port 4181.

A request about a database of the host `local`:

```json
{
  "subject": {"type": "user", "id": "alice"},
  "action": {"name": "query.run.write"},
  "resource": {
    "type": "database",
    "id": "analytics",
    "properties": {"ancestors": [{"type": "service", "id": "db"}, {"type": "host", "id": "local"}]}
  }
}
```

### Actions of the services

portal reads the actions of each service at `/.well-known/procurauth/actions`. The URL comes from the label `portal.actions`. This label is an absolute http or https URL. If the label is absent, the URL is the origin of the health URL (`portal.health`) with this path. A service without these two labels has only `access`.

- The read has a timeout (`access.actions_timeout`) and a size limit (`access.max_document_bytes`, 256 KiB by default). portal reads the document again after `access.actions_refresh`.
- For an https URL, portal verifies the certificate and the host name with the embedded Mozilla roots. A read that fails this check gives no document. For a service with a private CA, use an http URL on the internal network.
- When the PDP gives `invalid_resource` to a service, portal reads the document of this service again at once. The service can have a new tree that portal does not know yet. portal does this at most once in each `access.actions_refresh` interval for each service. The early read has the same timeout, size limit and number of concurrent reads as the other reads. The log has one line `actions_read_early` with the service.
- The field `server` of the document must be the id of the service. Set `portal.id` to the name of the server.
- An action can have the member `approval`. Its only value is `"optional"`: a grant of this action can have `confirm`. The member is optional. Any other value makes the document bad. See "Approval of an action".
- The top-level list `resource_types` declares the tree of types (protocol 5.2). Each item has `name`, `parent` and `description`. A name starts with `a-z`, then has `a-z`, `0-9` and `_`, 1 to 32 characters. It is not `service`. The parent is `service` or another declared type. The tree has no cycle and at most 4 levels below `service`. The list has at most 16 types.
- When the document has this list, each type in the list of an action is `service` or a declared type. A type occurs one time in the list of an action.
- The document can have an optional list `resources`. This list names the nodes of the tree that the service knows, for example its hosts. Each item has `path` (the text form of a grant, for example `host:prod`), an optional `label`, and an optional list `actions`. A path follows the declared types. A path occurs one time. Each action of an item lists the type of the node. Without `actions`, the item takes each action that lists its type. The settings pages show each named resource apart, with the actions that apply on it only. The list has 256 items at most. The list is not part of AuthZEN: a service that does not give it works as before, and the superadmin types the ids.
- A document that does not obey one of these rules is bad as a whole. portal keeps the last good document, and the Services page shows the problem.

```json
{
  "server": "db",
  "resource_types": [
    {"name": "host", "parent": "service", "description": "One database server."},
    {"name": "database", "parent": "host", "description": "One database of a host."}
  ],
  "actions": [
    {"name": "query.run.read", "resource_types": ["service", "host", "database"], "description": "Run read-only queries."},
    {"name": "explorer.read", "resource_types": ["service", "host", "database"], "description": "Browse the databases."}
  ],
  "resources": [
    {"path": "host:prod", "label": "Production"},
    {"path": "host:replica", "label": "Read replica", "actions": ["query.run.read"]}
  ]
}
```

- A `404` means that the service does not publish actions. The service then has only `access`. `access` is the full permission. portal lets the user in. The service does no other check.
- Any other failure keeps the last good document. The Services settings page shows the problem. The PDP also uses this document: see "AuthZEN PDP".
- The page of a user has one editor of grants, the same editor as the forms of the keys (see "Editor of grants"). The editor shows the tree of the resources of a service and the actions of the chosen node. A person who gives a grant on a resource does not type its id: the editor lists the resources. portal takes them from the member `resources` of the action document and from the options of the service (portal 3.1a, protocol 5.6).
- One **Save access** sends every grant of the account, with its limit values. The form replaces the grants of the account. The grants on the service `portal` (the rights on the keys) have their own form. A grant that the document of the service does not name now stays in the editor. The editor shows it, so a person can remove it. A grant of the configuration shows a lock and cannot change on the page.
- A limit field shows the largest value that the document allows. This is a hint. An empty field gives no value: the limits of the service still apply. The protocol has no default value for a limit.
- While a service has not answered, portal refuses a new grant below it.
- The Services page keeps the section **Grants to check**. It lists each grant below a service that does not follow the document of the service. The PDP does not apply these grants. The section does not list the grants of a service that has not answered yet.
- The log also has one warning `grant_to_check` for each such grant, with the login, the grant and the problem. portal writes it at the first answer of the service after the start, and at each change of its document.

### Migration of the database

At the first start of the version with grants, the server changes the database. SQLite goes to schema version 3. PostgreSQL goes to version 2. The migration deletes the access data of the earlier versions. It adds the table of the grants. The accounts and the history stay. After the migration, no user has a grant. Give the grants again in the settings pages, or in `user` blocks. Until then, only the superadmin can open the services.

The grants on sub-resources need one more change. SQLite goes to schema version 4. PostgreSQL goes to version 3. The table `user_grants` gets the columns `sub_resource_type` and `sub_resource_id`. The migration keeps all grants. A grant of the earlier version applies to the whole service. An earlier portal refuses the new schema: it does not start on it.

The credential generation of the superadmin needs one more change on PostgreSQL. PostgreSQL goes to version 4. The table `sessions` gets the column `admin_generation`. The sessions of the superadmin and the impersonated sessions of version 3 end at the first start. The superadmin signs in again. The sessions of the users stay. An earlier portal refuses version 4: it does not start on it. SQLite does not change, because its sessions are in memory.

The resource tree (portal 4.2.0) needs one more change. SQLite goes to schema version 5. PostgreSQL goes to version 5. The columns `sub_resource_type` and `sub_resource_id` of `user_grants` become one column `resource_path`. It holds the segments after the service in their text form, for example `host:local/database:analytics`. It is empty for a grant on the whole service. The primary key is the user, the action, the service and the path.

- The migration keeps each grant. A grant on one sub-resource `(database, a/b)` becomes the path `database:a%2Fb`.
- The migration does not add a level. A grant `database:analytics` stays at depth 1. When the document of the service puts `database` below `host`, this grant does not follow the tree. The PDP does not apply it. The Services page lists it under **Permissions to check**, and the log has a warning `grant_to_check` for it. Give the grant again on the new tree, for example `host:local/database:analytics`, then remove the old grant.
- A row that does not fit the new rules stops the migration. Examples: a type that does not start with `a-z`, an id of more than 256 bytes. The transaction rolls back. The database stays at the earlier version, and the new portal does not start. The error names `resource_path`.
- An earlier portal refuses version 5: it does not start on it. During a rolling update, an instance of portal 4.1.0 that still operates cannot read or write the grants after the migration. It keeps the grants that it has in memory until it stops.

The keys and limits need one more change. SQLite goes to schema version 6. PostgreSQL goes to version 6. The migration adds the tables `keys`, `key_grants`, `key_history`, `budget_counters` and `directory_version`, and it adds the column `limits` to `user_grants`. A key belongs to one user. The table `keys` holds the SHA-256 digest of a key and a short hint, never the key. When a user is deleted, the database deletes the keys of the user, and with them their grants. Each write of a user or a key adds 1 to `directory_version`, and the other instances read the directory again at the next round. The migration keeps all other data. An earlier portal refuses the new schema: it does not start on it. Therefore, a return to an earlier portal needs a restore of the database from before the migration.

The kind of a key needs one more change. SQLite goes to schema version 7. PostgreSQL goes to version 7. The table `keys` gets the column `kind` with the values `mcp` and `api`. A key that exists before this version is an MCP key. The migration keeps all other data. An earlier portal refuses version 7: it does not start on it. The grants of the Inbox have no migration. A database that holds grants with the names of an earlier version needs new grants (see "Upgrades").

The remembered sessions need one more change. SQLite goes to schema version 8. PostgreSQL goes to version 8. The tables `sessions` (PostgreSQL only) and `sessions_log` get the column `remembered`. A session that exists before this version is not remembered. The migration keeps all other data. An earlier portal refuses version 8: it does not start on it. During a rolling update, update all instances before you rely on the remembered sessions. The default of `session.ttl` is now 24 hours, not 8 hours. A file that sets no value gets the longer life.

The external services (5.0.5) changed the schema version 8 in place, as the second factor did. They add the table `external_services`: the id, the name, the link and the creation time of each service. A database that an earlier 5.0 made lacks the table. The portal stops at start and says so. See "Upgrades".

The approvals (5.1.0) change the schema version 8 in place, as the external services did. They add the column `confirm` to `user_grants` and to `key_grants`, and the column `interaction` to `keys`. A database that an earlier 5.0 made lacks the columns, and the portal stops at start and says so. A database of schema version 7 or less gets the columns from the migration. A key of kind `api` then gets `one_shot`, and the other keys get `conversational`. See "Upgrades".

### Paths open without a session (`portal.exclude`)

A caller that cannot sign in (for example, a webhook or a probe) needs a path that portal does not authenticate. The service declares this path with `portal.exclude`. This is an annotation in K3s. Write **one rule per line**. portal ignores blank lines and `#` comments:

```yaml
portal.exclude: |
  POST /dagu/api/v1/webhooks/*
  GET  /dagu/api/v1/health
  PUT,POST /dagu/hooks/**
```

A rule has this form: `[METHOD[,METHOD]] /path`. A segment is written out with the characters `[A-Za-z0-9._~-]`. A segment has up to 64 characters. `*` stands for exactly one segment. `**` stands for one or more segments. `**` is valid only at the end of a rule. There is no regular expression. There is no partial wildcard (`fo*`). A rule that skips authentication must be easy to read. A pattern engine is one more thing that can fail. Without a method, the rule matches any method. With methods, the rule matches only those methods. A request with no method header matches none of them.

The rules are strict by design:

- a service can open only paths under **its own `href`**. It can open a path only while it owns that path. The longest prefix wins, as for the gate. For example, `/dagu/admin` declared by another service stays closed to Dagu's rules;
- portal reads the path every way that a proxy or a backend might read it (`%2e%2e`, `//`, `%2f` as a character and as a separator, `\`). portal lets the request through only if **every** reading belongs to the same service and matches one of its rules. `/dagu/api/v1/webhooks/..%2f..%2fadmin` is not a webhook;
- a wildcard never stands for a segment that hides a separator or holds a `;`, raw or encoded. A backend that removes path parameters reads `..;` as `..`. `/auth/*` is never open;
- a rule that cannot be read makes the **whole label** unusable. Examples are a regular expression, a bad method, a path outside the `href`, or more than 8 rules. In this case, nothing is opened. The service is reported in the catalogue problems. The service is not listed until it is fixed. The gate keeps the path of its `href` closed: only the superadmin can open it;
- portal ignores the query string. portal records nothing in a session, because there is no session. The request is counted in `portal_forward_auth_requests_total{result="excluded"}`.

The Services settings page lists the rules of every service. The whole exposure can be audited in one place. A path that a rule opens is protected only by what the service does itself, for example by a webhook token. portal does not ask anything for these paths.

## Keys and limits

A key lets a program call a service for a user. Without the block `keys {}`, the keys are off. Each address of the key pages answers 404.

### The two kinds of key

A key has exactly one kind. The prefix of the key text gives the kind. The kind decides who verifies the key.

| Prefix | Kind | Used by | Verified by | At the other verifier |
|---|---|---|---|---|
| `mcp_` | MCP key | Agents | The key edge, `/auth/verify-key` | Refused: `401`, no detail |
| `api_` | API key | Services, webhooks, callers such as Grafana and Alertmanager | In process, by the API of the Inbox on the portal port | Refused: `401`, no detail |
| `whk_` | Reserved | Nobody | Nobody | Refused as an unknown prefix |

The key text has 53 characters: the prefix, 43 random base62 characters and 6 base62 characters. The last 6 characters are the CRC-32 (zlib, polynomial 0xEDB88320) of the prefix and the 43 characters. The portal shows the text one time, when it makes the key. It keeps only the SHA-256 digest and the last 4 characters.

A verifier refuses an unknown prefix, a bad length, a bad character and a bad checksum before any lookup. It then checks the kind. All these refusals are `401` with no detail.

### Rules of the keys

- **A key follows its owner.** This holds for both kinds. The rights of a key are the rights that the key holds and the current rights of its owner. When the owner loses a right, the key loses it at once. Other instances follow within `keys.directory_max_age`.
- **A key is deleted with its owner.** When the superadmin deletes an account of the database, the portal deletes its keys, their grants and their counters. The page of the account tells the number: "Delete carol and 3 keys?". An account of the file cannot be deleted.
- **Creation rights are by kind.** `keys.own.create` and `keys.own.use` apply to the resource type `key_kind`, with the nodes `key_kind:mcp` and `key_kind:api`. An account can have the right to make MCP keys and not API keys. The ceiling `max_key_lifetime` has one value for each kind.
- **The pane shows only what the account may make.** With two kinds, the pane of a new key has a selector. With one kind, it has no selector. With no kind, the page has no **New key**.
- **A key holds what its kind lists.** An MCP key holds grants on services. It holds no action of the portal. An API key holds `alert.send` and `alert.read` on groups of the Inbox, and `alert.send_user` on the service. It holds the grant `access on service:portal` for the budget `alerts`. An API key holds no grant on another service.
- **A key never goes beyond its owner.** The account can tick only the rights that the owner holds. A right that the owner lost has a warning mark in the pane.
- **A key has an interaction.** The value is `conversational` or `one_shot`. A `conversational` key is for an agent that discusses with a person in free text. A `one_shot` key is for a system that decides with no free text, and it gets closed questions only. The owner sets the value with two radio buttons in the form of the key, and the agent cannot set it. The default is `conversational` for an `mcp_` key and `one_shot` for an `api_` key. This release stores the value, and a later release uses it.

### Switches

The block `keys` has two switches. `keys.mcp` has the default `true`. `keys.api` has the default `false`. A kind that is off has no key, no right, no chip and no option in the selector. The key edge or the API of the Inbox refuses its keys.

- The kind `mcp` needs the blocks `identity` and `pdp`.
- The kind `api` needs the block `inbox`.
- A block `keys` with both kinds off stops the program at start.
- A block `api_key` needs `keys.api = true`. A block `key` of a block `user` needs `keys.mcp = true`.

### The page and the edge

- **Key page:** `GET /auth/keys` is one page. The list of the keys is on the left. It has a kind icon, the chips of the kinds, the filter Mine and All, and a box in each row for **Revoke (N)**. The form of a key is a pane on the right. With the pane closed, the toolbar has **New key**. With the pane open, the toolbar button is absent, and the head of the pane has the title, **Cancel** and the action (**Create** or **Save**). `GET /auth/keys/new` and `GET /auth/keys/edit?id=<id>` are direct links that open the page with the pane in that state. The kind selector is a small GET form: `?kind=` loads the pane again. An API key has the matrix of the Inbox and the budget of the alerts. An MCP key has the services only. The pane shows the text of a new key one time, with a **Copy** button and **Done**. A refused form returns to the pane of a new key, with the reason in a notice.
- **Edge:** `GET /auth/verify-key` is the ForwardAuth address for the key routes of agents. It reads only memory. A good `mcp_` key gives status 200 and an identity token. A bad key gives status 401, the header `WWW-Authenticate: Bearer error="invalid_token"` and an empty body.
- **Senders:** the routes of the Inbox verify the `api_` keys. See "Senders of alerts" in the section "Inbox".

### Rights, limits and commands

- **Rights of the portal for the keys:** the service `portal` has these actions for the keys: `keys.own.read`, `keys.own.create`, `keys.own.edit`, `keys.own.revoke`, `keys.own.use`, `keys.all.read`, `keys.all.revoke` and `users.read`. The ceiling `max_key_lifetime` limits the age of a key. The Inbox adds its own actions to the same service (see "Rights of the Inbox").
- **Limits:** a grant can hold limit values. A ceiling is a value that the service applies. A budget is a count in a window (`second`, `minute`, `hour` or `day`) that the PDP counts. The PDP counts one use for the key and for its owner, and it takes the unit from both counters or from none. When a budget is empty, the PDP answers `false` with the reason `budget_exceeded` and `retry_after`.
- **Counters:** the table `budget_counters` holds the counters. PostgreSQL uses a second pool of connections (`pdp.counter_connections`). `keys.max_counter_rows` limits the number of rows.
- **Revoke:** the page revokes keys one at a time or in a group. The command `portal revoke-keys --config FILE (--user LOGIN | --all)` revokes keys without the web page. With SQLite, it refuses to run when an instance runs.
- **Accounts of the file:** a block `user "<login>"` defines an account of the file, with optional blocks `key "<name>"`. These blocks are MCP keys of the file. An API key of the file is a block `api_key` of the block `inbox`. A login comes from the file or from the database, never from both. The list of keys in the tab Access lists the MCP keys of the file. It does not list the `api_key` blocks.

## Start and stop

The instance opens its ports only when these tasks end:

1. The directory is loaded and the counter rows exist.
2. The catalogue has the full list of the services.
3. The first read of the document of each service ended, or `access.start_read_deadline` passed.

The startup probe of the pod MUST wait longer than the sum of these times. The liveness probe reads `/healthz`, which never reads the database.

With the block `inbox`, one exception exists. The portal listener opens at once and serves the routes of the Inbox (`/v1/`, `/api/v1/`, `/readyz`, `/api/version`), so that an alert and a phone never wait for the catalogue. Every other route of the portal, `/healthz` included, answers `503` with `starting` until the three tasks end. The gate never decides with an incomplete catalogue. The startup probe reads `/healthz`.

On SIGTERM, the instance serves 5 seconds more. Then it closes its listeners and tells each connection to close. It waits at most 10 seconds for the answers in progress. Set `terminationGracePeriodSeconds` to 20.

The instance reads the action documents at equal intervals, 64 at the same time at most. The catalogue reads the IngressRoutes and the pods in two lists, page by page, and it probes 64 services at the same time at most.

## Launcher

The launcher (`/`) shows one row for each group and one tile for each service. The launcher shows only the services that the viewer can open.

### Labels of a service

A service declares itself with labels (Docker Compose container labels) or annotations (Kubernetes IngressRoute). The prefix is `portal.`. The same names work in both runtimes. The portal also reads the `homepage.*` labels and the `gethomepage.dev/*` annotations. A `portal.*` value wins.

| Label | Use |
|---|---|
| `portal.enabled` | `true` lists the service. `false` hides it, and the gate keeps the path of its `href` closed: only the superadmin passes. A `portal.name` also lists it. |
| `portal.id` | The id of the service. It is the audience of its identity tokens and the resource id of its grants. The default is the slug of the group and the name. The id `portal` is reserved for the internal scope of the portal features. |
| `portal.name` | The display name. |
| `portal.group` | The group of the row. The default is `Other`. |
| `portal.href` | Mandatory. A path or an http(s) URL. The service owns the longest path prefix (see "The gate"). A path with a broken percent escape or a `\` is not usable. An escape of `/`, of a control character or of bytes that are not UTF-8 is not usable either. |
| `portal.description`, `portal.icon` | The text and the icon of the tile. |
| `portal.weight` | An integer. The tiles sort by weight, then by name. The default is 0. |
| `portal.health` | An http(s) URL for the health probe. |
| `portal.pods` | K3s only. A label selector of the pods. The ready pods give the health. |
| `portal.actions` | The URL of the action document. See "Actions of the services". |
| `portal.exclude` | The paths open without a session. See "Paths open without a session". |

A label that is not usable keeps this service out of the launcher. The Services page names the problem.

### State of a service

portal finds two states for each service:

- The health state: up or down. A declared `portal.health` URL gives it (HTTP probe). Otherwise, the runtime gives it: the state of the container (Docker), or the number of ready pods of `portal.pods` (K3s). One ready pod gives up.
- The runtime health: healthy, unhealthy or starting. The health check of the runtime gives it, when the runtime has one. It adds detail to the health state. It does not change it. The metric `portal_catalog_services_down` counts only the health state.

Each tile shows one state word. The word always tells the state, so the colour is never the only sign.

| Word | Meaning | Look |
|---|---|---|
| `down` | The health state is down. This word has priority over the runtime health. | Red pill. The whole tile is red. |
| `unhealthy` | The health check of the runtime fails. | Warning pill. |
| `starting` | The health check of the runtime has not passed yet. | Neutral pill. |
| `healthy` | The health check of the runtime passes. | Green dot, the same as `up`. |
| `up` | The health state is up. The runtime has no health check for the service. | Green dot. |
| no word | No check applies, or no check has answered yet. | None. |

The runtime health for each runtime:

- **Docker, Compose and Swarm.** The result of the `HEALTHCHECK` of the container. portal reads it from the list of the containers, the same request as the labels. A recent Engine API gives the member `Health.Status`. An older API gives the end of the `Status` text: `(healthy)`, `(unhealthy)` or `(health: starting)`. A container without `HEALTHCHECK` has no runtime health. A stopped container has no runtime health. A `portal.health` URL does not remove the runtime health.
- **Swarm.** A task container gets a new name for each task. portal uses the label `com.docker.swarm.service.name` as the origin when the label `com.docker.compose.service` is not there. Thus the origin of the service does not change from one task to the next.
- **K3s.** The readiness probes of the containers of the pods of `portal.pods`. portal reads them from the same pod list as the number of ready pods. This list holds the spec and the status of each pod. Thus the runtime health needs no other request and no other RBAC verb: `get` and `list` on `pods` are sufficient.
  - portal uses only the pods that have a container with a readiness probe. A terminating pod does not count. Without such a pod, the service has no runtime health.
  - A ready pod is healthy. A pod that is not ready is starting when its phase is `Pending`, when a container has `started` at `false`, or when a container waits. A container that waits with a reason other than `ContainerCreating` or `PodInitializing`, for example `CrashLoopBackOff`, makes its pod unhealthy. Another pod that is not ready is unhealthy.
  - One unhealthy pod makes the service unhealthy. The service is starting when a pod starts and no probed pod is ready. Otherwise, the service is healthy.
  - With a `portal.health` URL, portal does not read the pods. The service then has no runtime health.

### Group heads

Each row has a head. The head shows the name of the group, the number of services, and the badges `N down` (red) and `N unhealthy` (warning). When the viewer sees one group only, the head gives no useful data. The launcher then does not show it, unless the group has a down or an unhealthy service. The row keeps the name of the group as its accessible name. With two groups or more, each row has its head.

### External services

The superadmin can add a service by hand. It has a name and a link. It needs no container and no IngressRoute. It is a tile that opens the link, and nothing more.

- **Add.** The button **Add external service** is on the Services page. It opens a form with a name and a link. The button and the route `POST /auth/services/add` exist only when the grants can change: `access.grants_mode` is not `static`. Only the superadmin in its own right adds or deletes a service. The form has the anti-CSRF token and the origin check, as the forms of the accounts have.
- **Link.** The link is an absolute `http` or `https` URL with a host. It has 1 to 2048 bytes. It has no space, no control character, no backslash and no user information (`user:password@`). A port is a number from 1 to 65535. The portal removes the spaces around the link and changes nothing else.
- **Name.** The name has 1 to 32 characters, with no control character and at least one letter or digit. The portal removes the spaces around it.
- **Id.** The id is `ext-` and the slug of the name: "Team wiki" gives `ext-team-wiki`. The id is the resource id of the grant `access` on the service. Two names that have the same slug have the same id, and the portal refuses the second name. It never adds a number to an id.
- **Tile.** The group is always `External`, and it is the last row of the launcher. The tile has no description, no icon of its own and no state word. It shows the icon of the library that has the name of the service. Without this icon, it shows the first letter of the name. It shows the badge `external`. The tile opens the absolute URL, and `ui.services_new_tab` applies to it as to the other tiles.
- **Permission.** The only permission is the grant `access`. It shows the tile of the service to its holder. The superadmin sees every tile. A new account sees none. The page of an account and the page "New user" show the service with the badge and the switch **Open**. They show no list of actions. The portal refuses each other grant on an external service, in each form: the page of an account, "New user", and the forms of the keys.
- **No protection.** The portal does not authenticate the access to the link. Anyone who knows the address can open it. The grant only shows or hides the tile. The Services page and the badge say so: "External link. This server does not protect it."
- **Keys.** A key never holds a right on an external service. The forms of the keys do not offer it, and they refuse a grant that names it.
- **Never a client.** An external service has no actions document, no identity token, no PDP question and no health probe. The portal never requests the link, so it cannot reach a host by this way.
- **No path.** An external service owns no path of the gate, also when its link points to the host of the portal. It cannot hide, shadow or open the path of another service. The grant `access` on it opens no path.
- **Same id.** A service that discovery lists with the same id wins. The external service then has no tile. The Services page and the launcher of the superadmin show a problem. Delete the external service.
- **Delete.** The trash icon in the head of the panel of the selected service deletes the service and each grant that names its id, in one transaction. If an account opens the service, the confirmation asks for the name first.
- **Limit.** The portal keeps 256 external services at most.
- **Several instances.** The database keeps the entries. Each instance reads them with the directory, in the next round (a few seconds), and it merges them into its catalogue. A restart is not necessary.
- **History.** An add and a delete write the audit events `external_added` and `external_deleted`. The id of the service is in the field of the account. The tab Activity shows "External service added" and "External service deleted". The metric `portal_user_admin_total` counts the actions `external_add` and `external_delete`.
- **File.** The start refuses a block `user` that gives an external service any grant other than `access` on the whole service. It refuses a block `key` that names an external service, and a block `pdp_client` that has the id of an external service as its name. The error names the block and the grant. The start checks the external services that the database holds.

## Settings pages and history

The superadmin gets a gear on the launcher. The gear opens the settings. The settings are **one HTML page for each tab**. This is the same design as the pages of clickhouse-dash. Each page has one header row of 48 px. The row has **Back to services** at the left and the tabs next to it. The theme selector and **Sign out** stand at the right. The row never wraps. In a narrow window, only the strip of the tabs scrolls sideways, with a thin bar that stays visible.

The tabs are Access, Services, Activity and Inbox. A tab shows only when its feature is on. A tab that the file turns off is absent from the page. It is never grey. A page with one tab only has no strip. The settings live under `/auth/settings`. Traefik already routes `/auth/` to the portal. A time shows with no year when it is in the current year.

The pages work without script, with one exception: the script draws the editor of grants and the matrices. Without script, the pages list the grants as text. The pages answer 404 to anyone who is not the superadmin in its own right. This applies to a user, and to the superadmin who browses as a user. The exceptions are the tabs Access and Inbox, which an account opens with its own permissions. A page carries only what it shows. The launcher carries none of it. The first element of each page is a skip link to the content. The settings stylesheet loads on the settings pages only.

The header stays at the top. The page does not scroll at 1440x900 and at 1280x720. A long list scrolls in its own panel. No element has a maximum width: each page uses the whole window. The page Security is the exception: it has a column of 680 px. Two panels stand side by side from 1200 px and stack under it. A panel has a small title, a count and its controls. A chip with a count filters the list of its panel. A chip is a link with a query parameter, so it works without script. A chip with a count of zero is absent. A state is one icon with a name for a screen reader. A row is one line of 26 px. A time shows in the time zone of the browser, with the UTC time in its title. A control is a flat icon. A confirmation opens in the place of its button. A popover opens from an icon. It closes on a click outside and on the key Escape.

### The four tabs and the child pages

1. **Access** (`/auth/settings`). Two lists stand side by side from 1400 px: the accounts and the keys. The superadmin sees both. An account with a right of the keys sees the list of keys alone, and the page has no strip. The superadmin is not a row of the list of accounts. A row of the accounts shows the login, which is a link to the page of the account. The row shows the groups of the account as small chips: two chips, then a chip "+N" that opens the list of all of them. A group chip is a link that filters by this group. The row shows the services that the account opens ("2 of 5"). Then come the features with their rights (an icon and a count) and the keys. The last columns are the last sign-in, the sessions and the refused sign-ins of the last 30 days. For the superadmin, a column **2FA** shows which accounts have a second factor, and the head shows the count (for example 1/2). The column is absent when `authentication.mfa` is `off` or when the database read fails. No column hides at a small width. From 1400 px the two lists stand side by side. Under 1400 px they stack and share the height of the window. An account of the file has the badge `file`. The column Groups exists only with the block `inbox`. The columns of the features and of the keys exist only with the block `keys` or the block `inbox`. With neither block, the column **Opens** shows the services of each account as chips. The head of the accounts has **End all sessions** and **New user**. **End all sessions** asks for a confirmation in place, and you sign in again too. **New user** is a link to the page "New user". It is absent when `users.allow_creation` is `false` or when `access.grants_mode` is `static`. The list of keys has the chips of the kinds, the chip "mine" and **Revoke (N)**. A chip of a kind shows only when the list holds the two kinds. A chip keeps its place when a filter leaves no key for it: it is dimmed and it is not a link. **Revoke (0)** does nothing, also with the keyboard. A key has a box when you may revoke it. **Revoke (N)** asks for a confirmation in place. **New key** is a link to the page "New key". It is absent when the account may make no kind of key. The column Owner and the chip "mine" exist only when the account sees the keys of other accounts. 
2. **Filters of the Access tab.** One row under the header has the filters. The field **User** keeps one account. The chips **Group** are links: one chip for each group of the block `inbox`, with its member count. The field **Service** keeps one service. The service field shows the name of the service. It accepts the name or the id. The URL carries the filters: `?user=&group=&service=`. The form uses the method GET, so the page works without script. All the filters apply together. Each active filter shows as a chip with a cross, and **Clear** removes them all. The head of a list shows "N of M". A list with no row says so and has a link **Clear**.

   | Filter | Accounts | Keys |
   |---|---|---|
   | `user` | The account. | The keys that the account owns. |
   | `group` | The members: the accounts that hold `inbox.receive` on the group. | The keys with a grant on the group. |
   | `service` | The accounts that open the service. The column **Permissions** shows how many permissions each account has on it. | The keys with a grant on the service. |

   A value that names nothing gives an empty list. It never gives an error. A filter on a group and a service together gives no key. The chips of the kinds count the keys that the filters keep. The bar is for the superadmin. The panel-only page has no filter by group.
3. **Services** (`/auth/settings/services`). One table lists the services by group. Each row shows the name and the id, and the path. It also shows the number of paths open without sign-in and the accounts that have `access`. The last columns give the number of actions, of resource types and of resources. A service whose actions document cannot be read has a warning icon. A click on a row selects it, with a link (`?svc=<id>`). The panel **Opened by** shows the accounts that open the selected service, with their number of grants. It then shows the accounts that do not open it. Each of these has a plus link that gives access. That link is the address `/auth/settings/give?user=<login>&service=<id>`. It redirects to the page of the account, with the service open in the editor and the grant `access` ticked. The page saves nothing until the person clicks **Save**. The chips **All** and **Services to check** filter the table (`?f=check`). They are absent when no service needs a check. The selected service is always a row of the filtered table. Above the panel, a line says where the document of the service comes from and how old the copy is, with the reason of the last failed read. The editor of grants shows the same line. The panel **To check** lists, for the selected service only, the grants that its document does not declare: the account (a link), the grant and the problem. A document that the portal cannot read is one more line. The panel is absent when there is none. The page of one service does not exist: its route answers 404. This page has no filter by user, group or service. The button **Add external service** opens a small form with a name and a link (see "External services"). An external service has the badge `external` and a dash in the columns of the actions, the types and the resources. Its panel has a trash icon and says "External link. This server does not protect it."
4. **Activity** (`/auth/settings/activity`). One list has two views, with the chips **Sign-ins** and **Changes**. The chip **Refused** keeps the refused sign-ins (`?r=failed`). The chip **Changes** shows the audit events (`?v=changes`). Above the list, a graph shows the connections of 30 days. Under it, three marks give the first day, the middle day and today, in UTC. The two views list the same 30 days, and each chip counts the rows of its list. A list holds 1000 rows at most, and a line says when it is cut. Each line of the sign-ins shows the result, the time, the login as typed, the address and the browser. Each line of the changes shows the time, the change, the account and who did it.
5. **Inbox** (`/auth/settings/inbox`). The tab shows only when the file has the block `inbox` and the account has a permission of the Inbox. See "The tab Inbox" in the section "Inbox".

Five child pages fill the whole window. The tab Access stays marked on each of them. Each has the same head: a crumb **Access >**, the title, **Cancel** at the right and the primary button. The address `/auth/keys` does not exist: the list of keys is in the tab Access.

- **Page of an account** (`/auth/settings/users/<login>`). One row of tools has the way back, the login and **Log in as**. It also has **Change the password**, a popover with the field and **Generate**. Next come **Activity of the account** and **Delete**. **Activity of the account** is a popover with the figures, the graph, the latest sessions and the services used. The row ends with the dates of creation and of the last sign-in, and **Save**. **Delete** opens in place and names the keys that go with the account: "Delete carol and 3 keys?". An account that has keys asks for its login before it deletes. **Save** is grey until a grant changes. Under the row, one field has the groups of the account as chips. Another field has the keys of the account as chips: four chips, then "+N". A group chip writes the grant `inbox.receive` on its group. The column Receive of the Inbox matrix shows the same grant, and the two always agree. A key chip opens the list of the keys of the account. Under the chips, the groups **Services** and **Server permissions** stand side by side. The editor of grants draws them (see "Editor of grants"). The address `?service=<id>` opens a service in the group **Services**. One form sends every grant. A new password ends the sessions of the account at once. In the mode `static`, the page has no form and the group chips are links. In the mode `hybrid`, a grant of the configuration has a lock and cannot change. The page of an account of the file shows its grants with a lock, and it changes nothing. The page of the superadmin shows its activity only. The superadmin has every grant, and the configuration file gives its login and password. The **Generate** button fills a password field with a random 20-character password. The browser draws this password. The password is never sent anywhere before the form is submitted.
- **New user** (`/auth/settings/new-user`). One row has the fields **Login**, **Password** (with **Generate**) and **Groups**. The editor of grants fills the rest of the window. **Create** makes the account with its grants in one request, and the portal opens the page of the new account. A new account has no grant until a grant is ticked. The route answers 404 when `users.allow_creation` is `false`. A refused try returns to this page. The message shows under the field that it names, and a banner says that the account is not made. A right on a service with no switch on it gives the page again with status 422. The page keeps the login and the ticked grants, and the banner names the service. The password does not come back. Without script, the page has three empty rows for grants.
- **New key** (`/auth/keys/new`). One row has the fields **Kind** (API or MCP), **Name** and **Expires**. A right on a service with no switch on it gives the page again with status 422. The page keeps the name and the grants, and the banner names the service. An MCP key shows the editor of the services on the whole width. An API key shows the Inbox matrix and the row **Direct alerts** on the left. The row **Direct alerts** shows only when the document of the portal has the action `alert.send_user`. On the right, an aside lists the calls of the API that the ticked rights allow. The aside needs script. With one kind that the account may make, the field **Kind** is absent. **Create** shows the page "Key made" with the text of the key one time.
- **Edit key** (`/auth/keys/edit?id=<id>`). The page is like "New key". The kind is a fact and does not change. A right that the owner lost shows a mark "without effect". A trash icon revokes the key, with a confirmation in place.
- **Key made.** The page after a creation. It shows the text of the key one time, with **Copy** and **Done**. The answer has the header `Cache-Control: no-store`.

Each service row of the editor shows its level of control as an icon after its name. The names of all rows have the same width, so the info text starts at the same place in each row. A service that publishes its document has a shield with a check mark. After a failed read, the icon is a warning triangle. A service with no document has no icon: it has access only. Each service block has one source line. A service that publishes its document shows "The service publishes its permissions. Last read: 2 min ago." The tooltip gives the address of the document. After a failed read, the block shows the age of the last good copy. The copy ends after `access.actions_max_age`. A service with no document shows "The service publishes no permissions. Access only." The group **Server permissions** shows "Published by this server. Built in, always current." An external service shows the badge `external` and the switch. It has no button of permissions and no list of actions.

An account that is not the superadmin sees the tabs Access and Inbox only, and only the ones that it may open. The tab Access needs a right to read keys. The code builds the list of tabs from three facts. The first fact is that the viewer is the superadmin. The second is that the viewer may read keys. The third is that the viewer has a permission of the Inbox. The launcher has no tile for the Inbox: the Inbox is a feature of the portal, as the keys are. The superadmin opens the pages with the gear. An account that may read keys, or that has a permission of the Inbox, gets the gear too. The gear links to the first tab of the account.

A form of an account returns to the page of the account, or to the editor of the service, with the outcome in a notice. The notice is one code from a fixed list (`?s=<code>`). The address never carries text.

### Panel-only mode and the switches

The portal can run as a panel of services only. Each part that is off is absent from the pages. It is never grey. A route that is off answers 404.

| Switch | Default | When off or absent |
|---|---|---|
| Block `inbox` | absent | No tab Inbox, no routes `/v1/*`, `/api/v1/*`, `/api/version`, no phone endpoint, no group of Inbox permissions, no kind `api`. |
| Block `keys` | absent | No list of keys, no **New key**, no page of a key, no group of key permissions, no key count in the list of accounts. |
| `keys.mcp` | `true` | The key edge refuses `mcp_` keys. No MCP chip, no MCP right. |
| `keys.api` | `false` | The API of the Inbox refuses `api_` keys. No API chip, no API right. |
| `users.allow_creation` | `true` | The create route answers 404. No **New user** button. The accounts that exist stay editable. |
| Block `identity`, block `pdp` | absent | No identity token and no AuthZEN PDP. The kind `mcp` needs both. |

A file with no block `keys`, no block `inbox` and `users.allow_creation = false` shows three tabs: Access, Services and Activity. The tab Access has the list of accounts alone, with the column **Opens**. It has no **New user**, no column of features, no column of keys and no filter by group. Six contradictions stop the program at start. The error names the key, the expected value and the value found:

- `keys.api = true` and no block `inbox`.
- `keys.mcp = true` and no block `identity` or no block `pdp`.
- A block `keys` with `keys.mcp = false` and `keys.api = false`.
- A block `api_key` and `keys.api = false`, or no block `keys`.
- A block `key` of a block `user` and `keys.mcp` not `true`.
- `access.grants_mode = "static"` and `users.allow_creation = true`. In this mode the database holds no account.

### Editor of grants

A service can have hundreds of actions and hundreds of resources. A box for each pair of an action and a node does not fit in a page. The forms of the keys and the forms of the accounts use one editor. The page holds the data of the editor as a JSON block (`<script type="application/json">`, which the content security policy does not run). The script `settings.js` draws the editor from this block.

At the left is the tree of the resources of a service, with a search field and the number of ticked grants of each node. A list of the services shows when the editor has more than one. At the right are the actions of the chosen node, in groups by the first part of the name. Each action has one line. A list of more than 60 actions starts with its groups closed. A filter and the button **Ticked** narrow the list. The box of a group ticks the actions that the list shows. An action that a grant above the node covers shows ticked, with the words "via the whole service" or "via" and the node. An action shows on the whole service only when its document lists the type `service`.

A ticked action whose document has `"approval": "optional"` shows a lock button at the end of its row. The button is flat and has no text. It has two states: an open lock for no approval, and a closed lock for "asks for an approval". On the form of a key, the lock is closed and disabled when a grant of the owner asks for an approval. The key must then ask too. A permission of the file shows a closed lock that cannot change. The server reads the state from the field `confirm<n>`, which has the value `1` when the lock is closed.

A ticked action shows its limit fields on its own line, with the largest value of the document as a hint. Each service has a row with a switch for the grant `access` and a count of its permissions. The list of the actions has no row `access`, because the switch is this grant. The limits of `access` show on the line of the source, at the right. On a form of a key, the button **Same as mine** ticks all the permissions that the owner holds on that service.

Any right on a service opens the service in the editor. A person can tick an action on the whole service, on a resource or on a sub-resource at any depth. The editor then ticks the switch of the service in the same step. The row shows "Open" and the count of the permissions at once. The rule is the same for each way to tick: one action, the box of a group, **Same as mine**, and the link "Give access". The last right that a person removes does not untick the switch, because an account can have access only. When a person unticks the switch, the editor removes the rights of the service too, because they do nothing without access. The row then says "Access off" and the number of rights removed, with a button **Undo**. The note stays until the next change. **Undo** puts the rights and the switch back. A re-tick of the switch does not bring them back. A live region tells a screen reader about the note and about a service that opens by itself. The focus and the scroll of the lists stay after each tick. The tree answers to the keys Up, Down, Home, End, Left and Right. A permission that comes from the configuration file cannot be removed, so the switch stays on. The group **Server permissions** has no switch and no such rule.

The form of a key offers a service only when the owner holds `access` on it, because the key cannot go beyond its owner. The server checks the same rule. It refuses a form of an account or of a key that has a right on a service and no grant `access` on this service. The notice names the service and says that the right needs the switch Can open on it. For `POST /auth/users/add`, `/auth/keys/create` and `/auth/keys/edit`, the answer is the form again, with status 422 and the values that were sent. This applies to `POST /auth/users/add`, `/auth/users/update`, `/auth/keys/create` and `/auth/keys/edit`. The grants on the service `portal` and the `user` blocks of the file are not checked this way.

The service lists its own resources. The editor shows the nodes of the member `resources` of the action document. When a node can have children that the document does not list, the editor asks the portal: `GET /auth/options?service=<id>&type=<type>&parent=<path>&q=<text>&after=<cursor>`. The portal asks the service at `/.well-known/procurauth/resources` (protocol 5.6), with an identity token for the person who fills the form. The portal makes this token only when the person can open the service. The service lists only what this person can see. The portal asks for 50 items at a time. It keeps each answer for 60 seconds, for at most 5,000 entries and 32 MiB (portal 3.1c). The tree shows **Show more** under a page that has a next cursor. A search asks the service for the children of the chosen node. A service that has no such route gives no option. The page of the superadmin and the page of a user who makes keys use the route. The CSP allows `connect-src 'self'` for it.

The page also writes the ticked grants as hidden fields, with the names that the server reads: `grant<n>`, `limit<n>.<name>[.<window>]` and `confirm<n>`. A browser without script sends these fields as they are, so a change that does not touch the grants keeps them. The form of a key also has a text field for each blank grant in a `<noscript>` element. A key holds 512 grants at most.

The group **Server permissions** is not a tree. It shows plain rows and compact matrices. The part **Keys** is a plain row. The part **Key kinds** has the longest lifetime of each kind. The part **Inbox** has one row for each group, with the columns Receive, Send and Read, and the budget of the alerts. The part **Phones** is a plain row. A feature that the file turns off has no row. A critical group has a mark, because a limit never stops its alerts. The kind API comes before the kind MCP, and the critical groups come last. The editor writes the grant `access on service:portal` only when a window of the budget has a value. The matrices need script. Without script, the page lists the grants as text.

The portal accepts a document of 1,024 actions, 4,096 resources and 256 KiB by default. The keys `access.max_actions`, `access.max_resources` and `access.max_document_bytes` set these bounds. The minimum of each key is the number of protocol 5.3c.

### Look and feel

The pages share the look of the launcher. The panels are graphite, with a thin border and a small upper-case title. The controls are 34 to 36 px high. The pages have one accent. The theme selector is one icon button that shows the mode. Its list holds the two other modes (System, Dark, Light), and the browser remembers the choice. The file `web/portal.css` holds the tokens, the header, the buttons, the launcher and the sign-in card. The file `web/settings.css` holds the rules that only the settings pages use. The portal serves them at `/auth/style.css` and `/auth/settings.css`. The content security policy allows requests of the script from the same origin only.

The pages use these component names. `zone` is a panel with a bounded list. `grid` is a table of one-line rows. `ib` is a flat icon button, `pop` is a popover and `ask` is a confirmation in place. The other names are `chip`, `uiField`, `uiInput`, `uiBanner` and `uiState`. A time is a `<time class="when">` element. The server writes the time in UTC. The script `settings.js` shows the time of the browser instead and keeps the UTC text in the tooltip. A state (key, session, sign-in) is one icon with a hidden word for a screen reader. A form with the attribute `data-gate` keeps its submit buttons disabled until its fields are valid.

### Sign-in log

The program records each sign-in attempt that reaches the password check. The record has these items. The first items are the time (UTC, to the second) and whether the attempt succeeded. Then come the **login as typed** and whether that login names an account. The last items are the client address and the browser. The tab Activity shows the entries of the last 30 days, the newest first, up to 1000 rows. The chips show the number of attempts of these 30 days and the number of refused attempts among them. The chip *Refused* uses `?r=failed`.

The password is never recorded. This applies to the database, the page and the logs. What a visitor types is hostile data. The program never uses this data raw. The program keeps two groups of characters as a visible `\u{..}` escape. The first group is the control characters (newline, NUL, escape sequences). The second group is the characters that hide or reorder text (zero-width characters, right-to-left overrides, line separators). The text is cut at 64 characters, with an ellipsis. The text is used only as a bound SQL parameter. The page HTML-escapes the text where it prints it. The CSP forbids inline script and style anyway. In the JSON logs (`login_failed`, `login_success` carry `login`), the text stays inside one escaped string, so it cannot forge a line. An attempt blocked by the rate limiter is counted in the metrics. It is not listed one by one. The log keeps at most 20,000 attempts (newest first). So a siege cannot fill the volume.

A browser that the gate refuses goes to a 403 page. The page does not give the name of the service. The page tells the user to ask the administrator for access. The page has the buttons **Back to services** and **Sign out**.

**Log in as** gives the superadmin a session as the user. The session is flagged *impersonated*. It is not counted as a connection of that user. The history tags it *by the superadmin*. A banner offers **Back to superadmin**. This button needs no password. The impersonated session itself proves that the superadmin opened it. Account management is refused while the superadmin impersonates a user.

### End all sessions

Use this tool after an incident, for example after a suspected theft of a cookie. It ends every session of every account. The sessions of the superadmin end too. Accounts, permissions and history stay. Each user must sign in again.

There are two ways to start it:

- **Settings page.** Open the tab **Access**. Click the icon **End all sessions** in the head of the accounts. The question opens in the place of the icon. Click **End**. Your own session ends. The portal sends the browser to the sign-in page. Only the superadmin in its own right can do this. The route is `POST /auth/users/end-sessions`. It needs the anti-CSRF token of the account routes and the field `confirm=end-all-sessions`. A request without the field, or with an extra field, gets 400 and changes nothing. A user, or the superadmin who browses as a user, gets 403.
- **Command.** Run `portal end-sessions --config FILE`. The command deletes every session in the shared database and exits. It prints the number of sessions that it ended. It needs a `postgres` block in the file. With SQLite the sessions are in the memory of the portal process, and a different process cannot end them. The command then fails and changes nothing. Restart the portal to end the sessions of a SQLite configuration. In K3s, run the command in a portal pod with the file `/etc/portal/config.hcl`.

Each use writes one audit event, `sessions_ended`. The chip **Changes** of the tab Activity shows it as "All sessions ended". The actor is the superadmin login for the page, and `cli` for the command. The history closes each session with the reason `revoked`. The metric `portal_user_admin_total{action="end_sessions"}` counts the page calls. An instance of a shared database refuses an ended session within 5 seconds, because it caches a session for that time.

The history is stored in the database (SQLite or PostgreSQL). The program keeps the history for `storage.retention_days` days. The default is 90 days. The history contains these records:

- sessions: login, address, user agent, start, last seen, end, the reason for the end, and whether the person ticked "Remember me";
- the services that each session used. The history holds counts per service. It never holds a path or a query;
- administrative events: users added or deleted, impersonation, all sessions ended, and failed sign-ins;
- the sign-in attempts above.

The per-account failed counters count only logins that name an account. A login that does not exist is counted apart. It appears in the sign-in log, as typed. A person sometimes types a password in the login field. This is the one item in this log that the log must not hold. That is why the log is short-lived. Only the superadmin can see it.

Requests only touch memory. One writer thread flushes every 15 seconds in a single transaction. An event is dropped rather than make a request slower. The program counts a dropped event in `portal_tracking_dropped_total`.

With SQLite, sessions that a previous run left open end with the reason *restart*. With PostgreSQL, sessions outlive a restart. Only a session whose instance stopped before it recorded its end is closed in this way. This happens after the session has been idle for longer than `session.idle_timeout`. A session that is still in the shared database is never closed in this way. Times are UTC.

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

The key `session.secret_mode` has two values.

```hcl
session {
  secret_mode = "ephemeral"
}
```

`ephemeral` makes a new 32-byte secret from the OS CSPRNG at each start of the process. The file must not give a secret in this mode.

```hcl
session {
  secret_mode = "provided"
  secret_file = "/run/secrets/portal_session_secret"   # or: secret_env = "PORTAL_SESSION_SECRET"
}
```

`provided` takes the secret from `session.secret_env` or from `session.secret_file`. Give exactly one of the two. The secret has at least 32 bytes. The program refuses a secret that is in clear text in the file.

The secret signs the return targets of the authentication. It also signs the anti-CSRF tokens of the forms. With SQLite, the opaque sessions stay in the memory of the process in both modes. A restart therefore ends all sessions. With PostgreSQL, the sessions are in the database. `provided` is **required** with PostgreSQL. All instances must sign with the same key. If they do not, one instance refuses a form that another instance served. The program refuses `ephemeral` with the `postgres` block.

## Several instances (PostgreSQL)

By default, one process owns a SQLite file. The process keeps the sessions in its memory. So only one instance can be active. The optional `postgres` block moves all data to a shared PostgreSQL database. Then any number of instances can run at once. If one instance stops, the only cost is the requests that it served.

```hcl
session {
  secret_mode = "provided"                   # required: every instance signs with the same key
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
  password_file   = "/run/secrets/portal_db_password"   # or: password_env = "PORTAL_DB_PASSWORD"
  pool_size       = 8                        # default 4, 1 to 64
  connect_timeout = "5s"                     # default, 1s to 60s
}
```

The client is a synchronous PostgreSQL client. It uses plain TCP. It has no TLS stack and no C dependency. The connection stays inside the cluster. The server must accept `scram-sha-256` over a non-TLS connection from the pods. At startup, the first connection is retried for about one minute. Then the schema is created or updated under an advisory lock. Several instances that start together are therefore safe. A statement that runs for more than 15 s is cancelled. A broken connection is replaced, and the operation is retried once. This is what a failover of the database looks like.

This is what the instances share, and how:

- **Sessions** are rows of a `sessions` table. A row holds the digest of the cookie, the client binding, the identity, the expiry and the last use. A login writes the session and its line of the history in one transaction. Each instance keeps a copy of what it checked. The copy is kept for **5 seconds**. ForwardAuth asks the database about one session once per 5 s, not once per request. The binding (address and User-Agent), the idle timeout and the absolute lifetime are enforced exactly as with SQLite. The idle timeout is renewed at most every 5 s of activity. A logout, a deleted account or a password change ends the session in the database at once. The session then stops on the other instances within 5 seconds (the copy). If the database cannot be reached, a session that was checked less than 2 minutes ago is still accepted. A failover therefore does not sign everybody out. New sign-ins fail with `503` in the meantime.
- **Superadmin sessions** also carry the credential generation of the superadmin. This value is an HMAC-SHA-256, under the session secret, of the superadmin login and password. The database never holds a plain hash of the password. An instance accepts a superadmin session or an impersonated session only with its own generation. At start, an instance deletes these sessions when they have another generation. Thus a new superadmin login, password or session secret ends the old superadmin sessions on every instance. Rotate the credential after a suspected theft of a superadmin cookie. During a rolling update, an old instance and a new instance refuse the superadmin sessions of each other. The sessions of the users stay.
- **Sessions of the accounts** carry the stamp of the credential that opened them. The stamp is a SHA-256 digest of the source of the account, its login and its password hash. An instance accepts a session only while the account has the same stamp. Thus a new or removed `password_hash` of an account of the file ends its sessions on every instance. A new account of the same login does not get the sessions of an old account.
- **Accounts** are read from the database every 5 seconds by every instance. An account that is created, edited or deleted on one instance reaches the others within 5 seconds. A login that the instance does not know yet makes it read the accounts again at once. This happens at most once per second. An account that was just created can therefore sign in on any instance. A session that was opened for this account elsewhere is never ended for that reason.
- **History** is written by every instance. The writes are batched every 15 s, as with SQLite. A session that ends by logout, revocation, expiry or idleness is recorded once. The instance whose call ended the session records it. The database gives each ended session to exactly one caller. The start or the stop of an instance never closes the sessions of the other instances.
- **Each instance has its own rate limits.** The failed-login limiters (per IP and global) are in memory. With two instances, an attacker gets up to twice the budget. A lockout on one instance does not lock the other instance. `session.max_active_sessions` and `session.max_sessions_per_account` are checked against the database. Two instances that race at a limit can each overshoot it by one.

Liveness and readiness are unchanged. `/healthz` does not touch the database. `/metrics` adds `portal_shared_store_errors_total{use="sessions|directory"}` and `portal_directory_age_seconds`. The second value is the time since the database last answered a read of the accounts. It grows when the refresh fails.

The instance that adds an account checks `users.max_users` against its own copy. Two instances that add accounts at the same moment can exceed the limit by one.

### Migration from SQLite to PostgreSQL

The command `portal import-sqlite --config FILE --from SQLITE_FILE` copies data to PostgreSQL. It copies these items: the accounts (with their grants), the sessions of the history, the service accesses, the audit events and the sign-in log. It copies them into the empty PostgreSQL database that the configuration names. The copy is one transaction. The command prints what it copied. The command refuses a target that already holds any of these items. In this case, it changes nothing. The command only reads the SQLite file. The SQLite file can be the live file. Sessions do not move. They were in the memory of the old process. The sessions that the file still shows as open end where they were last seen, with the reason *restart*. Every user signs in again after the switch. Changes made to the SQLite file after the import are not copied. Do not change accounts between the import and the switch.

## Inbox

The portal can run the Inbox. The Inbox sends alerts to phones. It makes sure that each phone shows each alert one time. The Inbox starts when the file has a block `inbox`. Without this block, the portal has no Inbox and opens no route for it.

The Inbox is a feature of the portal. It runs in the same process and in the same Tokio runtime. It starts and stops with the portal. A phone and a key of the file do not need a session of the portal. The Inbox delivers alerts when the rest of the portal is not ready. The rights come from the directory of the portal. The wire protocol of the phones is protocol 5: enrollment with a one-time code, the Noise IK session, and items that the owner answers. The chapter [`docs/inbox/`](docs/inbox/README.md) is the full guide. [`docs/inbox/SPEC.md`](docs/inbox/SPEC.md) has the numbered requirements.

### The Inbox in a nutshell

- **Alert.** An alert has one group and one title. The title is one line of 128 characters at most. An alert has no body and no severity. Each alert has the field `kind`. The only value today is `alert`.
- **Group.** A group is a named list of phones that follow it. The file defines the groups. A group can be `critical`: no limit ever stops an alert to it.
- **Sender.** A sender sends alerts with an API key (`api_`) as a bearer token. A sender is a script, Grafana, Alertmanager or a service. The portal also sends alerts about itself (see "Alerts of the portal").
- **Phone.** A phone is an Android phone with the app Portal Inbox. It enrolls with a one-time code of an account. The login of the account is the name of the user of the phone.
- **Subscription.** A phone subscribes to a group. The account must hold `inbox.receive` on the group. For each group, the person chooses on the phone how it reacts: `alarm`, `ring` or `notify`.
- **Exactly-once display.** The transport is at least once. The phone ignores a repeat. The user sees each alert one time. See the list of the guarantees in `docs/inbox/README.md`.
- **Degraded mode.** If PostgreSQL has no writable primary, the Inbox goes on. It keeps alerts in memory, delivers them to the connected phones and writes them when the database returns.
- **Keys.** An `api_` key sends. An `mcp_` key does not send alerts at this step. The Inbox verifies the `api_` keys in process.

The roadmap has three steps. Step 1 (now): services and scripts send alerts with `api_` keys. Step 2: agents send alerts, first through the same route with an `api_` key of their own. Step 3: agents send questions and approvals, and a person answers on the phone. Step 3 ends the rule that alerts are one way. It needs a new protocol version. `docs/inbox/SPEC.md` marks these requirements as future.

```hcl
inbox {
  server_key_file = "/run/secrets/inbox_server_key"   # or: server_key_env

  devices {
    public_address        = "https://inbox.example.org"  # the public base URL of the portal; no default
    heartbeat_interval    = "20s"                      # no default
    session_timeout       = "60s"                      # no default
    resend_after          = "15s"                      # no default
    max_inflight          = 32                         # no default
    unreachable_after     = "2m"                       # no default
    offline_warning_after = "3m"                       # no default
  }

  enrollment {                                         # optional block
    code_ttl               = "10m"                     # default
    max_active_per_account = 5                         # default
  }

  storage {
    delivery_ttl             = "7d"                    # no default
    alert_retention          = "30d"                   # no default
    connection_log_retention = "90d"                   # no default
  }

  group "infra" {
    name        = "Infra prod"
    description = "Production outages"
    critical    = false                                # default; true: no limit on alert.send
  }

  api_key "grafana" {
    owner       = "grafana"                            # an account of the file: a block user
    secret_file = "/run/secrets/inbox_grafana_key"     # or: secret_env
    groups      = ["infra"]
    # direct_alerts = false                            # default; true: the key can alert an account (the owner needs alert.send_user)
  }
}

# An API key needs the kind api, and an account of the file as its owner.
access { grants_mode = "hybrid" }
keys   { mcp = false  api = true }
user "grafana" {
  grants = [
    "keys.own.use on service:portal/key_kind:api",
    "alert.send on service:portal/inbox_group:infra",
  ]
}
```

The delivery values have no default. The program stops at start when one of them is absent. They are `heartbeat_interval`, `session_timeout`, `resend_after`, `max_inflight`, `unreachable_after`, `offline_warning_after`, `delivery_ttl`, `alert_retention` and `connection_log_retention`. A wrong default could hide an alert, so the operator chooses each value. The reference below lists every key.

### Enrollment of a phone

A phone enrolls with a one-time code. The code belongs to one account of the portal. The login of the account is the user name of the phone. The phone cannot change it.

1. The account makes a code with the plus button of the zone **My phones** of the tab Inbox. An account that has `phone.read` sees the zone **Phones** instead, and the plus button is in that zone. The button is a form: it sends `POST /auth/settings/inbox/code`. An account that has the permission `inbox.receive` on one group at least can use it. The code is for the login of the session. The form has no login field. This is the only entry point.
2. The answer is the page of the tab, with the code in a popover that stays open until **Done**. The popover shows the text `portal-inbox://enroll?s=...&k=...&c=...`, a link that opens the app, a Copy button, the fingerprint and the end of the code. The answer has the header `Cache-Control: no-store`. A reload of the page does not show the code again. The code holds the key `inbox.devices.public_address` (`s`), the public key of the server (`k`) and a secret of 128 bits (`c`). The fingerprint is the first 20 hexadecimal characters (80 bits) of the SHA-256 digest of the public key, in five groups of 4.
3. The person types the code in the app, or taps it as a link. The app pins the key `k`. It connects to `GET /v1/enroll` and runs one Noise IK handshake with the prologue `portal-inbox/v1/enroll`. The first message holds the secret and the name of the phone.
4. The Inbox looks for the SHA-256 digest of the secret. A code works one time and lives `inbox.enrollment.code_ttl`. The Inbox stores the phone and marks the code as used in one transaction. Then it sends message 2 and closes the connection.

The Inbox keeps only the digest of the secret. An account has at most `inbox.enrollment.max_active_per_account` codes that are not used and not expired. The Inbox refuses another code, and the page tells the person. A session that "Log in as" opens can make a code for the account that it shows. The phone then belongs to that account.

A refusal has no message 2. The Inbox closes the WebSocket with the code 4410 in four cases. The code is unknown, used or expired. The Inbox cannot read the message. Or the limit `inbox.devices.max_devices` or `inbox.devices.max_per_account` is reached. The log has the reason in the event `enroll_refused`. The Inbox closes with 4426 when the protocol is not 4, and with 4429 after too many attempts from one address. The Inbox closes a connection that sends no message in 10 seconds. With a replica and `sync_alerts = true`, the Inbox sends message 2 only after the copy holds the phone.

When the superadmin deletes an account, the Inbox ends the state of the account. It revokes the phones of the account and deletes its codes that are not used. The sessions of these phones close with the code 4403. If the Inbox cannot do this, the account stays and the portal shows an error.

The end of an account also detaches the rows that carry its login. These are the phones, the alerts to the account, the alerts that it sent with the form, and its notification state. These rows then carry the login with the suffix `:ended:` and a random part. The connection log keeps the login. A new account with the same login thus gets no phone, no alert and no notification of the old account. The superadmin and the recipients still see the old alerts.

The creation of an account of the database does the same end first. It removes what an earlier holder of the login left, also an account of the file.

With SQLite, the Inbox also ends the state of a login that no account has any more. This is an account of the file that left the file, or an old login of the superadmin. The Inbox does it at the first look of the rights after the start, and when a phone of the login connects or enrolls. With PostgreSQL, the file of another instance can still have the login. A phone of a login that no account of this instance has gets the close code 1013, and the Inbox changes nothing. Revoke such a phone in the zone **Phones** when every instance has the new file.

### Listeners

The Inbox is a feature of the portal, so it has no listener for the senders or the phones. The listener of the portal, `server.listen_address`, serves the pages and all the routes of the Inbox. The portal gives the paths `/v1/`, `/api/v1/`, `/readyz` and `/api/version` to the Inbox and to no other handler, before every other route. A path under these prefixes that the Inbox does not know gets `404`. These routes need no session of the portal and set no cookie. The portal applies no CSRF check, no origin check and no security header to them. A proxy must send them to the portal without ForwardAuth (see "Reverse proxy").

**The routes of the phones** use the Noise handshake inside the WebSocket. A phone proves itself with it.

| Route | Use |
|---|---|
| `GET /v1/enroll` | WebSocket: enrollment of a phone with a one-time code. |
| `GET /v1/session` | WebSocket: the session of an enrolled phone. |

**The API of the senders** needs an API key (the kind `api`).

| Route | Use |
|---|---|
| `POST /api/v1/alerts`, `GET /api/v1/alerts/{id}` | Send an alert to a group (field `group`) or to one account (field `user`). Read its delivery status. |
| `POST /api/v1/webhooks/alertmanager/{group}`, `POST /api/v1/notify/{group}` | Webhooks. |
| `GET /readyz`, `GET /api/version` | Readiness and version. They need no key. |

`GET /healthz` is a route of the portal, not of the Inbox. `/readyz` answers `200` with PostgreSQL, also when the database has no writable primary (`degraded`). With SQLite it answers `503` when the file does not answer. After SIGTERM it answers `503` with `draining`. The cluster then removes the instance from its Service. `portal healthcheck` checks `/healthz` and the Inbox `/readyz` of the portal port. `portal inbox healthcheck --config FILE` checks the Inbox only.

**Two budgets of connections.** A phone session is long-lived. It must not starve the page requests. The listener keeps two budgets. The pages and the API use `server.max_connections`. A connection that upgrades to a phone session gives its permit of the pages back and takes a permit of the session budget. The session budget is `server.max_connections` plus two times `inbox.devices.max_devices`. The first term covers the phones that do not finish the handshake. A handshake has 15 seconds from the upgrade, and a ping does not extend this time. One client address holds at most `inbox.devices.max_handshakes_per_address` handshakes. The second term covers one live session and one session that the server has not yet detected as lost, for each phone. A phone that cannot get a permit gets `503` with `Retry-After: 5` before the upgrade. No WebSocket exists for it. The refusal by rate limit is different: it closes the WebSocket with the code 4429 after the upgrade.

**Stop.** At SIGTERM the Inbox sends the close code 1012 to every phone at once. The portal serves the pages 5 seconds more, as before.

The client address of a phone comes from `server.trusted_proxies`, as for the pages. The Inbox has no list of its own. It keys the limiter of the enrollment attempts. The key `inbox.devices.public_address` is the public base URL of the portal as the phones reach it: `http(s)://host[:port][/prefix]`, for example `http://192.168.1.10:4181` or `https://portal.example.org`. The app adds `/v1/enroll` and `/v1/session` to it. The portal has no base path: it serves its routes at the root. A proxy that publishes the portal under a prefix must remove the prefix before it sends the request (the Traefik middleware `StripPrefix`).

With the block `inbox.cluster`, the Inbox has a second listener for the other instances. Only the pods of the service must reach it. Each request carries an HMAC that the server key derives.

### Reverse proxy

Traefik must send `/v1/`, `/api/v1/` and `/api/version` to the portal without ForwardAuth. A phone, an Alertmanager and a script have no session. In Traefik, use a router with the rule ``PathPrefix(`/v1/`) || PathPrefix(`/api/v1/`) || Path(`/api/version`)``. Give it a high priority and the service of the portal. Add the middleware `retry` (4 attempts, initial interval 100 ms). Traefik retries a request only when the server did not answer at all. It never repeats a request after the first byte of the answer, so an upgrade that got `101` is not repeated. The files `demo/docker-compose.yml` (labels), `demo/traefik-dynamic.yml` and `deploy/k3s/ingress.yaml` (IngressRoute `portal-inbox`) show it. To restrict the phones by source address or by rate, add a middleware to this router. When no proxy is in front, the pages, the senders and the phones share the port of the portal directly. Do not route a path `/v1/` of another service to the same host: the portal owns this prefix.

### Storage

The Inbox uses the database of the portal. With SQLite, it uses the same file. With PostgreSQL, it uses the same database and the connection settings of the block `postgres`. There is no second `postgres` block. The key `inbox.storage.pool_size` sets the number of connections of the Inbox.

The Inbox and the portal have two independent chains of migrations. The portal keeps its version in `PRAGMA user_version` (SQLite) or in `schema_migrations` (PostgreSQL). The Inbox keeps its version in the table `inbox_schema_version`. Each chain refuses a schema that is newer than its binary. The tables of the Inbox are `devices`, `subscriptions`, `alerts`, `deliveries`, `connections`, `enrollment_codes` and, with PostgreSQL, `instances`.

Release 5.1.0 adds the tables of the items: `items`, `turns`, `approvals`, `item_events`, `approval_intents`, `approval_mute`, `account_prefs`, `items_heartbeat` and `content_key_check`. See "The engine of the items".

With PostgreSQL, any number of instances can run. They share the phones, the alerts and the sessions through the database, `LISTEN` and `NOTIFY`. If the database has no writable primary, the Inbox goes on. It accepts alerts in memory and delivers them to the phones that are connected. It writes them when the database is back. With the block `inbox.cluster`, the instances copy these alerts to each other before the sender gets its answer.

With SQLite, the block `inbox.replica` copies the whole file to a shared directory. The copy holds the tables of the portal too. Each file of the directory has the mode 0600, as the database file has. A directory that the portal creates has the mode 0700. Run each instance with the same user id. A form of the portal that changes the database answers only after the copy holds the change, as an alert does. The metrics `portal_inbox_replica_*` show the state of the copy.

An instance that owns the lease serves. A standby instance waits for the lease before it opens the file. It restores the newest copy that passes the consistency check. An instance that restarts on its own disk keeps its file when the file belongs to the epoch of the newest copy: that file is newer. An instance that loses its lease stops the whole process.

### Commands

| Command | Use |
|---|---|
| `portal inbox keygen --out FILE` | Makes the key of the server. The file holds one line: the Noise key pair of the server. The command never overwrites a file. |
| `portal inbox token --out FILE` | Makes the text of an API key (`api_`, 43 characters and 6 characters of checksum). The file never gets overwritten. |
| `portal inbox content-key --out FILE` | Makes a key that seals the contents of the items: the base64 text of 32 random bytes. The file never gets overwritten. |
| `portal inbox content-purge --config FILE --key ID` | Erases the sealed contents of a key that the file lost, and keeps the metadata. It refuses a key that the file still holds. |
| `portal inbox import-sqlite --config FILE --from SQLITE_FILE` | Copies a SQLite file into the empty PostgreSQL database that the configuration names. The file must have the current schema of the Inbox. A file of the earlier standalone alert server has an older schema, so the command refuses it. |
| `portal inbox healthcheck --config FILE` | Asks `/readyz` of the portal port. |

### Rights of the Inbox (the service `portal`)

The Inbox is a feature of the portal, and the portal has one internal scope for its features: the service `portal`. The code holds its action document. The scope has two features. The keys are in it when the file has the block `keys`. The Inbox is in it when the file has the block `inbox`. The settings pages show the scope as the group **Server permissions**, apart from the group **Services** (the workloads of the launcher). The launcher and the page **Services** never list it. The editor lists each group of the file as the node `inbox_group:<id>`, with the name of the group, so nobody types an id.

| Action | Applies to | Gives |
|---|---|---|
| `inbox.receive` | `inbox_group` | The phones of the account can subscribe to the group, for all the kinds of message. |
| `alert.send` | `inbox_group` | Send an alert to the group (the form, the API with a key). It has the budget `alerts`. |
| `alert.send_user` | the service | Send an alert to any account. The alert goes to each phone of the account. It has the budget `alerts`, the same count as `alert.send`. |
| `alert.read` | `inbox_group` | See the alerts of the group and their delivery. |
| `phone.read` | the service | See all the phones. |
| `phone.revoke` | the service | Revoke any phone. It needs `phone.read`. |
| `phone.log` | the service | See the connection log. |

The text of a grant is, for example, `alert.send on service:portal/inbox_group:infra`. The superadmin has all of them. A permission on the whole service is not possible for the actions of a group: give one permission for each group.

- **Limit.** The actions `alert.send` and `alert.send_user` list the budget `alerts`: a count of alerts in a window of a second, a minute, an hour or a day. The value is on the permission `access on service:portal`, as for each other service. For this service, `access` opens no path: it carries the budget only. The portal counts one use for each request of a key and each send of the form, for the key and for its owner. A request that an `Idempotency-Key` repeats is a use too. A webhook with many alerts is one use. A budget that has no room gives `429` with `Retry-After`. The name of a budget cannot start with `max_`, so the budget has the name `alerts`.
- **Critical groups.** A group with `critical = true` never counts a use. A limit never stops an alert to this group. An alert to an account is never critical, so it always counts.
- **An alert to an account.** `POST /api/v1/alerts` takes exactly one of `group` and `user`. A body with both, or with none, gets `400` with the code `invalid_target`. The target `user` needs `alert.send_user` for the key and for its owner. It goes to each phone of the account that is not revoked, and the phone needs no subscription. An account that does not exist gets `404 unknown_user`. The target can be an account of the database, an account of the file (a block `user`) or the superadmin, and the login is the key of the phones and of the notifications in each case. An account with no phone that works gets `409 no_phone`, and the Inbox stores nothing and counts no use. The level is Ring: the alert is never critical. The frame that the phone receives is the frame of any alert, with the group id `@direct` and the group name `Direct`. The sender, the target account and the superadmin can read the alert. The route `GET /api/v1/alerts/{id}` lets the key that sent it read it.
- **Phones.** A phone subscribes to a group only when its account has `inbox.receive` on the group. The message `welcome` lists these groups. A `subscribe` to another group gets the error `forbidden_group`. When an account loses the permission, the Inbox removes the subscription and tells the phone with the message `subscriptions`. The Inbox checks this when the directory of the portal changes, not for each alert. The Inbox never removes a subscription when it does not know. A directory that is not ready, old or empty changes nothing. An account that the directory does not know changes nothing. An alert that exists stays deliverable. The accounts of the file (blocks `user`) and the superadmin follow the same rules.
- **No block `inbox`.** The file has no Inbox actions, no tab Inbox, no route of the Inbox, no phone endpoint and no `api_` kind. A grant that names an Inbox action stops the start.

### Senders of alerts

The two kinds of key are in "Keys and limits". The kind `api_` sends alerts. A sender sends alerts with an API key. The header is `Authorization: Bearer <key>`. The portal reads the key only in this header. A request with the key in the query of the URL and no header gets `401`. Two sources of API key work at the same time.

- **A key of the file** is a block `api_key` of the block `inbox`. It names its owner, an account of the file (a block `user`), and its groups. At start, the portal checks the grants of the owner and keeps the groups that the owner may send to. The rights of a file account do not change while the server runs. This decision never reads the directory or the database. A key of the file works when the portal is not ready and when PostgreSQL is away. A key of the file has no budget. The key `direct_alerts = true` lets it alert an account too: the owner must hold `alert.send_user` on `service:portal`, and `groups` can then be empty. The list of keys does not list it.
- **A key of the database** is made in the tab **Access** with the kind API. The Inbox checks the key in process, then asks the portal for the decision `alert.send` on `service:portal/inbox_group:<id>` with the key and its owner. A decision no gives `403` with the code `forbidden_group`, or `forbidden_user` for an alert to an account. A budget with no room gives `429`. A key can also read the status of an alert (`GET /api/v1/alerts/{id}`) with `alert.read` on its group. A key sends no notification of the type resolved. When the directory of the portal is not ready, the Inbox answers `503` with the code `directory_not_ready`.

An `mcp_` key, a key with a bad form and an unknown prefix get `401` with no detail. The routes are `POST /api/v1/alerts`, `POST /api/v1/notify/{group}` and `POST /api/v1/webhooks/alertmanager/{group}`. The book icon of the tab Inbox opens a popover with the routes, the groups and the form of the header. `docs/inbox/README.md` has the long text: the recipes for Grafana and Alertmanager and the table of the HTTP statuses.

### The tab Inbox

The tab is one page, `/auth/settings/inbox`, with no sub-page. It works without script. The script only gives the Copy button, the close of a popover and the count of the bulk action. The page answers `403` to an account that holds no permission of the Inbox. Without the block `inbox`, each route answers `404`. Each other address under the prefix answers `404`. A `GET` of a form route answers `405`. A form is a POST with the `Origin` of the portal, the content type of a form and the field `csrf`. A refused form returns to the page with a notice. A session that "Log in as" opens can read the page and make a code. It does not send an alert and does not revoke a phone.

The button **New alert** opens a popup. A switch of two chips, **Group** and **User**, chooses the recipient. The switch exists only when the account may send to a group (`alert.send`) and to an account (`alert.send_user`). With one kind of recipient, the popup shows only its form. The form of a group has a select with the number of members of each group. The form of an account has a list of the accounts with their active phones. An account with no phone is disabled, with the reason "no phone enrolled". The form sends the field `to`: `group:<id>` or `user:<login>`. The list **Sent** has a column **To** with a chip `group:infra` or `@alice`. The chip with the account icon keeps the alerts that went to one account. It is absent when there is none.

Each part of the page needs a permission. A part that the account may not use is not in the page.

| Part | Needs |
|---|---|
| Zone **Sent**: the alerts, the graph, the chips | `alert.read` on one group at least. The page shows the groups of the permission only. |
| Zone **Phones**: all the phones, with their owner | `phone.read`. |
| Zone **My phones**: the phones of the account | `inbox.receive` on one group at least, and no `phone.read`. |
| **Revoke (N)** and the boxes | `phone.revoke` with `phone.read`, or a phone of the account. |
| The plus button | `inbox.receive` on one group at least. |
| Zone **Connections** | `phone.log`. |
| **Send alert** | `alert.send` on one group at least, or `alert.send_user`. The field `to` of the form has the value `group:<id>` or `user:<login>`. |
| The book icon | `alert.send` or `alert.read`. |

- **Filters:** a chip is a link with a query parameter. A chip with a count of zero is absent. A click on the chip that is on turns it off. The parameters are `p`, `g`, `a`, `ps`, `pp`, `mine`, `cs`, `login` and `open`. `p` is the period: `1h`, `6h`, `24h`, `7d` or `30d`. The default is `24h`. `g` is a group. `a` is `ok`, `wait`, `expired` or `none`. `none` is an alert that no phone could receive, because no phone had the group when the alert was made. `ps` is `online`, `unreachable`, `offline` or `revoked`. `pp=1` shows the phones with pending alerts. `mine=1` shows the phones of the account. `cs` is `open` or `closed`. `login` is the search of the connections. `open=<alert id>` opens one alert. A value that is not valid, an unknown parameter and a repeated parameter give `400`.
- **Sent:** the list holds the 500 newest alerts of the period. The count shows `500+` when older alerts of the period exist. The chips of the states and the counts of the groups come from these rows. The graph counts every alert of the period, in 12 to 30 bars, with the alerts of the critical groups on top. A row opens its detail in place with `open=<id>`: the phones that do not have the alert come first, then a few that have it. A chip keeps its place when a filter leaves no alert for it: it is dimmed and it is not a link. A chip exists when the period has an alert for it. The page has no chip for a kind of message that does not exist yet. With one group and no alert to an account, the page has no chip for the groups. A manual alert shows the origin "account". The column of the group shows `@login` for an alert to an account.
- **New alert:** a form that the server refuses gives the page again with status 422. The popup is open, with the title, the recipient and the reason. A new alert opens its row, with no banner. A revoke shows no banner: the row of the phone shows the state.
- **Phones:** the columns are the state (one icon), the phone, the owner and the app. The owner is in the zone **Phones** only. The last columns are the last time seen and the number of alerts that wait. **Revoke (N)** shows when a box is ticked. A click asks "Revoke N phones?" in the same place. The form sends the fields `id.<id>` with the value `1`.
- **Connections:** the newest 200 connections. The search is a form with the method GET. The last column shows the reason, or "open".

### Notifications in the browser

A person that is signed in gets a notification when an alert arrives for a group of the person or for the account. The person needs a permission of the Inbox features, as for the tab Inbox. A person with no such permission sees nothing of this. The feature has no key in the configuration.

**Which alerts.** An account receives the alerts for each group where it holds `inbox.receive`, and the alerts that are for the account itself. The superadmin receives the alerts for all groups. The permission `alert.read` is not necessary. It only decides if a notification has a link to the row of the alert in the tab Inbox. An alert that the account does not receive never leaves the database: the query holds the groups of the account.

**What the person sees.**

- **A badge on the tab Inbox.** It shows the number of unread alerts, up to `99+`. It changes on each settings page without a reload. A person with one tab only has no strip of tabs, so it has no badge.
- **A toast.** A toast shows the title, the group (or "Direct"), the level and a link to the row of the alert. A toast of a critical group stays until the person closes it. The other toasts go after 8 seconds. A pointer or the focus on a toast stops its timer. At most 3 toasts show. The rest is one link "+2 more" to the tab Inbox. A toast has no sound, does not take the focus and has a close button. The key Escape closes the newest toast. A screen reader reads a toast with `aria-live="polite"`. It reads a critical toast with `aria-live="assertive"`. The browser animation stops for a person that asks for less motion.
- **An inbox icon on the launcher**, next to the gear. It has the badge. A click opens a popover with the newest 8 notifications, newest first: the icon of the level, the title, the group, the age and a link. The popover has **Mark all read**, **Open the inbox** and the control of the level. A `details` element makes the popover, so it opens without script.

**The level.** Each account has a level of the toasts: **All** (the default), **Alerts** or **Off**. The control is in the popover and in the bar of the tab Inbox. The portal knows three levels of an alert. **Critical** is an alert for a group with `critical = true`. **Alert** is an alert for another group. **Ring** is an alert for an account. The phones choose their own action, so the portal has no other level. **Alerts** means Alert and Critical only: a Ring alert gives no toast. It stays in the list, and the badge counts it. **Off** gives no toast and no badge, and the script sends no request. The icon stays, so the person can turn the level on again. The store keeps the level for each account, in the table `web_notify`.

**Unread.** The store keeps a marker "read up to" for each account. It is the time of the newest alert that the person has seen. An alert that is newer than the marker is unread. The marker only moves forward. It moves when the person opens the tab Inbox and when the person uses **Mark all read**. The first visit of an account sets the marker to that moment: the older alerts are not unread. The marker is for the web only. The Android app has its own state. A session that "Log in as" opens can read the notifications. It moves no marker and sets no level.

**How the browser asks.** The script `web/notify.js` asks `GET /auth/settings/inbox/notifications?after=<ms>` every 10 to 15 seconds. The delay has a random part. The answer is JSON: the level, the unread count, and the alerts newer than the cursor, at most 10. The portal gives the answer from the shared store, so any instance gives the same answer. The portal has no state in memory for this, and no stream. The script keeps the cursor and the ids that it has shown in `sessionStorage`. A new page of the same tab shows no alert twice. The first request of a page shows no old alert. The cursor starts at the time of the page. The answer also holds the alerts of the 5 seconds before the cursor, and the script drops the ones that it knows. So a difference of up to 5 seconds between the clocks of two instances loses no toast. A tab that is hidden sends no request. When the tab shows again, the script asks at once. After an error, the delay doubles, up to 2 minutes. A redirect, a `401`, a `403` or a `404` stops the script: the session or the permission is gone.

**The cost.** An open tab that shows sends 4 to 6 requests each minute (4.8 on average). A request does this work:

1. It reads the session without a renewal of its idle timer and without a write. An open tab does not keep a session alive. With PostgreSQL, the request reads the table `sessions` at most one time for each 5 seconds, as any request does.
2. It reads the permissions of the account in memory.
3. It reads three small things in the store: the row of the account, the count of unread alerts (it stops at 100) and the alerts newer than the cursor. Each read uses an index (`alerts_created`, `alerts_target` and the key of `web_notify`).

It writes nothing, except for the first request of an account. 100 open tabs make about 8 requests each second on the instances together. The request has no limit of its own. It uses the connection budget `server.max_connections`, as any page does.

**Forms.** Two forms change the state: `POST /auth/settings/inbox/read` and `POST /auth/settings/inbox/notify` (the field `level` is `all`, `important` or `off`). They need the `Origin` of the portal, the content type of a form and the field `csrf` of the purpose `inbox`. The field `back` is `home` or `inbox`: the page that the redirect returns to. With the header `Accept: application/json`, the answer is the state as JSON, with no redirect. The script uses this. Without script, the redirect is the answer, so the popover and its forms work. An unknown field, a bad level and a bad `back` give `400`. A request with no session has the same answer as any other settings page: a redirect to the sign-in page.

**Migration.** The migration adds the table `web_notify` and the index `alerts_target` to the chain of the Inbox, for SQLite and for PostgreSQL. `portal inbox import-sqlite` copies the rows of `web_notify`. During a rolling update, an instance that already runs keeps working. An old instance that starts after the first new instance refuses the new schema, as in "Upgrades".

### Metrics

The metrics of the Inbox are in the Prometheus text of the portal, at `metrics.path`. They have the names `portal_inbox_*`. The gauges `portal_inbox_devices_unreachable`, `portal_inbox_oldest_pending_delivery_seconds`, `portal_inbox_replica_last_snapshot_age_seconds` and `portal_inbox_storage_degraded` are the ones to alert on. The Inbox has no OTLP export. The log of the Inbox is in the JSON lines of the portal, with the field `event`.

### Alerts of the portal

The portal can send an alert about itself. This is optional. It needs the block `inbox.watch`. Without this block, the portal sends no alert about itself.

```hcl
inbox {
  # ...
  watch {
    group             = "infra"   # required: the id of a block group
    service_unhealthy = true      # default
    unhealthy_after   = "2m"      # default
    sign_in_burst     = true      # default
    sign_in_failures  = 10        # default
    sign_in_window    = "5m"      # default
    repeat_after      = "1h"      # default
    recovered         = true      # default
  }
}
```

The portal sends two kinds of alert.

- **A service is unhealthy.** The portal reads the state of each service from the launcher. A service is unhealthy when its tile shows `down` or `unhealthy`. The portal sends the alert when the service stays unhealthy for `unhealthy_after`. The title is `Service Notes is unhealthy for 2 minutes`. A service that is healthy or starting in this time does not raise the alert. A service with no health check is never unhealthy. The portal alerts only for the services that the launcher already probes.
- **Sign-ins fail in a burst.** The portal counts the refused sign-ins: a wrong password, an unknown login and a sign-in that a limiter blocks. It sends the alert when `sign_in_failures` sign-ins are refused in `sign_in_window`. The window slides. The title is `10 refused sign-ins in 5 minutes`. The title has no login, no address and no password. The log of sign-ins has these details.

An alert about a service, and its recovery, follow the rule of the launcher. A member of the group reads the alert only when the member may open the service: the superadmin, or an account with `access` on the service. The rule applies to the phones, the notifications and their count, the page of the Inbox and the keys. A key reads the alert only when its owner may open the service. An alert about a burst names no service, so each member of the group reads it.

The Inbox makes a delivery of an alert about a service only for the phones of these accounts. It checks the rights again before it sends a delivery. When the account lost the access, the Inbox removes the delivery, and the phone never gets the alert. An alert that a phone received stays on the phone. When the portal cannot tell the rights of an account, the account sees no alert about a service. Give `access` on a service to each member that must get the alerts about it (see `SECURITY.md`).

The title shows the values of the configuration, not the measured time. The reason is that each instance must build the same title for the same event.

`repeat_after` is the shortest time between two alerts of the same kind and the same subject. A service that stays unhealthy raises the alert again after this time. A burst that continues does the same. A burst that stops raises no new alert. With `recovered = true`, the portal sends `Service Notes is healthy again` when a service is healthy again after an unhealthy alert. A service that only starts again does not count as healthy.

The portal gives the alert to the Inbox inside the process. There is no HTTP request and no token. The Inbox gives it the same guarantees as an alert of the API. The alert and its deliveries are in one transaction. The idempotency key holds, and the replica copies the alert. The source of the alert is `portal`. If the database has no writable primary, the spool of the degraded mode takes the alert.

With several instances, each instance detects the same event. The idempotency key has only shared facts: `portal:unhealthy:<service id>:<bucket>`, `portal:recovered:<service id>:<bucket>` and `portal:burst:<bucket>`. The bucket is the Unix time divided by `repeat_after`. The Inbox accepts one alert for one key, so the phones get one alert. Two instances can read one event on two sides of a bucket limit. The phones then get a second alert. Each instance counts only the refused sign-ins that it sees.

The detectors run in a background task. They never block the sign-in or the launcher. The sign-in path only adds to a counter that already exists. If the Inbox refuses an alert, the portal logs the event `watch_failed`. It tries again after 10 seconds. If the portal restarts, it forgets what it sent. The key stops a second alert in the same bucket. The portal does not send `healthy again` for an alert that an earlier process sent.

The metrics `portal_watch_sent_total{kind}` and `portal_watch_failed_total{kind}` count the alerts, with the kinds `service_unhealthy`, `service_recovered` and `sign_in_burst`. A key that comes near its budget does not raise an alert in this version.

### Items and content keys

The blocks `inbox.items` and `inbox.at_rest` prepare the approvals and the questions that an agent sends to a person. They are optional, and each one needs the other.

The block `inbox.items` sets the limits: the life of a request, the open items, the lease, the retention and the digest. The key reference lists the keys. The keys that have no default are mandatory.

The portal seals the content of an item with a key of the server before it writes it. It never writes a content in clear text. The block `inbox.at_rest` names the keys. `active_key` seals the new contents. The other keys stay readable, so a rotation is possible.

A key is the base64 text of 32 random bytes. `portal inbox content-key --out FILE` makes one. The file has `secret_env` or `secret_file`, never the text.

**This is not end-to-end encryption.** The portal reads every content, because it opens the sealed bytes to serve the owner.

The seal protects a leak of the database, a backup, a copy of the replica and a disk image. It protects against a database administrator who has no key. It protects against an attacker who can write in the database.

The seal does not protect against a person who has the key and the process of the portal. It does not protect a portal that is compromised while it runs. It does not protect a stolen phone or browser. It does not protect the plain HTTP hop from the proxy to the portal.

Keep the keys outside the data volume and outside the backups of the database. A lost key makes the contents unreadable and keeps the metadata. See [`docs/inbox/SECURITY.md`](docs/inbox/SECURITY.md).

At start, the portal compares each key with a check value in the table `content_key_check`. A key that changed under the same id stops the start. `portal check-config` shows the id, `active` or `read-only`, and the source of each key. It hides the secret.

### The engine of the items

One engine runs the approvals and the questions. It lives in the Inbox and needs the blocks `inbox.items` and `inbox.at_rest`. The engine has no route and no frame yet: the next lots of 5.1.0 add them.

An item has one of seven states: `pending`, `replied`, `answered`, `abandoned`, `expired`, `cancelled` and `superseded`. The states `pending` and `replied` are open. The server decides `abandoned` and `expired` with its own clock. It checks them at each read, at each answer and in a task that runs every 2 seconds. A clock of a phone changes nothing.

The agent states the wait time as `lease_s` or as `until`. The effective lease is 3 times `lease_s` from the last renewal. It never passes the start of the chain plus `inbox.items.max_lifetime`. A renewal writes the lease at most one time in `lease_s`.

When the lease and `inbox.items.lease.grace` end, the item becomes `abandoned`. This is not a refusal. During `inbox.items.lease.revive_window`, a renewal of the same key brings the item back, at most `inbox.items.lease.max_revivals` times. Each revival sends one message.

An answer is one compare-and-set on the row of the item. Two devices that answer at the same time get one `accepted` and one `already_answered`. An answer after the lease and its grace gets `abandoned`. An answer with another effect hash than the stored one gets `effect_mismatch` and changes nothing.

A supersede is one transaction. It makes the old item `superseded` and makes a new item in the same chain. The chain keeps its start time, so a supersede never extends `max_lifetime`.

The row of the table `alerts` for an item has the kind of the item, a fixed title and the column `request_id`. It never holds a body. A change of state adds a row of the database kind `settled`. No frame has this kind. A renewal adds no row. The table `item_events` has one row for each change of state and never a body.

The task of the server writes a heartbeat in the table `items_heartbeat`. After a gap longer than `inbox.items.lease.grace`, it extends each open lease by the gap. With PostgreSQL, two instances never take the same row. An item never uses the spool. When PostgreSQL has no writable primary, a write of an item gets `503 storage_degraded`.

The task erases the sealed body after `inbox.items.body_purge_after`. It deletes a final item after `inbox.items.retention` and its events after `audit_retention`. The effect hash stays with the metadata until the item goes.

The metrics `portal_item_*` count the events, the answers, the lease writes and the bodies that the portal served. The journal has the lines `approval_requested`, `approval_answered`, `approval_consumed`, `approval_abandoned`, `approval_revived`, `approval_superseded`, `approval_expired`, `body_served`, `body_read` and `body_effect_mismatch`. No line holds a body.

### Stop

On SIGTERM, the Inbox tells each phone to reconnect with the close code 1012. It still serves for 5 seconds. Then it closes its listeners and waits for the sessions at most 10 seconds. It writes what the memory holds to the database. With the block `inbox.replica`, it takes a last copy and gives the lease back, so a standby instance takes over at once.

## Brand tokens and logo

The logo is the direction P1, the Doorway. The files are in `assets/brand/`. Its README gives the colours, the typeface and the rules of use.

The stylesheet `web/portal.css` names the brand colours one time, in its first token block. Every other colour token refers to them.

| Token | Value | Use |
|---|---|---|
| `--brand` | `#1238F0` (Signal Blue) | The fill of primary controls, the mark on a light page. White text on it has a ratio of 7.4:1. |
| `--brand-cyan` | `#7FF0FF` | The focus ring on a dark surface. |
| `--brand-300` | `#7D98FF` | Accent text and thin marks on a dark surface. |
| `--brand-ink` | `#0B1220` | Text on a light surface. |

The tokens that a rule uses are `--accent-fill`, `--accent-fill-hover`, `--accent-tint`, `--accent-text`, `--accent-line` and `--focus-ring`.
`--accent-fill` is for a surface that holds white text. `--accent-text` is for text. `--accent-line` is for a mark of 2 pixels or more, such as the underline of a tab. `--focus-ring` is for the focus outline.
The dark and light values of each token pass these ratios: 4.5:1 for text, 3:1 for a focus ring and for the border of a field.
A rule never holds a colour literal. Only the token blocks hold literals.

No page shows the logo or the name of the product in its body. The logo is the icon of the browser tab only. The name is in the title of the tab only.
The icon of the tab is `web/favicon.svg`, a copy of `assets/brand/logo-icon.svg`. The key `ui.favicon` controls it. The default value is `"default"`.
With `"default"`, the pages after sign-in link the icon. The server sends it at a hashed path under `/auth/a/` to a signed-in browser only.
With `"none"`, no page after sign-in links an icon. The icon path then answers as an unknown path does.
The sign-in pages, the code step and the error and status pages never link the icon of the tab. They link the empty icon `data:,` only. The Content-Security-Policy has `img-src 'self' data:` for it. A visitor with no session gets the answer of an unknown path for the icon path. The pages are anonymous on purpose.
The title of the launcher is the value of `server.name`. The title of a settings page is `Settings: <page> · <server.name>`. The sign-in pages keep the title `Authentication`.
A browser that does not support an SVG icon shows its default icon. This is the case for old versions of Safari on macOS and for some other old browsers.

## Local test

See "Quick start" for the Compose stack and `demo/README.md` for the demo with Traefik. The Compose stack publishes its ports on loopback only. In production, the same binary runs behind Traefik, and its ports are not public.

## HCL example

```hcl
server {
  listen_address     = "0.0.0.0:4181"
  name               = "Portal"                       # default; the app shows it as the name of the server
  workers            = 4
  max_body_bytes     = "32KiB"
  max_connections    = 1024
  max_headers        = 64
  max_header_bytes   = "32KiB"
  header_read_timeout = "10s"
  body_read_timeout   = "10s"
  max_login_body_reads = 64
  # The pod network of the cluster, where Traefik runs. [] trusts no proxy.
  trusted_proxies     = ["10.42.0.0/16"]
}

authentication {
  superadmin_login_file    = "/run/secrets/portal_superadmin_login"
  superadmin_password_file = "/run/secrets/portal_superadmin_password"
  # The key of the TOTP secrets (64 hexadecimal characters). The policy `authentication.mfa` is "optional" by default.
  totp_key_file            = "/run/secrets/portal_totp_key"
}

users {
  max_users      = 64
  allow_creation = true
}

access {
  grants_mode = "dynamic"
}

identity {
  issuer           = "https://portal.home.internal"
  signing_key_file = "/run/secrets/portal_identity_key"
}

pdp {
  listen_address = "0.0.0.0:4182"
  url            = "http://portal:4182"
}

storage {
  database       = "/var/lib/portal/portal.db"
  retention_days = 90
}

session {
  secret_mode         = "ephemeral"
  cookie_name         = "__Host-portal"
  ttl                 = "24h"
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

## Configuration

The program reads one HCL file. The HCL parser is a small, strict subset parser. Blocks hold `key = value` lines. A block has no nested block, with one exception. The block `inbox` holds the blocks `devices`, `enrollment`, `storage`, `replica`, `cluster` and `watch`. It also holds the blocks with a label `group` and `api_key`. A block can have a label (`user "alice" { ... }`). A value is a scalar or a list of quoted strings (`["a", "b"]`). The file is the only source of the configuration. The program reads an environment variable or a file only when the file names it.

### Rules

- **The schema is closed.** An unknown block, an unknown key, a duplicate block or a duplicate key stops the program at start.
- **An error stops the program before it serves.** The message gives the key, the values that the key accepts, and the value that the file gave. The message never shows a secret.
- **The program does not change a value.** It does not clamp a number. It does not trim a text. It does not change the case of a text.
- **The types are strict.** Write a number without quotes (`workers = 4`). Write a boolean without quotes, in lower case (`true` or `false`). Write every other value in quotes. A number in quotes or a word without quotes stops the program.
- **A unit is mandatory.** A duration ends with `ms`, `s`, `m`, `h` or `d`. A size ends with `B`, `KiB` or `MiB`.
- **A name of a choice has the exact case of the reference.** `Strict` is valid and `strict` is not. `warning` is not a log level.
- **A key that does not apply must not be in the file.** When `security.login_rate_limit_enabled` is `false`, the keys `login_attempts_per_ip`, `login_window` and `login_lockout` stop the program. The same rule applies to the global limiter. It also applies to `metrics.path` when `metrics.enabled` is `false`. It also applies to `storage.database` with the `postgres` block.
- **Two keys that contradict each other stop the program.** For example, `session.idle_timeout` is longer than `session.ttl`, or `session.remember_ttl` is shorter than `session.ttl`.

### Secrets: `_env` and `_file`

These attributes hold a secret: `authentication.superadmin_login`, `authentication.superadmin_password`, `session.secret`, `postgres.password`, `identity.signing_key`, the `token` of each `pdp_client` block, `inbox.server_key` and the `secret` of each `api_key` block. The file never holds the value. Give exactly one of these two forms:

| Form | The value is |
|---|---|
| `<name>_env = "VAR"` | the content of the environment variable `VAR` |
| `<name>_file = "/path"` | the content of the file, without its last line end |

The program stops at start in these cases:

- The file gives none of the forms, or both of them.
- The file gives the secret in clear text (`<name> = "..."`).
- The variable does not exist, or its value is empty.
- The file does not exist, cannot be read, or is empty. The path is not absolute, or it holds `..`.

The reference tells which attribute accepts `_env` and `_file`. Only these attributes accept them.

### Kubernetes API

The Kubernetes source needs the host and the port of the API of the cluster. The configuration names the two environment variables that hold them. The program reads no other variable. At start, both variables must exist.

```hcl
discovery {
  kubernetes          = true
  kubernetes_host_env = "KUBERNETES_SERVICE_HOST"
  kubernetes_port_env = "KUBERNETES_SERVICE_PORT_HTTPS"
}
```

The two `_env` keys are mandatory when `kubernetes` is `true`. They are not valid when `kubernetes` is `false`.

### Effective configuration

`portal check-config --config FILE` checks the file. It checks the variables and the files that the file names. Then it prints the value that applies to each key. It also prints the source of each value. It ends with `configuration ok`.

```text
server.workers = 4  # default
server.max_body_bytes = "8KiB"  # file
authentication.superadmin_login = <hidden>  # env:PORTAL_SUPERADMIN_LOGIN
session.secret = <hidden>  # file:/run/secrets/portal_session_secret
```

The source is `default`, `file`, `env:VAR` or `file:/path`. A secret always shows `<hidden>`. The superadmin login also shows `<hidden>`. The command prints only the keys that apply. The server writes the same values to its log at start. The event name is `config`. The server writes one line for each key.

### Configuration reference

This reference comes from the table of keys in `src/config/keys.rs`. Run `portal config-reference` to generate it again. A test fails when this text is not the generated text.

<!-- config-reference:begin -->
#### Block `server`

The HTTP server.

| Key | Type | Default | Description |
|---|---|---|---|
| `listen_address` | text: an IP address and a port, such as 0.0.0.0:4181 | required | The address and the port that the server listens on. |
| `name` | text: 1 to 64 bytes, not blank, no control character | `"Portal"` | The name of this instance. The phones show it as the name of the server, and the welcome message uses it. The browser tab shows it at the end of the title of the pages after sign-in. No page body shows it. |
| `workers` | number, 1 to 64 | `4` | The number of worker threads. |
| `max_body_bytes` | size, 1KiB to 1MiB | `"32KiB"` | The largest request body that the server reads. The form of a user holds one field for each grant that it shows. |
| `max_connections` | number, 1 to 100000 | `1024` | The largest number of open TCP connections of the pages and the API. A connection that upgrades to a phone session gives its permit back and takes a permit of the session budget: `server.max_connections` plus two times `inbox.devices.max_devices`. This budget exists only with the block `inbox`. |
| `max_headers` | number, 8 to 256 | `64` | The largest number of headers in a request. |
| `max_header_bytes` | size, 8KiB to 1MiB | `"32KiB"` | The largest size of the headers of a request. |
| `header_read_timeout` | duration, 1s to 1m | `"10s"` | The time that a client has to send the request headers. |
| `body_read_timeout` | duration, 1s to 1m | `"10s"` | The time that a client has to send the request body. A sign-in also gets its Argon2 check in this time, or the server answers `503`. |
| `max_login_body_reads` | number, 1 to 100000 | `64` | The largest number of sign-in requests that the server reads, or that wait for their Argon2 check, at the same time. The server answers `503` at once to a sign-in request above this number. Slow sign-in bodies and a queue of sign-ins thus cannot hold all the connections that ForwardAuth needs. It must be less than `server.max_connections`. |
| `trusted_proxies` | list of at most 64 quoted strings: IP networks such as "10.42.0.0/16" or "127.0.0.1/32"; the address is the first address of the network | required | The networks of the proxies (Traefik) that set `X-Forwarded-For`. The server reads this header only from a TCP peer in these networks. It then takes the first address from the right that is not in these networks. From any other peer, the client address is the address of the peer. `[]` trusts no proxy. The client address keys the session binding, the login limiter and, for the phones, the limiter of the enrollment attempts (the routes of the phones use this list too). |

#### Block `authentication`

The superadmin: the one account that is not in the database. The second factor of the sign-in.

| Key | Type | Default | Description |
|---|---|---|---|
| `superadmin_login` | secret: give `superadmin_login_env` (a variable name) or `superadmin_login_file` (an absolute path), not both | required | The login of the superadmin. It has 1 to 64 characters, and no upper-case letter, no space, no control character and no colon. The portal keeps the colon for the ids of keys. The server does not change it. A new login ends the sessions of the superadmin and the impersonated sessions. An account of the database or a block `user` with this login stops the start. |
| `superadmin_password` | secret: give `superadmin_password_env` (a variable name) or `superadmin_password_file` (an absolute path), not both | required | The password of the superadmin. At start, the server hashes it with Argon2id and erases it from memory. It has at most `security.max_password_bytes` bytes. The server refuses the example values of `.env.example` and of the README. A new password ends the sessions of the superadmin and the impersonated sessions. |
| `mfa` | `off`, `optional`, `required` | `"optional"` | The second factor of the sign-in: a TOTP code from an authenticator app (RFC 6238). `off`: the portal shows no second-factor page and never asks for a code. `optional`: each account chooses to turn it on. `required`: each account, the superadmin included, must enrol after the password step, and the session starts only after the second factor. No separate switch exists for the superadmin. Under `optional`, the superadmin signs in with the password only, so scripts that sign in keep working. A value other than `off` needs `authentication.totp_key_env` or `authentication.totp_key_file`. |
| `totp_key` | secret: give `totp_key_env` (a variable name) or `totp_key_file` (an absolute path), not both | required | The key that encrypts the TOTP secrets in the database (AES-256-GCM). It has 64 hexadecimal characters, which are 32 random bytes. Make it with `openssl rand -hex 32`. All instances use the same key. At start, the server compares the key with a check value in the database. A key that does not match stops the server, and no secret changes. The server never replaces a secret under another key. To use a new key, run `portal reset-two-factor --all` first. Applies only when `authentication.mfa` is `optional` or `required`. |

#### Block `users`

The accounts in the database.

| Key | Type | Default | Description |
|---|---|---|---|
| `max_users` | number, 1 to 1024 | `64` | The largest number of accounts in the database. |
| `allow_creation` | `true` or `false` | `true` | `true`: the superadmin can make accounts in the database. `false`: the route that makes an account answers `404`, and the pages show no button and no entry that makes an account. The accounts that exist stay editable, as `access.grants_mode` says. The value `true` contradicts `access.grants_mode = "static"`: in this mode the database holds no account. |
| `max_grants` | number, 1 to 30000 | `512` | The largest number of grants of one user. A key holds at most 512 grants, at any value of this key. A user with 3 grants for each service and the 8 actions of the service `portal` needs 3 x the number of services + 8 grants. |

#### Block `access`

The grants of the users, and the actions that the services publish.

| Key | Type | Default | Description |
|---|---|---|---|
| `grants_mode` | `static`, `dynamic`, `hybrid` | `"dynamic"` | Where the grants come from. `static`: only the `user` blocks; the settings pages show the grants and do not change them. `dynamic`: only the settings pages; a `user` block stops the server. `hybrid`: both; the grants of a `user` block are read-only in the settings pages. |
| `actions_timeout` | duration, 100ms to 30s | `"2s"` | The time that the server waits for the `/.well-known/procurauth/actions` document of one service. |
| `actions_refresh` | duration, 10s to 1d | `"5m"` | The time between two reads of the `/.well-known/procurauth/actions` documents. It is also the shortest time between two early reads of the document of one service, after an `invalid_resource` of the PDP. |
| `actions_max_age` | duration, 1m to 7d | `"1h"` | The longest time that the server keeps the last good document of a service after its last good read. After this time, the server acts as if the service had no good document. The edge also refuses each key for this service. The value must be more than `access.actions_refresh`. |
| `max_actions` | number, 64 to 4096 | `1024` | The largest number of actions in the document of one service. A larger document is bad. Protocol 5.3c asks a reader to accept 64 at least. |
| `max_resources` | number, 256 to 65536 | `4096` | The largest number of resources in the document of one service. A larger document is bad. Protocol 5.3c asks a reader to accept 256 at least. |
| `max_document_bytes` | number, 65536 to 4194304 | `262144` | The largest size, in bytes, of the document of one service. The server stops the read at this size, and the document is bad. Protocol 5.3c asks a reader to accept 64 KiB at least. The server keeps the last good document of each service in memory. |
| `start_read_deadline` | duration, 1s to 5m | `"5s"` | The longest time that the server waits, at start, for the first read of the documents of the services. After this time, the server opens its ports and reads the other documents in the background. |

#### Block `identity`

This block is optional. If it is present, the server gives an identity token to each service that it lets a user open.

| Key | Type | Default | Description |
|---|---|---|---|
| `header` | text: an HTTP header name of 1 to 64 characters: letters, digits and '-' | `"X-Identity-Token"` | The response header of `/auth/verify` that holds the identity token. Traefik copies it to the request with `authResponseHeaders`. |
| `issuer` | text: 1 to 256 visible ASCII characters, for example https://portal.example.internal | required | The claim `iss` of each identity token. Each service compares it with its `identity.issuer`. |
| `token_ttl` | duration, 10s to 1h | `"5m"` | The life of an identity token (`exp - iat`). It must not be more than the `identity.max_age` of the services. The server gives the same token again until half of its life is gone. |
| `signing_key` | secret: give `signing_key_env` (a variable name) or `signing_key_file` (an absolute path), not both | required | The Ed25519 private key that signs the identity tokens (algorithm `Ed25519`): a PKCS#8 key in PEM, or the base64 text of the PEM on one line. `portal identity-public-key` prints the public key for the services, as a JWK Set. |

#### Block `pdp`

This block is optional. If it is present, the server is an AuthZEN PDP on a second, internal listener.

| Key | Type | Default | Description |
|---|---|---|---|
| `listen_address` | text: an IP address and a port, such as 0.0.0.0:4182 | required | The address and the port of the PDP listener. Do not expose it through Traefik: only the services use it. |
| `url` | text: an http or https URL without a path, a query or a fragment | required | The URL at which the services reach the PDP. `/.well-known/authzen-configuration` gives the endpoints under this URL. |
| `max_body_bytes` | size, 1KiB to 1MiB | `"64KiB"` | The largest request body that the PDP reads. |
| `max_evaluations` | number, 1 to 1024 | `64` | The largest number of evaluations in one request to `/access/v1/evaluations`. |
| `plain_http` | `true` or `false` | `false` | Set it to `true` to accept an `http` value of `pdp.url`. Then the operator states that other means secure the link to the PDP. With `false`, an `http` URL stops the server. With `true`, an `https` URL also stops the server. With `true`, the start log has a warning. |
| `counter_connections` | number, 1 to 64 | `2` | The number of database connections that count the uses of the budgets. Each connection counts approximately 1,000 uses each second. A use that waits more than 500 ms gets the HTTP status 503. |

#### Block `keys`

This block is optional. If it is present, the server accepts keys: a user makes keys in the tab Access. A key has one kind: `mcp` for an agent, verified at the key edge and by the PDP, and `api` for a service, verified by the API of the inbox. The keys `mcp` and `api` turn each kind on or off. The kind `mcp` needs the blocks `identity` and `pdp`. The kind `api` needs the block `inbox`. The block needs one kind on at least.

| Key | Type | Default | Description |
|---|---|---|---|
| `mcp` | `true` or `false` | `true` | `true`: the kind `mcp` is on. A user can make MCP keys, with the prefix `mcp_`, and the key edge and the PDP accept them. `false`: the kind is off. The key edge refuses these keys, and the pages show no MCP key, no MCP right and no MCP chip. The value `true` needs the blocks `identity` and `pdp`. |
| `api` | `true` or `false` | `false` | `true`: the kind `api` is on. A user can make API keys, with the prefix `api_`, and the API of the inbox accepts them. `false`: the kind is off. The API of the inbox refuses these keys, and the pages show no API key, no API right and no API chip. The value `true` needs the block `inbox`. A block `api_key` needs the value `true`. |
| `default_lifetime` | duration, 1d to 365d | `"30d"` | The lifetime that the form of a new key proposes. The form uses the lower of this value and the longest lifetime of the owner (`max_key_lifetime`). |
| `directory_max_age` | duration, 15s to 5m | `"30s"` | The longest age of the directory of the users and the keys. When the last good read of the database is older, the PDP answers `false` for each key and the edge refuses each key. The directory is read again every 5 seconds, so the minimum is 3 rounds. |
| `max_per_user` | number, 1 to 10000 | `100` | The largest number of active keys of one user. An expired key and a key that is too old do not count. |
| `max_counter_rows` | number, 1 to 100000000 | `2000000` | The largest number of counter rows. The server refuses a write that makes more rows. A file that needs more rows stops the server at start. One row is approximately 300 bytes. |
| `max_directory_grants` | number, 1 to 100000000 | `4000000` | The largest number of grants of the directory: the grants of the users and the keys. The server refuses a write that makes more grants. A file that needs more grants stops the server at start. One grant takes approximately 200 bytes of memory. |
| `history_days` | number, 1 to 3650 | `90` | The number of days that the server keeps the summary row of a revoked key. |

#### Block `storage`

The history, and the SQLite file.

| Key | Type | Default | Description |
|---|---|---|---|
| `database` | text: an absolute path without '..' | `"/var/lib/portal/portal.db"` | The path of the SQLite file. Applies only without the `postgres` block. |
| `retention_days` | number, 1 to 3650 | `90` | The number of days that the history keeps the sessions, the service accesses, the audit events and the sign-in attempts. |

#### Block `postgres`

This block is optional. If it is present, the server uses a shared PostgreSQL database in place of the SQLite file.

| Key | Type | Default | Description |
|---|---|---|---|
| `host` | text: a host name or an address of at most 253 characters | required | The host name or the address of the PostgreSQL server. |
| `port` | number, 1 to 65535 | `5432` | The TCP port of the PostgreSQL server. |
| `database` | text: 1 to 63 letters, digits, '.', '-' or '_' | required | The name of the database. |
| `user` | text: 1 to 63 letters, digits, '.', '-' or '_' | required | The name of the database user. |
| `password` | secret: give `password_env` (a variable name) or `password_file` (an absolute path), not both | required | The password of the database user. It is valid UTF-8. |
| `pool_size` | number, 1 to 64 | `4` | The number of connections that the server keeps to the database. |
| `connect_timeout` | duration, 1s to 1m | `"5s"` | The time that the server waits for a new connection. |

#### Block `session`

The sessions and the signing key.

| Key | Type | Default | Description |
|---|---|---|---|
| `cookie_name` | text: a cookie name that starts with __Host- | `"__Host-portal"` | The name of the session cookie over HTTPS. Over plain HTTP the server uses the same name without the prefix, because browsers refuse `__Host-` cookies there. |
| `ttl` | duration, 1m to 7d | `"24h"` | The lifetime of a session from the sign-in. The session ends at this time, even when it is in use. The cookie of this session is a browser-session cookie. |
| `idle_timeout` | duration, 1m to 7d | `"30m"` | A session that nobody uses for this time ends. The value is at most `session.ttl`. A remembered session uses `session.remember_idle_timeout`. |
| `remember` | `true` or `false` | `true` | The sign-in form shows a box "Remember me". A person who ticks it gets a remembered session with a persistent cookie. With `false`, the form has no box and the server ignores the field. |
| `remember_ttl` | duration, 1m to 90d | `"30d"` | The lifetime of a remembered session from the sign-in. The cookie has the same life. The value is at least `session.ttl`. An impersonated session is never remembered. Applies only when `session.remember` is `true`. |
| `remember_idle_timeout` | duration, 1m to 90d | `"7d"` | A remembered session that nobody uses for this time ends. The value is at least `session.idle_timeout` and at most `session.remember_ttl`. Applies only when `session.remember` is `true`. |
| `bind` | `ip_user_agent`, `user_agent`, `none` | `"ip_user_agent"` | What a session cookie is tied to: the client address and the User-Agent, the User-Agent only, or nothing. |
| `same_site` | `Strict`, `Lax` | `"Strict"` | The SameSite attribute of the session cookie. |
| `secret_mode` | `ephemeral`, `provided` | `"ephemeral"` | `ephemeral`: the server makes a new random signing key at each start. `provided`: the key comes from `session.secret_env` or `session.secret_file`. The `postgres` block needs `provided`, because all instances use the same key. |
| `secret` | secret: give `secret_env` (a variable name) or `secret_file` (an absolute path), not both | required | The signing key of the login forms, of the return links and of the anti-CSRF tokens. It has at least 32 bytes. The server refuses the example value of `.env.example`. A new value ends the sessions of the superadmin and the impersonated sessions. Applies only when `session.secret_mode` is `provided`. |
| `max_active_sessions` | number, 1 to 1000000 | `4096` | The largest number of open sessions. A login above this number gets the status 503. |
| `max_sessions_per_account` | number, 1 to 1000000 | `16` | The largest number of open sessions of one account, the superadmin included. A new sign-in of an account at this number ends the session of that account that was used least recently. Thus one account cannot fill `session.max_active_sessions`. An impersonated session does not count. The value is at most `session.max_active_sessions`. |

#### Block `security`

The limits and the login limiters.

| Key | Type | Default | Description |
|---|---|---|---|
| `max_password_bytes` | number, 8 to 16384 | `1024` | The largest size of a password. |
| `max_forwarded_uri_bytes` | number, 128 to 65536 | `4096` | The largest size of the forwarded URI. |
| `max_cookie_header_bytes` | number, 256 to 65536 | `8192` | The largest size of the Cookie header. |
| `login_rate_limit_enabled` | `true` or `false` | `true` | Turn the limiter of failed logins for each client address on or off. |
| `login_attempts_per_ip` | number, 1 to 1000000 | `5` | The number of failed logins for each client address in one window before the lockout. Applies only when `security.login_rate_limit_enabled` is `true`. |
| `login_window` | duration, 1s to 1d | `"1m"` | The window in which the failed logins of one client address count. Applies only when `security.login_rate_limit_enabled` is `true`. |
| `login_lockout` | duration, 1s to 1d | `"5m"` | The time that a client address stays locked out. Applies only when `security.login_rate_limit_enabled` is `true`. |
| `global_login_rate_limit_enabled` | `true` or `false` | `true` | Turn the global limiter of failed logins on or off. A good login does not clear it. |
| `global_login_attempts` | number, 1 to 1000000 | `100` | The number of failed logins from all clients in one window before the lockout. The superadmin login has a separate budget of the same size. Failures against other logins never lock out the superadmin. Applies only when `security.global_login_rate_limit_enabled` is `true`. |
| `global_login_window` | duration, 1s to 1d | `"1m"` | The window in which the failed logins of all clients count. Applies only when `security.global_login_rate_limit_enabled` is `true`. |
| `global_login_lockout` | duration, 1s to 1d | `"30s"` | The time that the logins of the budget stay locked out: all other logins, or the superadmin login. Applies only when `security.global_login_rate_limit_enabled` is `true`. |
| `max_rate_limit_entries` | number, 64 to 1000000 | `4096` | The largest number of client addresses that the limiter remembers. Applies only when at least one login limiter is on. |
| `max_concurrent_password_checks` | number, 1 to 64 | `1` | The largest number of Argon2 checks that run at the same time. |

#### Block `metrics`

The Prometheus endpoint.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | `true` or `false` | `true` | Turn the Prometheus endpoint on or off. |
| `path` | text: an absolute URL path of at most 128 bytes, not /healthz, /readyz or /api/version, and not under /auth/, /v1/ or /api/v1/ | `"/metrics"` | The URL path of the Prometheus endpoint. Applies only when `metrics.enabled` is `true`. |

#### Block `discovery`

The sources of the service catalogue.

| Key | Type | Default | Description |
|---|---|---|---|
| `interval` | duration, 2s to 1h | `"15s"` | The time between two reads of the sources. |
| `probe_timeout` | duration, 200ms to 30s | `"3s"` | The time that a health probe and a source request wait for an answer. |
| `kubernetes` | `true` or `false` | `false` | Read the IngressRoute annotations through the API of the cluster. The server needs the ServiceAccount of the pod. |
| `kubernetes_host_env` | text: the name of an environment variable | required | The environment variable that holds the host of the API of the cluster, for example `KUBERNETES_SERVICE_HOST`. At start, the variable must exist and must not be empty. Applies only when `discovery.kubernetes` is `true`. |
| `kubernetes_port_env` | text: the name of an environment variable | required | The environment variable that holds the HTTPS port of the API of the cluster, for example `KUBERNETES_SERVICE_PORT_HTTPS`. At start, the variable must exist and must hold a port number. Applies only when `discovery.kubernetes` is `true`. |
| `docker_socket` | text: an absolute path without '..' | none | The Docker socket that the server reads the container labels from. Without this key, the Docker source is off. |

#### Block `ui`

The pages.

| Key | Type | Default | Description |
|---|---|---|---|
| `icons_dir` | text: an absolute path without '..' | `"/icons"` | The directory of the offline icon library. If the directory is missing, the pages show initials. |
| `favicon` | `default`, `none` | `"default"` | The icon of the browser tab. With `default`, the tab shows the built-in logo on the pages after sign-in only, and the server sends the icon to a signed-in browser only. With `none`, no page after sign-in links an icon, and the icon route gives the answer of an unknown path. A page before sign-in never has an icon: it has the empty icon. |
| `services_new_tab` | `true` or `false` | `true` | `true`: a tile of the launcher opens its service in a new browser tab. `false`: the tile opens the service in the same tab. The value applies to every service. A service cannot choose. |

#### Block `logging`

The log.

| Key | Type | Default | Description |
|---|---|---|---|
| `level` | `error`, `warn`, `info`, `debug` | `"info"` | The lowest level that the server logs. |

#### Block `inbox`

This block is optional. If it is present, this server runs the inbox: it sends alerts to phones. The inbox uses the database of the portal. The blocks `inbox.devices` and `inbox.storage` are mandatory inside this block. The blocks `inbox.replica`, `inbox.cluster`, `inbox.watch`, `inbox.items` and `inbox.at_rest` are optional. The block `inbox.items` has the blocks `lease` and `digest`. The block `inbox.at_rest` has the blocks `key "<id>"`. The blocks `group "<id>"` and `api_key "<name>"` are inside this block.

| Key | Type | Default | Description |
|---|---|---|---|
| `server_key` | secret: give `server_key_env` (a variable name) or `server_key_file` (an absolute path), not both | required | The key of the server for the phones. Make it one time with `portal inbox keygen`. A new key makes each enrolled phone and each enrollment code invalid. Back it up. |

#### Block `inbox.devices`

This block is inside the block `inbox`. It sets the public address that phones use, the limits of the phones, and the delivery values. The phones use the listener of the portal, as the senders do. The routes of the phones have no session: phones prove themselves.

| Key | Type | Default | Description |
|---|---|---|---|
| `public_address` | text: the public base URL of the portal as the phones reach it: `host:port`, a domain, or an http(s) URL with an optional path prefix; 1 to 512 bytes, no white space, no control character | required | The public base URL of the portal, as the phones reach it from outside: `host:port`, a domain, or an http(s) URL `http(s)://host[:port][/prefix]`, for example `http://172.17.0.1:8088`. The phones add `/v1/enroll` and `/v1/session` to it. The portal serves these routes on `server.listen_address`, with the pages. A proxy that publishes the portal under a prefix must remove the prefix before it sends the request: the portal has no base path. It is the `s` of the enrollment code. The inbox does not open a connection to it. |
| `max_devices` | number, 1 to 100000 | `1000` | The largest number of enrolled phones that are not revoked. |
| `max_per_account` | number, 1 to 100000 | `10` | The largest number of enrolled phones of one account that are not revoked. The inbox refuses the enrollment of one more phone of the account, and the code stays unused. Thus one account cannot use all the places of `inbox.devices.max_devices`. The value is at most `inbox.devices.max_devices`. |
| `heartbeat_interval` | duration, 5s to 1h | required | The app sends a ping at this interval. |
| `session_timeout` | duration, 10s to 1d | required | A session without a frame for this time ends. The value is at least two times `inbox.devices.heartbeat_interval`. |
| `resend_after` | duration, 1s to 1d | required | The inbox sends an alert again to a phone that did not confirm the receipt after this time. |
| `max_inflight` | number, 1 to 1024 | required | The largest number of alerts that the inbox sends to one session before the phone confirms them. |
| `unreachable_after` | duration, 10s to 30d | required | The inbox reports a subscribed phone as unreachable when it did not see the phone for this time. The value is at least `inbox.devices.session_timeout`. |
| `offline_warning_after` | duration, 10s to 30d | required | The app warns its user after it was disconnected for this time. The value is at least two times `inbox.devices.heartbeat_interval`. |
| `max_auth_failures` | number, 1 to 1000 | `5` | The number of failed attempts of one client address in the window `inbox.devices.auth_failure_window`. Each started enrollment counts until it succeeds. A session handshake that proves no enrolled phone counts too, also when it ends at its time limit. At this number, the address cannot start an enrollment, and such a handshake gets the close code 4429. A phone that proves its key always gets its session. |
| `auth_failure_window` | duration, 1s to 1d | `"10m"` | The window of `inbox.devices.max_auth_failures`. |
| `max_handshakes_per_address` | number, 1 to 100000 | `16` | The largest number of phone WebSockets of one client address that did not finish the handshake. The routes `/v1/enroll` and `/v1/session` count together. A new WebSocket of an address at this number ends the oldest handshake of the address with the close code 1013. Thus one client holds at most this part of the session budget. The phones behind one NAT address share this number. |
| `max_request_bytes` | size, 4KiB to 1MiB | `"64KiB"` | The largest request body that the listener reads. |

#### Block `inbox.enrollment`

This block is optional and is inside the block `inbox`. It sets the enrollment codes: an account of the portal makes a code, and a phone uses it one time to enroll. Every key has a default.

| Key | Type | Default | Description |
|---|---|---|---|
| `code_ttl` | duration, 1m to 1d | `"10m"` | An enrollment code works for this time after an account made it. A code works one time. |
| `max_active_per_account` | number, 1 to 100 | `5` | The largest number of enrollment codes of one account that are not used and not expired. The inbox refuses a new code above this number. |

#### Block `inbox.storage`

This block is inside the block `inbox`. It sets the lifetime of alerts and of the connection log. The inbox keeps its tables in the database of the portal: the SQLite file of the block `storage`, or the database of the block `postgres`.

| Key | Type | Default | Description |
|---|---|---|---|
| `delivery_ttl` | duration, 1m to 3650d | required | The inbox does not send an alert again to a phone that did not receive it within this time. |
| `alert_retention` | duration, 1h to 3650d | required | The inbox deletes the alerts, delivered or expired, that are older than this. The value is at least `inbox.storage.delivery_ttl`. |
| `connection_log_retention` | duration, 1h to 3650d | required | The inbox deletes the connection records that are older than this. |
| `pool_size` | number, 2 to 64 | `8` | The number of connections that the inbox uses for requests and sessions. The inbox keeps one more connection for the notifications of the other instances. Applies only with the `postgres` block. |

#### Block `inbox.replica`

This block is optional and is inside the block `inbox`. If it is present, the inbox copies the SQLite file to a directory that the standby instances share. A standby instance takes over when the owner is lost. The block needs the SQLite file: it must not be present with the block `postgres`. The copy covers the whole file, with the tables of the portal.

| Key | Type | Default | Description |
|---|---|---|---|
| `dir` | text: an absolute path without '..' | required | The directory that every instance that can run the inbox shares. It is a volume of plain files. It must not be the directory of `storage.database`. |
| `snapshot_interval` | duration, 1s to 1d | `"10s"` | The inbox copies a changed database at most this often. An accepted alert, an enrollment, a revocation and a form of the portal that changes the database do not wait for this time. |
| `lease_ttl` | duration, 5s to 1h | `"30s"` | The other instances consider an instance lost when its lease did not change for this time. They then take over. |
| `sync_timeout` | duration, 1s to 1m | `"5s"` | The time that an accepted alert, or the answer to a form of the portal, waits for the copy that holds the change. |
| `sync_alerts` | `true` or `false` | `true` | `true`: the inbox confirms an alert only after the copy holds it. If the copy fails, the inbox answers `503` and the sender sends the alert again. `false`: the inbox confirms an alert after the local commit and copies it as soon as possible. |

#### Block `inbox.cluster`

This block is optional and is inside the block `inbox`. If it is present, the instances copy to each other the alerts that they accept while PostgreSQL has no writable primary. The block needs the block `postgres`.

| Key | Type | Default | Description |
|---|---|---|---|
| `listen` | text: an IP address and a port, such as 0.0.0.0:8082 | required | The address and the port where this instance receives the requests of the other instances. Only the pods of the service can reach this port. |
| `peers` | list of at most 16 quoted strings: names with a port such as "inbox-peers.apps.svc.cluster.local:8082" | required | The names that list the instances, for example a headless Service, with the port of `inbox.cluster.listen`. The inbox asks each address that the names resolve to who it is. It skips its own address. The list has at least one name. |

#### Block `inbox.watch`

This block is optional and is inside the block `inbox`. If it is present, the portal sends alerts about itself to one group: when a service of the launcher is unhealthy, and when sign-ins fail in a burst. Without this block, the portal sends no alert about itself.

| Key | Type | Default | Description |
|---|---|---|---|
| `group` | text: the id of a block `group` of the block `inbox` | required | The group that receives the alerts of the portal. It must be the label of a block `group` of the block `inbox`. |
| `service_unhealthy` | `true` or `false` | `true` | `true`: the portal sends an alert when a service of the launcher stays unhealthy for `inbox.watch.unhealthy_after`. A service is unhealthy when its tile shows `down` or `unhealthy`. A service with no health check is never unhealthy. |
| `unhealthy_after` | duration, 10s to 1d | `"2m"` | How long a service stays unhealthy before the alert. A service that is healthy or starting in this time does not raise the alert. |
| `sign_in_burst` | `true` or `false` | `true` | `true`: the portal sends an alert when `inbox.watch.sign_in_failures` sign-ins are refused in the window `inbox.watch.sign_in_window`. A wrong password, an unknown login and a sign-in that a limiter blocks count. Each instance counts the refusals that it sees. |
| `sign_in_failures` | number, 2 to 10000 | `10` | The number of refused sign-ins in the window that raises the alert. |
| `sign_in_window` | duration, 10s to 1d | `"5m"` | The window of `inbox.watch.sign_in_failures`. It slides: the portal counts the refusals of the last window. |
| `repeat_after` | duration, 1m to 7d | `"1h"` | The shortest time between two alerts of the same kind and the same subject. A service that stays unhealthy raises the alert again after this time. A burst that continues does the same. The alerts of this time have one shared identity, so two instances send one alert. |
| `recovered` | `true` or `false` | `true` | `true`: the portal sends a short alert when a service is healthy again after the portal sent an alert about it. |

#### Block `inbox.items`

This block is optional and is inside the block `inbox`. It sets the limits of the items: the approvals and the questions that an agent sends to a person. It has the blocks `lease` and `digest`. Each limit that the file does not give a default for is mandatory: a wrong default would hide an item or keep it too long. The block needs the block `inbox.at_rest`, and the block `inbox.at_rest` needs this block.

| Key | Type | Default | Description |
|---|---|---|---|
| `max_lifetime` | duration, 1d to 365d | required | The longest life of a chain of requests, from the start of the thread. At this age the item is `expired`, which is a neutral end. A replacement does not extend it. It does not set how long a request waits. Recommended: `"90d"`. |
| `max_open_per_key` | number, 1 to 1000 | required | The most open items of one key. Recommended: `5`. |
| `max_open_per_owner` | number, 1 to 10000 | required | The most open items of one owner. Recommended: `20`. |
| `max_open_total` | number, 1 to 1000000 | required | The most open items in all. A new item above this number is refused. |
| `max_chain` | number, 1 to 50 | `10` | The most replacements in one thread. |
| `max_turns` | number, 2 to 200 | `40` | The most turns in one thread. |
| `stale_after` | duration, 1m to 365d | `"24h"` | The age from which a screen shows the banner "old request" and asks for one more confirmation. |
| `retention` | duration, 1d to 3650d | required | How long a finished item stays, with its metadata only. It must be at least `max_lifetime` plus `lease.revive_window`. |
| `body_purge_after` | duration, 0s to 3650d | required | The age from which the sealed contents are erased. The metadata and the effect hash stay. It must not be above `retention`. Recommended: `"30d"`. |
| `audit_retention` | duration, 1d to 3650d | required | How long the audit events stay. They hold metadata only. It must be at least `retention`. |
| `max_body_bytes` | size, 1KiB to 32KiB | `"16KiB"` | The largest content of an item of a question. An approval has its own limits. |
| `wait_max` | duration, 0s to 1m | `"25s"` | The longest `wait` that the API accepts for a long poll. |

#### Block `inbox.items.lease`

This block is inside the block `inbox.items`. The lease is the time that an item stays open while its agent shows that it is alive. An agent that sends no keepalive loses the item.

| Key | Type | Default | Description |
|---|---|---|---|
| `min` | duration, 5s to 1h | required | The smallest `lease_s` that the portal accepts. Recommended: `"30s"`. |
| `max` | duration, 5s to 30d | required | The largest `lease_s`. The lease that applies is three times `lease_s`. It must be at least `lease.min` and at most `max_lifetime`. Recommended: `"7d"`. |
| `grace` | duration, 0s to 1h | required | The time after the end of a lease before the item is `abandoned`. Recommended: `"2m"`. |
| `revive_window` | duration, 0s to 365d | required | The time after an abandon in which a keepalive of the same key opens the item again. `"0s"` means never. It must not be above `max_lifetime`. Recommended: `"7d"`. |
| `max_revivals` | number, 0 to 10 | `3` | The most times that one item opens again. |

#### Block `inbox.items.digest`

This block is inside the block `inbox.items`. It sets the default summary of the open items that a phone gets. An account can change its own summary, but never above a limit of this file.

| Key | Type | Default | Description |
|---|---|---|---|
| `mode` | `off`, `daily`, `every` | required | The default summary of the open items. `off`: no summary. `daily`: one summary each day at `at`, in `timezone`. `every`: one summary at each interval `every`. Recommended: `"daily"`. |
| `at` | text: a time of the day as HH:MM, from 00:00 to 23:59 | required | The time of the daily summary, as `HH:MM` on a 24-hour clock. Applies only when `inbox.items.digest.mode` is `daily`. |
| `timezone` | text: an IANA time zone name, such as Europe/Paris or UTC | required | The time zone of `at`, as an IANA name. The start checks the form of the name. A later release checks the name in a time zone database. Applies only when `inbox.items.digest.mode` is `daily`. |
| `every` | duration, 1h to 7d | required | The interval between two summaries. Applies only when `inbox.items.digest.mode` is `every`. |

#### Block `inbox.at_rest`

This block is optional and is inside the block `inbox`. The portal seals the contents of the items with a key of the server before it writes them. It never writes a content in clear text. The block has one block `key "<id>"` for each key. The block needs the block `inbox.items`, and the block `inbox.items` needs this block.

| Key | Type | Default | Description |
|---|---|---|---|
| `active_key` | text: the label of a block `key` of this block: 1 to 16 characters of a-z, 0-9, '-' and '_' | required | The key that seals the new contents. The other keys of the block stay readable: use them for a rotation. No switch turns the seal off. |
| `admin_body_read` | `true` or `false` | `false` | `true`: the superadmin can read the contents of the items. The portal writes one audit event and counts each read, never the content. `false`: no route lets an administrator read a content. Only the owner reads it. |

#### Block `user "<login>"`

This block has a label and can occur more than one time. The grants of one user that the configuration gives (modes `static` and `hybrid`). The label is the login, as the server keeps it: 1 to 32 characters of a-z, 0-9, '.', '-' and '_'. The account itself is in the database. The superadmin has all grants and has no `user` block.

| Key | Type | Default | Description |
|---|---|---|---|
| `grants` | list of at most 30000 grants, each a quoted text or an object with `grant`, `limits` and `confirm`: a grant such as "access on service:grafana" or "query.run.write on service:clickhouse-dash/host:local/database:analytics": an action, the word on, service: and the id of the service, then 0 to 4 segments /type:id with each id percent-encoded. To give limit values, write { grant = "access on service:datastore", limits = { requests = [ { amount = 300, window = "minute" } ] } }. To ask for an approval, write { grant = "query.run.write on service:clickhouse-dash/host:local", confirm = true } | required | The grants of the user. The user is an account of the file, with no row in the database. `access` lets the user open the service. The other actions are the names that the service publishes at `/.well-known/procurauth/actions`. A grant names a node of the resource tree of the service: `<action> on service:<id>`, then 0 to 4 segments `/<type>:<id>`, for example `query.run.read on service:clickhouse-dash/host:local/database:analytics`. A grant covers its node and each node below it. A type starts with a-z, then has a-z, 0-9 and '_', 1 to 32 characters, and is not `service`. Each id is percent-encoded: each byte outside A-Z a-z 0-9 - . _ ~ is %XX with upper-case hex digits, for example `bucket:a%2Fb` for the id `a/b`. Another encoding of the same id stops the start. A decoded id has 1 to 256 bytes, without a control character. The action `access` has grants on the service only. The `confirm` member of an object is `true` or `false`, and the default is `false`. With `confirm = true`, each use of the action needs the approval of a person. A grant of the service `portal` or of the action `access` with `confirm = true` stops the start. The `limits` object holds one member for each limit. A ceiling is a whole number. A budget is a list of objects with `amount` and `window` (`second`, `minute`, `hour` or `day`). A budget value is only on the grant `access` on a service. A grant has at most 8 limit values. A value is at most 1,000,000,000. The limit values take at most 1024 bytes as compact JSON. The number of grants is at most `users.max_grants`. At start, the server checks only the form of each grant: the services and their trees of types are known at run time. The Services page of the settings shows a grant that does not follow the tree of its service. The PDP does not apply this grant. A block `user` stops the server when it gives `keys.own.create`, or an action that needs another action that it does not give. |
| `password_hash` | secret: give `password_hash_env` (a variable name) or `password_hash_file` (an absolute path), not both | none | The Argon2id digest of the password of the user, in the PHC string form, as the database keeps it. Without it, the account opens no session, and only its keys act for it. |

#### Block `key "<name>"`

This block has a label and can occur more than one time. One key of a user of the file. The block is inside a block `user`. The label is the name of the key: 1 to 64 characters, with no control character. Two keys of one user have two names. The subject id of the key is `key:` and the first 16 hexadecimal characters of the SHA-256 digest of the login, a zero byte and the name. A key of the file has no expiry date. To stop it, remove the block and restart each instance. The block needs the block `keys`.

| Key | Type | Default | Description |
|---|---|---|---|
| `grants` | list of at most 512 grants, each a quoted text or an object with `grant`, `limits` and `confirm`: the same form as `user.grants` | required | The grants of the key, in the form of `user.grants`. A key holds at most 512 grants. A grant of an action of the service `portal` stops the server. The decision for a key never gives more than the grants of its owner. The key needs an approval when a grant of the key or a grant of its owner has `confirm = true`. |
| `interaction` | `conversational`, `one_shot` | `"conversational"` | What the key can do when the server asks a person. `conversational`: an agent that can discuss and read free text. `one_shot`: a system that decides without free text and gets closed questions only. The agent cannot change this value. |
| `key` | secret: give `key_env` (a variable name) or `key_file` (an absolute path), not both | none | The text of the key: `mcp_`, 43 characters and 6 characters of checksum. Give `key_env` or `key_file`, or give `key_sha256_env` or `key_sha256_file`: exactly one of the four. A bad form stops the server. |
| `key_sha256` | secret: give `key_sha256_env` (a variable name) or `key_sha256_file` (an absolute path), not both | none | The SHA-256 digest of the text of the key: 64 hexadecimal characters, in upper or lower case. A digest that the database or another block also holds stops the server. The digest must be the digest of a text of the form `mcp_`, 43 characters and 6 characters of checksum: the server cannot check this at start. |

#### Block `group "<id>"`

This block has a label and can occur more than one time. One group of alerts. The block is inside the block `inbox`. The label is the id of the group: 1 to 64 characters of a-z, 0-9, '-' and '_'. A phone subscribes to a group. A key sends an alert to a group.

| Key | Type | Default | Description |
|---|---|---|---|
| `name` | text: 1 to 128 bytes, not blank | required | The name of the group that the phones show. |
| `description` | text: at most 512 bytes | `""` | The description of the group. |
| `critical` | `true` or `false` | `false` | `true`: the group is critical. A limit on the action `alert.send` never applies to a critical group. `false`: the limit of a key applies to the group. |

#### Block `api_key "<name>"`

This block has a label and can occur more than one time. One API key of the file: the key of a monitoring tool, a script or a webhook that sends alerts. The block is inside the block `inbox`. The label is the name of the key: 1 to 64 characters of a-z, 0-9, '-' and '_'. The key follows its owner, as each key does: it can do only what the key and its owner both have the right to do. The owner is an account of the file (a block `user`), so the key and its account have one source. The rights of a file account do not change while the server runs, so the server decides at start which groups the key can send to. It also keeps the groups of the key where the owner holds `alert.read`: only these give the status of an alert (`GET /api/v1/alerts/{id}`). This decision does not wait for the database. The block needs `keys.api = true`. The key is not in the list of keys.

| Key | Type | Default | Description |
|---|---|---|---|
| `owner` | text: the login of an account of the file (a block `user`) | required | The login of the owner of the key. The owner must be an account of the file: a block `user`. The owner must hold `keys.own.use` on `service:portal/key_kind:api`, `alert.send` on each group of the key, and `alert.send_user` on `service:portal` when `direct_alerts` is `true`. |
| `secret` | secret: give `secret_env` (a variable name) or `secret_file` (an absolute path), not both | required | The text of the key: `api_`, 43 characters and 6 characters of checksum, 53 characters in all. Make it with `portal inbox token`. Two keys must not have the same text. A bad form stops the server. |
| `groups` | list of at most 64 quoted strings: ids of groups that the block `inbox` defines | required | The groups that the key can send alerts to. The list has at least one group, except when `direct_alerts` is `true`: then it can be empty. Each group must exist, and the owner must hold `alert.send` on it. |
| `direct_alerts` | `true` or `false` | `false` | `true`: the key can send an alert to any account (`POST /api/v1/alerts` with the field `user`). The owner must hold `alert.send_user` on `service:portal`. `false`: the key sends to groups only. |
| `interaction` | `conversational`, `one_shot` | `"one_shot"` | What the key can do when the server asks a person. `one_shot`: a system that decides without free text and gets closed questions only. `conversational`: an agent that can discuss and read free text. The service cannot change this value. |
| `webhook_send_resolved` | `true` or `false` | `false` | `true`: the notifications "resolved" of Alertmanager and Grafana become alerts. `false`: the inbox ignores them. |

#### Block `pdp_client "<server>"`

This block has a label and can occur more than one time. One server that asks the PDP for decisions. The label is the name of the server: the id of its service in the catalogue, and the audience of its identity tokens. The PDP answers only for the resources of this server.

| Key | Type | Default | Description |
|---|---|---|---|
| `token` | secret: give `token_env` (a variable name) or `token_file` (an absolute path), not both | required | The bearer token of the server. It has at least 32 bytes. Two servers must not have the same token. |

#### Block `inbox.at_rest.key "<id>"`

This block has a label and can occur more than one time. One key that seals the contents of the items. The block is inside the block `inbox.at_rest`, which has 1 to 8 of these blocks. The label is the id of the key. A key that is not the `active_key` stays readable and seals nothing.

| Key | Type | Default | Description |
|---|---|---|---|
| `secret` | secret: give `secret_env` (a variable name) or `secret_file` (an absolute path), not both | required | The key: the base64 text of 32 random bytes, 44 characters. Make it with `portal inbox content-key`. The bytes must not all be equal. Two keys must not have the same text. The file never holds the text: give `secret_env` or `secret_file`. |
<!-- config-reference:end -->

## HTTP protection

Hyper parses HTTP/1. The application configures bounded HTTP behavior:

- maximum simultaneous TCP connections;
- maximum number of request headers;
- maximum Hyper header buffer;
- header-read timeout;
- application body-size limit;
- body-read timeout;
- bounded concurrent Argon2 checks.

Dynamic `Location`, `Set-Cookie` and `Retry-After` values are converted to typed HTTP header values. An invalid value fails closed. The program does not emit it.

The crate uses `#![forbid(unsafe_code)]` for project code.

## CSRF and Origin/Referer verification

`POST /auth/login`, `POST /auth/logout` and the account routes (`POST /auth/users/add`, `/delete`, `/impersonate`, `/impersonate-end`, `/end-sessions`) require both:

1. a CSRF token. For login, the token is the double-submit cookie. The cookie lives 12 hours. When it is gone, the portal gives the sign-in page again with a new token and the sentence "The page was open too long" (status 403). For the other routes, the token is an HMAC of the session ID and of the purpose (`logout`, `users`). The form of the button carries the HMAC. The HMAC has no cookie that can expire under a page left open. A token for one action cannot be replayed on another action; and
2. a same-origin `Origin` check. The program uses `Referer` only when `Origin` is absent.

The account routes also check on the server that the session belongs to the superadmin. The session must not be an impersonated one. In K3s, these paths do not pass through ForwardAuth. portal therefore cannot rely on the gate.

The Sign out button sends a POST. This POST ends the session at once. `GET /auth/logout` only redirects to `/`.

When requests arrive through Traefik, the program builds the expected public origin from the unique `X-Forwarded-Proto` and `X-Forwarded-Host` values. For direct local access, the program uses `http` and the unique `Host` header. The program rejects these inputs: partial forwarded-origin metadata, duplicate headers, `Origin: null`, foreign origins, userinfo and invalid schemes.

Authentication pages return `Referrer-Policy: same-origin`. The Referer fallback stays available. Paths do not leak across origins.

## Opaque session behavior

Session cookies use:

```text
__Host-portal=<opaque random token>; Path=/; HttpOnly; SameSite=Strict; Secure                  (HTTPS)
portal=<opaque random token>; Path=/; HttpOnly; SameSite=Strict                                 (plain HTTP)
```

These cookies have no `Max-Age`. The browser deletes them when it ends its session. The server ends the session after `session.ttl`. When the person ticks "Remember me", the cookie has `Max-Age=<session.remember_ttl>`, for example `Max-Age=2592000` (see [Remember me](#remember-me)).

After **Log in as** or **Back to superadmin**, `Max-Age` is the time left: the new session ends when the session of the password sign-in would have ended. A switch never gives a fresh `session.ttl`, and it never gives the remembered rule.

The program reads the scheme for each request from `X-Forwarded-Proto`. A session is bound to the client address and the User-Agent of the login. The client address follows the rule of `server.trusted_proxies` (see "Anti-bruteforce"). The binding is set by `session.bind = "ip_user_agent"`. The other values are `user_agent` and `none`. A session ends after `session.idle_timeout` without use. The default is `30m`. A remembered session ends after `session.remember_idle_timeout` without use. The default is `7d`.

The token contains no identity, no expiration timestamp, no claims and no signed application data. The server stores only a SHA-256 digest of each token. It also stores the expiration time. With SQLite, this data is in memory. With PostgreSQL, this data is in the shared database.

The session store is bounded by `session.max_active_sessions`. If the store is full, a successful password check cannot create a session. The program returns `503`. It does not remove an arbitrary live session.

One account has at most `session.max_sessions_per_account` sessions (16 at first). A new sign-in of an account at this number ends the session of that account that was used least recently. Thus one account cannot fill the store, and the other accounts can still sign in. An impersonated session does not count.

## Anti-bruteforce

Two independent controls run before Argon2. Each control is local to the process. With several instances, each instance has its own counters. See above.

- per-IP failed-password limiter;
- global failed-password limiter for distributed attacks.

The global limiter has two budgets of the same size. One budget is for the superadmin login. The other budget is for all other logins. Failures against other logins thus never refuse the sign-in of the superadmin. Failures against the superadmin login lock out only the superadmin. While a budget is locked out, the answer tells whether a typed login is the superadmin login. The superadmin login is not a secret: the identity tokens carry it.

The per-IP limiter uses the client address. The client address comes from `X-Forwarded-For` only when the TCP peer is in `server.trusted_proxies`. For any other peer, the client address is the address of the peer. A client that connects to the port directly thus cannot choose its address.

Only failed password checks use the failure budgets. A successful authentication does not clear the per-IP failures, because they can name other logins. They expire after `security.login_window`. A successful authentication does not clear the global failure history either. This is intentional.

`max_concurrent_password_checks` separately limits the number of concurrent Argon2 operations.

## Hardening of the sign-in page

The sign-in page has five measures. The full description is in `docs/login-hardening.md`. No measure adds a configuration key. The box "Remember me" has its own keys (see [Remember me](#remember-me)).

- **Anti-enumeration (real protection).** An unknown login, a wrong password and a failed check of the form give the same answer: status 401, the same page, the same headers. Each of them runs one Argon2 check and counts as one failure. A refusal of a limiter (status 429) is the only visible difference. It does not depend on the login, with one documented exception: the budget of the superadmin login.
- **Hide the product (obscurity, not security).** The page has a neutral title, no comment and no version. The stylesheet and the script have neutral paths, `/auth/a/<hash>.css` and `/auth/a/<hash>.js`. The cookie of the form is `lc`. The default name of the session cookie is not changed: set `session.cookie_name` for a neutral name.
- **Anti-bot checks (real protection, limited).** The form has a signed token with a minimum age of 1 second and a maximum age of 12 hours. The box "Remember me" is read with the other fields. It never changes the answer to a refusal. It has a trap field that a person cannot reach. A failed check looks exactly like a wrong password, and it counts as a failed sign-in. The metric `portal_login_form_check_failures_total` counts the failed checks. The names of the login and password fields stay stable, so that password managers work.
- **Script minification (obscurity, not security).** The script and the stylesheet that every page loads are minified at the first request, with no tool at build time. A test runs `node --check` on the script when `node` exists.
- **Random names and structure (obscurity, not security).** The sign-in page, the status pages and the launcher load only the shared stylesheet and the theme script. Their classes, identifiers and data attributes have short names that look random. The names are an HMAC of the plain names, under a key that comes from the session secret. All instances with the same secret use the same names, and a restart does not change them. The server also changes the structure of the HTML at each render: it inserts 0 to 3 inert elements, and it changes the order of the attributes and of the elements of the head. The look of the page does not change. The settings pages keep the plain names. The documentation lists what still identifies the product.

A script that signs in with `curl` must read the field `token` of the page, wait one second, and post the field.

## Prometheus

When enabled, `/metrics` exports Prometheus metrics with bounded cardinality. The metrics cover: HTTP status and route; login success and failure; CSRF and origin failures; IP and global rate limits; session checks; active sessions; session-capacity rejections; Argon2 concurrency; HTTP connections; sign-in bodies refused at the limit `server.max_login_body_reads` (`portal_login_body_rejected_total`); the state of the catalogue (`portal_catalog_complete`); and uptime. The Inbox adds the metrics `portal_inbox_*`, and the alerts of the portal add `portal_watch_*` (see "Operations").

Do not put client IPs, raw URIs, passwords, cookies or return targets into metric labels.

See `examples/prometheus.yml`.

## Global Traefik ForwardAuth

The project is designed for installation as an entryPoint-level middleware. A request to a protected application first reaches `/auth/verify`. Without a valid session, the service returns a redirect to `/auth/login`. The redirect contains a validated, HMAC-signed path and query as the return target. After a successful login, the service returns the browser to that target.

`/auth/verify` explicitly lets the public auth paths `/auth/login`, `/auth/logout` and `/auth/style.css`. This prevents a loop when the ForwardAuth middleware is global. `/auth/success` still requires a valid session.

Any other path that the portal receives redirects to the login page when there is no session. This applies to `/auth/…` and to the catch-all route. It applies whether or not the path exists. Once the user is signed in, the portal answers 404. In the Traefik routes, the catch-all runs ForwardAuth first. Then Traefik rewrites the path to `/__portal-fallback-404`. This keeps the portal's own endpoints from being exposed on arbitrary paths.

See:

- `examples/traefik-entrypoint-snippet.yaml`
- `examples/traefik-dynamic.yaml`
- `demo/`: a complete local stack with Traefik, the portal, a service with a front, and the routes for keys (`demo/README.md`)

## Absolute JS/CSS paths

The authentication cookie has `Path=/`. So same-origin resources carry the same session cookie. Examples are `/app.js`, `/css/app.css`, `/api/...` and `/images/...`. After login, these resources pass the global ForwardAuth check.

Authentication does not rewrite application paths. If an application mounted under `/testservice` emits `/app.js`, Traefik still needs a routing rule. The rule maps `/app.js` to the intended backend.

## Reproducible dependency resolution

Bootstrap `Cargo.lock` on a development machine with Docker. Then commit it:

```bash
./scripts/update-lockfile.sh
```

From that point, `Cargo.lock` is part of the source tree. Review and commit it whenever dependencies change. Normal build paths never regenerate it:

```bash
cargo fetch --locked
cargo test --frozen
cargo build --release --frozen
```

The steps that compile and test the code use `--frozen`. These steps run after the dependencies are fetched. The Docker build follows the same pattern.

When you change `Cargo.toml` on purpose, run the same maintenance script. Review the lockfile diff that results. Commit the lockfile before CI or release.

## Native commands

```text
portal run --config FILE
portal check-config --config FILE
portal config-reference
portal identity-public-key --config FILE
portal healthcheck --config FILE
portal import-sqlite --config FILE --from SQLITE_FILE
portal end-sessions --config FILE
portal revoke-keys --config FILE (--user LOGIN | --all)
portal reset-two-factor --config FILE (--user LOGIN | --all)
portal hash-password < password.txt
portal inbox keygen --out FILE
portal inbox token --out FILE
portal inbox content-key --out FILE
portal inbox import-sqlite --config FILE --from SQLITE_FILE
portal inbox healthcheck --config FILE
portal version
```

## Build and release

The product has one version. It is the field `version` of the root `[package]` in `Cargo.toml`. The crates of the phone have the same version in `[workspace.package]`. `tools/version.sh` prints it and stops if the two differ. The binary, the Android app and the release tag use this version.

Everything is built in Docker. The host needs Docker with BuildKit, POSIX `sh` and `python3`. No command pushes an image or a file to a registry.

| Command | Result |
|---|---|
| `tools/build.sh server` | `dist/portal_<version>_linux_<arch>`: a static musl binary, checked for dynamic links. |
| `tools/build.sh image` | The local images `portal:<version>` and `portal:local`. |
| `tools/build.sh apk` | `dist/portal-inbox-<version>.apk`, signed. See `tools/android/build-apk.sh` for the signing variables. |
| `tools/build.sh dist` | `dist/portal_<version>_linux_<arch>.tar.gz` and `dist/SHA256SUMS`. The archives are reproducible. |
| `tools/build.sh all` | The four commands above. |

The image is `docker/Dockerfile`. It builds on `rust:1.98-alpine3.24`, so the binary is static. The runtime stage is `scratch`, with the user `10001`, and it works with a read-only root file system. Its `HEALTHCHECK` is `portal healthcheck`. For the other architecture, use `docker buildx build --platform linux/arm64`. The build runs natively on that architecture. There is no cross compilation.

`docker compose up` at the root of the repository starts the portal with the Inbox, and PostgreSQL. `deploy/README.md` describes the Compose stack and the K3s manifests.

| Check | Command |
|---|---|
| Format and clippy | `tools/dev/lint.sh` |
| Tests of the workspace | `tools/dev/test.sh --workspace --no-fail-fast` |
| Configuration files of `deploy/` | `tools/dev/check-configs.sh` |
| Compose stack | `tools/dev/smoke.sh` |

The GitHub workflow `ci.yml` runs these checks and the build of the app. It also fails when a workflow mentions a container registry. The workflow `release.yml` starts on a tag `vX.Y.Z`. The tag must be equal to the version in `Cargo.toml`. The release holds the binaries for amd64 and arm64, the signed APK and `SHA256SUMS`. It holds no image. `tools/release/local-release.sh` builds the same files on this machine when GitHub Actions is not available.

## Operations

### Back up

Back up two things: the database and the key file of the Inbox server. With SQLite, copy the file with `sqlite3 .backup`, or stop the instance first. With PostgreSQL, use the backup of the database cluster.

If you lose the key file of the Inbox server, every enrolled phone and every enrollment code becomes invalid. Each person must then enroll again. A new key file has the same effect. Keep the file secret.

### Metrics

Prometheus reads `metrics.path` of the pages listener. Alert on the metrics in bold.

| Metric | Use |
|---|---|
| **`portal_inbox_oldest_pending_delivery_seconds`** | Age of the oldest alert that still waits for a phone. |
| **`portal_inbox_devices_unreachable`** | Subscribed phones that the Inbox did not see for `inbox.devices.unreachable_after`. |
| **`portal_inbox_storage_degraded`** | 1 while the database has no writable primary and the Inbox holds alerts in memory. |
| **`portal_inbox_replica_last_snapshot_age_seconds`** | Age of the newest snapshot (with `inbox.replica` only). |
| `portal_inbox_alerts_accepted_total{source,kind,target}` | Messages accepted, by source, by kind (`alert`) and by target (`group` or `user`). The source names the route: the JSON route, a webhook, the notify route, the form of the page or the portal itself. |
| `portal_inbox_alerts_duplicate_total{source,kind,target}` | Submissions that the idempotency key merged. |
| `portal_inbox_deliveries_created_total`, `portal_inbox_deliveries_sent_total`, `portal_inbox_delivery_receipts_total` | Deliveries made, frames sent (with repeats), receipts from phones. |
| `portal_inbox_delivery_latency_seconds` | Histogram. Time from acceptance to the first receipt. |
| `portal_inbox_deliveries_pending` | Deliveries that wait for a phone. |
| `portal_inbox_devices_enrolled`, `portal_inbox_device_sessions` | Phones that are not revoked, and connected sessions. |
| `portal_inbox_device_session_closes_total{reason}` | Session ends, by reason. |
| `portal_inbox_auth_failures_total{kind}` | Failed authentications. |
| `portal_inbox_enrollments_total` | Phones enrolled. |
| `portal_inbox_alerts_spooled_total{source,kind,target}`, `portal_inbox_spool_alerts`, `portal_inbox_spool_rejected_total` | The spool of the degraded mode. |
| `portal_inbox_alerts_refused_total{stage}` | Alerts that the Inbox refused one at a time, without a stop of the others. `webhook`: an alert of a webhook payload. `replay`: a spooled alert that the database refused, one count for each try. |
| `portal_inbox_replica_epoch`, `portal_inbox_replica_snapshots_total`, `portal_inbox_replica_snapshot_failures_total`, `portal_inbox_replica_sync_failures_total` | The replica (with `inbox.replica` only). |
| `portal_watch_sent_total{kind}`, `portal_watch_failed_total{kind}` | Alerts of the portal about itself, by kind: `service_unhealthy`, `service_recovered`, `sign_in_burst`. |
| `portal_directory_age_seconds` | Time since the database last answered a read of the directory. It grows when the refresh fails. |

The other `portal_*` metrics are in the sections "Prometheus" and "Several instances". The example `examples/prometheus.yml` shows a scrape job.

### Logs

The log is JSON, one line for each event, with the field `event`. The Inbox writes in the same log.

### Upgrades

The new binary changes the schema at start. It only adds. Each chain of migrations refuses a schema that is newer than the binary. An earlier binary does not start on a new schema. A return to an earlier version needs a restore of the database from before the upgrade.

Version 5.0.0 is the first version of the merged product. Read these points before you upgrade from a portal with grants and keys:

- The Inbox is new. Without the block `inbox`, nothing changes for the portal.
- A key gets a kind. A key of an earlier version becomes an MCP key. The rights to make and to use a key now apply to the kinds: give `keys.own.create` and `keys.own.use` on `key_kind:mcp` or `key_kind:api`. These two actions are not valid on the whole service.
- An account that has no permission `inbox.receive` loses its subscriptions at the first check that has a directory. The account also cannot make an enrollment code. Give the permission before you turn the Inbox on for the people that use it.
- A phone of the earlier standalone alert server (protocol 3) cannot connect. It gets the close code 4426. Each phone must enroll again with a code.
- The files of the standalone alert server (its deploy directory and its image) do not exist. See `deploy/README.md`.
- An old name is an unknown name. The configuration has no alias for a block, a key or a command of an earlier name. `portal check-config` names the key that it refuses.

Roll the instances one at a time. A new instance opens its ports only when its catalogue is complete. See "Start and stop".

Version 5.1.0 adds the blocks `inbox.items` and `inbox.at_rest`. The release is not complete: this text lists the points that exist. Read them before you upgrade from 5.0.x:

- The blocks are optional, and each one needs the other. Without them, the Inbox runs as before.
- The tables of the items are new: `items`, `turns`, `approvals`, `item_events`, `approval_intents`, `approval_mute`, `account_prefs`, `items_heartbeat` and `content_key_check`. The table `alerts` has two new columns, `request_id` and `seq`.
- This changed the base schema of the Inbox in place: the schema version did not change, and there is no migration. A database that 5.0.x made lacks the tables. With the block `inbox.at_rest`, the portal stops at start and names the table. Make a new inbox database.
- A sealed content needs its key. The portal stops at start when sealed rows name a key that the file lost. The error gives the key id and the count. Add the key again, or run `portal inbox content-purge`.
- `portal inbox import-sqlite` refuses a SQLite file that holds items.
- The content keys must live outside the data volume and outside the backups of the database. The Compose script `init-secrets.sh` makes the file `inbox_content_key`. The K3s Secret `portal-secret` gets the key `inbox_content_key`.

Version 5.1.0 adds approvals to grants and the interaction of a key. Read these points before you upgrade from 5.0.5:

- A grant can have the property `confirm`, and a key has the attribute `interaction`. See "Approval of an action" and "Rules of the keys". The PDP does not use them in this step.
- The columns `user_grants.confirm`, `key_grants.confirm` and `keys.interaction` are new. They changed the schema version 8 in place: the schema version did not change, and there is no migration. A database that 5.0.5 made lacks the columns, and the portal stops at start and says so. Make a new database, or add the columns by hand.
- For SQLite, add `confirm INTEGER NOT NULL DEFAULT 0 CHECK (confirm IN (0, 1))` to `user_grants` and to `key_grants`. Add `interaction TEXT NOT NULL DEFAULT 'conversational' CHECK (interaction IN ('conversational', 'one_shot'))` to `keys`. For PostgreSQL, use `confirm BOOLEAN NOT NULL DEFAULT false`.
- Then run `UPDATE keys SET interaction = 'one_shot' WHERE kind = 'api';`. A start from an empty database makes the columns.
- An item of `grants` in the file can have `confirm = true`. A grant of the service `portal` or of the action `access` with `confirm = true` stops the start.
- The attribute `interaction` is new in the blocks `key` and `api_key`. Its default is `conversational` for `key` and `one_shot` for `api_key`.
- The command `import-sqlite` copies the new columns. A file that lacks them gets the defaults.

Version 5.0.5 changes how the launcher opens a service, and it adds external services. It adds one table to the database. It has no change of the protocol. Read these points before you upgrade from 5.0.4:

- A tile of the launcher opens its service in a new browser tab. The new key `ui.services_new_tab` (default `true`) sets this for every service. Set it to `false` to open the service in the same tab.
- The label `portal.target` is gone. A service cannot choose its own behavior. The portal ignores the label if a service still sets it.
- The superadmin can add an external service: a name and a link, with the badge `external`. See "External services". The Settings page Services has the button **Add external service**. The configuration has no new key for it.
- The table `external_services` is new. It changed the schema version 8 in place: the schema version did not change, and there is no migration. The portal has no deployment with users. A database that 5.0.4 made lacks the table. The portal stops at start and says so. Make a new database, or add the table by hand. For SQLite, run `CREATE TABLE external_services (id TEXT PRIMARY KEY, name TEXT NOT NULL, href TEXT NOT NULL, created_at INTEGER NOT NULL);`. For PostgreSQL, use `created_at BIGINT NOT NULL`. A start from an empty database makes the table.
- The command `import-sqlite` copies the external services too.

Version 5.0.4 changes the settings pages and fixes one fault. It has no change of the database, the configuration or the protocol. Read these points before you upgrade from 5.0.3:

- The page of an account that another instance made a moment ago opens at once. Before, the redirect after "New user" could give "Page not found" when it reached an instance that had not read the accounts yet. The pages and the actions of the superadmin that name an unknown account now read the accounts again from the shared database first.
- The account page and the New user page show the server permissions as the first row of the Services list, named Server, and no longer in a separate column.

Version 5.0.3 changes the settings pages and the sign-in page. It has no change of the database, the configuration or the protocol. Read these points before you upgrade from 5.0.2:

- The grant editor and the pages have a layout for narrow screens. A row of an action with limits has a second line for the limits.
- A service row shows an icon after its name when the service publishes its actions. The text badge `access only` is gone.
- The sign-in box has the label "Stay signed in for 30 days". The label "Remember me" is gone.
- The group of the portal rights has the name "Server permissions".

Version 5.0.2 follows protocol 4.1 and removes a weak way to send a key. Read these points before you upgrade from 5.0.1:

- The PDP refuses a node below the service that has no `resource.properties.ancestors`. It answers `false` with the reason `invalid_resource`. A node of depth 1 has the chain `[{"type": "service", "id": "<service>"}]`.
- The Inbox API reads an API key only in the header `Authorization: Bearer`. The query `?token=` is gone. A request with this query gets the status 401.

Version 5.0.1 closes the findings of a security audit. Read these points before you upgrade from 5.0.0:

- The base schema changed in place. A PostgreSQL database needs the column `sessions.credential`, and each database needs the column `alerts.service`. A database without them is refused at start: make a new database. The homelab has no migration step.
- A cluster of instances must run one version. The copies of alerts between instances now carry the service of a watch alert.
- Three keys are new: `session.max_sessions_per_account` (16), `inbox.devices.max_handshakes_per_address` (16) and `inbox.devices.max_per_account` (10). A file that sets `session.max_active_sessions` below 16 must also set the first key.
- A watch alert about a service reaches only the people who may open that service. Give `access` on the service to the members of `inbox.watch.group` that must get its alerts.
- A file key reads the status of an alert only when its owner holds `alert.read` on the group.
- A workload that is not listed but has a path stays closed to everyone except the superadmin.
- The webhook keys of Alertmanager and Grafana have a new form: an alert that is firing during the upgrade is created once more.
- The enrollment fingerprint shows 80 bits. The app and the page show the same value.

## Limitations

- The Inbox is Android only. iOS cannot keep a connection in the background without the push service of Apple.
- The portal has no TLS. Noise protects the phones. An API key travels in clear text on plain HTTP. Put a TLS proxy in front of the portal for the senders.
- The Android app accepts a plain `http://` address, and it accepts an `https://` address with the trust store of the phone. The app has no network security configuration and no certificate code of its own. The manifest allows clear text for each address, because the portal is often on a private network. A narrower rule is an open point.
- With SQLite, only one instance is active. A standby needs the block `inbox.replica`.
- An alert that the Inbox accepts while the database has no writable primary exists only in memory until the database returns. If every instance that holds it stops, the Inbox loses it.
- Each instance has its own rate limits and its own counter of refused sign-ins. The counters of the wrong second-factor codes of an account are in the database.
- The second factor is a TOTP code. A person can give it to a false page, as a password. WebAuthn stops this, and it is the next step. The portal has no fresh second factor for sensitive actions yet.
- The phone settings `alarm`, `ring` and `notify` are local to the phone. The server does not know them.
- The enrollment code is text. There is no QR code in this version.
- The permissions `inbox.receive`, `alert.send` and `alert.read` are given one group at a time. No permission covers all groups.
- A session that "Log in as" opens can make an enrollment code for the account that it shows. The owner has not reviewed this point.
- The pages that draw the matrices need script. Without script, the pages list the grants as text.
- The count of the states in the zone **Sent** comes from the 500 newest alerts of the period. The graph counts every alert.
- Each instance counts only the refused sign-ins that it sees. A burst on several instances can stay under the threshold of the alert of the portal.
- Steps 2 and 3 of the roadmap (agents, questions, approvals) do not exist yet. Alerts are one way in this version.
- The release builds the binary for amd64 and for arm64. The image for arm64 is not built in this repository. No image is published.

## Licence

The licence of the merged code is an open point. The root package says `MIT OR Apache-2.0`, and the shared crates say `LicenseRef-Proprietary`. The owner has not decided. The files `LICENSE-MIT` and `LICENSE-APACHE` come from the earlier portal. They do not decide the licence of the merged code.

