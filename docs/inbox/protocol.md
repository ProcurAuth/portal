# Device protocol (version 4)

The code is the reference: [`crates/inbox-core/src/proto.rs`](../../crates/inbox-core/src/proto.rs),
[`enroll.rs`](../../crates/inbox-core/src/enroll.rs) and [`noise.rs`](../../crates/inbox-core/src/noise.rs).
The same crate runs on the server and in the Android app (JNI). The cryptography exists only once.

All messages are JSON. Binary fields are standard base64 with padding.
This document describes protocol version 4. An app that speaks another version
gets close code 4426 and must be updated.

Protocol 4 replaces the shared device password and OPAQUE with a one-time enrollment
code. An account of the portal makes the code. The login of the account is the
user name of the device. The device cannot change it.

## Keys

| Key | Where | Created by |
|---|---|---|
| Server Noise static key (X25519) | server key file | the server, at first start or by a command |
| Device Noise static key (X25519) | phone only | the phone, at enrollment |
| Enrollment secret (16 random bytes) | enrollment code; the server keeps only a digest | the server, for one account |

The server key file holds one line: the base64 of the 32 bytes of the private key.
The public key is derived. If the file is replaced, every device and every code is invalid.

## Enrollment code

The code is one text. It is also a deep link that the app opens.

```text
inbox://enroll?s=<address>&k=<server key>&c=<secret>
```

| Key | Value |
|---|---|
| `s` | Server address, percent-encoded (UTF-8). Same rules as the app: `host:port`, a domain, or an http(s) URL. 1 to 512 bytes, no white space, no control character. |
| `k` | Public key of the server. Base64url, no padding, 32 bytes. |
| `c` | Secret. Base64url, no padding, 16 random bytes. |

- The scheme and the host are lower case. Each of `s`, `k` and `c` appears exactly once.
- An unknown key, a duplicate key, a fragment, a wrong length or a bad encoding is an error.
- White space around the whole text is ignored.
- The key `k` is the pin. The app trusts no other key for this server.
- The app shows the address and the fingerprint of `k` before it connects. The fingerprint is
  the first 8 hexadecimal characters of the SHA-256 digest of the 32 bytes of `k`.
  The person can compare it with the value of the administrator.
- The secret never leaves the core. Kotlin code does not see it.

## Enrollment: `GET /v1/enroll` (WebSocket, binary frames)

`Noise_IK_25519_ChaChaPoly_BLAKE2s`, prologue `inbox/v1/enroll`. Each binary frame
carries one Noise message (65535 bytes at most). The device static key is new. The
phone makes it for this enrollment. The remote static key is the key `k` of the code.

```text
device                                           server
  Noise message 1
   {"protocol":4,"secret":b64,
    "device_name":s,"app_version":s}          ->   finds the code, stores the device
                                              <-  Noise message 2
                                                   {"protocol":4,"device_id":hex32,
                                                    "server_name":s,"user_name":s}
```

1. Device to server, Noise message 1. The `secret` is the standard base64 (with padding)
   of the 16 bytes. `device_name` has 1 to 64 bytes and no control character.
   `app_version` has 1 to 64 bytes and no control character.
2. Server to device, Noise message 2. `device_id` is 32 lowercase hexadecimal characters.
   `server_name` and `user_name` are not empty. `user_name` is the login of the account.
3. The device is enrolled only after it reads message 2. Then it stores the address,
   the pinned key, the device id, the device key pair and the user name.
4. The server stores the device key and links it to the account before it sends message 2.
   If message 2 does not arrive, the code is used and the phone needs a new code.

**Refusal.** On refusal the server sends no message 2. It closes the WebSocket with a close code.

| Close code | Cause | Text in the app |
|---|---|---|
| 4410 `ENROLL_REFUSED` | The code is unknown, already used or expired. The account is revoked. Message 1 cannot be read or is not valid. | "The code is not valid. Ask for a new one." |
| 4426 `PROTOCOL` | Message 1 is valid but `protocol` is not 4. The first frame is a text frame. | "Update the app." |
| 4429 `RATE_LIMITED` | Too many attempts from this address. | "Too many attempts." |

The server uses the same code 4410 for all causes of refusal. Then an observer cannot tell
a used code from an unknown code. An HTTP answer 429 to the upgrade request has the
same meaning as 4429.

Security properties:
- Only the holder of the server key can read message 1. The secret is safe from a passive observer.
- Only the holder of the server key can make a valid message 2 for this handshake.
  A message 2 from an earlier handshake, from another key or after a change is refused.
- A replay of message 1 is not useful. The secret works one time.
- The code is a bearer secret until the first use. Show it only to its owner.

## Session: `GET /v1/session` (WebSocket, binary frames)

`Noise_IK_25519_ChaChaPoly_BLAKE2s`, prologue `inbox/v1/session`. Each WebSocket
binary frame carries exactly one Noise message (at most 65535 bytes).

1. Device to server, Noise message 1 that holds
   `{"protocol":4,"device_id":hex32,"app_version":s}`.
2. Server to device, Noise message 2 that holds `{"protocol":4,"server_name":s}`.
   The device rejects any key other than the pinned server key.
3. Device to server, first transport frame `{"t":"ready"}`. Only after this does
   the server count the session as established, so a replayed message 1 cannot
   displace a live session.
4. Server to device: `welcome`, then alerts.

### Device to server

| `t` | Fields | Meaning |
|---|---|---|
| `ready` | | First transport frame. |
| `ack` | `alert_id`, `stage` (`received`) | Automatic delivery receipt, sent once the alert is stored durably. Alerts are one-way: there is no human acknowledgement. Idempotent. |
| `subscribe` | `group` | Subscribes this device to the group. How the device reacts (alarm, ring, notification) is a local setting of the device: the server never knows it. |
| `unsubscribe` | `group` | |
| `ping` | `nonce` (u64) | Heartbeat; the server answers `pong`. |

The messages `set_user_name` and `user_name` do not exist in protocol 4. The server refuses an unknown message.

### Server to device

| `t` | Fields |
|---|---|
| `welcome` | `server_name`, `groups[] {id,name,description}` (only the groups that the account may subscribe to), `subscriptions[]` (group ids), `heartbeat_interval_ms`, `offline_warning_after_ms`, `server_time_ms`, `user_name` (login of the account, read-only) |
| `alert` | `alert_id` (hex, 128 random bits), `group`, `group_name`, `title` (one line, at most 128 characters), `created_at_ms` |
| `subscriptions` | `subscriptions[]`, sent after each change. The server also sends it when the account loses the permission for a group: the server removes the subscription, and the message tells the phone. |
| `pong` | `nonce` |
| `error` | `code`, `message`. The codes are `unknown_group` (the group does not exist), `forbidden_group` (the account has no permission `group.subscribe` on the group, `message` is the group), and `unavailable` (the storage or the rights are not ready: send the message again later). |

### Rights of the account

A phone subscribes only to a group for which its account has the permission `group.subscribe` of the service `inbox` of the portal. This adds no message and no version: protocol 4 stays as it is.

- The `welcome` lists only the groups that the account may subscribe to.
- `subscribe` to another group gets the error `forbidden_group`. When the portal cannot tell the rights (its directory is not ready or is old), the error is `unavailable`, and the phone can send the message again.
- When the account loses the permission, the server removes the subscription and sends `subscriptions`. This check runs when the directory of the portal changes, and at each `welcome`. It does not run for each alert. The next alerts of the group are not made for the phone.
- The server never removes a subscription when it does not know. A directory that is not ready, a directory that is old, an empty directory and an account that the directory does not know remove nothing.
- An alert that exists is always delivered. A phone that loses the permission still gets the alerts that the server made before. They stay in its backlog until the phone confirms them or they expire.

### Delivery rules

- **Fan-out at acceptance.** When an alert is accepted, one delivery row is created
  for every device subscribed to the group at that instant.
- **Retransmission.** The server sends rows whose receipt has not arrived yet,
  oldest first, with at most `max_inflight` rows in flight. A row that stays
  without receipt for `resend_after` is sent again, and the whole backlog is resent
  after every reconnection.
- **Order on the device.** The app stores the alert durably *before* it sends
  `ack received`, and deduplicates on `alert_id`. Notifications use an identifier
  derived from `alert_id`, so a retransmission never shows twice.
- **Expiry.** A delivery not received within `storage.delivery_ttl` is no longer
  sent and is reported as expired. A revoked device's rows are abandoned.

### Close codes

| Code | Meaning | App behaviour |
|---|---|---|
| 1012 | The server restarts | Reconnect immediately. |
| 1001 | Session timeout | Reconnect. |
| 4401 | Unknown device or key mismatch | Enroll again. |
| 4403 | Device revoked | Enroll again. |
| 4409 | Replaced by a newer session of the same device | Do not fight it. |
| 4410 | Enrollment refused (only on `/v1/enroll`) | Ask for a new code. |
| 4426 | Unsupported protocol | Update the app. |
| 4429 | Rate limited | Back off. |

## Decisions

These choices fill the gaps of the specification. Change them only with a new protocol version.

1. **Query form.** The link is `inbox://enroll?s=..&k=..&c=..`. The earlier design note
   used `inbox://enroll#<code>`. The query form is the contract.
2. **Strict code.** The parser is strict. The scheme and the key names are case sensitive.
   Base64url padding is an error. Bits at the end that are not zero are an error.
3. **Order of keys.** The order of `s`, `k` and `c` in the query is free.
4. **Fingerprint.** The fingerprint is the first 8 hexadecimal characters (lower case) of
   SHA-256 over the 32 bytes of the server key. The core computes it.
5. **No reason on refusal.** The server sends 4410 for every refusal that concerns the code
   or message 1. It sends 4426 only when message 1 is valid and the version is wrong,
   or when the first frame is a text frame.
6. **Lifetime of a code.** The server decides. The merge design says one use and 10 minutes.
   The server compares the digest of the secret in constant time.
7. **Server key file.** One line, base64 of the 32 private bytes. No version byte.
   The old file format with an OPAQUE setup is not read.
8. **Device name.** The app trims it and shows the phone model as default. The server
   checks 1 to 64 bytes and no control character, and does not trim.
9. **Login is read-only.** The phone stores `user_name` from message 2 and updates it
   at each `welcome`. The phone has no setting for it.
10. **Old servers in the app.** Database version 6 removes every stored server of protocol 3
    with its groups and alerts. The app tells the person once: "Enroll again".
11. **Timeouts of the app.** It waits 20 seconds for the WebSocket to open and 90 seconds
    for message 2. The server SHOULD close an enrollment connection that sends no message 1
    within 10 seconds.
12. **One attempt.** An enrollment connection carries one handshake. The server closes it
    after message 2 or after a refusal. The phone closes it with code 1000.

## Admin API (`server.admin_listen`)

The admin front and its API have **no password**, by design: network placement,
or an authenticating reverse proxy, is the access control. The server still
protects a front bound to localhost or a private network against hostile web
pages opened in the operator's browser:

| Check | Refused with | Protects against |
|---|---|---|
| `Host` must be one of `admin.allowed_hosts` (exact match) | 403 | DNS rebinding |
| `POST` requires `Content-Type: application/json` and `X-Inbox-Request: 1` | 403 | Cross-site request forgery: a cross-origin page cannot set these without a CORS preflight, which Inbox never grants |
| `Sec-Fetch-Site: cross-site` | 403 | Cross-site requests from browsers that send fetch metadata |

Endpoints: `GET /api/admin/overview`, `GET /api/admin/alerts?limit=`,
`GET /api/admin/alerts/{id}`, `GET /api/admin/connections?limit=&user=<login>`,
`POST /api/admin/alerts` (`{group, title, idempotency_key}`) and
`POST /api/admin/devices/{id}/revoke` (`{}`).
