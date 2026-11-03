# Security model

`portal` is a ForwardAuth barrier, the launcher page of the lab, and the AuthZEN PDP of the deployment (section 11 of the MCP server contract). It has one superadmin, defined by the environment, and users with grants. It intentionally does not implement groups, OAuth/OIDC, or password reset. Accounts and connection history are stored in one SQLite file. When several instances run, they use a shared PostgreSQL database instead (the `postgres` block).

A grant is an action on a service, or on a node of the resource tree of a service. The grants come from the settings pages, from `user` blocks of the configuration, or from both (`access.grants_mode`). `/auth/verify` decides the action `access` with the grants in memory. The services decide their other actions. They ask the PDP. See "Grants and the gate" in the README.

## Core invariants

- The project code must not contain `unsafe` Rust.
- Hyper does the HTTP/1 parsing.
- The server limits the request headers, the connection count, the body size, and the read times.
- The startup fails if the HCL configuration is unknown or invalid.
- The server reads the superadmin password from an environment variable or from a file. The read is bounded.
- The server hashes the superadmin password once with Argon2id. Then the server zeroizes the password.
- The server stores user passwords only as Argon2id hashes.
- Each login attempt costs exactly one Argon2 check. This applies to every login name.
- A dummy hash replaces an unknown login name.
- A failed login returns the same message.
- A new user has no grant and opens no service. Access comes from a grant only. The superadmin has all grants.
- A path that no service owns is open to each signed-in user only when the catalogue is complete. The catalogue is complete when each configured discovery source answered at least once. Until then, only the superadmin passes the gate.
- A workload with unusable labels keeps the path of its `href` closed. A workload whose id another workload uses with another `href` also keeps its path closed. Only the superadmin passes a closed path.
- The gate reads the forwarded path in every way that a proxy or a backend can read it.
- The gate allows the request only if every reading is allowed.
- The gate refuses a URI that cannot be read.
- The portal receives only the headers of a request (ForwardAuth). It never receives the body of a service.
- The launcher lists only the services for which the user has `access`.
- Discovery problems name workloads. Only the superadmin sees them.
- Only the superadmin can administer accounts. This covers add, edit, delete, and log in as.
- The server checks this rule on the server side.
- Account administration uses a purpose-bound CSRF token and a same-origin check.
- A new password is hashed under the same Argon2 permit as a login.
- A new password ends the sessions of that account.
- A new superadmin login, password or session secret ends the sessions of the superadmin and the impersonated sessions. This also applies with PostgreSQL, where the other sessions survive a restart.
- **Log in as** and **Back to superadmin** keep the end of the session that the password opened. A switch never gives a fresh `session.ttl`.
- The server refuses at start the example superadmin passwords and the example session secret that the repository publishes. The error names the key and the source. It never shows the value.
- The server writes every change to the audit history.
- The 403 page names no service. A user does not learn the name of a service that is not open to that user.
- In the mode `static`, the settings pages refuse a change of grants. The server does not use the grants of the database.
- In the mode `hybrid`, the settings pages cannot change a grant of the configuration.
- The settings pages give a grant below a service only for a level that the service declares and an action that lists its type. The form gives one id for each type of the chain. While the service has not answered, the pages refuse the grant. Each resource id is exact. The server does not trim it or change its case. An id with a control character, or of more than 256 bytes, is refused.
- The text form of a grant encodes each id with one canonical percent-encoding. Thus a `/` or a `:` in an id never divides a segment, and one grant has one text only. The parser refuses another encoding of the same id.
- At start, the server does not know the services. Thus it checks only the form of a grant of the configuration. The Services page shows a grant that does not follow the tree of its service. The PDP does not apply this grant.
- The **Generate** button in the add-user form draws a 20-character password in the browser.
- The button uses `crypto.getRandomValues` and rejection sampling over an alphabet without look-alike characters.
- The superadmin sees the generated password, so that the superadmin can give it to the user.
- The password reaches the server only when the superadmin submits the form.
- The history records addresses and user agents. The server keeps them for `storage.retention_days`.
- The history never records a path, a query, a password, or what was typed in a login that does not exist.
- The server limits the Argon2 concurrency.
- The server limits the rate of login failures per IP address and globally.
- The global limiter has a separate budget for the superadmin login. Failures against other logins never refuse the sign-in of the superadmin.
- The server reads a bounded number of sign-in bodies at the same time (`server.max_login_body_reads`). Slow sign-in bodies cannot hold every connection that ForwardAuth needs.
- A successful authentication creates a random, opaque session identifier of 256 bits.
- Active sessions are held in limited process RAM. With PostgreSQL, they are held in the shared database as SHA-256 digests. The database never stores the cookie.
- Each instance keeps a copy of each active session for a few seconds.
- A logout revokes the exact session on the server.
- With SQLite, a process restart invalidates all sessions.
- With PostgreSQL, sessions survive a process restart.
- A logout, a deleted account, or a changed password ends the session in the database at once.
- The other instances end their copy of the session within 5 seconds.
- Over HTTPS, the cookies are host-only (`__Host-`), `Secure`, `HttpOnly`, and `Path=/`. Their SameSite value is Strict or Lax.
- Over plain HTTP, the cookies use the same name without the prefix. They have no `Secure` attribute (see "Plain HTTP").
- A session works only from the client address and User-Agent that logged in (`session.bind`).
- The client address comes from `X-Forwarded-For` only when the TCP peer is in `server.trusted_proxies`. For any other peer, the client address is the address of the peer.
- A session ends after `session.idle_timeout` without use.
- The server renders the launcher. The launcher references nothing outside the binary.
- The CSP of the launcher is `default-src 'none'`. The launcher uses its own style, its own script, and its own images.
- The server HTML-escapes every value taken from a label.
- Every `href` is a local path or an http(s) URL.
- POST login and POST logout require CSRF protection. For login, the protection is a double-submit cookie. For logout, it is an HMAC of the session ID.
- The server also validates the same origin with Origin and Referer.
- Return paths are local path and query values only. The server signs them with HMAC.
- A return path with an ASCII control byte (0x00 to 0x1F, 0x7F) is refused, raw or after one percent-decode. A URL parser of the URL Standard removes TAB, CR and LF, so `/<TAB>/host` would become `//host`.
- The server validates dynamic HTTP header values with typed Hyper header parsing.
- Prometheus labels never contain unbounded values, secret values, or values that users control.
- Security failures fail closed.

## Identity token

With the `identity` block, `/auth/verify` gives a JWT to each service that a user can open (protocol section 9). The server signs the token with Ed25519 (the `alg` value is `Ed25519`). The token holds `iss`, `sub` (the login), `aud` (the id of the service), `iat`, and `exp`. It holds no permission. Its life is `identity.token_ttl`. The default is 5 minutes. The maximum is 1 hour.

- The server gets the private key from `identity.signing_key_env` or from `identity.signing_key_file`. The server reads the key once at start. A key that is not a PKCS#8 Ed25519 key stops the server. The effective configuration and the log show only its `kid`.
- The server keeps one token for each session and each service. It issues the token again when half of its life is gone. The cache holds at most 16384 tokens. A token stays valid until `exp` after a logout. Keep `token_ttl` short.
- Traefik must list the header in `authResponseHeaders`. Traefik then replaces a header of the same name that a client sends. A service must verify the signature, `iss`, `aud`, and `exp` of each token. A service must accept the token only in this header.

## AuthZEN PDP

With the `pdp` block, the server answers AuthZEN requests on a second listener (`pdp.listen_address`). Traefik does not route to this listener.

- Each server has its own bearer token. A `pdp_client` block, with `_env` or `_file`, gives the token. The token must contain at least 32 bytes. Each server uses a different token. The PDP keeps only the SHA-256 of each token. The PDP compares tokens in constant time. The PDP returns `401` for a request without a valid token.
- The PDP answers a server only for its own resources. A resource of a type other than `service` is a node below the service of the server that asks. The bearer token tells the server. The first ancestor of the node must be this service. The chain must follow the parents of the last known document of the service. Else, the PDP returns `false` with the reason `invalid_resource`. The service of another server gets the same answer. Thus a request cannot name a node of another service, or a database without its host.
- A node without ancestors is a node of depth 1. This keeps the requests of a document of one level. With a tree, such a request on a deeper type is not valid.
- A grant covers its node and each node below it. The PDP compares the chains segment by segment, with exact types and decoded ids. A grant does not cover its parent or a node of another branch.
- With `context.match = "subtree"`, a grant on a node below the resource also gives `true`. This answer only tells that something below the resource is open: a service uses it to show a navigation element, never to permit an operation. An operation asks with the default `"node"`. A bad value gives `false` with the reason `invalid_request`.
- A grant below the service applies only when the last known document of the service follows it. The action must list the type, and the path must follow the parents. The PDP keeps the last document that it read for each service. A failed read does not remove this document. If the document does not follow the grant, the PDP does not apply it (fail closed). If portal never read the document of the service, the PDP returns `false` for each node below this service. A grant on the whole service is not affected. The check is at run time. The start checks only the form of the grants.
- The PDP fails closed. It returns `false` for an unknown user, an unknown subject type, or a bad action name. It returns `400` and no decision for a request that it cannot read. The PDP limits the body size (`pdp.max_body_bytes`). It limits the batch size (`pdp.max_evaluations`).
- The log records each decision (`pdp_decision`). Each entry has the server, the subject, the action, the resource, the decision, and the reason. The resource is the chain with the ids percent-encoded, so that an id cannot add a line or a field to the log. The counters `portal_pdp_decisions_total` and `portal_pdp_refused_total` are in `/metrics`.
- The PDP has no TLS. The deployment must limit the network of the listener to the services and the portal. This differs from AuthZEN, which requires TLS. In K3s, the NetworkPolicy `allow-portal-pdp` admits to the port 4182 only the pods with the label `portal-pdp-client: "true"` in the namespaces `apps`, `observability` and `edge`.

## Keys

- A key has one of two kinds. The prefix gives the kind: `mcp_` for agents, `api_` for services and webhooks. The prefix `whk_` is reserved. The key edge `/auth/verify-key` verifies `mcp_` keys. The API of the Inbox verifies `api_` keys. Each verifier refuses the other kind with `401` and no detail.
- The portal never stores the text of a key. It stores the SHA-256 digest and the last 4 characters. The text has the prefix, 43 random base62 characters and a CRC-32 check of the prefix and the 43 characters. A typing error or a changed prefix gets no database read.
- The edge `/auth/verify-key` reads only memory. A refusal gives status 401 with an empty body. The body and the headers never tell which check failed. The log has at most one line each minute for each address and cause.
- A key holds only the actions of the service `portal` that its kind lists. An MCP key lists none. An API key lists `alert.send`, `alert.send_user` and `alert.read`, and it holds `access on service:portal` for the budget `alerts`. An API key holds no grant on another service. The PDP ignores any other grant.
- A key never gives more than its owner. This holds for both kinds. The PDP checks both sides at each question, and the lowest ceiling applies. A key also needs `keys.own.use` on its kind. When the owner loses a right, the key loses it at once. When an account is deleted, its keys go with it.
- The rights to make and to use a key are by kind (`key_kind:mcp` and `key_kind:api`). The switches `keys.mcp` and `keys.api` turn a kind off. A kind that is off has no key edge or no route.
- A secret of the configuration (password hash, key text) uses only `_env` or `_file`. A secret in clear text stops the start.
- The portal counts the uses of a budget in the database, with one statement that takes the unit from each counter or from none. A failed count gives status 503, never `true`.
- A form of the key page has a size limit and at most 4 forms are read at the same time.
- On SIGTERM, the instance closes its listeners after 5 seconds and waits at most 10 seconds for the answers in progress.

## Actions of the services

The server reads `/.well-known/procurauth/actions` of each service. It reads the document from an internal URL that a label gives (`portal.actions`, or the origin of `portal.health`). The read has a timeout. It has a size limit of 64 KiB. The read does not follow redirects. The concurrency is limited. The server refuses the whole document in these cases:

- a bad action;
- a duplicate action;
- the action `access`;
- a `server` that is not the id of the service.

A person who may set the labels of a workload chooses the actions that the settings pages offer for it. This is the trust boundary of the discovery.

## Session secret modes

Exactly two modes exist.

### `ephemeral`

```hcl
session {
  secret_mode = "ephemeral"
}
```

The process generates a fresh 32-byte secret with `getrandom` at startup. The process zeroizes the secret at application state teardown.

### `provided`

```hcl
session {
  secret_mode = "provided"
  secret_file = "/run/secrets/portal_session_secret"   # or: secret_env = "PORTAL_SESSION_SECRET"
}
```

Give exactly one of `secret_env` (the name of an environment variable) or `secret_file` (an absolute path). The value must contain at least 32 bytes. The program reads it with a finite upper bound. The program removes one trailing line end from the content of the file. Inline HCL secrets are not supported.

The secret protects HMAC-signed authentication control data. Examples are the return target and the anti-CSRF tokens. With SQLite, active opaque sessions stay only in RAM in both modes. Therefore `provided` does **not** make them survive a process restart. With PostgreSQL, the sessions are in the database. `provided` is required. Every instance must sign with the same key. `ephemeral` is rejected.

## Opaque sessions

A session ID is a random 32-byte value. It is encoded with unpadded base64url. The cookie is not a JWT. It contains no claims and no expiration data.

The server stores only these values:

- a SHA-256 digest of the token;
- an `Instant` expiration;
- a last-use time;
- a hash of the client binding.

Logout therefore revokes the session on the server. It does not only delete the browser cookie. A cookie with another binding is refused. The refusal does not change the session. A thief cannot end the owner's session this way.

The store is limited by `session.max_active_sessions`. The server removes expired entries during session operations. When capacity is reached, new sessions fail closed with `503`.

With PostgreSQL, the digest, the binding hash, the identity, the expiry, and the last use are rows of the shared database. The instance that served the login writes these rows. The same statement enforces the binding and the idle timeout, and finds the session. A refused binding renews nothing. Each instance trusts its own copy of a session for 5 seconds at most. A revocation made on another instance takes up to 5 seconds to reach the copy. While the database is unreachable, a copy checked less than 2 minutes ago is still honoured. This copy is bounded by the session's own expiry. This rule prevents the sign-out of every user. Logins fail closed. The failed-login limiters stay per instance. With N instances, an attacker has N times the budget.

A row also holds the credential generation of the superadmin: HMAC-SHA-256 under the session secret of the superadmin login and password. The database never holds a plain hash of the superadmin password. Thus a reader of the database cannot test password guesses without the secret. A superadmin or impersonated row is valid only with the generation of the instance that reads it. At start, each instance deletes the superadmin and impersonated rows of the other generations. After a suspected theft of a superadmin cookie, rotate the superadmin password or the session secret: the rotation ends the stolen session on every instance.

### Incident action: end all sessions

The superadmin can end every session from the page **Users**. The command `portal end-sessions --config FILE` does the same on a PostgreSQL database. Both end the sessions of the superadmin too. The route `POST /auth/users/end-sessions` has the checks of the account routes: the `Origin` check, the anti-CSRF token and the superadmin in its own right. It also needs the field `confirm=end-all-sessions`. Each use writes the audit event `sessions_ended`. With a shared database, an instance can accept a cached session for up to 5 seconds after the end. The command does not work with SQLite, because the sessions are in the memory of the process: restart the portal.

## Plain HTTP

The portal knows nothing about TLS. Traefik terminates TLS, and the portal listeners speak plain HTTP. Traefik can also serve plain HTTP. Examples are a LAN without certificates and a test stack. The server reads the scheme for each request. It uses the unique `X-Forwarded-Proto` value that Traefik sets (`trustForwardHeader: false`). Any value other than a single `https` counts as HTTP.

Browsers refuse `Secure` and `__Host-` cookies over HTTP. Therefore, over HTTP, the cookies have no prefix (`portal`, `lc`). They have no `Secure` attribute.

Over HTTP, an attacker on the network can read the password and the session cookie. The binding limits the value of a stolen cookie. The binding uses the client address and User-Agent, relayed by `authRequestHeaders: [Cookie, User-Agent]`. The idle timeout also limits the value of a stolen cookie. Only HTTPS removes the exposure. Use HTTPS on every network that is not trusted.

## Discovery and the launcher

The catalogue comes from container labels. The labels come from the Docker socket, which is mounted read-only. The catalogue also comes from IngressRoute annotations. They come from the in-cluster API. The ServiceAccount is limited to `get` and `list` on `ingressroutes` and `pods`.

A person who can create a container or an IngressRoute chooses what the launcher lists. This is the trust boundary. It is acceptable for a single administrator. A workload with unusable labels is skipped. The page reports it. The workload is not listed in part. The gate keeps the path of its `href` closed, because Traefik still routes the workload.

The gate opens a path that no service owns to each signed-in user. This rule is safe only when the catalogue holds every service. The first snapshot after a start is empty. A source that has not answered yet gives a snapshot without its services. Thus, until each configured source answered once, the gate lets only the superadmin through. A source that failed at setup keeps this state for the life of the process. The metric `portal_catalog_complete` shows the state.

Health probes are plain GET requests to URLs taken from labels. They do not follow redirects. They do not read the body. They have a timeout and limited concurrency. They send nothing secret. The probe client does not verify certificates. It asks only whether something answers.

Icons are SVG files. They are included in the image and indexed once at startup. A request can name only an indexed icon. A request cannot build a path. The server serves each icon with `Content-Security-Policy: default-src 'none'; sandbox`.

## Request-origin checks

Before the service parses the body of `POST /auth/login` or `POST /auth/logout`, the service verifies the request origin.

- With Traefik forwarding metadata, the service requires one unique `X-Forwarded-Proto` header and one unique `X-Forwarded-Host` header.
- Without forwarded metadata, the expected origin is `http` plus the unique `Host` header.
- If `Origin` exists, it must match the normalized expected origin exactly. A mismatch cannot use Referer instead.
- If `Origin` is absent, the service requires one unique absolute `Referer` with the same normalized origin.
- The service rejects `Origin: null`, duplicate headers, unsupported schemes, userinfo, and partial forwarded metadata.

This check is defense in depth. It works in addition to the CSRF token.

## HTTP resource limits

The server configuration sets these limits:

```hcl
server {
  max_connections     = 1024
  max_headers         = 64
  max_header_bytes    = "32KiB"
  header_read_timeout = "10s"
  max_body_bytes      = "32KiB"
  body_read_timeout   = "10s"
}
```

Hyper's HTTP/1 connection parser receives the header limits and timeouts. The application collects the body in parts. It checks the size before each extension. A total read timeout wraps the collection.

`POST /auth/login` is the only route that reads a body before it knows the client. One connection can hold a slot for `header_read_timeout` plus `body_read_timeout`. The server therefore reads at most `server.max_login_body_reads` sign-in bodies at the same time. It refuses the other sign-ins at once with `503` and `Retry-After: 1`. This value must be less than `max_connections`, so ForwardAuth always has connections left.

The client address comes from `X-Forwarded-For` only when the TCP peer is in `server.trusted_proxies`. The server reads the header from the right. It skips the trusted addresses and takes the first address that is not trusted. A peer that is not trusted, a missing header, two headers, or an entry that is not an address give the address of the peer. Keep the networks of this list small. In K3s, the NetworkPolicy `allow-portal-http` admits only the Traefik and Prometheus pods to the port 4181.

## Password controls

Two limiters protect against password failures. The first is a limited per-IP limiter. The second is a separate global limiter. The global limiter is not reset on purpose by a successful login. Both stores are process-local.

The global limiter has two budgets of the same size: one for the superadmin login, one for all other logins. Anonymous failures against other logins thus cannot refuse the sign-in of the superadmin. Failures against the superadmin login lock out only the superadmin. This has a cost: while one budget is locked out, the answer tells whether a typed login is the superadmin login. The superadmin login is not a secret, because each identity token carries it in `sub`.

Argon2 checks run through an async semaphore and `spawn_blocking`. This keeps the CPU-heavy hash calculations away from the Tokio I/O workers.

## Metrics and logs

Security-relevant metrics cover these items:

- successful and failed logins;
- CSRF failures;
- origin failures (login, logout, users);
- IP and global rate-limit blocks;
- ForwardAuth results (including `denied`);
- session validation outcomes;
- active and capacity sessions;
- Argon2 checks;
- account administration results;
- history events dropped or failed to flush;
- HTTP request latency and status;
- connection saturation.

No metric label contains a login, an address, or a user agent.

The log records events such as:

```text
login_success
login_failed
login_rate_limited
origin_check_failed
session_capacity_reached
```

Do not log the password, session ID, CSRF token, HMAC secret, Cookie header, return-signature value, or Authorization header.

## Dependency resolution

The `Cargo.lock` file is generated explicitly with `./scripts/update-lockfile.sh` after a dependency change. It is reviewed and committed. CI, Docker, and release builds require the committed resolution. They run the actual test and build steps with `--frozen`. Release jobs never regenerate the dependency resolution.

## Known limits and deployment measures

These limits come from the deployment, not from the code. The portal cannot remove them. Apply the measures below.

### The session cookie goes to every service on the same origin

The session cookie has `Path=/`. The browser sends it with each request to the origin of the portal. In K3s, the services are routed by path on the same origin (`/grafana/`, `/dagu/` and others). Traefik forwards the `Cookie` header to the service. A service, or a person who controls a service, therefore receives the session cookie, the client address and the User-Agent. With these three values, the service can act as the user, also as the superadmin.

Measures:

- Remove the portal cookie from the requests to the services. Use a Traefik middleware that removes the `Cookie` header, or only the portal cookie, on the routes of the services.
- Better: give the portal its own host name, and give each service that you do not trust its own host name. A host-only `__Host-` cookie then never reaches these services.
- Use the identity token (`X-Identity-Token`) in the services, not the cookie.
- Keep `session.bind = "ip_user_agent"` and a short `session.idle_timeout`.

### The settings pages share the origin of the services

The settings pages and the account administration (`/auth/settings`, `/auth/users/*`) are on the same origin as the services. The `Origin` check and the anti-CSRF token do not separate the portal from a script that a service serves on the same origin. Such a script can read the settings pages and send the administration forms of the superadmin.

Measures:

- Serve the portal (`/auth/*`) on a host name of its own, and do not route services on that host name.
- Do not route on the portal origin a service that lets its users publish HTML or JavaScript.
- Open the settings pages only in a browser profile that does not open the services.

### Traefik path normalisation

The gate reads the path of `X-Forwarded-Uri` in each way that a proxy or a backend can read it, after it removes dot segments. Traefik must route on the same clean path. If Traefik routes on a path that still holds `..` or an encoded `/` (`%2F`), a request can match the route of a service while the gate reads it as a path without an owner.

Measures:

- On each entry point of Traefik, set `http.sanitizePath: true`. Set each key of `http.encodedCharacters` to `false`: `allowEncodedSlash`, `allowEncodedBackSlash`, `allowEncodedNullCharacter`, `allowEncodedSemicolon`, `allowEncodedPercent`, `allowEncodedQuestionMark` and `allowEncodedHash`. The Compose configuration of Traefik (`traefik/docker/compose.yaml`) sets these values. Set them also on the K3s entry point `websecure` (`traefik/k3s/controller.yaml`).
- Check the effective configuration of the entry point after each upgrade of Traefik.


## Inbox

The Inbox is a feature of the portal. The page `docs/inbox/SECURITY.md` gives its model. These are the invariants.

- A phone enrolls with a one-time code of an account of the portal. The code holds the address of the server, the public key of the server and a secret of 128 bits. The secret works one time and expires. The server keeps only its SHA-256 digest.
- The phone pins the public key of the code. The session is Noise IK between the key of the phone and this key. A network capture gives no secret and no material for an offline attack.
- The server limits the enrollment attempts for each client address.
- The routes of the phones (`/v1/enroll`, `/v1/session`) and the API of the senders share the listener of the portal. They have no session, set no cookie and need no ForwardAuth. The portal matches them before every other route and applies no session, CSRF or origin check to them. A phone session uses a budget of connections that is separate from the pages: a flood of phone sessions cannot starve the pages, and a flood of page requests cannot starve the phones.
- A phone subscribes only to a group for which its account has `inbox.receive`. A loaded directory that positively refuses a subscription removes it. A directory that is not ready, old or empty never removes one.
- A sender sends an alert with an API key (`api_`) as a bearer token. The server checks the form of the text (prefix, length, characters, checksum) before any lookup. A key of another kind gets `401` with no detail. The server compares the keys of the file in constant time. A key needs `alert.send` on the group for the key and for its owner. To alert an account, the key and its owner need `alert.send_user` on the service `portal`: this right is flat, so a sender can alert any account. An alert to an account has no group, so only the key that sent it can read its status. A key of the file decides its groups, and `direct_alerts`, at start and reads no directory.
- The page of the Inbox needs a session of the portal. A form carries an anti-CSRF token and a same-origin check. A session that the superadmin opens as another account cannot send an alert and cannot revoke a phone. Such a session can make an enrollment code for that account. The owner has not reviewed this point.
- The key file of the Inbox server and the secrets of the `api_key` blocks use only `_env` or `_file`. A secret in clear text stops the start. The log never holds a key text, a code or a secret.
- The portal listener has no TLS. An API key travels in clear text on plain HTTP. Use a TLS proxy for the senders.
- The Android app accepts clear text for each address. It has no network security configuration. This is an open point.
