# Security policy

Inbox carries alerts that may be safety-critical. Report vulnerabilities
privately to the repository owner (GitHub security advisory on this repository),
not in public issues. Include affected version, impact and reproduction steps.

## Scope and design

- Passwords are only used inside OPAQUE (`opaque-ke`, pinned exactly because
  credential records embed its serialization). Credential records are created
  offline with `inbox credential`.
- Device sessions are Noise IK (`snow`), keys pinned at enrollment; devices are
  revocable individually.
- Admin requests are HMAC-signed with timestamp and nonce after an OPAQUE login.
- Known limitation: on plain HTTP an active network attacker can modify the admin
  front's JavaScript. Serve the admin listener over TLS or on a private network.
- Secrets live in files referenced by the configuration (`*_file`); they are never
  logged. Losing the server key file invalidates every device and credential.

## Operator checklist

- Keep `server.admin_listen` private (Compose binds it to 127.0.0.1) or behind TLS.
- Restrict `server.trusted_proxies` to your reverse proxies.
- Back up the server key and the database; protect both (they are secrets).
- Alert on `inbox_devices_unreachable` and `inbox_oldest_pending_delivery_seconds`.
