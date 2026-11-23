# Design: the Inbox inside the portal

The Inbox is the part of the portal that sends alerts, questions and approvals to Android phones. This page gives the design choices and the reason for each one. The code is the reference when this page and the code disagree. The [requirements](../inbox/SPEC.md) state what the Inbox must do.

## Goals

The Inbox has four goals.

1. Each subscribed phone gets each alert, and shows it one time. This holds when the phone was off or disconnected.
2. One account gives one identity and one set of permissions for the launcher, the keys and the Inbox.
3. A person can answer an agent: approve an action, refuse it, or answer a question. The answer is bound to the exact action.
4. The deployment stays small: one binary, one configuration file, one database, one port.

## Decisions at a glance

| Choice | Reason |
|---|---|
| The Inbox is a module of the portal binary (`src/inbox/`). | One account, one set of permissions, one configuration file and one release. No second service to run. |
| The Inbox uses the port of the portal. | A reverse proxy routes by path. A separate exposure, budget or retry needs no second port. |
| The portal has no TLS code. | The proxy ends TLS. Noise protects the phone sessions on plain HTTP. |
| The phones enroll with a code that an account makes. | The login of the account is the identity of the phone. A shared password is not needed. |
| The phone pins the key of the server. | It replaces OPAQUE. The build has no PAKE crate. |
| The Inbox uses its own chain of migrations. | The Inbox tables are older than the merge and keep their names. |
| One scope of permissions: the service `portal`. | The features of the portal (keys, Inbox) share one list of permissions. |
| Two kinds of key: `mcp_` and `api_`. | The kind decides who verifies the key: the key edge or the API of the Inbox. |
| The server seals the body of an item at rest. | A leak of the database or of a backup shows no text. |
| The app has no push service of a third party. | The phone holds one encrypted connection to the portal. |

## Network

The binary has up to three listeners.

1. `server.listen_address` is the portal. It serves the pages, the settings, the metrics and all the routes of the Inbox.
2. `pdp.listen_address` is the AuthZEN decision point. Only the services reach it.
3. `inbox.cluster.listen` carries the copies of alerts between instances. It exists with the block `inbox.cluster` only.

### One port

The first design gave the phones a listener of their own. It also had a relay that published this listener. The final design serves everything of the Inbox on the port of the portal. The reason is that `/v1/enroll` and `/v1/session` are HTTP upgrade requests. The content is Noise-encrypted inside WebSocket frames. A reverse proxy carries them by the path of each request.

- **Routes.** The function `inbox::owns` names the paths of the Inbox: the prefixes `/v1/` and `/api/v1/`, `/readyz` and `/api/version`. The portal checks them before each other route. The Inbox also serves the MCP path that `inbox.mcp.path` sets.
- **No page rules.** The portal applies no session, no CSRF check, no origin check and no cookie to these routes. Phones prove themselves with the Noise handshake. Senders prove themselves with an `api_` key.
- **Unknown paths.** A path under `/v1/` that the Inbox does not know gets 404. A request that is not a GET to a route of the phones gets 405.
- **Proxy rule.** The proxy must send `/v1/`, `/api/v1/` and `/api/version` to the portal without ForwardAuth. A service on the same host must not use a path that starts with `/v1/`. See [Traefik and TLS](../traefik.md).
- **Prefix.** The portal matches its routes at the root. A deployment under a prefix works when the proxy removes the prefix. The app keeps the prefix of the public address and adds `/v1/enroll` or `/v1/session`.
- **Retry.** Traefik retries a request that got no answer (4 attempts, 100 ms apart). A WebSocket upgrade that got `101` is never repeated. The files in `deploy/` and `demo/` set the middleware.
- **Client address.** One source gives the client address: `server.trusted_proxies`. The limiter of the Inbox and the count of the enrollment attempts use it.

### Connection budget

A page connection holds one permit of `server.max_connections`. When it upgrades to a phone session, the permit returns. The phone session takes a permit of the session budget before the answer `101`.

The session budget is `server.max_connections` plus two times `inbox.devices.max_devices`. The first term covers the phones that do not finish the handshake. The second term covers one live session and one lost session of each phone. A handshake has a deadline of 15 seconds from the upgrade. One client address holds at most `inbox.devices.max_handshakes_per_address` handshakes. A new handshake of a full address ends the oldest one with the close code 1013.

A phone that cannot get a permit gets HTTP 503 with `Retry-After: 5`, before the upgrade. A flood of refused attempts then costs no session and no frame work. The rate limit still closes with the code 4429 after the upgrade.

### No knowledge of TLS

The listeners speak plain HTTP. The proxy ends TLS. The portal reads the scheme from `X-Forwarded-Proto` and the host from `X-Forwarded-Host`, and it trusts them from the networks in `server.trusted_proxies` only. HTTPS gives the session cookie its `__Host-` name and its `Secure` attribute. An `api_` key travels in clear text on plain HTTP, so the operator uses an HTTPS `inbox.devices.public_address` and an HTTPS URL for the senders. The app does not need a certificate code: for an `https://` address it uses the trust store of the phone.

### Start, stop and readiness

- **Start.** With the block `inbox`, the portal listener opens before the catalogue is complete. It answers the routes of the Inbox at once, so an alert and a phone never wait for the discovery. Each other route answers `503` with `starting` until the catalogue is complete, so the gate never decides with an incomplete catalogue.
- **Stop.** At the signal, the Inbox answers `503` at `/readyz` and sends the close code 1012 to each phone. The portal serves 5 more seconds, then waits at most 10 seconds for the connections.
- **Readiness.** `GET /readyz` gives `200 ready`, `200 degraded` (PostgreSQL with no usable primary), or `503 draining`. Without the block `inbox`, the portal answers `200`.
- **Lease loss.** A standby replica that loses its lease stops the whole process, because the SQLite file is shared with the portal. A standby instance waits for the lease before it opens the file. A small thread watches SIGTERM and SIGINT during this wait.

## The protocol of the phones

[The protocol page](../inbox/protocol.md) is the wire contract. This section gives the design.

### Enrollment

An account makes a secret of 128 bits in the tab Inbox. The text code holds the server address, the public key of the server and the secret. The deep link `portal-inbox://enroll?s=<address>&k=<key>&c=<secret>` holds the same data.

1. The phone starts Noise IK with the pinned key of the server. It sends the secret and its name in the first payload.
2. The server checks the digest of the secret. A secret works one time and lives `inbox.enrollment.code_ttl` (default 10 minutes).
3. The server stores the key of the phone and links it to the account. The phone then uses the normal session.

The pinned key replaces OPAQUE. The build has no `opaque-ke` crate.

- **Mandatory address.** `inbox.devices.public_address` is mandatory. It is the `s` of the code. Only the schemes `http` and `https` are valid.
- **One transaction.** The Inbox marks the code as used and stores the phone in one transaction. The checks of `inbox.devices.max_per_account` and `inbox.devices.max_devices` come first, so the code stays valid when a limit is reached.
- **Replication.** With `inbox.replica.sync_alerts = true`, the creation of a code and an enrollment wait for the snapshot. If the snapshot fails, the new phone is revoked and the person needs a new code.
- **Close codes.** All refusals of a code use the code 4410. The event `enroll_refused` holds the real reason in the field `reason`. A storage error closes with 1011 and leaves the code unused. A phone of an older protocol gets 4426.
- **Rate limit.** The limiter of the address counts each connection that sends a first frame, also a refused one. A success clears the count. The limiter never refuses a phone that proves its key.
- **Retention.** The task of retention deletes a code 24 hours after its expiry. A used or expired code is refused at once.
- **End of an account.** `enrollment::end_account` is the one operation for the end of an account. It deletes the unused codes, revokes the phones, and detaches the rows that carry the login. These rows get a login with `:ended:` and a random part. No login has a colon, so they match no later account.
- **Delete of an account.** The portal calls `end_account` before it deletes an account. If the Inbox fails, the account stays. With PostgreSQL, a code locks the row of its account. A code made during the delete is then found by a second call.
- **Codes and impersonation.** A session that the superadmin opened as an account can make a code for that account. The phone then belongs to that account.

### The field `kind`

Each message frame has the field `kind`. In protocol 5 the values are `alert`, `question`, `approval`, `turn` and `digest`. The core parser refuses any other value. The table `alerts` has the column `kind`. The metrics of the alerts have the label `kind`.

The app reads the field in Kotlin after the core decrypts the frame. A valid but unknown name shows as a generic message. The app never drops a message because of its kind.

### Name of the instance

`server.name` has the default `Portal`, 1 to 64 bytes, with no control character. The phones show it as the name of the server. The enrollment answer and the message `welcome` hold it.

## Rights: one scope

The portal has one internal scope for its features: the service `portal`. The PDP of the portal decides in process. The action document comes from the code and follows the file. The groups of the file are the nodes `inbox_group:<id>`.

The id `portal` is reserved for workloads. The launcher and the page **Services** never list the scope. The page of an account shows it as the first row, **Server**.

| Action | Applies to | Needs | Limit |
|---|---|---|---|
| `keys.own.read`, `keys.own.edit`, `keys.own.revoke` | service | `edit` and `revoke` need `keys.own.read` | none |
| `keys.own.create`, `keys.own.use` | `key_kind` | `create` needs `keys.own.read` | `use`: the ceiling `max_key_lifetime` |
| `users.read`, `keys.all.read`, `keys.all.revoke` | service | `keys.all.*` need `users.read` | none |
| `inbox.receive`, `alert.read` | `inbox_group` | none | none |
| `alert.send` | `inbox_group` | none | the budget `alerts` |
| `alert.send_user` | service | none | the budget `alerts` |
| `phone.read`, `phone.log` | service | none | none |
| `phone.revoke` | service | `phone.read` | none |
| `approval.read`, `approval.cancel` | service | none | none |
| `question.ask` | service | none | the budget `questions` |
| `question.read`, `question.cancel` | service | none | none |

- **Presence.** The document follows the file. Without the block `keys`, it has no action of the keys and no `users.read`. Without the block `inbox`, it has no action of the Inbox. A block `user` that names an action that the document does not hold stops the start.
- **Groups.** `inbox.receive`, `alert.send` and `alert.read` list only the type `inbox_group`. A permission on the whole service is not valid for them, so an account needs one permission for each group. A group that the file adds later needs a new permission.
- **Names.** The actions share a prefix by area. The editor groups the actions by the first part of the name.
- **The budget `alerts`.** The limit of `alert.send` is a budget, not a ceiling. A name that starts with `max_` is a ceiling by the ProcurAuth protocol, and a ceiling has no window. The value is on the grant `access on service:portal`. For this service, `access` opens no path: it only carries the budgets. The counters are the counters of the PDP.
- **What counts as a use.** One request of a key is one use. A webhook that holds many alerts is one use. A request that an `Idempotency-Key` repeats is a use too, because the decision comes before the check of duplicates. The superadmin has no limit.
- **Critical group.** A group with `critical = true` never counts a use, so a budget never limits it.
- **Entry.** The launcher has no tile for the Inbox. The gear opens the pages. An account that holds a permission that opens a part of the page gets the gear too.

## Keys: two kinds

A key has exactly one kind. The prefix of the text gives the kind.

| Prefix | Kind | Verified by |
|---|---|---|
| `mcp_` | MCP key, for agents | The key edge (`/auth/verify-key`), and the `/mcp` endpoint of the Inbox |
| `api_` | API key, for services, webhooks and callers such as Grafana | In process, by the API of the Inbox |
| `whk_` | Reserved | Nobody. It is refused as an unknown prefix. |

- **Text.** The prefix, 43 random base62 characters and 6 characters of checksum. The checksum is the CRC-32 of zlib over the prefix and the 43 characters, in base62. A verifier refuses an unknown prefix, a bad length, a bad character and a bad checksum before any lookup. All refusals are `401` with no detail. The portal keeps the SHA-256 digest and a short hint.
- **A key follows its owner.** The rights of a key are the rights of the key and the current rights of its owner. The portal has no service account. When the owner loses a right, the key loses it at once. Other instances follow within `keys.directory_max_age`. A key also needs `keys.own.use` on its kind.
- **Delete.** When an account is deleted, its keys, their grants and their counters go with it.
- **By kind.** `keys.own.create` and `keys.own.use` apply to the type `key_kind`, with the nodes `key_kind:mcp` and `key_kind:api`. The form of a new key shows only the kinds that the account may make.
- **What a key holds.** An API key holds `alert.send`, `alert.read`, `alert.send_user`, `question.ask` and `question.cancel`, and the grant `access on service:portal` for the budgets. An MCP key holds grants on services, and the two actions of the questions. An API key holds no grant on a service, because no gate opens for it. A key never holds a `keys.*` action.
- **The edge reads headers only.** The ForwardAuth request has no body, so a translator that reads an MCP body cannot live in the edge. The portal is itself the service of the Inbox, so it serves `/mcp` for `mcp_` keys and REST routes under `/api/v1/items` for `api_` keys. See [Inbox: agents](../inbox/agents.md).
- **A key follows its own items.** A key reads and speaks on an item only when it is the requester key of that item. Another item answers `not_found`.
- **Interaction.** A key has the attribute `interaction`: `conversational` or `one_shot`. The default is `conversational` for an `mcp_` key and `one_shot` for an `api_` key.
- **Confirm.** A grant of a key or of its owner can carry `confirm`. The decision asks for an approval when one side asks for it. A file with `confirm` on the service `portal` or on `access` stops at start.
- **Keys of the file.** A block `api_key "<name>"` of the block `inbox` names an owner (an account of the file), a secret by `_env` or `_file`, and the groups. It decides its groups at start, because the rights of a file account do not change while the server runs. It works when the portal is not ready and when PostgreSQL is away. It has no budget, and the list of keys does not show it. The check at start stops the program when the owner lacks a right.
- **Order of the checks of a sender.** The API checks the form and the kind of the text first. It then compares the text with the keys of the file in constant time. Only a text that is none of them goes to the check of a key of the database.
- **Interface to the portal.** The trait `inbox::rights::Authorizer` is the one interface between the Inbox and the rights of the portal. The portal implements it in `src/http/inbox_rights.rs`. Before the portal sets it, a key of the database gets `503 directory_not_ready`.
- **Answers.** `403 forbidden_group` for a decision no. `401` for a text that is not a valid key, or a key of another kind. `429 rate_limited` with `Retry-After`. `503 directory_not_ready` when the directory is too old. `503 rights_unavailable` when the count fails.

## Subscriptions

- **Welcome.** The message `welcome` lists the groups that the account may subscribe to. When the portal cannot tell, it lists all the groups.
- **Subscribe.** A group that the account may not subscribe to gets the error `forbidden_group`. The error `unavailable` means that the portal cannot tell. A new subscription never rests on a guess.
- **Removal.** A task of the Inbox looks at the generation of the directory every 2 seconds. When it changes, the task removes the subscriptions that a loaded directory refuses. At each `welcome`, the Inbox does the same for the phone. The removal changes the table of subscriptions only: the deliveries that exist stay.
- **Safety rule.** The Inbox changes nothing when the portal answers `Unknown`. This happens when the directory of a shared database is older than `keys.directory_max_age`, or when a shared directory does not have the account. An account with no permission is a positive no.
- **Absent account.** With SQLite, a login that the portal does not have ends its state. With PostgreSQL, a login that is not found is `NotHere`: the phone gets the close code 1013 and the Inbox writes nothing.

## Alerts to one account

A message has one target: a group or an account. The store keeps `target_kind` (`group` or `user`) and `target_id`. The type in the code is `Target` (`src/inbox/target.rs`).

- **Right.** One flat action, `alert.send_user`, on `service:portal`. A sender with this right can alert any account. The other choice was a resource type with one node for each account.
- **Level.** An alert to an account has the level Ring and is never critical, so it always counts against the budget.
- **Read.** The sender, the target account and the superadmin read an alert to an account. `GET /api/v1/alerts/{id}` lets only the key that sent the alert read it.
- **Delivery.** An alert to an account goes to each phone of the account that is not revoked, with no subscription. An account with no active phone gets `409 no_phone`, and the Inbox stores nothing. This check comes before the count of the budget, so a refused alert uses no unit.
- **Existence.** The check of the target is `keyauth::person_exists`. It is true for the superadmin, for an account of the file and for an account of the database.
- **Protocol.** The frame of a direct alert has the group id `@direct` and the group name `Direct`. The id of a group of the file has only `a-z`, `0-9`, `-` and `_`, so the two never collide.
- **Source.** The source of an alert from the form of the page is `admin:<login>`. The source of a key is `producer:key:<id>`.

## Pages

The pages of the Inbox are part of the settings pages. [Settings pages and history](../settings-and-history.md) describes them. These rules explain the shape.

- **Server rendered.** Each page is HTML from the server. A filter, a chip, a period and the row of an alert are links with a query parameter. The forms work without script. The script of the settings pages draws the editor of grants and the matrices, and it counts the ticked rows of a bulk action.
- **Absent, not grey.** A part that the file disables is absent from the page. A route that is off answers 404.
- **Confirmation in place.** A destructive action opens a question in the same place (`details` element). A firm confirmation asks the person to type the name of the resource. The field has a `pattern`, so the browser blocks a wrong name without script.
- **One request, one alert.** The form of an alert carries a random key, so a double click sends one alert.
- **Bound lists.** The list of sent alerts holds the 500 newest alerts of the period. The count shows `500+` when older ones exist. The graph counts every alert.
- **Impersonation.** A session that the superadmin opened as an account can read the page and make a code. It cannot send an alert and cannot revoke a phone.
- **No product name in the body.** No page shows the logo or the name of the product in its body. See [Settings pages and history](../settings-and-history.md).

## Storage

- **Two chains.** The portal and the Inbox have two independent chains of migrations. The portal keeps its version in `PRAGMA user_version` (SQLite) and in `schema_migrations` (PostgreSQL). The Inbox keeps its version in the table `inbox_schema_version`. Each chain refuses a schema that is newer than the binary.
- **One database.** The Inbox uses the SQLite file or the PostgreSQL database of the portal. It has no `postgres` block. `inbox.storage.pool_size` (default 8) sets its own pool.
- **Table names.** The tables of the Inbox keep their names, for example `devices`, `subscriptions`, `alerts`, `deliveries`, `connections`, `enrollment_codes` and, with PostgreSQL, `instances`. They do not collide with the tables of the portal.
- **Clients.** The Inbox keeps an asynchronous PostgreSQL client (`tokio-postgres` and `deadpool-postgres`) and a SQLite writer thread. The portal keeps a synchronous client. One connection of the Inbox listens for `NOTIFY`.
- **Replica.** The snapshot of a standby replica is a copy of the whole SQLite file. It holds the tables of the portal too, so a standby instance that becomes active has the accounts and the keys.
- **Retention.** The Inbox deletes an alert after `inbox.storage.alert_retention` and the connection log after `connection_log_retention`. A delivery expires after `delivery_ttl`.
- **Import.** `portal inbox import-sqlite` copies a SQLite file at the current Inbox version into an empty PostgreSQL database. There is no copy from SQLite to SQLite.
- **No migration for some older databases.** A database from before 5.1.0 has no tables of items, and the start stops with an error that names the table. See [Operations](../operations.md#upgrades).

## Items: approvals and questions

An item is a request from an agent that a person must answer. The [requirements](../inbox/SPEC.md) give the rules. This section gives the reasons.

- **One recipient.** The owner of the key is the only recipient and the only approver. An item has no named account, no group, no delegation and no escalation. This keeps the question "who may approve" out of the system.
- **The service learns the verdict only.** The verdict is `pending`, `approved`, `denied` or `void`. No function gives a turn, a comment or a device to the HTTP layer of the approval API.
- **Two parts of a message.** A message has a command and an explanation. The command comes from the service and is bound to the effect hash. The explanation comes from the agent and is not verified.
- **Lease.** The agent says how long it waits. A lease that ends gives `abandoned`, which is not a refusal. A timeout gives `expired`, which is neutral. A timeout never means deny.
- **Gate.** A grant with `confirm` makes the PDP answer `approval_required`. The agent calls the tool again with the same arguments, and the PDP consumes the approval one time. The portal store keeps the counters and the Inbox database keeps the items, so no transaction covers both. The order (`consume`, count the budgets, then `release` when the count refuses) makes a crash lose one approval and never give two executions.
- **Sealed bodies.** The portal seals the body of an item with AES-256-GCM under a key from `inbox.at_rest`. The additional data bind the sealed value to the request id, the field, the sequence number and the effect hash.
- **What the seal covers.** The seal protects a leak of the database, a backup, a copy of the replica and a disk image. It does not protect against a person who has the key and the process. It is not end-to-end encryption. The superadmin reads a body only when `inbox.at_rest.admin_body_read` is true. See [Inbox: security model](../inbox/SECURITY.md).
- **The `evaluation_id`.** It holds no state. It has the id of a content key, the time of issue, the sealed facts of the question and an HMAC-SHA-256 under a sub-key. Each instance with the content keys can check it. The submission of the approval API has no subject, action or resource. A table of ids would break the rule that any instance can serve.
- **One engine.** The pure rules (states, transitions, lease, clock) are in `items.rs`. The operations are written once in `items_ops.rs`. One module per database holds the SQL. An operation reads the row with a lock, calls the rules and writes the result, so the answer is a compare-and-set. Each rule of time takes `now` from the trait `Clock`, so a test moves a manual clock by 90 days at once.
- **Limits in the transaction.** The cap of each hour, the mute and the deny time run in the transaction of the creation, after the lock of the creations. Two instances cannot both pass them. A unique index on `approvals.open_hash` is the last guard of the rule "one open request for each effect".
- **Mute.** After `inbox.approvals.mute_after_refusals` explicit refusals in a row (default 3), the key is muted for `mute_for` (default 1 hour), and the owner gets a plain alert.
- **No spool.** An item never goes to the spool of alerts. When the database has no writable primary, the creation, the renewal and the answer get `503 storage_degraded`. At the return, the portal extends each open lease by the length of the outage.
- **Deliveries.** The delivery of an item has no age limit: the lease and `max_lifetime` decide. At each session, the Inbox gives the phone one delivery for the newest request of each open item of its owner. It gives one more for each turn. The frames `sync` and `refresh` are answers, not deliveries.
- **Preferences.** The time zones, the quiet window and the due time of a digest are pure functions in `src/inbox/prefs.rs`. The database of the time zones is inside the binary (about 200 KB, from the crate `jiff`). The static image has no zone files.

## Alerts of the portal (watch)

The portal can send an alert when a service is unhealthy and when sign-ins fail in a burst. The block `inbox.watch` enables it. The code is in `src/watch.rs`.

- **Internal sender.** The task calls `accept_alert` of the Inbox with the source `portal`. The path is the path of an API alert. It has one transaction, the idempotency key, the copy of the replica and the spool of the degraded mode. No key and no HTTP request is involved.
- **Detectors.** Two small state machines decide. They take a monotonic time from the caller. A test can move this clock, and a jump of the wall clock does not start or stop an alert.
- **State of a service.** `down` and `unhealthy` are unhealthy. `starting` is not, and a service with no state is never unhealthy. A service that starts again breaks the unhealthy time, so a restart does not look like an outage.
- **Recovery.** The portal sends `healthy again` only after it sent an unhealthy alert, and only once. A service that leaves the catalogue is forgotten with no alert.
- **Sign-in burst.** The task sums three counters that exist. They count a wrong password or unknown login, a refusal by the limiter of the address, and a refusal by the global limiter. After an alert, the count restarts from zero.
- **Title.** The title has the values of the configuration, not the measured time. A name of a service is cut to 64 characters and has no control character. A login, an address and a password are never in a title.
- **Idempotency key.** The keys are `portal:unhealthy:<service id>:<bucket>`, `portal:recovered:<service id>:<bucket>` and `portal:burst:<bucket>`. The bucket is the Unix time divided by `repeat_after`. Two instances build the same key from the same event, so the Inbox accepts one alert.
- **Limits of the key.** Two instances that read one event on two sides of a bucket limit can send two alerts. Each instance counts the refused sign-ins that it sees, so a burst spread on several instances can stay under the threshold.
- **Memory.** The state of the detectors is in memory. A restart forgets it, and the portal waits `unhealthy_after` again.
- **Failure.** If the Inbox refuses an alert or does not answer in 20 seconds, the portal logs `watch_failed`, counts `portal_watch_failed_total{kind}` and tries again after 10 seconds. The detectors keep the alert due. The login path and the launcher never wait for the task.
- **Readers.** An alert about a service carries the id of the service. A person sees it only when the launcher rule `access::may_open` says that the person may open the service. The fan-out gives a delivery only to the phones of the audience, and `flush_deliveries` asks again before a phone gets it.

## Telemetry

The Inbox has no OpenTelemetry export. Its metrics are in the Prometheus text of the portal at `metrics.path`. Its log uses the JSON lines of the portal. The Inbox reads the host name from `/proc/sys/kernel/hostname` for the table of instances only.

## The Android app

The app is **Portal Inbox**. The application id and the Kotlin package are `app.portalinbox`. The deep link is `portal-inbox://`. The Noise prologues are `portal-inbox/v1/session` and `portal-inbox/v1/enroll`.

- **Shared core.** The crate `inbox-core` holds the protocol and the cryptography, with no I/O. The server and the app share it. `inbox-android` is the JNI bridge. The library is `libinbox_android.so`.
- **JNI check.** A unit test compares each `external fun` of `Native.kt` with the exported symbols. A name that does not match would crash the app at load.
- **No TLS code.** The app accepts an `http://` or `https://` base URL. For `https://` it uses the trust store of the phone. The manifest keeps `usesCleartextTraffic="true"`, because the portal often runs on a private network over plain HTTP.
- **Levels.** The user chooses for each group: Off, Notification, Ring or Alarm. The server never knows the choice. Approvals use at least Ring and never Alarm.
- **Biometric prompt.** To approve, the person confirms with the local biometric prompt.
- **Version.** The `versionName` is the version of the workspace. The `versionCode` is `major * 10000 + minor * 100 + patch`.
- **Tests.** The JVM unit tests are under `android/app/src/test`. `tools/android/build-apk.sh` runs them. The end-to-end tests use the emulator (`tools/android/emulator.sh`), not a physical phone.

## Build and release

- **One version.** The root package and the workspace packages share one version. `tools/version.sh` reads it and stops when the versions differ.
- **One image.** `docker/Dockerfile` builds a static musl binary on `FROM scratch`. The build runs natively for each architecture. No tool of the repository pushes an image to a registry.
- **Layers.** A layer holds the dependencies only, so a change of the sources rebuilds the crates of the repository and not the dependencies.
- **Compose and K3s.** `docker-compose.yml` runs the portal and PostgreSQL. `deploy/compose/docker-compose.ha.yml` adds a second instance and Traefik. `deploy/k3s/` has a Deployment with two replicas. The IngressRoute `portal-inbox` sends `/v1/`, `/api/v1/` and `/api/version` to the portal without ForwardAuth, with a retry middleware.
- **Release.** A release holds binaries, the signed APK and `SHA256SUMS`. See [Build and release](../build-and-release.md).

## Left out on purpose

- **iOS.** An iOS app cannot keep a background connection without the push service of Apple.
- **A push service of a third party.** The phone holds one encrypted connection to the portal.
- **Acknowledgement of an alert.** An alert stays one way. Items carry the answers.
- **Other recipients of an item.** The owner of the key is the only recipient.
- **End-to-end encryption, a signature of the phone, a risk class.** The portal seals bodies at rest, and the owner decides on the phone.
- **A Grafana plugin.** Grafana contact points use the webhook route.

## Open questions

- **QR code.** A QR code for enrollment needs a camera library in the app. The code is text or a link today.
- **Group-wide permissions.** A permission for each group is mandatory. A permission on the whole service for `inbox.receive`, `alert.send` and `alert.read` is a possible later change.
- **Upgrade hazard of `inbox.receive`.** An account with no `inbox.receive` loses its subscriptions at the first check with a loaded directory, and cannot make an enrollment code. Give the permission before the Inbox starts.
- **Network security configuration of the app.** The manifest allows clear text for each address. A configuration that allows it only where needed is not made.
- **Shared counter of refused sign-ins.** A shared counter needs a table and a write for each refusal.
- **Alert near a budget.** A key that comes near its budget raises no alert.
- **Alert from an `mcp_` key.** An `mcp_` key does not send alerts, and there is no tool `send_alert`. An agent sends alerts through the REST route with an `api_` key.
- **Reserved prefix.** `whk_` is reserved and not implemented.
- **Physical phone.** The tests use the emulator only.
- **Arm64 image.** The release builds the arm64 binary on a native runner. The arm64 image is not tested.
