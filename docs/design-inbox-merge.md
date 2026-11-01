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
- **Call for the codes.** `POST /auth/inbox/api/enrollment-codes` answers `201` with the JSON object of the design. The body must be `{}`. The call and `GET /auth/inbox/api/csrf` are open to any signed-in account. The other calls stay for the superadmin. A session that the superadmin opened as another account makes a code for that account.
- **Deletion of an account.** The portal calls the inbox before it deletes the account. If the inbox fails, the account stays. A device that enrolls while the account is deleted either finds its code gone or is revoked afterwards. The call needs the inbox only when the block `inbox` exists. File accounts cannot be deleted, so they never reach this call.
- **Copies.** `portal inbox import-sqlite` and the snapshot of the replica carry the table `enrollment_codes`.
- **Old files of the standalone inbox.** The command `import-sqlite` accepts only a file at the current version of the inbox chain. A file from before step 5b must start once with this binary to migrate.
- **Deploy files.** `deploy/inbox/config.example.hcl` is now a complete configuration of the portal with the inbox. The other files of `deploy/inbox/` still describe the standalone binary. A later step replaces them.

### Alerts of the portal
This section lists what step 6 does, and the choices that the owner left open. Decision D12 gives the scope. A key that comes near its budget is not in this version.

- **Block.** `inbox.portal_alerts` is optional. Without it, nothing runs and the metrics have no series. Its keys and defaults are in the README: `group` (mandatory), `service_unhealthy`, `unhealthy_after` (`2m`), `sign_in_burst`, `sign_in_failures` (10), `sign_in_window` (`5m`), `repeat_after` (`1h`) and `recovered`. The block is a plain block inside `inbox`, like `inbox.replica`.
- **Checks at start.** `group` must be the label of a block `group`. The message names the key, the groups that exist and the value. The start also stops when `service_unhealthy` and `sign_in_burst` are both `false`: the block would send nothing. The other keys have no condition on each other.
- **Internal sender.** The task calls `accept_alert` of the inbox directly with the source `portal`. The path is the path of an API alert: one transaction, the idempotency key, the copy of the replica and the spool of the degraded mode. There is no token and no HTTP request. The source `portal` does not collide with `producer:<name>`, `webhook:<name>` or `admin`.
- **Detectors.** Two small state machines in `src/portal_alerts.rs` decide. They take a monotonic time from the caller, so a test moves the clock. A task reads the catalogue and the counter of refused sign-ins once each second.
- **State of a service.** The rules are the rules of the tile: `down` and `unhealthy` are unhealthy, `starting` is not, and a service with no state is never unhealthy. A service that is starting, or that has no state, breaks the unhealthy time. The owner did not say this. The reason is that a restart must not look like an outage.
- **Recovery.** The portal sends `healthy again` only after it sent an unhealthy alert, and only once. A service that only starts again does not count as healthy. A service that leaves the catalogue is forgotten with no alert.
- **Sign-in burst.** The task reads the sum of three counters that exist: wrong password or unknown login, blocked by the limiter of the address, and blocked by the global limiter. The sign-in path has no new code. A refusal that a limiter blocks counts, as the task says. After an alert, the count restarts from zero. The same refusals never give a second alert when `repeat_after` is shorter than `sign_in_window`.
- **Title.** The title has the values of the configuration, not the measured time: `Service Notes is unhealthy for 2 minutes`. The reason is that the inbox answers `409` when two alerts have the same key and two titles. The sender counts a `409` as a duplicate. Two instances with two configurations, during an update, then do not try again without end. A name of a service is cut to 64 characters and has no control character. A login, an address and a password are never in a title.
- **Idempotency key.** The keys are `portal:unhealthy:<service id>:<bucket>`, `portal:recovered:<service id>:<bucket>` and `portal:burst:<bucket>`. The bucket is the Unix time divided by `repeat_after`. An id that makes the key longer than 128 bytes gives the first 16 bytes of its SHA-256 digest in hexadecimal. Two instances build the same key from the same event, so the inbox accepts one alert. The limit of this choice: two instances that read one event on two sides of a bucket limit send two alerts. A wall clock that goes back can give a key that the inbox already used, and the inbox then drops the alert as a duplicate. The durations use the monotonic clock, so a jump of the wall clock does not start or stop an alert.
- **Per instance.** Each instance counts the refused sign-ins that it sees. A burst that is spread on several instances can stay under the threshold on each of them. A shared counter would need a table and a write for each refusal. The owner can ask for it later.
- **Memory.** The state of the detectors is in memory. A restart forgets it. The portal then waits `unhealthy_after` again, and the key stops a second alert in the same bucket. It does not send `healthy again` for an alert of an earlier process.
- **Failure.** If the inbox refuses an alert or does not answer in 20 seconds, the portal logs `portal_alert_failed`, counts it in `portal_alerts_failed_total{kind}` and tries again after 10 seconds. The detectors keep the alert due until the inbox accepts it. The login path and the launcher do not wait for the task. The task never panics on a send error.
- **Metrics.** `portal_alerts_sent_total{kind}` counts the alerts that the inbox accepted. A duplicate that another instance already stored counts as sent, and the log event `portal_alert_sent` has the field `duplicate`.
- **Start.** The task starts after the portal has a complete catalogue, in the same list as the listeners, and it ends with them.
- **Test.** The end-to-end tests use a fake Docker socket for the unhealthy service. The smallest `unhealthy_after` is `10s`, so these tests take about one minute each. The test with two instances needs PostgreSQL (`INBOX_TEST_PG`).
