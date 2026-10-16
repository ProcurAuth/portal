# portal

portal is an authentication service for Traefik `ForwardAuth`. It also contains the launcher page of the lab. portal is written in Rust. It does these functions:

- A superadmin is defined by the environment.
- Users get grants from the settings pages or from the configuration.
- A gate protects each service.
- portal gives an identity token to the services.
- portal is an AuthZEN PDP for the services.
- portal keeps a history of who signed in and what they used.

portal is the access gateway and the PDP of section 11 of the MCP server contract. The launcher (`/`) lists the services that declare themselves by label (Docker Compose) or by annotation (Kubernetes IngressRoute). The launcher groups the services in full-width rows. It shows their health and an offline icon library. See the repository README for the labels.

The runtime image is `FROM scratch`. The HTTP stack is Hyper 1.x on Tokio. Discovery uses `reqwest` (rustls) for the Kubernetes API and for health probes. Pages are rendered in Rust. The only script is the theme selector. The settings page for users adds one more small script, for the password generator. Accounts and history live in one SQLite file. The SQLite file is bundled in the static binary. To run several active instances, use a shared PostgreSQL database instead (see [Several instances](#several-instances-postgresql)). There is no Redis, no JavaScript framework, no CDN and no runtime shell. Nothing is fetched from the internet at runtime.

## Authentication model

The login page asks for a **login** and a **password**.

The **superadmin** is the one account that is not in the database. Its login and its password come from an environment variable or from a file. The configuration names the source (see [Secrets](#secrets-_env-and-_file)):

```hcl
authentication {
  superadmin_login_file    = "/run/secrets/portal_superadmin_login"
  superadmin_password_file = "/run/secrets/portal_superadmin_password"
}
```

The superadmin login has no upper-case letter and no space. The program refuses a login that breaks this rule at start. The sign-in form compares logins in lower case. The program does not change the login.

At start, the program reads the password with a size limit. The program hashes the password once with Argon2id. The program then erases the plaintext from memory. The superadmin can see and open everything. The superadmin is the only account that can manage the other accounts.

The superadmin adds and deletes **users** from the settings pages (the gear of the launcher). The database stores their Argon2id hashes. The database is the SQLite file `storage.database`, or PostgreSQL. The request path reads an in-memory copy. The request path never waits on the database. A user has a list of grants (see [Grants and the gate](#grants-and-the-gate)). Each login attempt runs exactly one Argon2 check. This applies to any login name. The check uses the superadmin hash, the hash of the account, or a dummy hash. The failure message is the same in all cases. Neither the time nor the wording shows whether a login exists.

A successful login creates a 256-bit random **opaque session ID**. The browser receives only that random ID. With SQLite, the server keeps the active-session set in bounded RAM. Each session has an expiration time. Logout deletes the session at once. A restart of the process clears all sessions. With PostgreSQL, the sessions are in the shared database. Only the SHA-256 of the cookie is stored. The sessions are valid on every instance and survive a restart.

## Grants and the gate

### Grants

A grant is an action on a resource. A grant has one of two forms:

- `<action> on service:<id>`, for example `access on service:grafana`. The grant applies to the whole service.
- `<action> on service:<id>/<type>:<resource id>`, for example `query.run.write on service:clickhouse-dash/database:analytics`. The grant applies to one sub-resource of the service.

The `<id>` is the id of the service in the catalogue. The id is the label `portal.id`, or the slug of the group and the name.

- `access` lets the user open the service. portal decides `access` in `/auth/verify`.
- The other actions are the actions that the service publishes at `/.well-known/access-actions` (contract 11.4). The service decides these actions. The service asks the PDP of portal.
- A grant on a service covers all sub-resources of this service. For example, `query.run.read on service:db` covers each database of `db`.
- A grant on a sub-resource covers only this sub-resource: the same type and the same id. It does not cover the whole service. It does not cover another sub-resource.
- The service gives its resource types. Each action of `/.well-known/access-actions` has a list `resource_types`. A grant on a sub-resource names an action and one of its types. The type `service` is the whole service: it is not a sub-resource type.
- A grant on a sub-resource applies only when the last known document of the service declares its type for its action. If the document does not declare the type, the PDP does not apply the grant. If portal never read the document of the service, the PDP does not apply any grant on a sub-resource of this service. A grant on the whole service does not need a declaration.
- A type has 1 to 32 characters of `a-z`, `0-9` and `_`. A resource id has 1 to 128 characters, without a space or a control character. portal uses the id as it is. portal does not change its case.
- `access` has no grant on a sub-resource. `access` opens the whole service.
- A new user has no grant. The user sees no service until the superadmin gives a grant.
- The superadmin has all grants. The superadmin has no `user` block.

portal has no groups of users. Each user has their own grants.

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

A `user` block can give a grant on a sub-resource, for example `"query.run.write on service:db/database:analytics"`. A grant with a bad form stops the start. At start, portal does not know the services: the catalogue and the actions documents come at run time. Thus a resource type that the service does not declare does not stop the start. The Services settings page shows this grant under **Grants to check**. At run time, the PDP does not apply a grant whose type the service does not declare (see "AuthZEN PDP").

### The gate

`/auth/verify` finds the service from the path in `X-Forwarded-Uri`. The service owns the longest path prefix of its `href`. The prefix matches whole segments. portal then decides `access` in its own process. portal uses the grants in memory.

Traefik routes on the clean path, but sends the raw URI. portal therefore reads the path in every way that a proxy or a backend can read it. These ways include `%2e%2e`, `//`, `%2f` as a character and as a separator, and `\`. portal lets the request through only if every way is permitted. For example, portal judges `/grafana/../clickhouse/` as a request for ClickHouse.

- A refused browser navigation goes to `/auth/denied`. Traefik limits the body of a ForwardAuth answer. The answer cannot hold the page. Other requests get a plain `403`.
- The page `/auth/denied` does not give the name of a service. A user does not see a service that is not open to that user.
- `/auth/*` is always open to a signed-in user. A path that no service owns (an absolute asset path) is open to each signed-in user.
- The launcher shows only the services for which the user has `access`. The discovery problems name workloads. Only the superadmin sees these problems.

portal stays a ForwardAuth server. Traefik is the only proxy. portal receives only the headers of a request. portal never receives the body of a request or of a response.

### Identity token

With the `identity` block, `/auth/verify` gives an identity token (contract 11.3) to each service that a user can open. The token is in the response header `identity.header`. The default is `X-Identity-Token`. Traefik copies the token to the request. Add the header to `authResponseHeaders` of the ForwardAuth middleware. Traefik then removes a header of the same name that the client sends.

```hcl
identity {
  issuer           = "https://portal.home.internal"
  header           = "X-Identity-Token"
  token_ttl        = "5m"
  signing_key_file = "/run/secrets/portal_identity_key"
}
```

The token is a JWT. Its algorithm is `EdDSA` (Ed25519). The header has `kid`. `kid` is the RFC 7638 thumbprint of the public key. The claims are:

| Claim | Value |
|---|---|
| `iss` | `identity.issuer` |
| `sub` | the login of the user (the superadmin has its own login) |
| `aud` | the id of the service |
| `iat`, `exp` | the time of issue, and `iat + identity.token_ttl` |

The token contains no permission. The service asks the PDP. portal keeps one token for each session and each service. portal gives the same token again until half of its life is gone. A path that no service owns gets no token.

Make the key with OpenSSL. Give the public key to each service in its `identity.public_keys_file` key:

```bash
openssl genpkey -algorithm ed25519 -out portal_identity_key
portal identity-public-key --config /etc/portal/config.hcl > portal_identity_public.pem
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
| `POST /access/v1/evaluation` | One decision: `subject`, `action`, `resource`, `context`. The answer is `{"decision": true}` or `{"decision": false}`. |
| `POST /access/v1/evaluations` | A batch. The top-level members are the defaults of each item. `options.evaluations_semantic` is `execute_all` (default), `deny_on_first_deny` or `permit_on_first_permit`. The batch has at most `pdp.max_evaluations` items. |
| `POST /access/v1/search/action` | The actions that the subject can do on the resource. The answer is one page. |
| `GET /.well-known/authzen-configuration` | The endpoints under `pdp.url`. This endpoint needs no token. |

Rules:

- Each server has its own `pdp_client` block and its own token. The label is the name of the server. The label is the id of its service. This id is also the audience of its identity tokens. The server sends `Authorization: Bearer <token>`. A request without a valid token gets status `401`.
- The PDP answers a server only for its own resources. The resource `{"type": "service", "id": "<name of the server>"}` is the whole service. A request about the service of a different server gets `false`.
- A resource of another type is a sub-resource, for example `{"type": "database", "id": "analytics"}`. The service of a sub-resource is always the server that asks: its token tells which server it is.
- A grant on the service covers each of its sub-resources. A grant on a sub-resource covers only the same type and the same id. The three endpoints use the same rule.
- A grant on a sub-resource applies only when the last known document of the service declares its type for its action. The PDP keeps the last document that it read for each service. A failed read does not remove it. A new document that does not declare the type removes the effect of the grant at once. If portal never read the document of the service, the PDP does not apply any grant on a sub-resource of this service. The decision is `false`, and the reason in the log is `the service does not declare the resource type`. A grant on the whole service is not affected. After a change of the `portal.actions` URL of a service, a failed read leaves no document. The PDP then does not apply the grants on its sub-resources.
- The subject type must be `user`. The subject must be an account or the superadmin. If not, the decision is `false`.
- The PDP fails closed. A request that the PDP cannot read gets status `400` and no decision.
- The PDP returns the header `X-Request-ID` of the request.
- The log records each decision (`pdp_decision`). The record has the server, the subject, the action, the resource, the decision and the reason.
- The PDP has no TLS. The deployment must limit the network of the PDP to the services and portal. This differs from AuthZEN, which requires TLS.
- In K3s, the NetworkPolicy `allow-portal-pdp` (`#tools/k3s/network/policies.yaml`) admits to the port 4182 only the pods with the label `portal-pdp-client: "true"`. These pods must be in the namespace `apps`, `observability` or `edge`. Give this label to the pod template of each server that asks the PDP. The policy `allow-edge` does not select the portal pods. The policy `allow-portal-http` opens only the port 4181 of portal.

A request about a sub-resource:

```json
{
  "subject": {"type": "user", "id": "alice"},
  "action": {"name": "query.run.write"},
  "resource": {"type": "database", "id": "analytics"}
}
```

### Actions of the services

portal reads the actions of each service at `/.well-known/access-actions`. The URL comes from the label `portal.actions`. This label is an absolute http or https URL. If the label is missing, the URL is the origin of the health URL (`portal.health`) with this path. A service without these two labels has only `access`.

- The read has a timeout (`access.actions_timeout`) and a size limit of 64 KiB. portal reads the document again after `access.actions_refresh`.
- The field `server` of the document must be the id of the service. Set `portal.id` to the name of the server.
- A `404` means that the service does not publish actions. The service then has only `access`. `access` is the full permission. portal lets the user in. The service does no other check.
- Any other failure keeps the last good document. The Services settings page shows the problem. The PDP also uses this document: see "AuthZEN PDP".
- The page of a user shows one check box for each action of each service. The check box `access` comes first. A box that the page did not show keeps its grant. A grant does not end when a service stops to publish its action for a short time.
- Under **Grants on sub-resources**, the page of a user shows the grants on sub-resources of each service. To add one, select the action and the resource type, then type the resource id. The lists show only the actions and the types that the service declares. portal does not list the resources of a service. **Remove** deletes one grant. A grant of the configuration shows the mark *configuration*. It cannot change on the page.
- While a service has not answered, portal refuses a new grant on its sub-resources.
- The Services page keeps the section **Grants to check**. It lists each grant on a sub-resource whose type the service does not declare. The PDP does not apply these grants. The section does not list the grants of a service that has not answered yet.

### Migration of the database

At the first start of the version with grants, the server changes the database. SQLite goes to schema version 3. PostgreSQL goes to version 2. The migration deletes the access data of the earlier versions. It adds the table of the grants. The accounts and the history stay. After the migration, no user has a grant. Give the grants again in the settings pages, or in `user` blocks. Until then, only the superadmin can open the services.

The grants on sub-resources need one more change. SQLite goes to schema version 4. PostgreSQL goes to version 3. The table `user_grants` gets the columns `sub_resource_type` and `sub_resource_id`. The migration keeps all grants. A grant of the earlier version applies to the whole service. An earlier portal refuses the new schema: it does not start on it.

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
- a wildcard never stands for a segment that hides a separator. `/auth/*` is never open;
- a rule that cannot be read makes the **whole label** unusable. Examples are a regular expression, a bad method, a path outside the `href`, or more than 8 rules. In this case, nothing is opened. The service is reported in the catalogue problems. The service is not listed until it is fixed;
- portal ignores the query string. portal records nothing in a session, because there is no session. The request is counted in `portal_forward_auth_requests_total{result="excluded"}`.

The Services settings page lists the rules of every service. The whole exposure can be audited in one place. A path that a rule opens is protected only by what the service does itself, for example by a webhook token. portal does not ask anything for these paths.

## Settings pages and history

The superadmin gets a gear on the launcher. The gear opens the settings. The settings are **one HTML page per tab**. This is the same design as the pages of clickhouse-dash. Each page has a header with **Back to panel**. Below the header is a row of underlined tab links. The current tab has a line under it. The row has no fill. The page follows the row. The settings live under `/auth/settings`. Traefik already routes `/auth/` to the portal. The pages work without script. The pages answer 404 to anyone who is not the superadmin in its own right. This applies to a user, and to the superadmin who browses as a user. A page carries only what its tab shows. The launcher carries none of it. The browser therefore builds a small DOM on each page. A launcher with twelve services has about 150 elements. The settings used to add 700 more elements to it. There are three tabs:

1. **Users** (`/auth/settings`). The accounts appear first. Each account shows the number of failed sign-ins of the last 30 days as a red counter. **Add a user** is one button. It opens the form in place. The button also opens the form after a refused attempt. Each account has one card. The card shows the last sign-in and a graph of the connections. It shows the latest sessions (time, address, browser, duration, and how each session ended). It also shows the services that the account used and the services that it can open. The card has the buttons **Log in as**, **Edit** and **Delete**. **Delete** is a two-step action. **Edit** shows one row for each service. The row has the box `access`. The row has one more box for each action that the service publishes. **Edit** changes the grants. **Edit** also changes the password when a new password is typed. Leave the password field empty to keep the password. A new password ends the sessions of that account at once. A change of grants applies from the next request. In the mode `static`, the boxes cannot change. In the mode `hybrid`, a grant of the configuration cannot change. Below the services that the account can open, the card shows the grants on sub-resources. The card has one form for each service that declares resource types. A new user has no grant. The add form has no box. The **Generate** button of both forms fills the password field with a random 20-character password. The browser draws this password. The password is never sent anywhere before the form is submitted. Below the accounts, the page shows connections per day over 30 days. The bars are neutral. Today's bar uses the accent. The peak day is written out. A graph is one SVG of three paths. The page also shows the recent events. The password generator is the only script of these pages. It loads on this page only.
2. **Services** (`/auth/settings/services`). The page lists each service. For each service, it shows its id. It shows the actions that the service publishes (`access only` if it publishes none). It shows the users who have `access` (`Superadmin only` if there is none). It shows the paths open without sign-in. It shows a warning when the actions document cannot be read.
3. **Logins** (`/auth/settings/logins`). This tab shows the sign-in log. See below.

### Sign-in log

The program records each sign-in attempt that reaches the password check. The record has these items: the time (UTC, to the second); whether the attempt succeeded; the **login as typed**; whether that login names an account; the client address; and the browser. The page shows the figures of the last 24 hours. These figures are the attempts, the refused attempts, and the distinct addresses refused. The log shows the newest entries first. The page shows 50 rows for each page. The *Refused* filter uses `?r=failed`. The page number uses `?p=N`.

The password is never recorded. This applies to the database, the page and the logs. What a visitor types is hostile data. The program never uses this data raw. Control characters (newline, NUL, escape sequences) and characters that hide or reorder text (zero-width characters, right-to-left overrides, line separators) are kept as a visible `\u{..}` escape. The text is cut at 64 characters, with an ellipsis. The text is used only as a bound SQL parameter. The page HTML-escapes the text where it prints it. The CSP forbids inline script and style anyway. In the JSON logs (`login_failed`, `login_success` carry `login`), the text stays inside one escaped string, so it cannot forge a line. An attempt blocked by the rate limiter is counted in the metrics. It is not listed one by one. The log keeps at most 20,000 attempts (newest first). So a siege cannot fill the volume.

A browser that the gate refuses goes to a 403 page. The page does not give the name of the service. The page tells the user to ask the administrator for access. The page has the buttons **Back to panel** and **Sign out**.

**Log in as** gives the superadmin a session as the user. The session is flagged *impersonated*. It is not counted as a connection of that user. The history tags it *via superadmin*. A banner offers **Back to superadmin**. This button needs no password. The impersonated session itself proves that the superadmin opened it. Account management is refused while the superadmin impersonates a user.

The history is stored in the database (SQLite or PostgreSQL). The program keeps the history for `storage.retention_days` days. The default is 90 days. The history contains these records:

- sessions: login, address, user agent, start, last seen, end, and the reason for the end;
- the services that each session used. The history holds counts per service. It never holds a path or a query;
- administrative events: users added or deleted, impersonation, and failed sign-ins;
- the sign-in attempts above.

The per-account failed counters count only logins that name an account. A login that does not exist is counted apart. It appears in the sign-in log, as typed. A person sometimes types a password in the login field. This is the one item in this log that the log must not hold. That is why the log is short-lived. Only the superadmin can see it.

Requests only touch memory. One writer thread flushes every 15 seconds in a single transaction. An event is dropped rather than make a request slower. The program counts a dropped event in `portal_tracking_dropped_total`.

With SQLite, sessions that a previous run left open end with the reason *restart*. With PostgreSQL, sessions outlive a restart. Only a session whose instance stopped before it recorded its end is closed in this way. This happens after the session has been idle for longer than `session.idle_timeout`. Times are UTC.

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

The client is a blocking PostgreSQL client. It uses plain TCP. It has no TLS stack and no C dependency. The connection stays inside the cluster. The server must accept `scram-sha-256` over a non-TLS connection from the pods. At startup, the first connection is retried for about one minute. Then the schema is created or updated under an advisory lock. Several instances that start together are therefore safe. A statement that runs for more than 15 s is cancelled. A broken connection is replaced, and the operation is retried once. This is what a failover of the database looks like.

This is what the instances share, and how:

- **Sessions** are rows of a `sessions` table. A row holds the digest of the cookie, the client binding, the identity, the expiry and the last use. A login writes the session and its line of the history in one transaction. Each instance keeps a copy of what it checked. The copy is kept for **5 seconds**. ForwardAuth asks the database about one session once per 5 s, not once per request. The binding (address and User-Agent), the idle timeout and the absolute lifetime are enforced exactly as with SQLite. The idle timeout is renewed at most every 5 s of activity. A logout, a deleted account or a password change ends the session in the database at once. The session then stops working on the other instances within 5 seconds (the copy). If the database cannot be reached, a session that was checked less than 2 minutes ago is still accepted. A failover therefore does not sign everybody out. New sign-ins fail with `503` in the meantime.
- **Accounts** are read from the database every 5 seconds by every instance. An account that is created, edited or deleted on one instance reaches the others within 5 seconds. A login that the instance does not know yet makes it read the accounts again at once. This happens at most once per second. An account that was just created can therefore sign in on any instance. A session that was opened for this account elsewhere is never ended for that reason.
- **History** is written by every instance. The writes are batched every 15 s, as with SQLite. A session that ends by logout, revocation, expiry or idleness is recorded once. The instance whose call ended the session records it. The database gives each ended session to exactly one caller. The start or the stop of an instance never closes the sessions of the other instances.
- **Each instance has its own rate limits.** The failed-login limiters (per IP and global) are in memory. With two instances, an attacker gets up to twice the budget. A lockout on one instance does not lock the other instance. `session.max_active_sessions` is checked against the database. Two instances that race at the limit can each overshoot it by one.

Liveness and readiness are unchanged. `/healthz` does not touch the database. `/metrics` adds `portal_shared_store_errors_total{use="sessions|directory"}`.

The instance that adds an account checks `users.max_users` against its own copy. Two instances that add accounts at the same moment can exceed the limit by one.

### Migration from SQLite to PostgreSQL

The command `portal import-sqlite --config FILE --from SQLITE_FILE` copies data to PostgreSQL. It copies these items: the accounts (with their grants), the sessions of the history, the service accesses, the audit events and the sign-in log. It copies them into the empty PostgreSQL database that the configuration names. The copy is one transaction. The command prints what it copied. The command refuses a target that already holds any of these items. In this case, it changes nothing. The command only reads the SQLite file. The SQLite file can be the live file. Sessions do not move. They were in the memory of the old process. The sessions that the file still shows as open end where they were last seen, with the reason *restart*. Every user signs in again after the switch. Changes made to the SQLite file after the import are not copied. Do not change accounts between the import and the switch.

## Local test

The Compose file contains the HCL directly through `configs.content`. The superadmin login and password come from the host environment. Compose writes them to secret files. The database is in a named volume.

```bash
export PORTAL_SUPERADMIN_LOGIN='admin'
export PORTAL_SUPERADMIN_PASSWORD='test1234'
docker compose up -d --build
```

Open this URL:

```text
http://localhost:4181/auth/login
```

Sign in as `admin` / `test1234`. The gear opens the settings pages.

Stop the stack with:

```bash
docker compose down
```

The local Compose file publishes port 4181 only on loopback. This is intentional. In production, the same binary runs behind Traefik. The port is not published publicly.

## HCL example

```hcl
server {
  listen_address     = "0.0.0.0:4181"
  workers            = 4
  max_body_bytes     = "32KiB"
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

users {
  max_users = 64
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

## Configuration

The program reads one HCL file. The HCL parser is a small, strict subset parser. Blocks hold `key = value` lines. A block has no nested block. A block can have a label (`user "alice" { ... }`). A value is a scalar or a list of quoted strings (`["a", "b"]`). The file is the only source of the configuration. The program reads an environment variable or a file only when the file names it.

### Rules

- **The schema is closed.** An unknown block, an unknown key, a duplicate block or a duplicate key stops the program at start.
- **An error stops the program before it serves.** The message gives the key, the values that the key accepts, and the value that the file gave. The message never shows a secret.
- **The program does not change a value.** It does not clamp a number. It does not trim a text. It does not change the case of a text.
- **The types are strict.** Write a number without quotes (`workers = 4`). Write a boolean without quotes, in lower case (`true` or `false`). Write every other value in quotes. A number in quotes or a word without quotes stops the program.
- **A unit is mandatory.** A duration ends with `ms`, `s`, `m`, `h` or `d`. A size ends with `B`, `KiB` or `MiB`.
- **A name of a choice has the exact case of the reference.** `Strict` is valid and `strict` is not. `warning` is not a log level.
- **A key that does not apply must not be in the file.** When `security.login_rate_limit_enabled` is `false`, the keys `login_attempts_per_ip`, `login_window` and `login_lockout` stop the program. The same rule applies to the global limiter. It also applies to `metrics.path` when `metrics.enabled` is `false`. It also applies to `storage.database` with the `postgres` block.
- **Two keys that contradict each other stop the program.** For example, `session.idle_timeout` is longer than `session.ttl`.

### Secrets: `_env` and `_file`

Six attributes hold a secret. They are `authentication.superadmin_login`, `authentication.superadmin_password`, `session.secret`, `postgres.password`, `identity.signing_key` and the `token` of each `pdp_client` block. The file never holds the value. Give exactly one of these two forms:

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
| `workers` | number, 1 to 64 | `4` | The number of worker threads. |
| `max_body_bytes` | size, 1KiB to 1MiB | `"32KiB"` | The largest request body that the server reads. The form of a user holds one field for each grant that it shows. |
| `max_connections` | number, 1 to 100000 | `1024` | The largest number of open TCP connections. |
| `max_headers` | number, 8 to 256 | `64` | The largest number of headers in a request. |
| `max_header_bytes` | size, 8KiB to 1MiB | `"32KiB"` | The largest size of the headers of a request. |
| `header_read_timeout` | duration, 1s to 1m | `"10s"` | The time that a client has to send the request headers. |
| `body_read_timeout` | duration, 1s to 1m | `"10s"` | The time that a client has to send the request body. |

#### Block `authentication`

The superadmin: the one account that is not in the database.

| Key | Type | Default | Description |
|---|---|---|---|
| `superadmin_login` | secret: give `superadmin_login_env` (a variable name) or `superadmin_login_file` (an absolute path), not both | required | The login of the superadmin. It has 1 to 64 characters, and no upper-case letter, no space and no control character. The server does not change it. |
| `superadmin_password` | secret: give `superadmin_password_env` (a variable name) or `superadmin_password_file` (an absolute path), not both | required | The password of the superadmin. At start, the server hashes it with Argon2id and erases it from memory. It has at most `security.max_password_bytes` bytes. |

#### Block `users`

The accounts in the database.

| Key | Type | Default | Description |
|---|---|---|---|
| `max_users` | number, 1 to 1024 | `64` | The largest number of accounts in the database. |

#### Block `access`

The grants of the users, and the actions that the services publish.

| Key | Type | Default | Description |
|---|---|---|---|
| `grants_mode` | `static`, `dynamic`, `hybrid` | `"dynamic"` | Where the grants come from. `static`: only the `user` blocks; the settings pages show the grants and do not change them. `dynamic`: only the settings pages; a `user` block stops the server. `hybrid`: both; the grants of a `user` block are read-only in the settings pages. |
| `actions_timeout` | duration, 100ms to 30s | `"2s"` | The time that the server waits for the `/.well-known/access-actions` document of one service. |
| `actions_refresh` | duration, 10s to 1d | `"5m"` | The time between two reads of the `/.well-known/access-actions` documents. |

#### Block `identity`

This block is optional. If it is present, the server gives an identity token to each service that it lets a user open.

| Key | Type | Default | Description |
|---|---|---|---|
| `header` | text: an HTTP header name of 1 to 64 characters: letters, digits and '-' | `"X-Identity-Token"` | The response header of `/auth/verify` that holds the identity token. Traefik copies it to the request with `authResponseHeaders`. |
| `issuer` | text: 1 to 256 visible ASCII characters, for example https://portal.example.internal | required | The claim `iss` of each identity token. Each service compares it with its `identity.issuer`. |
| `token_ttl` | duration, 10s to 1h | `"5m"` | The life of an identity token (`exp - iat`). It must not be more than the `identity.max_age` of the services. The server gives the same token again until half of its life is gone. |
| `signing_key` | secret: give `signing_key_env` (a variable name) or `signing_key_file` (an absolute path), not both | required | The Ed25519 private key that signs the identity tokens (algorithm `EdDSA`): a PKCS#8 key in PEM, or the base64 text of the PEM on one line. `portal identity-public-key` prints the public key for the services. |

#### Block `pdp`

This block is optional. If it is present, the server is an AuthZEN PDP on a second, internal listener.

| Key | Type | Default | Description |
|---|---|---|---|
| `listen_address` | text: an IP address and a port, such as 0.0.0.0:4182 | required | The address and the port of the PDP listener. Do not expose it through Traefik: only the services use it. |
| `url` | text: an http or https URL without a path, a query or a fragment | required | The URL at which the services reach the PDP. `/.well-known/authzen-configuration` gives the endpoints under this URL. |
| `max_body_bytes` | size, 1KiB to 1MiB | `"64KiB"` | The largest request body that the PDP reads. |
| `max_evaluations` | number, 1 to 1024 | `64` | The largest number of evaluations in one request to `/access/v1/evaluations`. |

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
| `ttl` | duration, 1m to 7d | `"8h"` | The lifetime of a session. |
| `idle_timeout` | duration, 1m to 7d | `"30m"` | A session that nobody uses for this time ends. The value is at most `session.ttl`. |
| `bind` | `ip_user_agent`, `user_agent`, `none` | `"ip_user_agent"` | What a session cookie is tied to: the client address and the User-Agent, the User-Agent only, or nothing. |
| `same_site` | `Strict`, `Lax` | `"Strict"` | The SameSite attribute of the session cookie. |
| `secret_mode` | `ephemeral`, `provided` | `"ephemeral"` | `ephemeral`: the server makes a new random signing key at each start. `provided`: the key comes from `session.secret_env` or `session.secret_file`. The `postgres` block needs `provided`, because all instances use the same key. |
| `secret` | secret: give `secret_env` (a variable name) or `secret_file` (an absolute path), not both | required | The signing key of the login forms, of the return links and of the anti-CSRF tokens. It has at least 32 bytes. Applies only when `session.secret_mode` is `provided`. |
| `max_active_sessions` | number, 1 to 1000000 | `4096` | The largest number of open sessions. A login above this number gets the status 503. |

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
| `global_login_attempts` | number, 1 to 1000000 | `100` | The number of failed logins from all clients in one window before the lockout. Applies only when `security.global_login_rate_limit_enabled` is `true`. |
| `global_login_window` | duration, 1s to 1d | `"1m"` | The window in which the failed logins of all clients count. Applies only when `security.global_login_rate_limit_enabled` is `true`. |
| `global_login_lockout` | duration, 1s to 1d | `"30s"` | The time that all logins stay locked out. Applies only when `security.global_login_rate_limit_enabled` is `true`. |
| `max_rate_limit_entries` | number, 64 to 1000000 | `4096` | The largest number of client addresses that the limiter remembers. Applies only when at least one login limiter is on. |
| `max_concurrent_password_checks` | number, 1 to 64 | `1` | The largest number of Argon2 checks that run at the same time. |

#### Block `metrics`

The Prometheus endpoint.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | `true` or `false` | `true` | Turn the Prometheus endpoint on or off. |
| `path` | text: an absolute URL path of at most 128 bytes, not /healthz and not under /auth/ | `"/metrics"` | The URL path of the Prometheus endpoint. Applies only when `metrics.enabled` is `true`. |

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

#### Block `logging`

The log.

| Key | Type | Default | Description |
|---|---|---|---|
| `level` | `error`, `warn`, `info`, `debug` | `"info"` | The lowest level that the server logs. |

#### Block `user "<login>"`

This block has a label and can occur more than one time. The grants of one user that the configuration gives (modes `static` and `hybrid`). The label is the login, as the server keeps it: 1 to 32 characters of a-z, 0-9, '.', '-' and '_'. The account itself is in the database. The superadmin has all grants and has no `user` block.

| Key | Type | Default | Description |
|---|---|---|---|
| `grants` | list of at most 512 quoted strings: grants such as "access on service:grafana" or "query.run.write on service:clickhouse-dash/database:analytics": an action, the word on, service: and the id of the service, then optionally / and a sub-resource as type:id | required | The grants of the user. `access` lets the user open the service. The other actions are the names that the service publishes at `/.well-known/access-actions`. A grant on a service covers all the sub-resources of this service. A grant on a sub-resource, `<action> on service:<id>/<type>:<resource id>`, covers only this sub-resource. The type has 1 to 32 characters of a-z, 0-9 and '_', and is not `service`. The resource id has 1 to 128 characters, without a space. The action `access` has no grant on a sub-resource. At start, the server checks only the form of each grant: the services and their resource types are known at run time. The Services page of the settings shows a grant whose type the service does not declare. The PDP does not apply this grant. |

#### Block `pdp_client "<server>"`

This block has a label and can occur more than one time. One server that asks the PDP for decisions. The label is the name of the server: the id of its service in the catalogue, and the audience of its identity tokens. The PDP answers only for the resources of this server.

| Key | Type | Default | Description |
|---|---|---|---|
| `token` | secret: give `token_env` (a variable name) or `token_file` (an absolute path), not both | required | The bearer token of the server. It has at least 32 bytes. Two servers must not have the same token. |
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

`POST /auth/login`, `POST /auth/logout` and the account routes (`POST /auth/users/add`, `/delete`, `/impersonate`, `/impersonate-end`) require both:

1. a CSRF token. For login, the token is the double-submit cookie. For the other routes, the token is an HMAC of the session ID and of the purpose (`logout`, `users`). The form of the button carries the HMAC. The HMAC has no cookie that can expire under a page left open. A token for one action cannot be replayed on another action; and
2. a same-origin `Origin` check. The program uses `Referer` only when `Origin` is absent.

The account routes also check on the server that the session belongs to the superadmin. The session must not be an impersonated one. In K3s, these paths do not pass through ForwardAuth. portal therefore cannot rely on the gate.

The Sign out button sends a POST. This POST ends the session at once. `GET /auth/logout` only redirects to `/`.

When requests arrive through Traefik, the program builds the expected public origin from the unique `X-Forwarded-Proto` and `X-Forwarded-Host` values. For direct local access, the program uses `http` and the unique `Host` header. The program rejects these inputs: partial forwarded-origin metadata, duplicate headers, `Origin: null`, foreign origins, userinfo and invalid schemes.

Authentication pages return `Referrer-Policy: same-origin`. The Referer fallback stays available. Paths do not leak across origins.

## Opaque session behavior

Session cookies use:

```text
__Host-portal=<opaque random token>; Path=/; Max-Age=<ttl>; HttpOnly; SameSite=Strict; Secure   (HTTPS)
portal=<opaque random token>; Path=/; Max-Age=<ttl>; HttpOnly; SameSite=Strict                       (plain HTTP)
```

The program reads the scheme for each request from `X-Forwarded-Proto`. A session is bound to the client address and the User-Agent of the login. The binding is set by `session.bind = "ip_user_agent"`. The other values are `user_agent` and `none`. A session ends after `session.idle_timeout` without use. The default is `30m`.

The token contains no identity, no expiration timestamp, no claims and no signed application data. The server stores only a SHA-256 digest of each token. It also stores the expiration time. With SQLite, this data is in memory. With PostgreSQL, this data is in the shared database.

The session store is bounded by `session.max_active_sessions`. If the store is full, a successful password check cannot create a session. The program returns `503`. It does not remove an arbitrary live session.

## Anti-bruteforce

Two independent controls run before Argon2. Each control is local to the process. With several instances, each instance has its own counters. See above.

- per-IP failed-password limiter;
- global failed-password limiter for distributed attacks.

Only failed password checks use the failure budgets. A successful authentication clears the per-IP state. It does not clear the global failure history. This is intentional.

`max_concurrent_password_checks` separately limits the number of concurrent Argon2 operations.

## Prometheus

When enabled, `/metrics` exports Prometheus metrics with bounded cardinality. The metrics cover: HTTP status and route; login success and failure; CSRF and origin failures; IP and global rate limits; session checks; active sessions; session-capacity rejections; Argon2 concurrency; HTTP connections; and uptime.

Do not put client IPs, raw URIs, passwords, cookies or return targets into metric labels.

See `examples/prometheus.yml`.

## Global Traefik ForwardAuth

The project is designed for installation as an entryPoint-level middleware. A request to a protected application first reaches `/auth/verify`. Without a valid session, the service returns a redirect to `/auth/login`. The redirect contains a validated, HMAC-signed path and query as the return target. After a successful login, the service returns the browser to that target.

`/auth/verify` explicitly lets the public auth paths `/auth/login`, `/auth/logout` and `/auth/style.css`. This prevents a loop when the ForwardAuth middleware is global. `/auth/success` still requires a valid session.

Any other path that the portal receives redirects to the login page when there is no session. This applies to `/auth/…` and to the catch-all route. It applies whether or not the path exists. Once the user is signed in, the portal answers 404. In the Traefik routes, the catch-all runs ForwardAuth first. Then Traefik rewrites the path to `/__portal-fallback-404`. This keeps the portal's own endpoints from being exposed on arbitrary paths.

See:

- `examples/traefik-entrypoint-snippet.yaml`
- `examples/traefik-dynamic.yaml`

## Absolute JS/CSS paths

The authentication cookie has `Path=/`. So same-origin resources carry the same session cookie. Examples are `/app.js`, `/css/app.css`, `/api/...` and `/images/...`. After login, these resources pass the global ForwardAuth check.

Authentication does not rewrite application paths. If an application mounted under `/testservice` emits `/app.js`, Traefik still needs a routing rule. The rule maps `/app.js` to the intended backend.

## Reproducible dependency resolution

After this Hyper migration, bootstrap `Cargo.lock` once on a development machine with Docker. Then commit it:

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
portal hash-password < password.txt
portal version
```

## Release

Start the GitHub `Release` workflow by hand from the GitHub Actions UI. Its version input is independent of the version in `Cargo.toml`. For example, `2.0.0` builds binaries that report `portal 2.0.0`. It publishes the `v2.0.0` assets.

Supported release targets:

```text
linux/amd64  -> x86_64-unknown-linux-musl
linux/arm64  -> aarch64-unknown-linux-musl
linux/arm/v7 -> armv7-unknown-linux-musleabihf
```
