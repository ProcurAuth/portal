# Security of the inbox

The inbox carries alerts that can be vital. Report a vulnerability in private to the owner of the repository, with a GitHub security advisory. Do not use a public issue. Give the version, the impact and the steps to reproduce it.

The security model of the portal is in the root `SECURITY.md`. This page gives the part of the inbox.

## Design

- **Enrollment.** An account of the portal makes a one-time code. The code holds the address of the server, the public key of the server and a secret of 128 bits. The phone pins the key. It sends the secret inside a Noise IK message to this key. A network capture gives neither the secret nor material for an offline attack.
- **Code.** The secret works one time and expires (`inbox.enrollment.code_ttl`). The server keeps only its SHA-256 digest. The page that shows the code has the header `Cache-Control: no-store`.
- **Limits.** The server limits the enrollment attempts for each client address (`inbox.devices.max_auth_failures` in `inbox.devices.auth_failure_window`). It closes a connection that sends no message in 10 seconds.
- **Sessions.** A session is Noise IK between the key of the phone and the pinned key of the server. Each phone can be revoked. A revoked phone gets the close code 4403.
- **Rights.** The permissions of the service `inbox` decide who sends, who reads and who revokes. A phone subscribes only to the groups for which its account has `group.subscribe`.
- **Producers.** The inbox compares the tokens of the file in constant time. A key of the portal needs `alert.send` on the group for the key and for its owner.
- **Pages.** The pages need a session of the portal. A form needs an anti-CSRF token and a same-origin check. A session that the superadmin opens as another account cannot send an alert and cannot revoke a phone.
- **No password.** The shared device password, OPAQUE and the admin front without a password do not exist.
- **Secrets.** The key of the server and the tokens come from `_file` or `_env`. A clear text secret in the file stops the start. The inbox never writes a secret to the log.
- **Cluster.** The requests between instances (`inbox.cluster`) carry an HMAC that the server key derives. Only the pods of the service must reach the cluster port.

## Limits of the model

- The port of the portal has no TLS. Noise protects the phones. The bearer token of a producer travels in clear text on plain HTTP. Put a TLS proxy in front for the producer routes, or use a private network.
- A code is a bearer secret until its first use. Whoever reads it can enroll a phone for the account. Show it only to its owner.
- A key file of the server that leaks lets an attacker act as the server. Replace it and enroll every phone again.
- The sessions that the superadmin opens as another account can read the phones and make a code for that account.

## Operator checklist

- Keep a copy of the key file of the server and of the database. Protect both. Both are secrets.
- Send alerts with the header `Authorization: Bearer`. Query strings are often in logs.
- List only the real proxies in `server.trusted_proxies`. Do not list the Docker gateway or a whole bridge range. The same list gives the client address for the limiter of the enrollment attempts.
- Send `/v1/`, `/api/v1/` and `/api/version` to the portal without ForwardAuth. A phone and a webhook have no session. Add a retry middleware that retries only when the server does not answer.
- Keep `inbox.cluster.listen` on the private network of the pods.
- Do not publish the port of the portal to the internet without a proxy that you control. Let the proxy send only `/v1/`, `/api/v1/` and `/api/version` to the internet, and keep the pages behind ForwardAuth. A phone session uses a budget of connections that is separate from the pages.
- Alert on `portal_inbox_devices_unreachable`, `portal_inbox_oldest_pending_delivery_seconds`, `portal_inbox_storage_degraded` and `portal_inbox_replica_last_snapshot_age_seconds`.
