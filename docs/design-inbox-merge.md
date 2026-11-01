# Design: the inbox inside the portal

Status: accepted by the owner on 2026-11-01. This page records the design of the merge, the choices that the work added, and what stays open. The code is the reference when this page and the code disagree.

The order of the work was: 1 this design; 2 the merged repository and the builds; 3 the hyper rewrite and the common store; 4 the rights, the pages and the keys as producers; 5 protocol 4 and enrollment by account; 6 the alerts of the portal; 7 the build and release files, and the demo with the emulator; 8 the documents. The word "step" below names the step that made a choice.

## 1. Goal

The portal and Inbox become one binary, `portal` (version 5.0.0). One account gives one identity and one set of permissions for the launcher, the keys and the inbox.

## 2. Decisions of the owner

The third column gives the answers and the changes that the work made later.

| # | Decision | Later answer or change |
|---|---|---|
| D1 | One binary. Inbox becomes the module `inbox`. | Step 3 moved the code to `src/inbox/`. |
| D2 | In production, PostgreSQL and at least two active instances. SQLite stays for development. The standby replica of SQLite stays. | See section 8. |
| D3 | A producer is a key of the portal or a token of the file. Both work at the same time. | Section 6 gives the order of the checks. This is an exception to 11.2 of the portal specification, and the register records it. |
| D4 | A phone enrolls through an account of the portal. The pseudonym is the login. The shared password is gone. | Step 5 made protocol 4 (section 5). An account needs `group.subscribe` to make a code. |
| D5 | The service `inbox` has the resource `group:<id>`. Its actions are `group.subscribe`, `alert.send`, `alert.read`, `device.read`, `device.revoke` and `connection.read`. | Section 6. |
| D6 | A limit can apply to `alert.send`. A group marked `critical` never has a limit. | The limit is the budget `alerts`. Section 6. |
| D7 | The admin front has no mode without a password. The pages are in the style of the portal, in the tab "Inbox". | Section 7. The JSON admin API is gone. |
| D8 | The code lives in the private repository `portal`. The history of Inbox is kept. The repository Inbox is archived at the end. | The history is in the git log. The archive is not done yet. |
| D9 | All HTTP code uses hyper. Inbox is written again on it. | Step 3. |
| D10 | The configuration follows the global rules. The delivery values have no default. | Section 9. |
| D11 | The binary has a listener `devices`. The DaemonSet `inbox-edge` relays it. | Section 4. |
| D12 | The portal sends an alert when a service is unhealthy and when sign-ins fail in a burst. | Step 6. Section 10. |
| D13 | Tests use the emulator. No physical phone. | `tools/android/emulator.sh`. |

## 3. Layout of the repository

- The root package `portal` is the binary. The code of the inbox is in `src/inbox/`.
- `crates/inbox-core` holds the protocol and the cryptography of the phones, with no I/O. The server and the app share it.
- `crates/inbox-android` is the JNI bridge. `android/` is the app.
- `crates/inbox-server` is gone. `docker/Dockerfile.inbox`, `deploy/inbox/` and the Inbox image are gone too.
- `docker/Dockerfile` builds the one image. `deploy/` holds the Compose files and the K3s manifests of the merged product.
- The history of Inbox is in the git log of the repository.

## 4. Network

The binary has up to four listeners.

1. `server.listen_address`: the pages of the portal, behind Traefik ForwardAuth. The admin pages of the inbox are here. The metrics of the portal and of the inbox are here, at `metrics.path`. No separate metrics listener exists.
2. `pdp.listen_address`: the AuthZEN PDP. Only the services reach it.
3. `inbox.devices.listen`: phones, producers, webhooks and the health routes. No session. Phones and producers prove themselves.
4. `inbox.cluster.listen`: the copies of alerts between instances. It exists with the block `inbox.cluster` only.

The listeners of the inbox have a cap: `server.max_connections` plus two times `inbox.devices.max_devices` connections. The old server had no cap. The DaemonSet `inbox-edge` relays the devices listener on a host port of each node.

## 5. Protocol 4 of the phones

[`inbox/protocol.md`](inbox/protocol.md) is the wire contract. This section gives the design and the server choices.

### 5.1 Enrollment

Enrollment uses one secret of 128 bits that an account makes in the tab "Inbox". The text code holds the server address, the public key of the server and the secret. The deep link `inbox://enroll?s=<address>&k=<key>&c=<secret>` holds the same text.

1. The phone starts Noise IK with the pinned key of the server. It sends the secret and its name in the first payload.
2. The server checks the digest of the secret. A secret works one time and lives `inbox.enrollment.code_ttl`.
3. The server stores the key of the phone and links it to the account. The phone then uses the normal session.

The pinned key replaces OPAQUE. The crate `opaque-ke` leaves the build. The earlier design note used `inbox://enroll#<code>`. The query form is the contract.

### 5.2 Server choices (step 5)

- **New keys.** `inbox.devices.public_address` is mandatory. It is the `s` of the code. It follows the address rules of the app, but only the schemes `http` and `https` are valid. The block `inbox.enrollment` is optional, because both keys have a default: `code_ttl` (`10m`, from `1m` to `24h`) and `max_active_per_account` (5, from 1 to 100).
- **Removed.** The key `inbox.devices.credential` and the command `portal inbox credential` do not exist. The key file holds the Noise key pair only. A key file of protocol 3 is not read.
- **Migration 7 of the inbox chain.** It adds the table `enrollment_codes(digest, login, created_at_ms, expires_at_ms, used_at_ms)`. The digest is the SHA-256 digest of the secret. The migration does not touch the devices of protocol 3. They cannot connect: the server answers 4426. Their rows keep their old pseudonym as `user_name`. They stay in the overview until an operator revokes them.
- **Retention.** The task of retention deletes a code 24 hours after its expiry. A code that is used or expired is refused at once, so the delay only keeps the rows for an operator.
- **One transaction.** The inbox marks the code as used and stores the device in one transaction. A refusal changes nothing. The check of `inbox.devices.max_devices` comes before the use of the code, so the code stays valid when the limit is reached.
- **Replication.** The creation of a code and an enrollment wait for the snapshot when `inbox.replica.sync_alerts` is true. If the snapshot fails, the inbox does not give the code, and it does not send message 2. The new device is revoked, and the phone needs a new code. With `sync_alerts = false`, the failure is only logged.
- **Close codes.** A refusal because of `max_devices` is 4410, like all the other refusals of the code. The event `enroll_refused` has the real reason in the field `reason`: `unknown_code`, `used_code`, `expired_code`, `too_many_devices`, `unreadable_message`, `unsupported_protocol`, `text_frame`, `no_message`, `rate_limited` or `replica_sync_failed`. A storage error closes with 1011, and the code stays unused. The rate limit closes with 4429 after the upgrade, as `/v1/session` does. The server does not answer the HTTP status 429 to the upgrade.
- **Rate limit.** The limiter of the address counts each connection that sends a first frame, also a refused one. A success clears the count of the address. A connection that sends no frame counts after 10 seconds.
- **Call for the codes.** Step 4 replaced the JSON call `POST /auth/inbox/api/enrollment-codes` with the button **Make a code** of the page **My phones** (`POST /auth/settings/inbox/code`). The function that makes the code is the same. A session that the superadmin opened as another account makes a code for that account.
- **Deletion of an account.** The portal calls the inbox before it deletes the account. If the inbox fails, the account stays. A device that enrolls while the account is deleted either finds its code gone or is revoked afterwards. The call needs the inbox only when the block `inbox` exists. File accounts cannot be deleted, so they never reach this call.
- **Copies.** `portal inbox import-sqlite` and the snapshot of the replica carry the table `enrollment_codes`.
- **Old files.** The command `import-sqlite` accepts only a file at the current version of the inbox chain. A file of the standalone Inbox is older, so the command refuses it. The owner has no phone in use, so no migration exists.

## 6. Rights

The inbox is the second internal service of the portal, after the service `portal`. The PDP of the portal decides in process. The action document comes from the code. The groups of the file are its resources.

### 6.1 The service

- **Types of the actions.** `group.subscribe`, `alert.send` and `alert.read` list only the type `group`. A permission on the whole service is not valid for them: the editor gives one permission for each group. A group that the file adds later needs a new permission. `device.read`, `device.revoke` and `connection.read` apply to the service.
- **Name of the limit.** The limit of `alert.send` is the budget `alerts`, not `max_alerts`. A name that starts with `max_` is a ceiling by the protocol, and a ceiling has no window. A budget has windows (second, minute, hour, day). The value is on the permission `access on service:inbox`, like the budgets of each other service. The counters are the counters of `counting.rs`. Step 4 adds no second mechanism.
- **What counts as a use.** One request of a key is one use. A webhook that holds many alerts is one use. A request that an `Idempotency-Key` repeats is a use too, because the decision comes before the check of duplicates. The form of the page counts for an account that is not the superadmin. The superadmin has no limit.
- **Critical group.** The key `inbox.group.<id>.critical` (boolean, default `false`) marks a group. A critical group skips the count. The inbox does not call the counter for it, so no memo of refusal can stop it either.
- **Dependencies.** The service has no `needs` rule like the service `portal`. The page **Phones** needs `device.read`. **Revoke** needs `device.revoke` and `device.read` together.
- **Catalogue.** The portal adds the service `inbox` to each catalogue that discovery gives, when the file has the block `inbox`. It is in the group "Portal" with the icon `tb:bell`. It owns no path, so the gate does not change. The id `inbox` is reserved for workloads in all cases, also without the block `inbox`, as the id `portal` is.
- **Tile.** The tile **Inbox** shows for the superadmin. It also shows for an account that holds a permission that opens a page of the tab. The permission `access` alone does not open a page. It links to `/auth/settings/inbox`.

### 6.2 Producers

- **Two kinds of token.** A token of the file is a block `producer`. A key of the portal starts with `mcp_`. The inbox looks at the tokens of the file first, in constant time. Only a token that is none of them goes to the check of a key. A token of the file therefore never waits for the portal.
- **Interface.** `crate::inbox::rights::Authorizer` has four calls: `check_key`, `decide`, `subscribable` and `directory_stamp`. The portal implements it in `src/http/inbox_rights.rs`. It gives it with `Slot::set_authorizer` right after the start of the inbox. The implementation holds only a weak reference to the state. Before this call, a key gets `503 directory_not_ready`.
- **Decision.** `decide` calls the decision of the PDP in process (`decide_key_for` in `pdp.rs`). It is the same code as for a PDP client, with the same counters. The log has the line `inbox_key_decision`.
- **Answers.** `403 forbidden_group` for a decision no. `401` for a text that is not a key, or a key that is not valid. `429 rate_limited` with `Retry-After`. `503 directory_not_ready` when the directory is too old. `503 rights_unavailable` when the count fails.
- **Status of an alert.** `GET /api/v1/alerts/{id}` also accepts a key with `alert.read` on the group of the alert. A key that may not read the group gets `404`, as a token of the file does.
- **Resolved.** A key never sends the notifications resolved of Alertmanager. The key `webhook_send_resolved` belongs to the blocks `producer`.

### 6.3 Subscriptions

- **Welcome.** The message lists the groups that the account may subscribe to. When the portal cannot tell, it lists all the groups, and nothing changes.
- **Subscribe.** A group that the account may not subscribe to gets the error `forbidden_group`. The error `unavailable` means that the portal cannot tell. A new subscription never rests on a guess. The protocol version stays 4.
- **Removal.** The task `rights::run` of the inbox looks at `Directory::generation` every 2 seconds. When the number changes, it reads the phones once and removes the subscriptions that a loaded directory refuses. At each `welcome`, the inbox does the same for this phone. Each instance of a PostgreSQL installation does it for all phones. The removal is idempotent.
- **Safety rule.** `PortalAuthorizer::subscribable` says `Unknown`, and the inbox changes nothing, in two cases. The directory of a shared database is older than `keys.directory_max_age` (60 seconds without the block `keys`). Or the account is not in the directory. An account that has no permission is a positive no. This includes a database account in the mode `static`, whose stored grants do not count in this mode.
- **Pending alerts.** The removal changes the table of subscriptions only. The deliveries that exist stay, and `pending_deliveries` does not look at the subscriptions.

## 7. Pages

- **Routes.** All the pages are under `/auth/settings/inbox`. The old routes `/auth/inbox/api/*` are gone with their tests. The functions that the pages call are in `src/inbox/admin.rs`, called in process.
- **Overview.** It needs `alert.read`, `device.read` or `connection.read`, and also `alert.send`. An account that only sends alerts needs a page for its form.
- **Impersonation.** A session that the superadmin opened as an account can read and make a code. It cannot send an alert and cannot revoke a phone.
- **Revoke.** A phone is revoked with `device.revoke` and `device.read`, or by its owner who holds `group.subscribe`. A request for a phone of another account answers like a request for a phone that does not exist.
- **Dedupe of the form.** The form of an alert carries a random key. A double click sends one alert. The source of the alert is `admin`.
- **Tests.** The end-to-end tests read the pages. The rows of the tables carry `data-*` attributes for them (`data-device`, `data-state`, `data-alert`, `data-reason` and others). The tests of the first steps that read the JSON overview now read the page **Phones**. The filter `since_ms` of the old alert list is gone, and so is the field `live.ip`.
- **Accounts of the tests.** A database account has no permission of the inbox at the start. The tests give `group.subscribe` on the two groups of the test server.
- **Lost with the old front.** The embedded front, the single page of Inbox and the page `/doc` are gone. The page **Documentation** replaces `/doc`.

## 8. Storage

- **Two chains.** The portal and the inbox have independent chains of migrations, not one chain. The owner chose this for step 3. The portal keeps its version in `PRAGMA user_version` (SQLite) and in `schema_migrations` (PostgreSQL). The inbox keeps its version in the table `inbox_schema_version`. Each chain refuses a schema that is newer than the binary.
- **One database.** The inbox uses the SQLite file or the PostgreSQL database of the portal. It has no `postgres` block. `inbox.storage.pool_size` (default 8) sets its own pool.
- **Table names.** The tables of the inbox keep their old names: `devices`, `subscriptions`, `alerts`, `deliveries`, `connections`, `enrollment_codes` and, with PostgreSQL, `instances`. They do not collide with the tables of the portal. A rename would change every query of the delivery path for no gain.
- **Clients.** The inbox keeps its asynchronous PostgreSQL client (`tokio-postgres` and `deadpool-postgres`) and its SQLite writer thread. The portal keeps its synchronous client. One connection of the inbox listens for `NOTIFY`.
- **Old files.** An old SQLite file of the standalone inbox cannot open in place: its `PRAGMA user_version` is the version of the old chain. `portal inbox import-sqlite` copies a file at the current inbox version into an empty PostgreSQL database. The command also reads a SQLite file of the portal. There is no copy from SQLite to SQLite.
- **Replica.** The snapshot is a copy of the whole SQLite file. It holds the tables of the portal too. A standby instance that takes over gets the accounts and the keys of the portal with the data of the inbox. An alert, an enrollment and a revocation wait for the snapshot when `sync_alerts` is true.
- **Database of K3s.** The K3s configuration uses the database `portal`. A database `inbox` that exists can stay unused.

## 9. Configuration

The block `inbox` joins the file of the portal. It holds these items: the name, the key file of the server, the groups and the token producers. It also holds the blocks `devices`, `enrollment` and `storage`. The optional blocks are `replica`, `cluster` and `portal_alerts`. The delivery values (`heartbeat_interval`, `session_timeout`, `resend_after`, `max_inflight`, `unreachable_after`, `offline_warning_after`, `delivery_ttl`, `alert_retention`, `connection_log_retention`) are mandatory and have no default. Every other key has a documented default, accepts `_env` and `_file` where it is a secret, and shows in `--print-config`. The README has the generated reference.

- **Blocks without a label.** The HCL parser accepts blocks without a label inside the block `inbox` (`devices`, `enrollment`, `storage`, `replica`, `cluster`, `portal_alerts`). It still refuses nested blocks everywhere else.
- **Secrets.** `server_key_file` and `token_file` are secrets: each accepts `_file` and `_env`. The program does not trim the value. A key file that `portal inbox keygen` made has one final line end, and the program removes it.
- **Keys of the old server.** `server.shutdown_grace`, `logging.format`, the block `telemetry`, the block `admin` and `limits.max_request_bytes` do not exist any more. The shutdown of the portal replaces `shutdown_grace`. The JSON log of the portal replaces the log format. `inbox.devices.max_request_bytes` replaces `limits.max_request_bytes`. `inbox.devices.max_auth_failures` and `inbox.devices.auth_failure_window` replace the block `security`. `inbox.devices.trusted_proxies` replaces `server.trusted_proxies`.
- **Defaults.** Only the delivery values have no default. The block `inbox.replica` has defaults. `sync_alerts` has the safe default `true`.
- **Example.** `deploy/config.example.hcl` is a complete configuration of the portal with the inbox.

## 10. Alerts of the portal

Decision D12 gives the scope. Step 6 made these choices where the owner left them open.

- **Block.** `inbox.portal_alerts` is optional. Without it, nothing runs and the metrics have no series. Its keys and defaults are in the README: `group` (mandatory), `service_unhealthy`, `unhealthy_after` (`2m`), `sign_in_burst`, `sign_in_failures` (10), `sign_in_window` (`5m`), `repeat_after` (`1h`) and `recovered`.
- **Checks at start.** `group` must be the label of a block `group`. The message names the key, the groups that exist and the value. The start also stops when `service_unhealthy` and `sign_in_burst` are both `false`: the block would send nothing.
- **Internal sender.** The task calls `accept_alert` of the inbox directly with the source `portal`. The path is the path of an API alert. It has one transaction, the idempotency key, the copy of the replica and the spool of the degraded mode. There is no token and no HTTP request. The source `portal` does not collide with `producer:<name>`, `webhook:<name>`, `notify:<name>` or `admin`.
- **Detectors.** Two small state machines in `src/portal_alerts.rs` decide. They take a monotonic time from the caller, so a test moves the clock. A task reads the catalogue and the counter of refused sign-ins once each second. The durations use the monotonic clock, so a jump of the wall clock does not start or stop an alert.
- **State of a service.** The rules are the rules of the tile: `down` and `unhealthy` are unhealthy, `starting` is not unhealthy, and a service with no state is never unhealthy. A service that starts, or that has no state, breaks the unhealthy time. The owner did not say this. The reason is that a restart must not look like an outage.
- **Recovery.** The portal sends `healthy again` only after it sent an unhealthy alert, and only once. A service that only starts again does not count as healthy. A service that leaves the catalogue is forgotten with no alert.
- **Sign-in burst.** The task reads the sum of three counters that exist. They count a wrong password or unknown login, a refusal by the limiter of the address, and a refusal by the global limiter. The sign-in path has no new code. After an alert, the count restarts from zero. The same refusals never give a second alert when `repeat_after` is shorter than `sign_in_window`.
- **Title.** The title has the values of the configuration, not the measured time: `Service Notes is unhealthy for 2 minutes`. The inbox answers `409` when two alerts have the same key and two titles. The sender counts a `409` as a duplicate. Two instances with two configurations, during an update, then do not try again without end. A name of a service is cut to 64 characters and has no control character. A login, an address and a password are never in a title.
- **Idempotency key.** The keys are `portal:unhealthy:<service id>:<bucket>`, `portal:recovered:<service id>:<bucket>` and `portal:burst:<bucket>`. The bucket is the Unix time divided by `repeat_after`. An id that makes the key longer than 128 bytes gives the first 16 bytes of its SHA-256 digest in hexadecimal. Two instances build the same key from the same event, so the inbox accepts one alert.
- **Limits of the key.** Two instances that read one event on two sides of a bucket limit send two alerts. A wall clock that goes back can give a key that the inbox already used, and the inbox then drops the alert as a duplicate.
- **Per instance.** Each instance counts the refused sign-ins that it sees. A burst that is spread on several instances can stay under the threshold on each of them. A shared counter would need a table and a write for each refusal. The owner can ask for it later.
- **Memory.** The state of the detectors is in memory. A restart forgets it. The portal then waits `unhealthy_after` again, and the key stops a second alert in the same bucket. It does not send `healthy again` for an alert of an earlier process.
- **Failure.** If the inbox refuses an alert or does not answer in 20 seconds, the portal logs `portal_alert_failed`. It counts the failure in `portal_alerts_failed_total{kind}`. It tries again after 10 seconds. The detectors keep the alert due until the inbox accepts it. The login path and the launcher do not wait for the task. The task never panics on a send error.
- **Metrics.** `portal_alerts_sent_total{kind}` counts the alerts that the inbox accepted. A duplicate that another instance already stored counts as sent, and the log event `portal_alert_sent` has the field `duplicate`.
- **Start.** The task starts after the portal has a complete catalogue, in the same list as the listeners, and it ends with them.
- **Test.** The end-to-end tests use a fake Docker socket for the unhealthy service. The smallest `unhealthy_after` is `10s`, so these tests take about one minute each. The test with two instances needs PostgreSQL (`INBOX_TEST_PG`).
- **Not in this version.** An alert when a key comes near its budget.

## 11. Process

- **Start.** The inbox starts before the portal waits for its catalogue. An alert does not wait for the discovery.
- **Stop.** At the signal, the inbox answers `503` at `/readyz` and tells the phones to reconnect (close code 1012) at once. It serves 5 more seconds like the portal. Then it closes the listeners. It sends the close code again to the sessions that started in the meantime. It waits for the sessions (10 seconds at most). It writes what its memory holds to the database.
- **Lease loss.** A replica that loses its lease stops the whole process, because the SQLite file is shared with the portal. The process ends with an error after the last snapshot.
- **Standby wait.** Before the portal opens the SQLite file, a standby instance waits for the lease. A thread with its own small runtime watches SIGTERM and SIGINT during this wait. It replaces the signal handlers of the old code and needs no unsafe code.
- **Host name.** The inbox does not read `HOSTNAME`. It reads the host name from `/proc/sys/kernel/hostname`, for the table of the instances only.
- **Telemetry.** The inbox has no OpenTelemetry export. Its metrics are in the Prometheus text of the portal with the same names. The crates `opentelemetry*`, `tracing` and `tracing-subscriber` are not in the build. The inbox writes its log with small macros that give the JSON lines of the portal. The spans are gone. The events `device_connected` and `device_disconnected` keep the fields that the old span gave (`device.id`, `device.name`, `user.name`, `client.address`). The task of the statistics sets the Prometheus gauges every 15 seconds, as before.
- **Comments.** The comments of the moved code are the old comments. The new modules and the changed interfaces have comments in Simplified Technical English. A full rewrite of the old comments is not done.

## 12. Build and release

The commands are in the section "Build and release" of the README. The files are in `deploy/README.md`. Step 7a made these choices where the owner left them open.

- **One version.** The root package and the workspace packages have the version 5.0.0. `tools/version.sh` reads the root package and stops when the workspace version differs. `tools/build.sh`, `tools/android/build-apk.sh`, `tools/release/local-release.sh` and the release workflow call it. The Gradle `versionName` is this version. The `versionCode` is `major * 10000 + minor * 100 + patch`, so 5.0.0 gives 50000. It is larger than every code that the old app used.
- **One image.** `docker/Dockerfile` builds on `rust:1.98-alpine3.24`. Alpine is musl, so the build needs no target and no cross compiler. The packages `musl-dev gcc make perl binutils` are enough for SQLite and for `aws-lc-sys`. CMake is not needed. There is no `rust-toolchain.toml`. The Dockerfile builds natively for each architecture. A `buildx` build for another architecture runs the compiler under QEMU or on a native builder.
- **Layers.** A layer holds the dependencies only. It uses the manifests and stub sources. A change of the sources rebuilds the crates of the repository, not the dependencies. `RUSTFLAGS` has only `--remap-path-prefix`. An explicit `crt-static` would break the proc macros on a musl host.
- **Runtime.** The image has `/tmp` (mode 1777) and `/var/lib/portal` (owner 10001). The process writes nowhere else. The image has no icon library. The pages show initials when `ui.icons_dir` is absent. A deployment that wants the icons mounts the library at `/icons`.
- **Inbox image removed.** `docker/Dockerfile.inbox` and its ignore file are gone. A root `.dockerignore` limits the context to `src`, `crates`, `web` and the manifests. `docker/Dockerfile` copies `crates/`, because the portal depends on `inbox-core`.
- **Compose.** `docker-compose.yml` at the root has the portal and PostgreSQL. It builds the image when it is absent. The secrets are files in a directory that `deploy/compose/init-secrets.sh` makes (`PORTAL_SECRETS_DIR` changes the directory). The script uses the image for `portal inbox keygen` and `portal inbox token`. There is no shared device password and no credential file. The file `.env.example` is gone, because the stack has no environment secrets.
- **Two instances.** `deploy/compose/docker-compose.ha.yml` adds a second instance and Traefik. It is a test aid. It needs no new image.
- **K3s.** `deploy/k3s/` has one Deployment with two replicas in the namespace `edge`, where the portal runs in the homelab. The inbox has no namespace of its own any more. The Services are `portal`, `portal-pdp`, `portal-devices` and `portal-peers`. The relay `inbox-edge` keeps its name and host port 8087, and it runs in `edge`. The probes use `/readyz` of the devices listener, because the portal has no `/readyz`.
- **CI.** `ci.yml` has four jobs: the policy, the Rust checks, the image with the configuration files and the Compose smoke test, and the Android compile check. `release.yml` replaces both release workflows. It builds each architecture on a native runner. It has no step that touches a registry.
- **Version of the release.** A tag `vX.Y.Z` starts the release. The old manual start with an independent version is gone.
- **No push.** No command and no workflow pushes an image to a registry. A release holds binaries, the signed APK and `SHA256SUMS`.

## 13. Deviations from the first plan

The first plan had one chain of migrations, one admin front without a password, OPAQUE, and an OpenTelemetry export. These are the points where the work did something else. The section that gives the details is in the second column.

| Deviation | Section |
|---|---|
| Two independent chains of migrations, not one. The first plan said "schema 7 adds the inbox tables". | 8 |
| The tables of the inbox keep their old names. | 8 |
| An old SQLite file of the standalone inbox cannot open in place. | 8 |
| The snapshot of the replica holds the tables of the portal too. | 8 |
| The inbox keeps its asynchronous PostgreSQL client. | 8 |
| The HCL parser accepts labelless blocks inside `inbox`. | 9 |
| Secrets `server_key_file` and `token_file`. Keys of the old server removed. | 9 |
| Only the delivery values have no default. | 9 |
| The inbox starts before the catalogue. | 11 |
| The listeners of the inbox have a cap on connections. | 4 |
| Shutdown, lease loss and standby wait. | 11 |
| No OpenTelemetry export. The log uses small macros. | 11 |
| Enrollment uses the pinned key, not OPAQUE. Protocol 4 server choices. | 5 |
| Alerts of the portal: internal sender, bucketed keys, per-instance count. | 10 |
| The limit is the budget `alerts`, not `max_alerts`. | 6 |
| Three actions list the type `group` only. | 6 |
| The inbox checks the tokens of the file before it checks a key. | 6 |
| Subscriptions follow permissions with a safety rule. | 6 |
| The pages are under `/auth/settings/inbox`, and the JSON admin API is gone. | 7 |
| One image, one version, K3s files only. | 12 |
| The metrics have no listener of their own. | 4 |

## 14. Open points

- **Licence.** The licence of the merged code is open. The root package says `MIT OR Apache-2.0`. The shared crates say `LicenseRef-Proprietary`.
- **QR code.** A QR code for enrollment needs a camera library in the app. It is not in this version. The code is text or a link.
- **Arm64 image.** The release builds the arm64 binary on a native runner. The arm64 image is not built or tested here.
- **K3s files.** The tests of the manifests are a render with kustomize and `check-config`. No cluster was used. The SQLite variant of the manifests is a comment only.
- **Group-wide permissions.** `group.subscribe`, `alert.send` and `alert.read` list only the type `group`. An account needs one permission for each group. The owner can add the type `service` to the three actions.
- **Upgrade hazard of `group.subscribe`.** An account that has no `group.subscribe` loses its subscriptions at the first check with a loaded directory. The enrollment code also needs this permission. Give the permission before the inbox goes live. The decision records `owner-decision-2026-11-01-inbox-step4-rights.md` holds this point.
- **Impersonation.** A session that the superadmin opens as an account can make an enrollment code for that account. The phone then belongs to that account. The owner has not reviewed this point.
- **Shared counter of refused sign-ins.** Each instance counts alone. A burst spread on several instances can stay under the threshold.
- **Alert near a budget.** A key that comes near its budget raises no alert in this version.
- **Physical phone.** The tests use the emulator only. The owner chose no physical phone.
- **Archive of Inbox.** D8 says that the repository Inbox is archived at the end. This is not done.
- **IngressRoute of the tile.** The IngressRoute `inbox` in `deploy/k3s/ingress.yaml` has `portal.href: /auth/inbox/`. No page serves this path. The portal lists its own tile (section 6.1), so this route should be fixed or removed.
- **Old comments.** The comments of the moved code are not rewritten in Simplified Technical English.
