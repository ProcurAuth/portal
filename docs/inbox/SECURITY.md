# Security of the Inbox

The Inbox carries alerts that can be vital. Report a vulnerability in private to the owner of the repository. Use a security advisory of the host of the repository. Do not use a public issue. Give the version, the impact and the steps to reproduce it.

The security model of the portal is in the root [`SECURITY.md`](../../SECURITY.md). This page gives the part of the Inbox.

## Design

- **Enrollment.** An account of the portal makes a one-time code. The code holds the address of the server, the public key of the server and a secret of 128 bits. The phone pins the key. It sends the secret inside a Noise IK message to this key. A network capture gives neither the secret nor material for an offline attack.
- **Code.** The secret works one time and expires (`inbox.enrollment.code_ttl`). The server keeps only its SHA-256 digest. The answer that shows the code has the header `Cache-Control: no-store`.
- **Limits.** The server limits the failed attempts for each client address (`inbox.devices.max_auth_failures` in `inbox.devices.auth_failure_window`). An enrollment attempt and a session handshake that proves no phone count. A phone that proves its key always gets its session. The server closes an enrollment that sends no message in 10 seconds, and a session handshake after 15 seconds. One client address holds at most `inbox.devices.max_handshakes_per_address` handshakes. One account has at most `inbox.devices.max_per_account` phones.
- **Sessions.** A session is Noise IK between the key of the phone and the pinned key of the server. Each phone can be revoked. A revoked phone gets the close code 4403.
- **Rights.** The permissions of the portal features (the service `portal`) decide who sends, who reads and who revokes. A phone subscribes only to the groups for which its account has `inbox.receive`.
- **Alerts about a service.** An alert of the portal about a service (`inbox.watch`) follows the rule of the launcher. A reader sees it only when the reader may open the service: the superadmin, or an account with `access` on the service. The rule applies to the phones, the notifications, the page of the Inbox and the keys. A key reads such an alert only when its owner may open the service. The Inbox checks the rights of a phone again before it sends such an alert. A phone of an account that lost the access never gets it.
- **Keys.** A sender uses an `api_` key. A verifier checks the form of the text (prefix, length, characters, checksum) and the kind before any lookup. An `mcp_` key and a bad text get `401` with no detail. A key follows its owner and never has more rights than its owner. A key of the file is compared in constant time. A key of the database needs `alert.send` on the group, for the key and for its owner. For an alert to an account, it needs `alert.send_user` on the service `portal`, for the key and for its owner.
- **Page.** The page needs a session of the portal. A form needs an anti-CSRF token and a same-origin check. A session that the superadmin opens as another account cannot send an alert and cannot revoke a phone.
- **No password.** The shared device password, OPAQUE and the admin front without a password do not exist.
- **Secrets.** The key of the server and the secrets of the `api_key` blocks come from `_file` or `_env`. A clear text secret in the file stops the start. The Inbox never writes a secret to the log.
- **Cluster.** The requests between instances (`inbox.cluster`) carry an HMAC that the server key derives. Only the pods of the service must reach the cluster port.

## Limits of the model

- The portal knows nothing about TLS. Noise protects the phones. The bearer key of a sender travels in clear text on plain HTTP. Terminate TLS in the reverse proxy, or use a private network.
- A code is a bearer secret until its first use. Whoever reads it can enroll a phone for the account. Show it only to its owner.
- A key file of the server that leaks lets an attacker act as the server. Replace it and enroll every phone again.
- A session that the superadmin opens as another account can read the phones and make a code for that account. The phone then belongs to that account. The owner has not reviewed this point.
- The Android app allows clear text for each address. It has no network security configuration.
- Each instance has its own rate limits and its own counter of refused sign-ins.
- When the directory of the portal is not ready or is old, the portal cannot tell who may open a service. A new alert of the portal about a service then reaches only the phones of the superadmin. A delivery of such an alert that exists for another phone waits until the portal can tell the rights again.
- With PostgreSQL, a phone of a login that this instance does not know gets the close code 1013 and no session. Examples are an account of the file that left the file and an old login of the superadmin. The Inbox does not end the state of this login, because the file of another instance can still have it. The phone tries again until a person revokes it in the zone **Phones**. Revoke it when every instance has the new file.

## Operator checklist

- Keep a copy of the key file of the server and of the database. Protect both. Both are secrets.
- Send alerts with the header `Authorization: Bearer`. The server reads the key only in this header. A request with a key in the query and no header gets `401`. Do not put a key in a URL: query strings are often in logs.
- Use an HTTPS `inbox.devices.public_address`.
- List only the real proxies in `server.trusted_proxies`. Do not list the Docker gateway or a whole bridge range. The same list gives the client address for the limiter of the enrollment attempts.
- Send `/v1/`, `/api/v1/` and `/api/version` to the portal without ForwardAuth. A phone and a webhook have no session. Add a retry middleware that retries only when the server does not answer.
- Keep `inbox.cluster.listen` on the private network of the pods.
- Do not publish the port of the portal to the internet without a proxy that you control. Let the proxy send only `/v1/`, `/api/v1/` and `/api/version` to the internet, and keep the pages behind ForwardAuth. A phone session uses a budget of connections that is separate from the pages.
- Give each sender its own `api_` key with the smallest set of groups. Revoke a key that leaks.
- Give `access` on a service to each member of `inbox.watch.group` that must get the alerts about this service.
- Alert on `portal_inbox_devices_unreachable`, `portal_inbox_oldest_pending_delivery_seconds`, `portal_inbox_storage_degraded` and `portal_inbox_replica_last_snapshot_age_seconds`.
