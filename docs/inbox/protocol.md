# Device protocol (version 5)

The code is the reference: [`crates/inbox-core/src/proto.rs`](../../crates/inbox-core/src/proto.rs),
[`enroll.rs`](../../crates/inbox-core/src/enroll.rs) and [`noise.rs`](../../crates/inbox-core/src/noise.rs).
The same crate runs on the server and in the Android app (JNI). The cryptography exists only once.

All messages are JSON. Binary fields are standard base64 with pad characters.
This document describes protocol version 5. An app that speaks another version
gets close code 4426 and must be updated. Protocol 5 has no compatibility with protocol 4.

Protocol 4 replaced the shared device password and OPAQUE with a one-time enrollment
code. An account of the portal makes the code. The login of the account is the
user name of the device. The device cannot change it.

Protocol 5 adds items that the owner answers: questions and approvals. It adds the effect hash,
the thread of an item and the preferences of the account. It adds no cryptography.

## Keys

| Key | Where | Created by |
|---|---|---|
| Server Noise static key (X25519) | server key file | the command `portal inbox keygen` |
| Device Noise static key (X25519) | phone only | the phone, at enrollment |
| Enrollment secret (16 random bytes) | enrollment code; the server keeps only a digest | the server, for one account |

The server key file holds one line: the base64 of the 32 bytes of the private key.
The public key is derived. If the file is replaced, every device and every code is invalid.

## Enrollment code

The code is one text. It is also a deep link that the app opens.

```text
portal-inbox://enroll?s=<address>&k=<server key>&c=<secret>
```

| Key | Value |
|---|---|
| `s` | Public base URL of the portal, percent-encoded (UTF-8). Same rules as the app: `host:port`, a domain, or an http(s) URL with an optional path prefix. The app adds `/v1/enroll` and `/v1/session`. 1 to 512 bytes, no white space, no control character. |
| `k` | Public key of the server. Base64url, no pad characters, 32 bytes. |
| `c` | Secret. Base64url, no pad characters, 16 random bytes. |

- The scheme and the host are lower case. Each of `s`, `k` and `c` appears exactly once.
- An unknown key, a duplicate key, a fragment, a wrong length or a bad base64 form is an error.
- White space around the whole text is ignored.
- The key `k` is the pin. The app trusts no other key for this server.
- The app shows the address and the fingerprint of `k` before it connects. The fingerprint is
  the first 20 hexadecimal characters (80 bits) of the SHA-256 digest of the 32 bytes of `k`,
  in five groups of 4 characters. The person can compare it with the value of the administrator.
- The secret never leaves the core. Kotlin code does not see it.

## Enrollment: `GET /v1/enroll` (WebSocket, binary frames)

`Noise_IK_25519_ChaChaPoly_BLAKE2s`, prologue `portal-inbox/v1/enroll`.
Each binary frame carries one Noise message (65535 bytes at most).
The device static key is new.
The phone makes it for this enrollment.
The remote static key is the key `k` of the code.

```text
device                                           server
  Noise message 1
   {"protocol":5,"secret":b64,
    "device_name":s,"app_version":s}          ->   finds the code, stores the device
                                              <-  Noise message 2
                                                   {"protocol":5,"device_id":hex32,
                                                    "server_name":s,"user_name":s}
```

1. Device to server, Noise message 1. The `secret` is the standard base64 (with pad characters)
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
| 4410 `ENROLL_REFUSED` | The code is unknown, already used or expired. Message 1 cannot be read or is not valid. The limit `inbox.devices.max_devices` or `inbox.devices.max_per_account` is reached. | "The code is not valid. Ask for a new one." |
| 4426 `PROTOCOL` | Message 1 is valid but `protocol` is not 5. The first frame is a text frame. | "Update the app." |
| 4429 `RATE_LIMITED` | Too many attempts from this address. | "Too many attempts." |

The server uses the same code 4410 for all causes of refusal.
Then an observer cannot tell a used code from an unknown code.
The log of the server has the real reason (event `enroll_refused`).
The server closes with 4429 after the upgrade.
It does not answer the HTTP status 429 to the upgrade request.

A storage error closes with 1011, and the code stays unused.
The server SHOULD close a connection that sends no message within 10 seconds.

Security properties:
- Only the holder of the server key can read message 1. The secret is safe from a passive observer.
- Only the holder of the server key can make a valid message 2 for this handshake.
  A message 2 from an earlier handshake, from another key or after a change is refused.
- A replay of message 1 is not useful. The secret works one time.
- The code is a bearer secret until the first use. Show it only to its owner.

## Session: `GET /v1/session` (WebSocket, binary frames)

`Noise_IK_25519_ChaChaPoly_BLAKE2s`, prologue `portal-inbox/v1/session`. Each WebSocket
binary frame carries exactly one Noise message (at most 65535 bytes).

1. Device to server, Noise message 1 that holds
   `{"protocol":5,"device_id":hex32,"app_version":s}`.
2. Server to device, Noise message 2 that holds `{"protocol":5,"server_name":s}`.
   The device rejects any key other than the pinned server key.
3. Device to server, first transport frame `{"t":"ready"}`. Only after this does
   the server count the session as established, so a replayed message 1 cannot
   displace a live session.
4. Server to device: `welcome`, then the frames of the sections below.

The server closes a session that does not send `ready` within 15 seconds of the upgrade, with the code 1001.
A ping or a pong does not extend this time.
A handshake that proves no enrolled device counts as a failure of the client address.
When the address has too many failures, such a handshake gets 4429.
A device that proves its key always gets its session.

A client address has at most `inbox.devices.max_handshakes_per_address` handshakes on both routes.
A new handshake of a full address ends the oldest handshake of this address with the code 1013.

## Rules of all frames

Each frame is one JSON object in one Noise message. The member `t` names the frame.
The plain text of a frame has at most 65519 bytes (`MAX_PLAINTEXT`).

- **Closed schema.** A parser refuses an unknown frame, an unknown member and a duplicate member.
  A frame that fails a check closes the session with an error, never a repair.
- **No signature.** No frame has a signature. The proof of an answer is the authenticated session
  and the effect hash in `shown_effect`.
- **No new cryptography.** The text of a frame is clear text inside the Noise session.
  No frame goes to a service. The service of an approval never speaks protocol 5.
- **Ids.** An id of an item, of a thread, of a delivery or of a device is 32 lowercase hexadecimal characters.
- **Times.** A time is an integer of Unix milliseconds. It is 0 or more.
- **Texts.** A limit in characters counts Unicode scalar values. A limit in bytes counts UTF-8 bytes.
  A line has no control character. The core crate gives `decode` and `encode` for each direction.
  Each one applies all the rules of this document.
- **Size.** The sender checks the size of the encoded frame. JSON escapes make a control character 6 bytes long.
  The server refuses an item at creation when its frame is too large.
- **Optional members.** An optional member is absent when it has no value. A parser also accepts `null`.

### Device to server

| `t` | Members | Use |
|---|---|---|
| `ready` | none | First transport frame. |
| `ack` | `id`, `stage` (`received`) | Delivery receipt. See "Receipts". |
| `subscribe` | `group` | Subscribes this device to the group. |
| `unsubscribe` | `group` | Removes the subscription. |
| `ping` | `nonce` (u64) | Heartbeat. The server answers `pong`. |
| `answer` | see "answer" | The only answer of an item. |
| `reply` | `request_id`, `client_msg_id`, `text` | A question of the owner to the agent. |
| `refresh` | `request_id` | Asks for the current state of one item. |
| `sync` | `open[]` of `{request_id, seq}` | Tells the items that the device holds open. |
| `set_prefs` | `quiet?`, `digest?` | Replaces the preferences of the account. |

### Server to device

| `t` | Members | Use |
|---|---|---|
| `welcome` | see "welcome" | First frame of a session. |
| `alert` | see "alert" | A one-way message. |
| `request` | see "request" | An item that waits for the owner. |
| `message` | see "message" | A turn in the thread of an item. |
| `settled` | see "settled" | An item is final. |
| `digest` | see "digest" | The count and the age of the open items. |
| `answer_result` | see "answer_result" | The result of an `answer` or a `reply`. |
| `subscriptions` | `subscriptions[]` | The groups of the device, after each change. |
| `pong` | `nonce` | Answer to `ping`. |
| `error` | `code`, `message` | A refusal. |

The messages `set_user_name` and `user_name` do not exist.
The server refuses an unknown message.
The codes of `error` are `unknown_group`, `forbidden_group`, `unavailable` and `not_enabled`.
`unknown_group`: the group does not exist.
`forbidden_group`: the account has no permission `inbox.receive` on the group.

`unavailable`: the storage or the rights are not ready, so send the message again later.
`not_enabled`: the server does not handle this frame.
A server without the blocks `inbox.items` and `inbox.at_rest` answers each of `answer`, `reply`, `refresh`, `sync` and `set_prefs` so.

## Receipts

The server sends `alert`, `request`, `message`, `settled` and `digest` from one queue.
Each row of the queue has an id of 32 hexadecimal characters. The receipt `ack` names this id.

| Frame | Member that holds the id of the delivery |
|---|---|
| `alert` | `alert_id` |
| `request`, `message`, `settled`, `digest` | `id` |

The device stores the frame durably before it sends `ack`. It deduplicates on the id of the delivery.
The server sends a row again until the receipt arrives. A receipt is idempotent.

Each device of an owner gets the same id for the same message, and each device has its own receipt.
The frames that answer `refresh` and `sync` have a new id that is in no queue. The device stores them as any frame.
The server ignores a receipt for an id that it does not know.
The field `alert_id` of `ack` in protocol 4 is now `id`. The old name is not valid.

## Frames of the server

### `welcome`

```json
{"t":"welcome","server_name":"Prod","groups":[{"id":"infra","name":"Infra","description":"Servers"}],
 "subscriptions":["infra"],"heartbeat_interval_ms":15000,"offline_warning_after_ms":60000,
 "server_time_ms":1788000000000,"user_name":"alice",
 "prefs":{"quiet":{"from":"22:00","to":"07:00","tz":"Europe/Paris","high_rings":true},
          "digest":{"mode":"daily","at":"09:00","tz":"Europe/Paris"}},
 "body_purge_after_ms":2592000000}
```

| Member | Type | Rule |
|---|---|---|
| `server_name` | string | Not empty. |
| `groups[]` | `{id, name, description}` | Only the groups that the account may subscribe to. |
| `subscriptions[]` | string ids | Groups of this device. |
| `heartbeat_interval_ms`, `offline_warning_after_ms` | u64 | |
| `server_time_ms` | i64 | |
| `user_name` | string | Login of the account. Read-only. |
| `prefs` | object | The preferences of the account. `{}` when the account has none. See "Preferences". |
| `body_purge_after_ms` | u64 | Age at which the server erases the body of a final item. 0: the server has no items. |

The server reads `prefs` in the table of the account at each session. A phone that sent `set_prefs` gets its values back here.

### `alert`

```json
{"t":"alert","alert_id":"c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0","kind":"alert","group":"infra","group_name":"Infra",
 "title":"Disk full on a node","created_at_ms":1788000000000}
```

| Member | Type | Rule |
|---|---|---|
| `alert_id` | id | Id of the delivery. |
| `kind` | string | Always `alert`. A parser refuses any other value in this frame. |
| `group`, `group_name` | string | An alert to one account has `@direct` and `Direct`. |
| `title` | string | One line, 1 to 128 characters, no control character. |
| `created_at_ms` | time | |

### `request`

A `request` is an item that waits for the owner. The same `request_id` can come again with a higher `seq`.
The frame is then the new state of the item, for example after a revival. The device keeps one item for each `request_id`.
A frame has the members of its kind only. The kinds of this frame are `question` and `approval`.

Members of both kinds:

| Member | Type | Rule |
|---|---|---|
| `id` | id | Id of the delivery. |
| `request_id` | id | Id of the item. |
| `kind` | string | `question` or `approval`. |
| `chain_id` | id | Id of the thread. The first item of a thread has `chain_id` equal to `request_id`. |
| `seq` | u64 | Number of the last turn of the thread. 0 when there is no turn. |
| `from` | object | `{label, key_name, owner, via, interaction}`. See below. |
| `urgency` | string | `low`, `normal` or `high`. |
| `quiet` | bool, optional | True when the item came inside the quiet hours. The server never holds an item back. |
| `created_at_ms`, `chain_created_at_ms`, `agent_seen_at_ms` | time | |
| `lease_mode` | string | `lease` or `until`. |
| `lease_until_ms` | time | With `lease` only. |
| `until_at_ms` | time | With `until` only. |
| `allow_custom` | bool | A question: a text can be the answer. An approval: a text can come with a decline. |
| `allow_ask_back` | bool | The owner can ask the agent a question with `reply`. |
| `supersedes`, `previous_id` | id, optional | The item that this one replaces. The item that this one follows. |

`from` holds `label` (1 to 64 characters, one line, computed by the portal), `key_name` (1 to 64 characters),
`owner` (login, 1 to 64 characters), `via` (`mcp` or `api`) and `interaction` (`conversational` or `one_shot`).
An item of a `one_shot` key has `allow_custom` and `allow_ask_back` false.

The portal writes `label` and `key_name` from the name that the owner gave to the key. A label that the agent gives never enters them.
A key that the portal does not find has the name `key` and the first 8 characters of its id.

Members of a question:

| Member | Type | Rule |
|---|---|---|
| `title` | string | One line, 1 to 128 characters, no control character, no bidi control, no line separator. |
| `explanation` | string, optional | Text of the agent, 1 to 8000 characters. |
| `command` | `{text, label?}`, optional | `text` has 1 to 32768 bytes. `label` is one line of 1 to 32 characters. |
| `input` | object | See below. |
| `hint` | object, optional | `{suggested, confidence?, source}`. `suggested` and `source` are lines of 1 to 32 characters. `confidence` is from 0 to 1. |
| `as_of_ms` | time, optional | The time of the data in the question. |

`input` holds `type` (`choice`, `confirm`, `text` or `none`), `multiple` (bool), `min` and `max` (0 to 255), `options[]` and `placeholder?`.
A `choice` has 2 to 8 options.
Each option is `{id, label, description?}`.
`id` has 1 to 32 characters of `a-z 0-9 _ -` and is unique.
`label` has 1 to 48 characters.

`description` has 1 to 160 characters.
For a `choice`:
`1 <= min <= max <= number of options`.
Without `multiple`, `min` and `max` are 1.
The other types have no option, `multiple` false and `min` and `max` 0.

Only `text` can have a `placeholder` of 1 to 128 characters.
A `text` question has `allow_custom` false.
A `none` question has both flags false.

Members of an approval: the member `approval` only.

| Member of `approval` | Type | Rule |
|---|---|---|
| `code` | string | 4 to 8 characters of `A-Z 0-9`. |
| `service` | string | One line, 1 to 64 characters. |
| `action` | string | One line, 1 to 128 characters. |
| `resource` | string | 0 to 512 characters, no control character. Empty: the service itself. |
| `subject` | string | One line, 1 to 128 characters. |
| `effect` | string | `sha256:` and 64 lowercase hexadecimal digits. See "Effect". |
| `op` | object | The operation. Strict rules of the effect. `op` and `view` together have at most 32768 bytes. |
| `view` | object | `{title, fields[], body, body_label?}`. See "Effect". |
| `explanation` | string, optional | Text of the agent, not verified. 1 to 8000 characters. It is not part of the effect. |
| `hint` | object, optional | As for a question. |
| `previous` | `{approved_at_ms, consumed}`, optional | The same service, subject, action, resource and effect were approved earlier. `consumed` tells whether the service used it. |
| `follows` | `{code, intent}`, optional | The item follows an earlier item that the owner declined with a text. The server reads it from `previous_id`. |

The command of an approval is the value that `view.body` points to in `op`. It is not in the frame a second time.
The device computes the effect again from `service`, `action`, `resource` and `op`.
It shows "Invalid request" when the result differs from `effect`, or when a pointer of `view` names no value.

```json
{"t":"request","id":"aaaa...","request_id":"bbbb...","kind":"approval","chain_id":"bbbb...","seq":0,
 "from":{"label":"claude-laptop","key_name":"claude-laptop","owner":"alice","via":"mcp","interaction":"conversational"},
 "urgency":"high","created_at_ms":1788000000000,"chain_created_at_ms":1788000000000,"agent_seen_at_ms":1788000001000,
 "lease_mode":"lease","lease_until_ms":1788000180000,"allow_custom":true,"allow_ask_back":true,
 "approval":{"code":"K7QF","service":"datastore","action":"data.write",
  "resource":"host:prod/database:analytics","subject":"key:claude-laptop","effect":"sha256:...",
  "op":{"tool":"run_write_query","host":"prod","sql":"INSERT INTO analytics.events SELECT 1"},
  "view":{"title":"ClickHouse write","fields":[["Host","/host"]],"body":"/sql","body_label":"SQL"},
  "explanation":"The migration needs this row."}}
```

The complete frames are in `crates/inbox-core/tests/fixtures/frames/`.

### `message`

A turn in the thread of an item.

| Member | Type | Rule |
|---|---|---|
| `id` | id | Id of the delivery. |
| `request_id` | id | |
| `seq` | u64 | Number of the turn, 1 or more. |
| `by` | string | `agent` or `user`. |
| `created_at_ms` | time | |
| `explanation` | string, optional | With `agent`. 1 to 8000 characters. |
| `command` | `{text, label?}`, optional | With `agent`. As in a question. |
| `text` | string | With `user` only. 1 to 8000 characters. |

An agent turn has `explanation`, `command` or both. A user turn has `text` and nothing else.

### `settled`

An item is final. The device removes it from the list "to answer" and withdraws its notification.

| Member | Type | Rule |
|---|---|---|
| `id` | id | Id of the delivery. |
| `request_id` | id | |
| `state` | string | `answered`, `abandoned`, `expired`, `cancelled` or `superseded`. |
| `reason` | string | 1 to 32 characters of `a-z 0-9 _`. See below. |
| `by` | `{via, device_name?}`, optional | `via` is `phone` or `web`. `device_name` is a line of 1 to 64 characters, with `phone` only. |
| `superseded_by` | id, optional | With `superseded` only. |
| `decision` | string, optional | `approve` or `deny`. With `answered` only, for an approval. |
| `at_ms` | time | |

The reasons that the server sends now are `answered`, `lease_lapsed`, `until_reached`, `max_lifetime`,
`agent_cancelled`, `key_revoked`, `owner_deleted`, `admin_cancelled`, `superseded` and `item_gone`.
`item_gone` answers a `refresh` or a `sync` for an item that the server does not hold any more. The item is `cancelled`.

The phone that answered gets `answer_result`, not `settled`. The server sends `settled` to the other phones of the owner.
A later version can add a reason. A device shows a reason that it does not know as it is.

### `digest`

The open items of the owner, as numbers. It has no body. The server sends no empty digest.

| Member | Type | Rule |
|---|---|---|
| `id` | id | Id of the delivery. |
| `count` | u32 | Number of open items, 1 or more. |
| `oldest_created_at_ms` | time | |
| `ids[]` | ids | The oldest first. At most 200. Not more than `count`. |

### `answer_result`

The result of an `answer` or a `reply`.

| Member | Type | Rule |
|---|---|---|
| `request_id` | id | |
| `client_msg_id` | string | The value of the frame of the device. |
| `status` | string | See the table below. |
| `state` | string, optional | The state of the item: `pending`, `replied`, `answered`, `abandoned`, `expired`, `cancelled` or `superseded`. |

| `status` | Result |
|---|---|
| `accepted` | The server took the frame. An `answer` is then the only answer of the item. |
| `already_answered` | Another answer won. The same `client_msg_id` sent again also gets this status. |
| `closed` | The item is final in another way. |
| `abandoned`, `superseded`, `expired`, `cancelled` | The item is in this state. |
| `effect_mismatch` | `shown_effect` is not the effect of the item. |
| `device_revoked` | The device is revoked. The state of the item does not change, and the server closes the session with 4403. |
| `custom_not_allowed` | The item has `allow_custom` false, and the frame has a text. |
| `ask_back_not_allowed` | The item has `allow_ask_back` false, or the thread has the most turns that the file allows. |

A `reply` or an `answer` for an item that the owner does not have gets `closed` and no `state`.
A frame that the server cannot handle now (the storage is away) gets the `error` frame `unavailable`. The outbox sends it again.

## Frames of the device

### `ack`

`{"t":"ack","id":"<32 hex>","stage":"received"}`. See "Receipts".

### `answer`

```json
{"t":"answer","request_id":"bbbb...","client_msg_id":"m-0001","action":"accept","shown_effect":"sha256:..."}
```

| Member | Type | Rule |
|---|---|---|
| `request_id` | id | |
| `client_msg_id` | string | 1 to 64 characters of `A-Z a-z 0-9 _ -`. The device sends the same value again until it gets `answer_result`. |
| `action` | string | `accept` or `decline`. |
| `shown_effect` | string, optional | With `accept` of an approval: the effect that the device computed from the `op` that it received. Never with `decline`. |
| `choice[]` | strings, optional | 0 to 8 option ids. With `accept` of a question. An answer by free text has an empty list. |
| `text` | string, optional | 1 to 8000 characters, no NUL. Never with `accept` of an approval. |
| `intent` | string, optional | `comment` or `change_request`. With `decline` and `text` only. |

For a `confirm` question the choice is `["yes"]` or `["no"]`.
A decline of an approval is the only refusal. A decline of a question means: no answer.
The user never edits the command. A `change_request` is a text for the agent.

### `reply`

`{"t":"reply","request_id":"bbbb...","client_msg_id":"m-0006","text":"Why this database?"}`.
`text` has 1 to 8000 characters, no NUL. The item stays open and gets the state `replied`.

### `refresh`

`{"t":"refresh","request_id":"bbbb..."}`. The server sends the `request` frame of the item, or its `settled` frame.
The `request` frame has the state of the item now (`seq`, lease). An item that the server does not hold gets `settled` with the reason `item_gone`.

### `sync`

`{"t":"sync","open":[{"request_id":"bbbb...","seq":2}]}`. At most 500 entries.
The server sends the `request` frames that the device lacks or knows in an older state.
It sends a `settled` frame for each item that is final or that it does not hold any more.
It sends nothing for an item that the device holds in the newest state.

### `set_prefs`

The frame has the members of `prefs`. It replaces the preferences of the account.
An absent block means: no preference of the account, use the default of the file of the portal.

The server stores the frame and sends no answer. The next `welcome` has the stored values.
The server applies no preference in version 5.1.0. The digest and the flag `quiet` come with a later lot.

## Preferences

| Member | Type | Rule |
|---|---|---|
| `quiet` | object, optional | `{from, to, tz, high_rings}`. |
| `quiet.from`, `quiet.to` | string | `HH:MM`, 24 hours. They differ. The window can cross midnight. |
| `quiet.tz` | string | IANA name, 1 to 64 characters of `A-Z a-z 0-9 _ + - /`. |
| `quiet.high_rings` | bool | True: an item with the urgency `high` still rings. |
| `digest` | object, optional | `{mode, at?, tz?, every_h?}`. |
| `digest.mode` | string | `off`, `daily` or `every`. |
| `digest.at`, `digest.tz` | string | With `daily` only, and both. |
| `digest.every_h` | u16 | 1 to 168. With `every` only. |

`off` has no other member.

## Effect

The effect binds a service, an action, a resource and an operation (`op`).
The rule is paragraph 17.3 of the ProcurAuth protocol. The code is `crates/inbox-core/src/effect.rs`.
The server, the phone (through JNI) and the tests use this code.

1. Make the JSON object `{"v":1,"service":..,"action":..,"resource":..,"op":..}`.
2. Write it as JCS text (RFC 8785). Put no white space between tokens. Sort the members by UTF-16 code unit.
   Use the short escapes `\b \t \n \f \r \" \\`. Write each other character below U+0020 as `\u00xx` in lower case.
3. Encode the text in UTF-8 and compute SHA-256.
4. The effect is `sha256:` and the 64 lowercase hexadecimal digits.

`op` is an object.
It has objects, arrays, strings, `true`, `false`, `null` and integers.
An integer has no fraction and no exponent.
Its absolute value is at most 2^53-1.
Any other number makes `op` invalid, for example `1.5`, `1.0`, `1e2`, `-0` and `9007199254740992`.

A duplicate member name makes `op` invalid.
An `op` that is not an object is invalid.
The code for an invalid `op` is `invalid_op`.
The core parser of `op` refuses it while it reads the frame.

A `view` is a template over `op`. It is not part of the effect.

| Member of `view` | Rule |
|---|---|
| `title` | One line, 1 to 128 characters. |
| `fields[]` | 0 to 12 pairs `[label, pointer]`. `label` is one line of 1 to 48 characters. |
| `body` | A pointer to the command. It names a string. |
| `body_label` | Optional. One line, 1 to 48 characters. |

A pointer is an RFC 6901 JSON Pointer of at most 256 bytes.
It must name a value of `op`.
A pointer of `fields[]` names a string, an integer, `true`, `false` or `null`.
It does not name an object or an array.
A bad view gives the code `invalid_view`.

The short form of an effect for the screen is its first 8 hexadecimal digits.
The screen shows it after `sha256:`.

The JNI functions `effectCompute(service, action, resource, opJson)` and `viewResolve(opJson, viewJson)` give these results.
Both return JSON text: `{"effect":"sha256:..."}`, `{"view":{...}}` or `{"error":{"code","message"}}`.
The vectors of the rule are in `crates/inbox-core/tests/fixtures/effect-vectors.json`.

## States of an item

| State | Open | Use |
|---|---|---|
| `pending` | yes | The agent waits. The lease is alive. |
| `replied` | yes | The owner asked the agent a question. The item stays open. |
| `answered` | no | One `answer` won. A decline of an approval is `answered` with the decision `deny`. |
| `abandoned` | no | The lease and its grace ended. The agent can revive the item in the window of the server. |
| `expired` | no | The date of the agent or the longest life was reached. |
| `cancelled` | no | The agent, an administrator, a revoked key or a deleted owner closed it. |
| `superseded` | no | The agent made a new item in the same thread. |

Only one `answer` wins. The server decides, not the clock of a phone.
The other devices of the owner get `settled`.

## Delivery rules

- **Fan-out at acceptance.** When an alert is accepted, one delivery row is created for every device subscribed to the group at that instant.
  An alert of the portal about a service has a row only for the devices of the accounts that may open the service.
  An item has a row for each device of its owner that is not revoked.
- **Retransmission.** The server sends rows whose receipt has not arrived yet,
  oldest first, with at most `max_inflight` rows in flight. A row that stays
  without receipt for `resend_after` is sent again, and the whole backlog is resent
  after every reconnection.
- **Order on the device.** The app stores the frame durably *before* it sends
  `ack received`, and deduplicates on the id of the delivery. Notifications use an identifier
  derived from this id, so a retransmission never shows twice.
- **Outbox.** The device keeps each `answer` and each `reply` until it gets `answer_result`.
  It sends them again after each reconnection.
- **Items have no age limit.** An item stays in the queue until the device confirms it. The lease and `max_lifetime` end an item.
  `storage.delivery_ttl` does not apply to an item.
- **Catch-up.** A device that enrolls after an item came gets the open items of its owner at its first session.
  The same holds at each reconnection. A device that is away gets the items that it lacks.
- **Order.** The server sends `request`, `message` and `settled` in the order of the queue.
  A device that was offline gets the `request` before its `settled`.
- **Abandoned items.** An item that is `abandoned` when its first delivery is due does not go to a device that never got it.
  The server withdraws the deliveries of this item for this device. A revival sends a new `request`.
- **Out of date.** The server withdraws a `settled` for an item that is open again, and a delivery that it cannot build.
- **Expiry.** A delivery not received within `storage.delivery_ttl` is no longer
  sent and is reported as expired. A revoked device's rows are abandoned.

## Rights of the account

A phone subscribes only to a group for which its account has the permission `inbox.receive` of the portal features.

- The `welcome` lists only the groups that the account may subscribe to.
- `subscribe` to another group gets the error `forbidden_group`. Sometimes the portal cannot tell the rights, because its directory is not ready or is old. Then the error is `unavailable`, and the phone can send the message again.
- When the account loses the permission, the server removes the subscription and sends `subscriptions`. This check runs when the directory of the portal changes, and at each `welcome`. It does not run for each alert.
- The server never removes a subscription when it does not know. A directory that is not ready, a directory that is old, an empty directory and an account that the directory does not know remove nothing.
- An alert that exists is always delivered. A phone that loses the permission still gets the alerts that the server made before.
- An alert of the portal about a service (`inbox.watch`) is an exception. The server sends it only while the account of the phone may open the service. When the account lost the access, the server removes the delivery from the backlog. When the portal cannot tell, the delivery waits.
- An item goes only to the devices of its owner. No other account receives it.

## An alert to one account

An alert can be for one account, not for a group. The frame is the same frame. It has the group id `@direct` and the group name `Direct`. The id of a group of the file has only the characters `a-z`, `0-9`, `-` and `_`, so the two never collide. The server sends the frame to each phone of the account that is not revoked, with no subscription. The phone never subscribes to `@direct`.

## The kind of a message

The kinds are `alert`, `question`, `approval`, `turn` and `digest`.
A parser refuses any other value.
The kind `alert` is also a member of the frame `alert`.
The server stores the kind in the column `kind` of the table `alerts`.
The kinds map to frames:

`alert` to `alert`, `question` and `approval` to `request`, `turn` to `message` and `digest` to `digest`.
The rule of one-way messages stays true for `alert` only.
The other kinds have an answer from the device.

## Close codes

| Code | Use | App behaviour |
|---|---|---|
| 1012 | The server restarts | Reconnect immediately. |
| 1001 | Session timeout or handshake timeout | Reconnect. |
| 1013 | A newer handshake from the same address took the place, or this instance does not know the account of the phone (PostgreSQL) | Reconnect after a back-off. |
| 4401 | Unknown device or key mismatch | Enroll again. |
| 4403 | Device revoked | Enroll again. |
| 4409 | Replaced by a newer session of the same device | Do not fight it. |
| 4410 | Enrollment refused (only on `/v1/enroll`) | Ask for a new code. |
| 4426 | Unsupported protocol | Update the app. |
| 4429 | Rate limited | Reconnect later. |

The server closes the session of a device that sends a frame that fails a rule of this document.

## Decisions

These choices fill the gaps of the specification. Change them only with a new protocol version.

1. **Query form.** The link is `portal-inbox://enroll?s=..&k=..&c=..`. The earlier design note
   used `portal-inbox://enroll#<code>`. The query form is the contract.
2. **Strict code.** The parser is strict. The scheme and the key names are case sensitive.
   A pad character in base64url is an error. Bits at the end that are not zero are an error.
3. **Order of keys.** The order of `s`, `k` and `c` in the query is free.
4. **Fingerprint.** The fingerprint is the first 20 hexadecimal characters (lower case, 80 bits) of SHA-256 over the 32 bytes of the server key.
   It has five groups of 4 characters with one space between them.
   The core computes it.
   The fingerprint is not on the wire, so its length is not part of the protocol version.
   A different key with the same fingerprint costs about 2^80 digests.
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
   at each `welcome`. The phone has no option for it.
10. **Old servers in the app.** The upgrade of the database of the app (versions 6 and 7) removes
    every stored server of protocol 3 with its groups and alerts. The app tells the person once:
    "Enroll again".
11. **Timeouts of the app.** It waits 20 seconds for the WebSocket to open and 90 seconds
    for message 2. The server SHOULD close an enrollment connection that sends no message 1
    within 10 seconds.
12. **One attempt.** An enrollment connection carries one handshake. The server closes it
    after message 2 or after a refusal. The phone closes it with code 1000.

## Other interfaces

The page of the Inbox is a page of the portal, at `/auth/settings/inbox`. It needs a session of the portal.
The senders send alerts to the same listener of the portal, on `/api/v1/`. The routes are in [README.md](README.md).
The protocol of the phones has no admin call.
