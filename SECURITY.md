# Security model

`portal` is a ForwardAuth barrier, the launcher page of the lab, and the AuthZEN PDP of the deployment (section 11 of the MCP server contract). It has one superadmin, defined by the environment, and users with grants. It intentionally does not implement groups, OAuth/OIDC, or password reset. Accounts and connection history are stored in one SQLite file. When several instances run, they use a shared PostgreSQL database instead (the `postgres` block).

A grant is an action on a service, or on one sub-resource of a service. The grants come from the settings pages, from `user` blocks of the configuration, or from both (`access.grants_mode`). `/auth/verify` decides the action `access` with the grants in memory. The services decide their other actions. They ask the PDP. See "Grants and the gate" in the README.

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
- The server writes every change to the audit history.
- The 403 page names no service. A user does not learn the name of a service that is not open to that user.
- In the mode `static`, the settings pages refuse a change of grants. The server does not use the grants of the database.
- In the mode `hybrid`, the settings pages cannot change a grant of the configuration.
- The settings pages give a grant on a sub-resource only for an action and a resource type that the service declares. While the service has not answered, the pages refuse the grant. The resource id is exact. The server does not trim it or change its case. An id with a space or a control character is refused.
- At start, the server does not know the services. Thus it checks only the form of a grant of the configuration. The Services page shows a grant whose type the service does not declare.
- The **Generate** button in the add-user form draws a 20-character password in the browser.
- The button uses `crypto.getRandomValues` and rejection sampling over an alphabet without look-alike characters.
- The superadmin sees the generated password, so that the superadmin can give it to the user.
- The password reaches the server only when the superadmin submits the form.
- The history records addresses and user agents. The server keeps them for `storage.retention_days`.
- The history never records a path, a query, a password, or what was typed in a login that does not exist.
- The server limits the Argon2 concurrency.
- The server limits the rate of login failures per IP address and globally.
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
- A session ends after `session.idle_timeout` without use.
- The server renders the launcher. The launcher references nothing outside the binary.
- The CSP of the launcher is `default-src 'none'`. The launcher uses its own style, its own script, and its own images.
- The server HTML-escapes every value taken from a label.
- Every `href` is a local path or an http(s) URL.
- POST login and POST logout require CSRF protection. For login, the protection is a double-submit cookie. For logout, it is an HMAC of the session ID.
- The server also validates the same origin with Origin and Referer.
- Return paths are local path and query values only. The server signs them with HMAC.
- The server validates dynamic HTTP header values with typed Hyper header parsing.
- Prometheus labels never contain unbounded values, secret values, or values that users control.
- Security failures fail closed.

## Identity token

With the `identity` block, `/auth/verify` gives a JWT to each service that a user can open (contract 11.3). The server signs the token with Ed25519 (`EdDSA`). The token holds `iss`, `sub` (the login), `aud` (the id of the service), `iat`, and `exp`. It holds no permission. Its life is `identity.token_ttl`. The default is 5 minutes. The maximum is 1 hour.

- The server gets the private key from `identity.signing_key_env` or from `identity.signing_key_file`. The server reads the key once at start. A key that is not a PKCS#8 Ed25519 key stops the server. The effective configuration and the log show only its `kid`.
- The server keeps one token for each session and each service. It issues the token again when half of its life is gone. The cache holds at most 16384 tokens. A token stays valid until `exp` after a logout. Keep `token_ttl` short.
- Traefik must list the header in `authResponseHeaders`. Traefik then replaces a header of the same name that a client sends. A service must verify the signature, `iss`, `aud`, and `exp` of each token. A service must accept the token only in this header.

## AuthZEN PDP

With the `pdp` block, the server answers AuthZEN requests on a second listener (`pdp.listen_address`). Traefik does not route to this listener.

- Each server has its own bearer token. A `pdp_client` block, with `_env` or `_file`, gives the token. The token must contain at least 32 bytes. Each server uses a different token. The PDP keeps only the SHA-256 of each token. The PDP compares tokens in constant time. The PDP returns `401` for a request without a valid token.
- The PDP answers a server only for its own resources. For a question about the service of another server, the PDP returns `false`. A resource of a type other than `service` is a sub-resource of the service of the server that asks. The bearer token tells the server. The request cannot name another service for a sub-resource.
- A grant on a service covers all its sub-resources. A grant on a sub-resource covers only the same type and the same id. It does not cover the service or another sub-resource.
- The PDP fails closed. It returns `false` for an unknown user, an unknown subject type, or a bad action name. It returns `400` and no decision for a request that it cannot read. The PDP limits the body size (`pdp.max_body_bytes`). It limits the batch size (`pdp.max_evaluations`).
- The log records each decision (`pdp_decision`). Each entry has the server, the subject, the action, the resource, the decision, and the reason. The counters `portal_pdp_decisions_total` and `portal_pdp_refused_total` are in `/metrics`.
- The PDP has no TLS. The deployment must limit the network of the listener to the services and the portal. This differs from AuthZEN, which requires TLS. In K3s, the NetworkPolicy `allow-portal-pdp` admits to the port 4182 only the pods with the label `portal-pdp-client: "true"` in the namespaces `apps`, `observability` and `edge`.

## Actions of the services

The server reads `/.well-known/access-actions` of each service. It reads the document from an internal URL that a label gives (`portal.actions`, or the origin of `portal.health`). The read has a timeout. It has a size limit of 64 KiB. The read does not follow redirects. The concurrency is limited. The server refuses the whole document in these cases:

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

## Plain HTTP

Traefik can serve plain HTTP. Examples are a LAN without certificates and a test stack. The server reads the scheme for each request. It uses the unique `X-Forwarded-Proto` value that Traefik sets (`trustForwardHeader: false`). Any value other than a single `https` counts as HTTP.

Browsers refuse `Secure` and `__Host-` cookies over HTTP. Therefore, over HTTP, the cookies have no prefix (`portal`, `portal-csrf`). They have no `Secure` attribute.

Over HTTP, an attacker on the network can read the password and the session cookie. The binding limits the value of a stolen cookie. The binding uses the client address and User-Agent, relayed by `authRequestHeaders: [Cookie, User-Agent]`. The idle timeout also limits the value of a stolen cookie. Only HTTPS removes the exposure. Use HTTPS on every network that is not trusted.

## Discovery and the launcher

The catalogue comes from container labels. The labels come from the Docker socket, which is mounted read-only. The catalogue also comes from IngressRoute annotations. They come from the in-cluster API. The ServiceAccount is limited to `get` and `list` on `ingressroutes` and `pods`.

A person who can create a container or an IngressRoute chooses what the launcher lists. This is the trust boundary. It is acceptable for a single administrator. A workload with unusable labels is skipped. The page reports it. The workload is not listed in part.

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

## Password controls

Two limiters protect against password failures. The first is a limited per-IP limiter. The second is a separate global limiter. The global limiter is not reset on purpose by a successful login. Both stores are process-local.

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
