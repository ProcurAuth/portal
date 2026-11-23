# Security of the Inbox

The Inbox carries alerts that can be vital. This page gives the security model of the Inbox: what it protects, how, and what it does not cover.

To report a vulnerability, follow the [security policy](../../SECURITY.md). Use the private channel of the policy, not a public issue. The security model of the whole portal is in [Security model and hardening](../security.md).

## Design

- **Enrollment.** An account of the portal makes a one-time code. The code holds the address of the server, the public key of the server and a secret of 128 bits. The phone pins the key. It sends the secret inside a Noise IK message to this key. A network capture gives neither the secret nor material for an offline attack.
- **Code.** The secret works one time and expires (`inbox.enrollment.code_ttl`). The server keeps only its SHA-256 digest. The answer that shows the code has the header `Cache-Control: no-store`.

**Limits.** The server limits the failed attempts for each client address (`inbox.devices.max_auth_failures` in `inbox.devices.auth_failure_window`). An enrollment attempt and a session handshake that proves no phone count. A phone that proves its key always gets its session. The server closes an enrollment that sends no message in 10 seconds, and a session handshake after 15 seconds. One client address holds at most `inbox.devices.max_handshakes_per_address` handshakes.

One account has at most `inbox.devices.max_per_account` phones.

- **Sessions.** A session is Noise IK between the key of the phone and the pinned key of the server. Each phone can be revoked. A revoked phone gets the close code 4403.
- **Rights.** The permissions of the portal features (the service `portal`) decide who sends, who reads and who revokes. A phone subscribes only to the groups for which its account has `inbox.receive`.

**Alerts about a service.** An alert of the portal about a service (`inbox.watch`) follows the rule of the launcher. A reader sees it only when the reader may open the service: the superadmin, or an account with `access` on the service. The rule applies to the phones, the notifications, the page of the Inbox and the keys. A key reads such an alert only when its owner may open the service. The Inbox checks the rights of a phone again before it sends such an alert.

A phone of an account that lost the access never gets it.

**Keys.** A sender uses an `api_` key. A verifier checks the form of the text (prefix, length, characters, checksum) and the kind before any lookup. An `mcp_` key and a bad text get `401` with no detail. A key follows its owner and never has more rights than its owner. A key of the file is compared in constant time.

A key of the database needs `alert.send` on the group, for the key and for its owner. For an alert to an account, it needs `alert.send_user` on the service `portal`, for the key and for its owner.

- **Page.** The page needs a session of the portal. A form needs an anti-CSRF token and a same-origin check. A session that the superadmin opens as another account cannot send an alert and cannot revoke a phone.
- **Secrets.** The key of the server and the secrets of the `api_key` blocks come from `_file` or `_env`. A clear text secret in the file stops the start. The Inbox never writes a secret to the log.
- **Cluster.** The requests between instances (`inbox.cluster`) carry an HMAC that the server key derives. Only the pods of the service must reach the cluster port.

## Contents at rest

The portal seals the contents of the items before it writes them. An item is an approval or a question. The content is the command, the explanation, the hint, the turns and the comment of the owner. The portal writes only the sealed bytes. It opens them in memory to serve the owner.

**This is not end-to-end encryption.** The portal reads every content.

The seal uses AES-256-GCM. The keys come from the file (`inbox.at_rest`). Each sealed value names its key. The associated data holds the request id, the field, the sequence number and the effect hash. A sealed value that moves to another row, field or effect does not open.

The seal covers these cases:

- A leak of the database, a dump or a disk image.
- A backup, and a copy of the SQLite replica.
- A database administrator who has no key.
- An attacker who can write in the database. The attacker cannot read, forge or move a sealed value.

The seal does not cover these cases:

- A person who has the key and the process of the portal.
- A portal that is compromised while it runs.
- A compromised host of the service that asks for the approval.
- A stolen phone or browser of the owner.
- A key that leaves with the database. Keep the keys outside the data volume and outside the backups.
- The plain HTTP hop from the proxy to the portal. Use a private network or a network policy.

The metadata stays in clear: the request id, the service, the action, the resource path, the owner, the state and the dates. A lost key makes the contents unreadable and keeps the metadata. The check value that the database keeps for each key reveals nothing about the key.

The portal erases the sealed body of a final item after `inbox.items.body_purge_after`. The effect hash and the audit events stay. A body that the portal serves is checked first: the portal computes the effect hash of the opened command again. On a difference, it serves nothing and writes the log line `body_effect_mismatch`. At start, the portal stops when sealed rows name a key that the file lost. The command `portal inbox content-purge --config FILE --key ID` erases the contents of that key and keeps the metadata.

To rotate a key, add the new key, make it active, and run `portal inbox rekey --config FILE`. The command seals the old contents again with the active key and keeps the associated data. Remove the old key only when the command reports no row on it. See [Rotate a content key](configuration.md#rotate-a-content-key).

## The agent channel

The agent sends its explanation and its talk to the portal, and the service never sees them. The service learns the verdict only. The portal checks the key in process, and the endpoint `/mcp` needs no ForwardAuth.

- The endpoint checks the method, the `Origin`, the client address, the key, the rate and the size, in this order. A request that fails a check gets its answer before the portal reads more of it.
- A key reads and speaks only on its own items. Another item gives `not_found`, as an item that does not exist.
- The text of the person that the agent reads is in an `untrusted` object, after a data notice. The text can hold instructions, so the agent must read it as data. The portal replaces a control character with U+FFFD and never shows the approver or the device.
- An `mcp_` key does not open the REST routes, and an `api_` key does not open the endpoint.
- A bad key is counted for the client address. An address over the limit gets `429` before the portal checks the key. Each instance keeps its own counters.

## Several portals on one phone

The identity of a portal on the phone is local. The user chooses the name and the color when the phone adds the portal. The phone derives the fingerprint from the pinned key. The portal can set none of these. The portal sends a name of its own in `welcome`. The phone shows it only as "The portal calls itself", and only when it differs from the name of the user.


- **Look-alike.** The phone compares a new portal with the portals that it holds, by key, address and name. It folds letters that look alike: `1` and `l`, `0` and `o`, `rn` and `m`, and some Cyrillic and Greek letters. It decodes Punycode. A result of this check shows a banner and requires a tick.

- **Same key.** The phone refuses a code that has the key of a portal that it holds. It does not use the code. Two addresses of one portal are two enrollments of one key, so the phone keeps one address and lets the user change it.

- **Link.** A link from another app is not proof of its origin. The phone shows the referrer as a hint and requires a tick. A link can add a portal. It can never replace a row, a key or an address.

- **Key change.** A handshake that does not end with the pinned key, or the close code 4401, stops the connection. The phone shows "Key changed" and sends nothing. The user can enroll again in place, from the screen of that portal only. The phone keeps the name, the color and the history. The screen shows the old fingerprint, and then both fingerprints with a required tick.

- **Fingerprint.** The fingerprint is a cue and not the boundary. An attacker can force a short prefix in hours. The full 80 bits cost about 2^80 digests. The boundary is the name and the color that the attacker cannot choose, and the fact that the user added the portal.

- **Cap.** The phone holds at most 10 portals, one color for each.

## Limits of the model

- The portal knows nothing about TLS. Noise protects the phones. The bearer key of a sender travels in clear text on plain HTTP. Use TLS from the clients to the reverse proxy, or use a private network.
- A code is a bearer secret until its first use. Whoever reads it can enroll a phone for the account. Show it only to its owner.
- A key file of the server that leaks lets an attacker act as the server. Replace it and enroll every phone again.
- A session that the superadmin opens as another account can read the phones and make a code for that account. The phone then belongs to that account. This is a known property of **Log in as**.
- The Android app accepts clear text for each address. It has no network security configuration.
- Each instance has its own rate limits and its own counter of refused sign-ins.
- When the directory of the portal is not ready or is old, the portal cannot tell who may open a service. A new alert of the portal about a service then reaches only the phones of the superadmin. A delivery of such an alert that exists for another phone waits until the portal can tell the rights again.
- With PostgreSQL, a phone of a login that this instance does not know gets the close code 1013 and no session. Examples are an account of the file that left the file and an old login of the superadmin. The Inbox does not end the state of this login, because the file of another instance can still have it. The phone tries again until a person revokes it in the zone **Phones**. Revoke it when every instance has the new file.

## Operator checklist

- Keep a copy of the key file of the server and of the database. Protect both. Both are secrets.
- Keep the content keys (`inbox.at_rest`) outside the data volume and outside the backups of the database. Keep a copy of them in a separate place. A backup of the database with its keys shows every content.
- Send alerts with the header `Authorization: Bearer`. The server reads the key only in this header. A request with a key in the query and no header gets `401`. Do not put a key in a URL: query strings are often in logs.
- Use an HTTPS `inbox.devices.public_address`.
- List only the real proxies in `server.trusted_proxies`. Do not list the Docker gateway or a whole bridge range. The same list gives the client address for the limiter of the enrollment attempts.
- Send `/v1/`, `/api/v1/` and `/api/version` to the portal without ForwardAuth. A phone and a webhook have no session. Add a retry middleware that retries only when the server does not answer.
- Keep `inbox.cluster.listen` on the private network of the pods.
- Do not publish the port of the portal to the internet without a proxy that you control. Let the proxy send only `/v1/`, `/api/v1/` and `/api/version` to the internet, and keep the pages behind ForwardAuth. A phone session uses a budget of connections that is separate from the pages.
- Give each sender its own `api_` key with the smallest set of groups. Revoke a key that leaks.
- Give `access` on a service to each member of `inbox.watch.group` that must get the alerts about this service.
- Alert on `portal_inbox_devices_unreachable`, `portal_inbox_oldest_pending_delivery_seconds`, `portal_inbox_storage_degraded` and `portal_inbox_replica_last_snapshot_age_seconds`.
