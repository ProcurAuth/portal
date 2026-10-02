# Device protocol (version 1)

The code is the reference: [`crates/inbox-core/src/proto.rs`](../crates/inbox-core/src/proto.rs),
[`enroll.rs`](../crates/inbox-core/src/enroll.rs) and [`noise.rs`](../crates/inbox-core/src/noise.rs).
The same crate runs on the server and in the Android app (JNI), so there is a
single implementation of the cryptography.

All messages are JSON. Binary fields are standard base64 with padding.
This document describes protocol version 2 (Inbox 0.6). Version 1 apps are refused
with close code 4426 and must be updated.

## Keys

| Key | Where | Created by |
|---|---|---|
| OPAQUE server setup | server key file | `inbox keygen` |
| Server Noise static key (X25519) | server key file | `inbox keygen` |
| Device credential record | server, `devices.credential_file` | `inbox credential` (offline) |
| Device Noise static key (X25519) | phone only | generated during enrollment |

OPAQUE uses ristretto255, 3DH with SHA-512 and Argon2id, with a fixed credential
identifier and context for device enrollment. A wrong password is detected by the
phone, which then never sends `finish`; the server therefore counts every started
enrollment as a failed attempt until it succeeds (`security.max_auth_failures`).

## Enrollment: `GET /v1/enroll` (WebSocket, text frames)

```text
device                                              server
  {"type":"start","protocol":2,
   "credential_request":b64}                   ->
                                               <-  {"type":"challenge","credential_response":b64}
  {"type":"finish","credential_finalization":b64,
   "device_name":s,"user_name":s?,"device_public_key":b64,
   "mac":b64}                                  ->   verifies OPAQUE, then mac
                                               <-  {"type":"accepted","device_id":hex32,
                                                    "server_name":s,"server_public_key":b64,"mac":b64}
```

- `K = HKDF-SHA256(salt="inbox/v1", ikm=OPAQUE session key, info=label)`.
- Client MAC: `HMAC-SHA256(K("enroll/client-mac"), device_name ‖ device_public_key [‖ "user" ‖ user_name])`.
  `user_name` is the pseudonym of the person using the device (1..64 bytes, no
  control characters). It is optional for apps 0.1 / 0.2, which omit it.
- Server MAC: `HMAC-SHA256(K("enroll/server-mac"), device_id ‖ server_name ‖ server_public_key ‖ device_public_key)`.
- Every MAC input part is prefixed by its length as a big-endian u64.
- On failure the server sends `{"type":"rejected","code":c,"message":m}`. Codes are
  `bad_password`, `rate_limited`, `protocol`, `too_many_devices` and `internal`.

A passive observer learns nothing. An active attacker who does not hold the
credential record cannot complete OPAQUE. It therefore cannot substitute its
own server key, and it cannot register a device.

## Session: `GET /v1/session` (WebSocket, binary frames)

`Noise_IK_25519_ChaChaPoly_BLAKE2s`, prologue `inbox/v1/session`. Each WebSocket
binary frame carries exactly one Noise message (at most 65535 bytes).

1. Device to server, Noise message 1, carrying
   `{"protocol":2,"device_id":hex32,"app_version":s}`.
2. Server to device, Noise message 2, carrying `{"protocol":2,"server_name":s}`.
   The device rejects any key other than the pinned server key.
3. Device to server, first transport frame `{"t":"ready"}`. Only after this does
   the server count the session as established, so a replayed message 1 cannot
   displace a live session.
4. Server to device: `welcome`, then alerts.

### Device to server

| `t` | Fields | Meaning |
|---|---|---|
| `ready` | | First transport frame. |
| `ack` | `alert_id`, `stage` (`received`) | Automatic delivery receipt, sent once the alert is stored durably. Alerts are one-way: there is no human acknowledgement. Idempotent. Legacy apps (0.1, 0.2) may send `displayed` / `acknowledged`: the server treats them as `received`. |
| `subscribe` | `group` | Subscribes this device to the group. How the device reacts (alarm, ring, notification) is a local setting of the device: the server never knows it. |
| `unsubscribe` | `group` | |
| `set_user_name` | `user_name` | Changes the pseudonym of this device (per server, any time). The server answers `user_name`, or `error` `invalid_user_name`. |
| `ping` | `nonce` (u64) | Heartbeat; the server answers `pong`. |

### Server to device

| `t` | Fields |
|---|---|
| `welcome` | `server_name`, `groups[] {id,name,description}`, `subscriptions[]` (group ids), `heartbeat_interval_ms`, `offline_warning_after_ms`, `server_time_ms`, `user_name` (recorded pseudonym or null) |
| `user_name` | `user_name` | Confirms `set_user_name`. |
| `alert` | `alert_id` (hex, 128 random bits), `group`, `group_name`, `description?` (at most 128 characters), `details?` (Markdown), `created_at_ms` |
| `subscriptions` | `subscriptions[]`, sent after each change |
| `pong` | `nonce` |
| `error` | `code`, `message` (for example `unknown_group`) |

### Delivery rules

- **Fan-out at acceptance.** When an alert is accepted, one delivery row is created
  for every device subscribed to the group at that instant.
- **Retransmission.** The server sends rows whose receipt has not arrived yet,
  oldest first, with at most `max_inflight` outstanding. A row that stays
  without receipt for `resend_after` is sent again, and the whole backlog is resent
  after every reconnection.
- **Order on the device.** The app stores the alert durably *before* sending
  `ack received`, and deduplicates on `alert_id`. Notifications use an identifier
  derived from `alert_id`, so a retransmission never shows twice.
- **Expiry.** A delivery not received within `storage.delivery_ttl` is no longer
  sent and is reported as expired. A revoked device's rows are abandoned.

### Close codes

| Code | Meaning | App behaviour |
|---|---|---|
| 1012 | Server restarting | Reconnect immediately. |
| 1001 | Session timeout | Reconnect. |
| 4401 | Unknown device or key mismatch | Enroll again. |
| 4403 | Device revoked | Enroll again. |
| 4409 | Replaced by a newer session of the same device | Do not fight it. |
| 4426 | Unsupported protocol | Update the app. |
| 4429 | Rate limited | Back off. |

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
`GET /api/admin/alerts/{id}`, `GET /api/admin/connections?limit=&user=<pseudonym>`,
`POST /api/admin/alerts` (`{group, description?, details?, idempotency_key}`) and
`POST /api/admin/devices/{id}/revoke` (`{}`).
