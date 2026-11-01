# Design: the inbox inside the portal

Status: accepted by the owner on 2026-11-01. This page is the work plan of the merge. The code and the specification follow it.

## Goal
The portal and Inbox become one binary, `portal` (version 5.0.0). One account gives one identity and one set of rights for the launcher, the keys and the inbox.

## Decisions of the owner
| # | Decision |
|---|---|
| D1 | One binary. Inbox becomes the module `inbox`. |
| D2 | In production, PostgreSQL and two active instances at least. SQLite stays for development. The standby replica of SQLite stays. |
| D3 | A producer is a key of the portal or a token of the file. Both work at the same time. |
| D4 | A phone enrolls through an account of the portal. The pseudonym is the login. The shared password is gone. |
| D5 | The service `inbox` has the resource `group:<id>`. Its actions are `group.subscribe`, `alert.send`, `alert.read`, `device.read`, `device.revoke` and `connection.read`. |
| D6 | A limit can apply to `alert.send`. A group marked `critical` never has a limit. |
| D7 | The admin front has no mode without a password. The pages are in the style of the portal, in the tab "Inbox". |
| D8 | The code lives in the private repository `portal`. The history of Inbox is kept. The repository Inbox is archived at the end. |
| D9 | All HTTP code uses hyper. Inbox is written again on it. |
| D10 | The configuration follows the global rules. The delivery values have no default. |
| D11 | The binary has a listener `devices`. The DaemonSet `inbox-edge` relays it. |
| D12 | The portal sends an alert when a service is unhealthy and when sign-ins fail in a burst. |
| D13 | Tests use the emulator. No physical phone. |

## Layout of the repository
- The root package `portal` is the binary. The code of the inbox is in `src/inbox/`.
- `crates/inbox-core` holds the protocol and the cryptography of the phones, with no I/O. The server and the app share it.
- `crates/inbox-android` is the JNI bridge. `android/` is the app.
- `crates/inbox-server` is gone. Step 3 moved its code to `src/inbox/`.
- `deploy/` holds the Compose files and the K3s manifests of both parts.
- The history of Inbox is in the git log of the repository.

## Network
The binary has three listeners.
1. `listen`: the pages of the portal, behind Traefik ForwardAuth. The admin pages of the inbox are here.
2. `devices.listen`: phones, producers, webhooks and the health routes. No session. Phones and producers prove themselves.
3. The listeners of the metrics and of the cluster, as the two parts have them today.

## Protocol 4 of the phones
Enrollment uses one secret of 128 bits that the account makes in the tab "Inbox". The text code holds the server address, the public key of the server and the secret. The deep link `inbox://enroll#<code>` holds the same text.
1. The phone starts Noise IK with the pinned key of the server. It sends the secret and its name in the first payload.
2. The server checks the digest of the secret. A secret works one time and lives 10 minutes.
3. The server stores the key of the phone and links it to the account. The phone then uses the normal session.

The pinned key replaces OPAQUE. The crate `opaque-ke` leaves the build. A QR code needs a camera library in the app. It is not in this version.

## Rights
The inbox is a second internal service of the portal. The PDP of the portal decides in process. The action document comes from the code. The groups of the file are its resources.
- A phone gets the groups for which its account has `group.subscribe`. A change of rights reaches the phones within the age bound of the directory.
- A key or a token of the file that sends an alert needs `alert.send` on the group. A token of the file lists its groups in the file.
- The admin pages need `alert.read`, `device.read`, `device.revoke` or `connection.read`.
- The limit of `alert.send` is a counter of the key. A group with `critical = true` skips it.

## Storage
One database, one chain of migrations. Schema 7 adds the inbox tables to the portal tables. PostgreSQL uses the blocking client of the portal on threads of its own. One connection listens for `NOTIFY`. SQLite keeps one writer thread. The replica snapshots the whole database. An alert, an enrollment and a revocation wait for the snapshot when `sync_alerts` is true.

## Configuration
The block `inbox` joins the file of the portal. It holds the name, the key file of the server, the groups, the token producers, the device values, the retention and the optional `replica`. Delivery values (`delivery_ttl`, `resend_after`, `unreachable_after`) are mandatory and have no default. Every other key follows the global rules: documented default, `_env` and `_file`, `--print-config`.

## Order of work
1. This design. 2. The merged repository and the builds. 3. The hyper rewrite and the common store. 4. The service `inbox`: rights, pages, keys. 5. Protocol 4 and enrollment by account. 6. Alerts of the portal. 7. The demo with the emulator.

## Open points
- The license of the merged code. Each part keeps its own for now.
- The QR code for enrollment.
- The tests on a physical phone. The owner chose none.

## Deviations
This section lists what step 3 does in another way than the plan above, and the reasons.

### Storage
- The two chains of migrations are independent, not one chain. The portal keeps its version in `PRAGMA user_version` (SQLite) and in `schema_migrations` (PostgreSQL). The inbox keeps its version in the table `inbox_schema_version`. Each chain refuses a schema that is newer than the binary. The owner chose two chains for this step.
- The tables of the inbox keep their old names: `devices`, `subscriptions`, `alerts`, `deliveries`, `connections` and `instances`. The names do not collide with the tables of the portal. Step 4 can add a prefix when it changes the tables. A rename now would change every query of the delivery path for no gain.
- An old SQLite file of the standalone inbox cannot open in place: its `PRAGMA user_version` is the version of the old chain. `portal inbox import-sqlite` copies such a file into an empty PostgreSQL database. The command also reads a SQLite file of the portal. There is no copy from SQLite to SQLite.
- The snapshot of the replica is a copy of the whole SQLite file. It holds the tables of the portal too. A standby instance that takes over gets the accounts and the keys of the portal with the data of the inbox.

### Configuration
- The HCL parser of the portal now accepts blocks without a label inside the block `inbox` (`devices`, `storage`, `replica`, `cluster`). It still refuses nested blocks everywhere else. The keys of such a block are in the table as `inbox.devices`, `inbox.storage`, `inbox.replica` and `inbox.cluster`.
- `server_key_file` and `token_file` are secrets: each accepts `_file` and `_env`. The program does not trim the value. A key file that `portal inbox keygen` made has one final line end, and the program removes it.
- The keys `server.shutdown_grace`, `logging.format`, the block `telemetry`, the block `admin` and `limits.max_request_bytes` do not exist any more. The shutdown of the portal replaces `shutdown_grace`. The log is the JSON log of the portal. `inbox.devices.max_request_bytes` replaces `limits.max_request_bytes`. `inbox.devices.max_auth_failures` and `inbox.devices.auth_failure_window` replace the block `security`. `inbox.devices.trusted_proxies` replaces `server.trusted_proxies`.
- A Required key has no default only for the delivery values. The block `inbox.replica` has defaults. `sync_alerts` has the safe default `true`.
- The inbox has no `postgres` block. It uses the block of the portal. `inbox.storage.pool_size` (default 8) sets its own pool.

### Process
- The inbox starts before the portal waits for its catalogue. An alert does not wait for the discovery.
- The listeners of the inbox are limited to `server.max_connections` plus two times `inbox.devices.max_devices` connections. The old server had no limit.
- At the signal, the inbox answers `503` at `/readyz` and tells the phones to reconnect (close code 1012) at once. It serves 5 more seconds like the portal. Then it closes the listeners, sends the close code again to the sessions that started in the meantime, waits for the sessions (10 seconds at most) and writes what its memory holds to the database.
- A replica that loses its lease stops the whole process, because the SQLite file is shared with the portal. The process ends with an error after the last snapshot.
- Before the portal opens the SQLite file, a standby instance waits for the lease. A thread with its own small runtime watches SIGTERM and SIGINT during this wait. It replaces the signal handlers of the old code and needs no unsafe code.
- Step 5 removes the command `credential` with the shared password. See the section "Protocol 4 on the server".
- The inbox does not read `HOSTNAME`. It reads the host name from `/proc/sys/kernel/hostname`, for the table of the instances only.

### Telemetry and log
- The inbox has no OpenTelemetry export. Its metrics are in the Prometheus text of the portal with the same names. The crates `opentelemetry*`, `tracing` and `tracing-subscriber` are not in the build. The inbox writes its log with small macros that give the JSON lines of the portal. The spans are gone. The events `device_connected` and `device_disconnected` keep the fields that the old span gave (`device.id`, `device.name`, `user.name`, `client.address`).
- The Prometheus gauges are set by the task of the statistics every 15 seconds, as before.

### Not done in this step
- `docker/Dockerfile.inbox`, `tools/build.sh`, `deploy/inbox/` and the old Inbox documents still describe the standalone binary `inbox`. A later step replaces them. `docker/Dockerfile` now copies `crates/`, because the portal depends on `inbox-core`.
- The comments of the moved code are the old comments. The new modules and the changed interfaces have comments in Simplified Technical English. A full rewrite of the old comments is not done.

### Protocol 4 on the server
This section lists what step 5b does, and the choices that the owner left open.

- **New keys.** `inbox.devices.public_address` is mandatory. It is the `s` of the enrollment code. It follows the address rules of the app, but only the schemes `http` and `https` are valid. `inbox.enrollment.code_ttl` (default `10m`, from `1m` to `24h`) and `inbox.enrollment.max_active_per_account` (default 5, from 1 to 100) are in the new block `inbox.enrollment`. The block is optional because both keys have a default.
- **Removed.** The key `inbox.devices.credential` and the command `portal inbox credential` do not exist. The key file holds the Noise key pair only. A key file of protocol 3 is not read.
- **Migration 7 of the inbox chain.** It adds the table `enrollment_codes(digest, login, created_at_ms, expires_at_ms, used_at_ms)`. The digest is the SHA-256 digest of the secret. The migration does not touch the devices of protocol 3. They cannot connect: the server answers 4426. Their rows keep their old pseudonym as `user_name`. They stay in the overview until an operator revokes them.
- **Retention.** The task of retention deletes a code 24 hours after its expiry. A code that is used or expired is refused at once, so the delay only keeps the rows for an operator.
- **One transaction.** The inbox marks the code as used and stores the device in one transaction. A refusal changes nothing. The check of `inbox.devices.max_devices` comes before the use of the code, so the code stays valid when the limit is reached.
- **Replication.** The creation of a code and an enrollment wait for the snapshot when `inbox.replica.sync_alerts` is true. If the snapshot fails, the inbox does not give the code, and it does not send message 2 (the new device is revoked and the phone needs a new code). With `sync_alerts = false`, the failure is only logged.
- **Close codes.** A refusal because of `max_devices` is 4410 like all the other refusals of the code. The event `enroll_refused` has the real reason in the field `reason`: `unknown_code`, `used_code`, `expired_code`, `too_many_devices`, `unreadable_message`, `unsupported_protocol`, `text_frame`, `no_message`, `rate_limited` or `replica_sync_failed`. A storage error closes with 1011: the code stays unused. The rate limit closes with 4429 after the upgrade, as `/v1/session` does. The server does not answer the HTTP status 429 to the upgrade.
- **Rate limit.** The limiter of the address counts each connection that sends a first frame, also a refused one. A success clears the count of the address, as before. A connection that sends no frame counts after 10 seconds.
- **Call for the codes.** Step 4 replaced the JSON call `POST /auth/inbox/api/enrollment-codes` with the button **Make a code** of the page **My phones** (`POST /auth/settings/inbox/code`). The function that makes the code is the same. A session that the superadmin opened as another account makes a code for that account.
- **Deletion of an account.** The portal calls the inbox before it deletes the account. If the inbox fails, the account stays. A device that enrolls while the account is deleted either finds its code gone or is revoked afterwards. The call needs the inbox only when the block `inbox` exists. File accounts cannot be deleted, so they never reach this call.
- **Copies.** `portal inbox import-sqlite` and the snapshot of the replica carry the table `enrollment_codes`.
- **Old files of the standalone inbox.** The command `import-sqlite` accepts only a file at the current version of the inbox chain. A file from before step 5b must start once with this binary to migrate.
- **Deploy files.** `deploy/inbox/config.example.hcl` is now a complete configuration of the portal with the inbox. The other files of `deploy/inbox/` still describe the standalone binary. A later step replaces them.

## Step 4: rights, pages and keys as producers

This section lists what step 4 does in another way than the plan above, and the choices that the owner left open.

### Rights
- **Name of the limit.** The limit of `alert.send` is the budget `alerts`, not `max_alerts`. A name that starts with `max_` is a ceiling by the protocol, and a ceiling has no window. A budget has windows (second, minute, hour, day). The value is on the permission `access on service:inbox`, like the budgets of each other service. The counters are the counters of `counting.rs`. Step 4 adds no second mechanism.
- **What counts as a use.** One request of a key is one use. A webhook that holds many alerts is one use. A request that an `Idempotency-Key` repeats is a use too, because the decision comes before the check of duplicates. The form of the page counts for an account that is not the superadmin, for the same action. The superadmin has no limit.
- **Critical group.** The key `inbox.group.<id>.critical` is new (boolean, default `false`). A critical group skips the count. The inbox does not call the counter for it, so no memo of refusal can stop it either.
- **Types of the actions.** `group.subscribe`, `alert.send` and `alert.read` list only the type `group`. A permission on the whole service is not valid for them: the editor gives one permission for each group. A group that the file adds later needs a new permission. The owner can change this by adding the type `service` to the list of the three actions.
- **Dependencies.** The service has no `needs` rule like the service `portal`. The page **Phones** needs `device.read`, and **Revoke** needs `device.revoke` and `device.read` together.
- **Catalogue.** The portal adds the service `inbox` to each catalogue that discovery gives, when the file has the block `inbox`. It is in the group "Portal" with the icon `tb:bell`. It owns no path, so the gate does not change. The id `inbox` is reserved for workloads in all cases, also without the block `inbox`, as the id `portal` is.
- **Tile.** The tile **Inbox** shows for an account that holds a permission that opens a page of the tab (not for `access` alone) and for the superadmin.

### Producers
- **Order of the tokens.** The inbox looks at the tokens of the file first, in constant time. Only a token that is none of them goes to the check of a key. A token of the file therefore never waits for the portal.
- **Interface.** `crate::inbox::rights::Authorizer` has four calls: `check_key`, `decide`, `subscribable` and `directory_stamp`. The portal implements it in `src/http/inbox_rights.rs` and gives it with `Slot::set_authorizer` right after the start of the inbox. It holds only a weak reference to the state. Before this call, a key gets `503 directory_not_ready`.
- **Decision.** `decide` calls the decision of the PDP in process (`decide_key_for` in `pdp.rs`). It is the same code as for a PDP client, with the same counters. The log has the line `inbox_key_decision`.
- **Answers.** `403 forbidden_group` for a decision no, `401` for a text that is not a key or a key that is not valid, `429 rate_limited` with `Retry-After`, `503 directory_not_ready` when the directory is too old, and `503 rights_unavailable` when the count fails.
- **Status of an alert.** `GET /api/v1/alerts/{id}` also accepts a key with `alert.read` on the group of the alert. A key that may not read the group gets `404`, as a token of the file does.
- **Resolved.** A key never sends the notifications resolved of Alertmanager. The setting `webhook_send_resolved` belongs to the blocks `producer`.

### Subscriptions
- **Welcome.** The message lists the groups that the account may subscribe to. When the portal cannot tell, it lists all the groups, and nothing changes.
- **Subscribe.** A group that the account may not subscribe to gets the error `forbidden_group`. The error `unavailable` means that the portal cannot tell. A new subscription never rests on a guess. The protocol version stays 4. `docs/inbox/docs/protocol.md` has the section "Rights of the account".
- **Removal.** The task `rights::run` of the inbox looks at `Directory::generation` every 2 seconds. When the number changes, it reads the phones once and removes the subscriptions that a loaded directory refuses. At each `welcome` the inbox does the same for this phone. Each instance of a PostgreSQL installation does it for all phones. The removal is idempotent.
- **Safety rule.** `PortalAuthorizer::subscribable` says `Unknown`, and the inbox changes nothing, when the directory of a shared database is older than `keys.directory_max_age` (60 seconds without the block `keys`) and when the account is not in the directory. An account that has no permission is a positive no. This includes a database account in the mode `static`, whose stored grants do not count in this mode.
- **Pending alerts.** The removal changes the table of subscriptions only. The deliveries that exist stay, and `pending_deliveries` does not look at the subscriptions.

### Pages
- **Routes.** All the pages are under `/auth/settings/inbox`. The old routes `/auth/inbox/api/*` are gone with their tests. The functions that the pages call are the ones of `src/inbox/admin.rs`, called in process.
- **Overview.** It needs `alert.read`, `device.read` or `connection.read`, and also `alert.send`. An account that only sends alerts needs a page for its form.
- **Impersonation.** A session that the superadmin opened as an account can read and make a code (this was already so). It cannot send an alert and cannot revoke a phone.
- **Revoke.** A phone is revoked with `device.revoke` and `device.read`, or by its owner who holds `group.subscribe`. A request for a phone of another account answers like a request for a phone that does not exist.
- **Dedupe of the form.** The form of an alert carries a random key. A double click sends one alert. The source of the alert is `admin`.
- **Tests.** The end-to-end tests read the pages. The rows of the tables carry `data-*` attributes for them (`data-device`, `data-state`, `data-alert`, `data-reason` and others). The tests of the first steps that read the JSON overview now read the page **Phones**. The filter `since_ms` of the old alert list is gone, and so is the field `live.ip`.
- **Accounts of the tests.** A database account has no permission of the inbox at the start. The tests give `group.subscribe` on the two groups of the test server.
