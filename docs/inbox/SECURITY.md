# Security policy

Inbox carries alerts that may be safety-critical. Report vulnerabilities
privately to the repository owner (GitHub security advisory on this repository),
not in public issues. Include affected version, impact and reproduction steps.

## Scope and design

- Enrollment uses a one-time code of an account. The phone pins the server key of the code
  and sends the secret inside a Noise IK message to that key. The secret works one time.
  Failed attempts are rate-limited per client address.
- Device sessions are Noise IK (`snow`), keys pinned at enrollment; devices are
  revocable individually.
- The admin front has no password, by design: whoever reaches
  `server.admin_listen` can send alerts and revoke devices. Network placement (or
  an authenticating reverse proxy) is the access control. The server refuses
  direct requests whose `Host` is not in `admin.allowed_hosts` (DNS rebinding;
  requests from non-loopback `server.trusted_proxies` are exempt), `POST`s
  without `Content-Type: application/json` and `X-Inbox-Request: 1` (CSRF), and
  requests marked `Sec-Fetch-Site: cross-site`.
- Secrets live in files referenced by the configuration (`*_file`); they are never
  logged. Losing the server key file invalidates every device and the device credential.

## Operator checklist

- Keep `server.admin_listen` private (Compose binds it to 127.0.0.1) or behind a
  reverse proxy that authenticates users, ideally over TLS.
- Keep `admin.allowed_hosts` to the exact hostnames used without a proxy.
- Prefer `Authorization: Bearer` over `?token=`: query strings are often logged.
- Restrict `server.trusted_proxies` to your reverse proxies' addresses: never the
  Docker gateway or a whole bridge range (host and SSH-tunnel traffic come from it).
- Back up the server key and the database; protect both (they are secrets).
- Alert on `inbox_devices_unreachable` and `inbox_oldest_pending_delivery_seconds`.
