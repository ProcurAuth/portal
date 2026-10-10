# Security model

`portal` is a ForwardAuth barrier and the launcher page of the lab. It has one superadmin defined by the environment and users with roles managed by the superadmin; it intentionally does not implement groups, OAuth/OIDC, or password reset. Accounts and connection history live in one SQLite file. Services declare the roles they accept in `portal.roles`; the roles that exist are listed in the configuration, and `/auth/verify` enforces them (see "Roles and the gate" in the README).

## Core invariants

- project code forbids `unsafe` Rust;
- HTTP/1 parsing is delegated to Hyper;
- request headers, connection count, body size, and read times are bounded;
- unknown/invalid HCL configuration fails startup;
- the superadmin password is read from a file, bounded, Argon2id-hashed once, then zeroized; user passwords are only ever stored as Argon2id hashes;
- a login attempt costs exactly one Argon2 check whoever the login names (a dummy hash stands in for an unknown login) and fails with the same message;
- a service with no `portal.roles` is open to the superadmin only: access is granted by naming a role, never by omission;
- the role gate reads the forwarded path every way a proxy or a backend might and allows the request only if every reading is allowed; an unreadable URI is refused;
- account administration (add, edit, delete, log in as) is superadmin-only, checked on the server, with a purpose-bound CSRF token and a same-origin check; a new password is hashed under the same Argon2 permit as a login and ends that account's sessions; every change is written to the audit history;
- the 403 page names a service only by looking its id up in the catalogue, so nothing taken from the URL is ever shown;
- the add-user form's **Generate** button draws a 20-character password in the browser with `crypto.getRandomValues` (rejection sampling over an alphabet without look-alike characters); it is shown to the superadmin to hand over and reaches the server only when the form is submitted;
- the history records addresses and user agents (kept `storage.retention_days`), never a path, a query, a password, or what was typed in a login that does not exist;
- Argon2 concurrency is bounded;
- login failures are rate-limited per IP and globally;
- successful authentication creates a 256-bit opaque random session identifier;
- active sessions are held only in bounded process RAM;
- logout revokes the exact session server-side;
- process restart invalidates all sessions;
- over HTTPS cookies are host-only (`__Host-`), `Secure`, `HttpOnly`, `Path=/`, and SameSite Strict/Lax; over plain HTTP the same name without the prefix and without `Secure` (see "Plain HTTP");
- a session only works from the client address and User-Agent that logged in (`session.bind`) and ends after `session.idle_timeout` without use;
- the launcher is rendered server-side and references nothing outside the binary (CSP `default-src 'none'`, own style, own script, own images); every value taken from a label is HTML-escaped and every `href` is a local path or an http(s) URL;
- POST login/logout require CSRF (login: double-submit cookie; logout: HMAC of the session ID) plus same-origin Origin/Referer validation;
- return paths are local path/query values only and are HMAC-signed;
- dynamic HTTP header values are validated by typed Hyper header parsing;
- Prometheus labels exclude unbounded or secret/user-controlled values;
- security failures are fail-closed.

## Session secret modes

Exactly two modes exist.

### `ephemeral`

```hcl
session {
  secret_mode = "ephemeral"
}
```

A fresh 32-byte secret is generated with `getrandom` at process startup and zeroized with application state teardown.

### `file`

```hcl
session {
  secret_mode = "file"
  secret_file = "/run/secrets/portal_session_secret"
}
```

The path must be absolute. The file must contain at least 32 bytes and is read with a finite upper bound. Inline HCL secrets are not supported.

The secret protects HMAC-signed authentication control data such as the return target. Opaque active sessions remain in RAM only in both modes. `file` therefore does **not** make active sessions survive a process restart.

## Opaque sessions

Session IDs are random 32-byte values encoded with unpadded base64url. The cookie is not a JWT and contains no claims or expiration data.

The server stores only a SHA-256 digest of the token plus an `Instant` expiration, a last-use time and a hash of the client binding. This makes logout true server-side revocation rather than merely deleting the browser cookie. A cookie presented with another binding is refused and leaves the session alone, so a thief cannot end the owner's session by trying it.

The store is bounded by `session.max_active_sessions`. Expired entries are pruned during session operations. When capacity is reached, new sessions fail closed with `503`.

## Plain HTTP

Traefik may serve plain HTTP (a LAN without certificates, a test stack). The scheme is read per request from the unique `X-Forwarded-Proto` value Traefik sets (`trustForwardHeader: false`); anything but a single `https` counts as HTTP. Browsers refuse `Secure` and `__Host-` cookies on HTTP, so over HTTP the cookies are named without the prefix (`portal`, `portal-csrf`) and carry no `Secure` attribute.

Over HTTP the password and the session cookie are readable on the network. The binding (client address and User-Agent, relayed by `authRequestHeaders: [Cookie, User-Agent]`) and the idle timeout limit what a stolen cookie is worth; only HTTPS removes the exposure. Use HTTPS wherever the network is not trusted.

## Discovery and the launcher

The catalogue comes from container labels (Docker socket, read-only mount) and IngressRoute annotations (in-cluster API, ServiceAccount limited to `get`/`list` on `ingressroutes` and `pods`). Whoever can create a container or an IngressRoute chooses what the launcher lists; that is the trust boundary and is acceptable for a single administrator. A workload with unusable labels is skipped and reported on the page, not half-listed.

Health probes are plain GET requests to URLs taken from labels, without redirects and without reading the body, with a timeout and bounded concurrency; they send nothing secret. The probe client does not verify certificates (it asks whether something answers).

Icons are SVG files shipped in the image and indexed once at startup; a request can only name an indexed icon, never build a path. Each icon is served with `Content-Security-Policy: default-src 'none'; sandbox`.

## Request-origin checks

Before parsing the body of `POST /auth/login` or `POST /auth/logout`, the service verifies the request origin.

- With Traefik forwarding metadata, both unique `X-Forwarded-Proto` and unique `X-Forwarded-Host` are required.
- Without forwarded metadata, the direct expected origin is `http` plus the unique `Host` header.
- If `Origin` exists, it must exactly match the normalized expected origin. A mismatch cannot fall back to Referer.
- If `Origin` is absent, a unique absolute `Referer` with the same normalized origin is required.
- `Origin: null`, duplicate headers, unsupported schemes, userinfo, and partial forwarded metadata are rejected.

This check is defense in depth in addition to the CSRF token.

## HTTP resource limits

The server configuration provides:

```hcl
server {
  max_connections     = 1024
  max_headers         = 64
  max_header_bytes    = "32KiB"
  header_read_timeout = "10s"
  max_body_bytes      = "8KiB"
  body_read_timeout   = "10s"
}
```

Hyper's HTTP/1 connection parser receives the header bounds/timeouts. Application body collection is incremental, size-checked before each extension, and wrapped in a total read timeout.

## Password controls

Password failures are protected by a bounded per-IP limiter and a separate global limiter. The global limiter is intentionally not reset by a successful login. Both stores are process-local.

Argon2 checks run through an async semaphore and `spawn_blocking`, preventing CPU-heavy password hashing from blocking Tokio's I/O workers.

## Metrics and logs

Security-relevant metrics include successful/failed logins, CSRF failures, origin failures (login, logout, users), IP/global rate-limit blocks, ForwardAuth results (including `denied`), session validation outcomes, active/capacity sessions, Argon2 checks, account administration results, history events dropped or failed to flush, HTTP request latency/status, and connection saturation. No label carries a login, address or user agent.

Logs include events such as:

```text
login_success
login_failed
login_rate_limited
origin_check_failed
session_capacity_reached
```

Never log the password, session ID, CSRF token, HMAC secret, Cookie header, return-signature value, or Authorization header.

## Dependency resolution

`Cargo.lock` is generated explicitly with `./scripts/update-lockfile.sh` after a dependency change, reviewed, and committed. CI, Docker, and release builds require that committed resolution and perform actual test/build steps with `--frozen`. Release jobs never regenerate dependency resolution.
