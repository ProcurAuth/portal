# Design: the Inbox inside the portal

Status: accepted by the owner on 2026-11-01. The rework of 2026-11-02 changed the names, the key kinds, the scope of the permissions and the settings pages. This page is the one design document of the Inbox. It records the decisions of the owner, the choices that the work added, the deviations and the open points. The code is the reference when this page and the code disagree.

The work had eight steps: 1 the design; 2 the merged repository and the builds; 3 the Hyper rewrite and the common store; 4 the rights, the pages and the keys; 5 protocol 4 and enrollment by account; 6 the alerts of the portal; 7 the build and release files, and the demo with the emulator; 8 the documents. The rework followed in batches A to F. A and B renamed the feature. C made one scope, two key kinds and the switches. D1 and D2 made the settings pages. E made the Android app. F wrote the documents. The word "step" or "batch" below names the stage that made a choice.

The plan of the rework is `demo/design-proposals/rework/REWORK-PLAN.md`. The specification of the portal and the decision records are in the repository of the specification. The specification has the same requirements in its section 15. [`inbox/SPEC.md`](inbox/SPEC.md) is the specification of the Inbox.

## 1. Goal

The portal and the earlier standalone alert server became one binary, `portal` (version 5.0.0). One account gives one identity and one set of permissions for the launcher, the keys and the Inbox. The feature has the name **Inbox**. It sends alerts to Android phones now. It will carry questions and approvals later (section 17).

## 2. Decisions of the owner

The third column gives the answers and the changes that the work made later.

| # | Decision | Later answer or change |
|---|---|---|
| D1 | One binary. The alert server becomes a module of the portal. | Step 3 moved the code to a module. Batch A named it `src/inbox/`. |
| D2 | In production, PostgreSQL and at least two active instances. SQLite stays for development. The standby replica of SQLite stays. | See section 10. |
| D3 | A sender is a key of the portal or a token of the file. Both work at the same time. | Batch C replaced the token of the file with the key of the file (the block `api_key`). Section 7 gives the order of the checks. The exception to 11.2 of the specification is removed. |
| D4 | A phone enrolls through an account of the portal. The pseudonym is the login. The shared password is gone. | Step 5 made protocol 4 (section 5). An account needs `inbox.receive` to make a code. |
| D5 | The Inbox has the resource `group:<id>`. Six actions. | Batch C moved them to the service `portal`. The type is `inbox_group`. The actions are `inbox.receive`, `alert.send`, `alert.send_user`, `alert.read`, `phone.read`, `phone.revoke` and `phone.log`. Section 6. |
| D6 | A limit can apply to `alert.send`. A group marked `critical` never has a limit. | The limit is the budget `alerts`. Section 6. |
| D7 | The admin front has no mode without a password. The pages are in the style of the portal, in the tab Inbox. | Section 9. The JSON admin API is gone. |
| D8 | The code lives in the private repository `portal`. The history of the standalone project is kept. The standalone repository is archived at the end. | The history is in the git log. The archive is not done yet (section 16). |
| D9 | All HTTP code uses Hyper. The alert server is written again on it. | Step 3. |
| D10 | The configuration follows the global rules. The delivery values have no default. | Section 11. |
| D11 | The binary has a listener `devices`. A relay DaemonSet publishes it. | Changed on 2026-11-01 by the owner in two steps. First the API of the senders moved to the port of the pages. Then everything of the Inbox moved to the port of the portal. No phone listener and no relay exist. Section 4. |
| D12 | The portal sends an alert when a service is unhealthy and when sign-ins fail in a burst. | Step 6. The feature has the name "watch" (batch A). Section 12. |
| D13 | Tests use the emulator. No physical phone. | `tools/android/emulator.sh`. |
| D14 | The rework of 2026-11-02. | Section 2.1. |
| D15 | An alert has one target: a group or one account. | Section 2.2. The right is `alert.send_user`. |

### 2.1 D14: the answers of the rework

The owner answered these points. The plan of the rework lists them as answered.

- **Name.** The feature is **Inbox**. The kinds of message are Alert, Question and Approval. The app is "Portal Inbox", with the application id `app.portalinbox` and the deep link `portal-inbox://`.
- **Permissions.** One internal scope `portal` holds the features: the keys and the Inbox. No second internal service exists.
- **Keys.** Two kinds: `mcp_` and `api_`. The prefix `whk_` is reserved. A key follows its owner for both kinds. No service account exists. The rights to make and to use a key are by kind.
- **Switches.** The portal can run as a panel of services only. The switches are `keys.mcp`, `keys.api` and `users.allow_creation`, and the blocks `keys` and `inbox`.
- **Pages.** Four tabs: Users, Services, Keys and Inbox. Dense pages with panels and chips. The row action is variant A: a selection and **Revoke (N)**.
- **Roadmap.** Step 1 is alerts. Step 2 is agents that send alerts. Step 3 is questions and approvals. The one-way rule for alerts ends at step 3 for the new kinds (section 17). The field `kind` exists from step 1.
- **Open questions with defaults.** The plan lists questions Q1 to Q6. The defaults apply. File keys replace the token blocks. The owner of a file key is a file account. The CRC-32 covers the prefix. The page of an account tells the number of keys. The switch defaults are `keys.mcp = true`, `keys.api = false` and `users.allow_creation = true`. Agents use an `api_` key of their own at step 2.

### 2.2 D15: an alert to one account

The owner asked: "il faut pouvoir alerter un user ou un groupe directement". The owner approved the defaults of the questions Q9, Q10 and Q11 of the plan.

- **Target.** A message has one target: a group or an account. The store keeps two columns, `target_kind` (`group` or `user`) and `target_id` (the id of the group, or the login). The type in the code is `Target`, in `src/inbox/target.rs`. The field `kind` of the message stays `alert`.
- **Migration.** The production database can hold rows. The Inbox chain gets one more migration, for SQLite and for PostgreSQL. It renames the column `group_id` of the table `alerts` to `target_id` and adds `target_kind` with the default `group` and a check of its two values. The rows that exist stay valid: they are alerts to a group. The rule is the rule of the column `kind`: an additive migration at the end of the chain. The command `import-sqlite` copies both columns. The snapshot of the replica copies the whole file.
- **Right (Q9).** One flat action, `alert.send_user`, on `service:portal`, with no resource type. It lists the budget `alerts`, the same budget as `alert.send`. The cost is that a sender with this right can alert any account. The other choice was a resource type with one node for each account.
- **Level (Q10).** An alert to an account has the level Ring and is never critical. A critical group never counts a use, so a budget never limits it. An alert to an account has no group, so it always counts.
- **Read (Q11).** The sender, the target account and the superadmin read an alert to an account. A right on a group does not fit. The page of the Inbox applies this rule. The route `GET /api/v1/alerts/{id}` lets only the key that sent the alert read it: a key must not read what only its owner receives.
- **Delivery.** An alert to an account goes to each phone of the account that is not revoked, with no subscription. It uses the same spool, time to live, resend, receipt, rate limit and budget. An account with no active phone gets `409` with the code `no_phone`, and the Inbox stores nothing. The check comes before the count of the budget, so a refused alert uses no unit.
- **Existence of the account.** The check of the target is `keyauth::person_exists`. It is true for the superadmin, for an account of the file and for an account of the database. The API and the form of the page use it, so the two give the same answer. Before this fix, the check read the database only, and an account of the file got `unknown_user`. The login is the one key of the account in the phones, in the alerts and in the notifications, for the three kinds.
- **Protocol.** Protocol 4 does not change. The frame of an alert has `group` and `group_name`. A direct alert has the group id `@direct` and the group name `Direct`. The id of a group of the file has only the characters `a-z`, `0-9`, `-` and `_`, so the two never collide. The app applies one local level to this id (Ring by default). The change of the app is not part of this decision.
- **Routes.** `POST /api/v1/alerts` takes exactly one of `group` and `user`. `POST /api/v1/notify/{group}` and the webhook of Alertmanager do not change. The form of the page sends the field `to` with the value `group:<id>` or `user:<login>`.
- **Source.** The source of an alert from the form of the page is `admin:<login>`, so the alert has a sender. The source of a key is `producer:key:<id>`, as before.
- **File key.** A block `api_key` has the key `direct_alerts` (default `false`). With `true`, the owner must hold `alert.send_user`, and `groups` can be empty.
- **Metrics.** The counters `portal_inbox_alerts_accepted_total`, `portal_inbox_alerts_duplicate_total` and `portal_inbox_alerts_spooled_total` have the label `target` (`group` or `user`). The graph of the page counts the alerts to a group only.

## 3. Layout of the repository

- The root package `portal` is the binary. The code of the Inbox is in `src/inbox/`. The code of the alerts of the portal is in `src/watch.rs`.
- `crates/inbox-core` holds the protocol and the cryptography of the phones, with no I/O. The server and the app share it.
- `crates/inbox-android` is the JNI bridge. `android/` is the app.
- The crate of the standalone server is gone. Its Dockerfile, its deploy directory and its image are gone too.
- `docker/Dockerfile` builds the one image. `deploy/` holds the Compose files and the K3s manifests of the merged product.
- The history of the standalone project is in the git log of the repository.

## 4. Network

The binary has up to three listeners.

1. `server.listen_address`: the portal. It serves the pages (behind Traefik ForwardAuth where needed) and the page of the Inbox. It serves the metrics of the portal and of the Inbox at `metrics.path`. It also serves all the routes of the Inbox. The routes are `/v1/enroll` and `/v1/session` for the phones (WebSocket), `/api/v1/*` for the senders and the webhooks, `/api/version` and `/readyz`. These routes have no session and no cookie. Phones prove themselves with the Noise handshake. Senders prove themselves with an `api_` key. The Inbox is a feature of the portal, not a second service.
2. `pdp.listen_address`: the AuthZEN PDP. Only the services reach it.
3. `inbox.cluster.listen`: the copies of alerts between instances. It exists with the block `inbox.cluster` only.

### 4.1 One port (decision of 2026-11-01)

The owner gave this instruction: "`/api/v1/alerts` must be on the same port. The Inbox must not be a separate service but presented as a feature of the portal."
The owner first let the phones keep a second port. The owner then decided, and this is final: everything of the Inbox is served on the port of the portal. This replaces D11 and the earlier answer about a relay. The reason is that `/v1/enroll` and `/v1/session` are HTTP upgrade requests. The content is Noise-encrypted inside WebSocket frames. A reverse proxy carries them with path routing. A separate exposure, connection budget and retry need no second port. The decision records of the specification repository hold the text.

- **Routes.** The listener of the portal serves `GET /v1/enroll`, `GET /v1/session`, `POST /api/v1/alerts`, `GET /api/v1/alerts/{id}`, `POST /api/v1/notify/{group}`, `POST /api/v1/webhooks/alertmanager/{group}`, `GET /api/version` and `GET /readyz`. The function `inbox::serve` answers the paths that `inbox::owns` names: the prefixes `/v1/` and `/api/v1/`, `/readyz` and `/api/version`. The portal checks these paths before every other route. A unit test checks every route constant of the portal against `owns`. A path under `/v1/` that the Inbox does not know gets 404. A request that is not a GET to a route of the phones gets 405.
- **No page rules.** The portal applies no session, no CSRF check, no origin check, no security header and no cookie to these routes. The upgrade answer is a plain 101. The connection of the portal uses `with_upgrades`, so a WebSocket can leave the connection task.
- **Removed.** These items do not exist any more: the phone listener and its accept loop, the key `inbox.devices.listen`, and the relay DaemonSet with its host port. The Service of the relay and its policies are gone too. So are the publication of the phone port in Compose and in the demo, and the second port of the Dockerfile `EXPOSE`. A file that still has `inbox.devices.listen` stops at start with the error "unknown configuration key". There is no alias.
- **Public address.** `inbox.devices.public_address` is the public base URL of the portal as the phones reach it: `http(s)://host[:port][/prefix]`. The `s` of the enrollment code holds it unchanged.
- **Prefix.** The portal has no base path key. It matches its routes at the root and assumes that the proxy removes a prefix before it sends the request. The app keeps the prefix of the address and appends `/v1/enroll` or `/v1/session`. A portal under a prefix works when the proxy removes the prefix (the Traefik middleware `StripPrefix`). The shipped files use no prefix.
- **Connection budget.** The pages and the API use `server.max_connections`. A connection holds one permit of this budget while it is a page connection. When it upgrades to a phone session, the connection task ends and the permit goes back. The phone session has taken a permit of the session budget before the answer 101. The session budget is `server.max_connections` plus two times `inbox.devices.max_devices`. The first term covers the phones that do not finish the handshake. The second term covers one live session and one session that the server did not detect as lost, for each phone. The old phone listener had no cap.
- **Refusal of a phone.** A phone that cannot get a permit gets HTTP 503 with `Retry-After: 5`, before the upgrade. No WebSocket exists for it, so a flood of refused attempts costs no session and no frame work, and Traefik does not retry an answer. The refusal by rate limit keeps the close code 4429 after the upgrade. A permit goes back when the session ends, also when the upgrade fails.
- **Client address.** There is one source: `server.trusted_proxies` and the function `transport::client_ip` of the portal. The limiter of the Inbox and the count of the enrollment attempts use this address. The key `inbox.devices.trusted_proxies` of the first merge was removed. The old function also read `X-Real-IP` and accepted two `X-Forwarded-For` headers. The portal function ignores both.
- **Retry.** Traefik has the middleware `retry` (4 attempts, initial interval 100 ms) on the router of the Inbox routes. It replaces the quick retry of the relay. The demo, the HA Compose proxy and the K3s IngressRoute `portal-inbox` have it. The IngressRoute also has a `ServersTransport` with a dial timeout of 1 second. Traefik retries only when the server does not answer at all. A WebSocket upgrade that got 101 is not repeated, and a 503 of the session budget is final. A check with Traefik in front confirmed that an upgrade through the middleware gives 101.
- **Start.** With the block `inbox`, the portal listener opens before the catalogue is complete. It answers the routes of the Inbox at once. Every other route answers `503` with `starting` until the catalogue is complete, so the gate never decides with an incomplete catalogue (portal 7.3b). Without the block `inbox`, the ports open as before. An alert and a phone never wait for the catalogue.
- **Stop.** The Inbox sends the close code 1012 to each phone at once. The portal serves 5 more seconds and waits at most 10 seconds for the connections. The Inbox waits for the sessions.
- **Readiness.** `GET /readyz` gives `200` `ready`, `200` `degraded` with PostgreSQL and no primary, and `503` `draining` after the signal. Without the block `inbox`, the portal answers `200`. `portal healthcheck` reads `/healthz` and the Inbox `/readyz` of the portal port. The startup probe of K3s reads `/healthz`, and its readiness probe reads `/readyz` of the portal port.
- **Proxy rule.** The proxy must send `/v1/`, `/api/v1/` and `/api/version` to the portal without ForwardAuth. The portal owns the prefix `/v1/`. A service that the proxy publishes on the same host must not use a path that starts with `/v1/`.
- **Behaviours that are lost.** Four items are lost. They are the separate phone port, the relay with its quick retry and its host port, and the own list of trusted proxies of the Inbox. The phone port had its own exposure. The phones and the pages now share the host and the TLS certificate. The operator restricts the phones with a rule on the routes of the proxy.

### 4.2 No knowledge of TLS

The portal has no TLS code on any listener. Its listeners speak plain HTTP. A reverse proxy (Traefik) terminates TLS and sends plain HTTP on a private network. The portal reads the scheme from `X-Forwarded-Proto` and the host from `X-Forwarded-Host`. It trusts these headers from the networks in `server.trusted_proxies` only. HTTPS gives the session cookie its `__Host-` name and its `Secure` attribute. Noise protects the phones on plain HTTP. An `api_` key travels in clear text on plain HTTP. The operator terminates TLS in the proxy for the senders and uses an HTTPS `inbox.devices.public_address`. The PDP has no TLS either, and the deployment limits its network.

## 5. Protocol 4 of the phones

[`inbox/protocol.md`](inbox/protocol.md) is the wire contract. This section gives the design and the server choices.

### 5.1 Enrollment

Enrollment uses one secret of 128 bits that an account makes in the tab Inbox. The text code holds the server address, the public key of the server and the secret. The deep link `portal-inbox://enroll?s=<address>&k=<key>&c=<secret>` holds the same text.

1. The phone starts Noise IK with the pinned key of the server. It sends the secret and its name in the first payload.
2. The server checks the digest of the secret. A secret works one time and lives `inbox.enrollment.code_ttl`.
3. The server stores the key of the phone and links it to the account. The phone then uses the normal session.

The pinned key replaces OPAQUE. The crate `opaque-ke` is not in the build. An earlier design note used `portal-inbox://enroll#<code>`. The query form is the contract.

### 5.2 Server choices (step 5)

- **New keys.** `inbox.devices.public_address` is mandatory. It is the `s` of the code. Only the schemes `http` and `https` are valid. The block `inbox.enrollment` is optional, because both keys have a default: `code_ttl` (`10m`, from `1m` to `24h`) and `max_active_per_account` (5, from 1 to 100).
- **Removed.** The key `inbox.devices.credential` and the command `portal inbox credential` do not exist. The key file holds the Noise key pair only. A key file of protocol 3 is not read.
- **Migration 7 of the Inbox chain.** It adds the table `enrollment_codes(digest, login, created_at_ms, expires_at_ms, used_at_ms)`. The digest is the SHA-256 digest of the secret. The migration does not touch the phones of protocol 3. They cannot connect: the server answers 4426. Their rows keep their old pseudonym as `user_name`. They stay in the list until an operator revokes them.
- **Retention.** The task of retention deletes a code 24 hours after its expiry. A code that is used or expired is refused at once. The delay only keeps the rows for an operator.
- **One transaction.** The Inbox marks the code as used and stores the phone in one transaction. A refusal changes nothing. The check of `inbox.devices.max_devices` comes before the use of the code, so the code stays valid when the limit is reached.
- **Replication.** The creation of a code and an enrollment wait for the snapshot when `inbox.replica.sync_alerts` is true. If the snapshot fails, the Inbox does not give the code, and it does not send message 2. The new phone is revoked, and the person needs a new code. With `sync_alerts = false`, the failure is only logged.
- **Close codes.** A refusal because of `max_devices` is 4410, like all the other refusals of the code. The event `enroll_refused` has the real reason in the field `reason`: `unknown_code`, `used_code`, `expired_code`, `too_many_devices`, `unreadable_message`, `unsupported_protocol`, `text_frame`, `no_message`, `rate_limited` or `replica_sync_failed`. A storage error closes with 1011, and the code stays unused. The rate limit closes with 4429 after the upgrade, as `/v1/session` does. The server does not answer the HTTP status 429 to the upgrade.
- **Rate limit.** The limiter of the address counts each connection that sends a first frame, also a refused one. A success clears the count of the address. A connection that sends no frame counts after 10 seconds.
- **The call for the codes.** The plus button of the zone **My phones** (or **Phones**) of the tab Inbox is a form. It sends `POST /auth/settings/inbox/code`. The answer is the page with the code in a popover that stays open until **Done**, with the header `Cache-Control: no-store`. A session that the superadmin opened as another account makes a code for that account. The earlier JSON call and the page that showed a code are gone.
- **Deletion of an account.** The portal calls the Inbox before it deletes the account. If the Inbox fails, the account stays. A phone that enrolls while the account is deleted either finds its code gone or is revoked afterwards. The call needs the Inbox only when the block `inbox` exists. File accounts cannot be deleted, so they never reach this call.
- **Copies.** `portal inbox import-sqlite` and the snapshot of the replica carry the table `enrollment_codes`.
- **Old files.** The command `import-sqlite` accepts only a file at the current version of the Inbox chain. A file of the standalone alert server is older, so the command refuses it. The owner has no phone in use, so no migration exists.

### 5.3 The field `kind` (batch B)

Each alert frame has the field `kind`. The value is `alert`. The core parser refuses any other value and a frame without the field. The table `alerts` has the column `kind`, with the default `alert`. It is the last migration of the Inbox chain, for SQLite and for PostgreSQL. The command `import-sqlite` copies the column. The snapshot of the replica copies the whole file, so it carries it too. The rows of the page have the field `kind`, and the table of alerts has the attribute `data-kind`. The metrics `portal_inbox_alerts_accepted_total`, `portal_inbox_alerts_duplicate_total` and `portal_inbox_alerts_spooled_total` have the label `kind`, and, since D15, the label `target`. The protocol version stays 4.

The metric `portal_inbox_auth_failures_total` already had a label `kind` that names the kind of failure. It is not the kind of a message. The name stays, to keep the meaning of the metric.

### 5.4 The name of the instance

The key `server.name` has the default `"Portal"`. It has 1 to 64 bytes, is not blank and has no control character. The phones show it as the name of the server: the enrollment answer and the message `welcome` hold it. No page of the portal shows it yet.

## 6. Rights: one scope

The portal has one internal scope for its features: the service `portal`. The PDP of the portal decides in process. The action document comes from the code and follows the file. The groups of the file are the nodes `inbox_group:<id>`. The first plan had a second internal service for the Inbox. Batch C removed it. The service id `inbox` is free for a workload again. The settings pages show the scope as the group **Portal features**. The launcher and the page **Services** never list it.

### 6.1 The actions

| Action | Applies to | Needs | Limit |
|---|---|---|---|
| `keys.own.read`, `keys.own.edit`, `keys.own.revoke` | service | `edit` and `revoke` need `keys.own.read` | none |
| `keys.own.create`, `keys.own.use` | `key_kind` | `create` needs `keys.own.read` | `use`: the ceiling `max_key_lifetime` |
| `users.read`, `keys.all.read`, `keys.all.revoke` | service | as before (portal 6.3) | none |
| `inbox.receive` | `inbox_group` | none | none |
| `alert.send` | `inbox_group` | none | the budget `alerts` |
| `alert.send_user` | service | none | the budget `alerts` |
| `alert.read` | `inbox_group` | none | none |
| `phone.read` | service | none | none |
| `phone.revoke` | service | `phone.read` | none |
| `phone.log` | service | none | none |

- **Presence.** The document follows the file. Without the block `keys`, it has no action of the keys and no `users.read`. Without the block `inbox`, it has no action of the Inbox. A block `user` that names an action that the document does not hold stops the start.
- **Types.** `inbox.receive`, `alert.send` and `alert.read` list only the type `inbox_group`. `alert.send_user` is flat: it lists the type `service`, and a grant on a group is not valid for it (D15). A permission on the whole service is not valid for them: the editor gives one permission for each group. A group that the file adds later needs a new permission. The resource type `inbox_group` has the parent `service`. A group receives all kinds of message. Step 3 adds actions, not types.
- **Names.** The names share a prefix. The editor groups the actions by the first part of the name.
- **Text of a grant.** `alert.send on service:portal/inbox_group:infra`. No migration reads an older text or an older action name. The seed of a demo database sets the new grants.
- **Name of the limit.** The limit of `alert.send` is the budget `alerts`, not `max_alerts`. A name that starts with `max_` is a ceiling by the protocol, and a ceiling has no window. A budget has windows (second, minute, hour, day). The value is on the permission `access on service:portal`, like the budgets of each other service. For this service, `access` opens no path: it carries the budget only. The editor offers the budget only when the file has the block `inbox`. The counters are the counters of `counting.rs`. No second mechanism exists.
- **What counts as a use.** One request of a key is one use. A webhook that holds many alerts is one use. A request that an `Idempotency-Key` repeats is a use too, because the decision comes before the check of duplicates. The form of the page counts for an account that is not the superadmin. The superadmin has no limit.
- **Critical group.** The key `critical` of a block `group` (boolean, default `false`) marks a group. A critical group skips the count. The Inbox does not call the counter for it, so no memo of refusal can stop it either. The editor shows a mark next to the group.
- **Dependencies.** `phone.revoke` needs `phone.read`. The other actions of the Inbox need no other action. The area **Phones** of the page needs `phone.read`.
- **Entry.** The launcher has no tile for the Inbox (changed on 2026-11-01). The superadmin opens the pages with the gear. An account that holds a permission that opens a part of the page gets the gear too, and it links to `/auth/settings/inbox`. The permission `access` alone does not open a part.
- **Reserved id.** The id `portal` is reserved for workloads in all cases. The id `inbox` is not reserved any more.

## 7. Keys: two kinds

A key has exactly one kind. The prefix of the text gives the kind. The kind decides who verifies the key.

| Prefix | Kind | Verified by | At the other verifier |
|---|---|---|---|
| `mcp_` | MCP key, for agents | The key edge, `/auth/verify-key` | `401`, no detail |
| `api_` | API key, for services, webhooks and callers such as Grafana | In process, by the API of the Inbox on the portal port | `401`, no detail |
| `whk_` | Reserved | Nobody | Refused as an unknown prefix |

- **Text.** The prefix, 43 random base62 characters and 6 base62 characters. The last 6 are the CRC-32 (zlib, polynomial 0xEDB88320) of the prefix and the 43 characters. A changed prefix fails before any lookup. A verifier refuses an unknown prefix, a bad length, a bad character and a bad checksum before any lookup. Then it checks the kind. All refusals are 401 with no detail. The portal keeps the SHA-256 digest and a hint of 4 characters. No key of an earlier form is read.
- **A key follows its owner.** This holds for both kinds. The rights of a key are the rights of the key and the current rights of its owner. The portal has no service account. When the owner loses a right, the key loses it at once. Other instances follow within `keys.directory_max_age`. The check also needs `keys.own.use` on the kind of the key.
- **Delete.** When an account of the database is deleted, its keys, their grants and their counters go with it (`ON DELETE CASCADE`). The page of the account names the number of keys: "Delete carol and 3 keys?". A pane that is open for a key of a deleted owner closes at the next directory version.
- **By kind.** `keys.own.create` and `keys.own.use` apply to the type `key_kind`, with the nodes `key_kind:mcp` and `key_kind:api`. The pane of a new key shows only the kinds that the account may make. With one kind, the selector is absent. With none, **New key** is absent. The ceiling `max_key_lifetime` has one value for each kind.
- **What a key holds.** An MCP key holds grants on services and no action of the portal. An API key holds `alert.send` and `alert.read` on groups, `alert.send_user` on the service, and the grant `access on service:portal` for the budget `alerts`. An API key holds no grant on a service, because no gate opens for it. The form and the save refuse it.
- **The edge reads headers only** (portal 2). A translator that reads an MCP body and calls a REST route cannot live in the edge. The portal is itself the service of the Inbox, so it can read bodies. A generic translator for other services is a later and separate component.
- **Migration.** The column `kind` of the table `keys` is the schema version 7 of SQLite and of PostgreSQL. A key of an earlier version is an MCP key. The plan said "no old data". A migration is still safe and small.
- **The kind is in the address.** `GET /auth/keys/new?kind=api` shows the pane of this kind. A selector with two or more kinds is a small GET form that loads the pane again. It works without script. The summary row of a revoked key has no kind, so the list shows the kind chip for the keys that exist only.
- **Key of the file of a service.** A block `api_key "<name>"` of the block `inbox` replaced the earlier token block, with no alias. It names an owner, which is the login of an account of the file, a secret by `_env` or `_file`, and the groups. The owner of a file key is a file account (portal 3.4a). The earlier token of 32 printable characters is not valid: the text must be an API key (`portal inbox token` makes one).
<<<<<<< ours
- **A file key decides its groups at start.** The key follows its owner, a file account. The rights of a file account do not change while the server runs. The start checks the secret and keeps only the groups that the owner may send to. The check of the key reads no directory and no database. A key of the file works when the portal is not ready and when PostgreSQL is away. It has no budget, as the earlier tokens had none. The list of keys does not list it. The file is its only source.
- **Check at start.** The secret has the form of an API key. The owner holds `keys.own.use` on the kind `api` and `alert.send` on each group. A fault stops the program. The error names the key and the expected value, and never the secret.
=======
- **A file key decides its groups at start.** The key follows its owner, a file account. The rights of a file account do not change while the server runs. The start checks the secret and keeps only the groups that the owner may send to. The check of the key reads no directory and no database. A key of the file works when the portal is not ready and when PostgreSQL is away. It has no budget, as the earlier tokens had none. The list of keys does not list it. The file is its only source.
- **Check at start.** The secret has the form of an API key. The owner holds `keys.own.use` on the kind `api` and `alert.send` on each group, and `alert.send_user` when the block has `direct_alerts = true`. A fault stops the program. The error names the key and the expected value, and never the secret.
>>>>>>> theirs
- **The MCP key of the file.** A block `key` of a block `user` has the kind `mcp`. A key of the file that uses `key_sha256_env` or `key_sha256_file` is an MCP key, because the portal cannot read the prefix of a digest. The text is checked only when the file gives it in clear.
- **The file needs a mode.** A block `user` is not valid in the mode `dynamic`. A file with a file key needs `access.grants_mode` set to `static` or `hybrid`. The shipped configurations use `hybrid`.
- **Order of the checks of a sender.** The API of the Inbox checks the form and the kind of the text first. It then looks at the keys of the file in constant time. Only a text that is none of them goes to the check of a key of the database. A key of the file never waits for the portal.
- **Interface.** `crate::inbox::rights::Authorizer` has four calls: `check_key`, `decide`, `subscribable` and `directory_stamp`. The portal implements it in `src/http/inbox_rights.rs`. It gives it with `Slot::set_authorizer` right after the start of the Inbox. The implementation holds only a weak reference to the state. Before this call, a key of the database gets `503 directory_not_ready`.
- **Decision.** `decide` calls the decision of the PDP in process (`decide_key_for` in `pdp.rs`). It is the same code as for a PDP client, with the same counters. The log has the line `inbox_key_decision`.
- **Answers.** `403 forbidden_group` for a decision no. `401` for a text that is not a key, a key that is not valid, or a key of another kind. `429 rate_limited` with `Retry-After`. `503 directory_not_ready` when the directory is too old. `503 rights_unavailable` when the count fails.
- **Status of an alert.** `GET /api/v1/alerts/{id}` also accepts a key with `alert.read` on the group of the alert. A key that may not read the group gets `404`, as a key of the file does.
- **Resolved.** A key of the database never sends the notifications resolved of Alertmanager. The key `webhook_send_resolved` belongs to the blocks `api_key`.

## 8. Subscriptions

- **Welcome.** The message lists the groups that the account may subscribe to. When the portal cannot tell, it lists all the groups, and nothing changes.
- **Subscribe.** A group that the account may not subscribe to gets the error `forbidden_group`. The error `unavailable` means that the portal cannot tell. A new subscription never rests on a guess. The protocol version stays 4.
- **Removal.** The task `rights::run` of the Inbox looks at `Directory::generation` every 2 seconds. When the number changes, it reads the phones once and removes the subscriptions that a loaded directory refuses. At each `welcome`, the Inbox does the same for this phone. Each instance of a PostgreSQL installation does it for all phones. The removal is idempotent.
- **Safety rule.** `PortalAuthorizer::subscribable` says `Unknown`, and the Inbox changes nothing, in two cases. The directory of a shared database is older than `keys.directory_max_age` (60 seconds without the block `keys`). Or the account is not in the directory. An account that has no permission is a positive no. This includes a database account in the mode `static`, whose stored grants do not count in this mode.
- **Pending alerts.** The removal changes the table of subscriptions only. The deliveries that exist stay, and `pending_deliveries` does not look at the subscriptions.

## 9. Pages

### 9.1 Shell and tabs

- **Shell.** The `body` of a settings page has the class `shell`. The header and the row of tabs stay at the top. The element `main` is the one scroller of the page. It has `scrollbar-gutter: stable`. The lists inside a page have no reserved gutter.
- **Four tabs at most.** The tabs are Users, Services, Keys and Inbox. `Frame::tabs` builds the list from the file and the viewer. A tab that the file turns off is not in the page. A panel-only file has two tabs. The tab Activity and its page are gone, with their route, their view, their tests and their style.
- **Components.** The code is in `src/ui/components.rs`. Each function returns HTML and escapes its text. The style is in `web/settings.css`. The script is in `web/settings.js`. `Place.groups` draws the two groups of the editor side by side. `sprite.svg` has all the icons of the mockups. The page builder inlines the symbols that its markup names. `zone` can have a part between its head and its list that does not scroll (`above_html`, for the graph), and a head with no title. `popover_with` makes a popover whose opener is a field, a button or an icon. `split_zones` places a main zone and a stack, or one zone alone. The box of the bulk action is named `<field>.<id>`.

| Part | Function | Markup | Without script |
|---|---|---|---|
| Flat icon link | `icon_link` | `a.ib` with `aria-label` and `title` | A link. |
| Flat icon post | `icon_post` | `form.inline > button.ib` | A form. |
| Icon with a count | `icon_count` | `span.feat` | A cell. |
| Count chips | `chips` | `span.chips > a.chip[.is-on]` with `aria-current` | A link with a query parameter. A chip of zero is absent. |
| Panel | `zone`, `zone_bare` | `section.zone > .zone__head + .scroll` | A section with its own scroller. |
| Toolbar | `toolbar` | `.tbar` | A row. |
| Two columns | `split` | `.split.split--users`, `--services`, `--even`, with `.stackcol` | A grid. It stacks under 1000 px. |
| Table | none (markup) | `table.grid` with `col.w28` to `col.w180`, `.r`, `tr.is-selected`, `a.rowLink` | A table. `a.rowLink` covers its row. |
| Popover | `popover` | `details.pop > summary.ib + .pop__menu` | A `details` element. |
| Confirm in place | `ask` | `details.ask > summary.ib + form.ask__panel` | A `details` element that holds a form. |
| Bulk action | `bulk_form`, `bulk_box`, `bulk_all_box`, `bulk_button` | `form.bulk`, `input.bulk__box[form]`, `details.bulk__ask[data-bulk]` | A form. The button has no count. |

- **Strength of a confirmation.** `Strength::Light` has one confirm button. `Strength::Firm` asks the person to type the name of the resource. The field has a `pattern`, so the browser blocks a wrong name without script. The account page uses `Firm` when the account has keys.
- **Side of a confirmation.** `Side::End` opens the question at the left of the cross. `Side::Start` opens it at the right. A button at the end of a toolbar uses `End`.
- **The bulk action.** The form is empty. The boxes and the button name it with the attribute `form`. With script, the button is absent until a row is ticked. It then shows "Revoke (3)". A click on it asks "Revoke 3 phones?" in the same place. `settings.js` keeps the count. The form sends one field `id.<id>` with the value `1` for each ticked phone or key. A form never repeats a name. The routes `POST /auth/settings/inbox/revoke` and `POST /auth/keys/revoke` revoke each id that the account may revoke. The answer is a redirect with a notice.
- **Removed code.** The styles `.stats`, `.stat*`, `.pageBack`, the old sub-navigation and the old pages, the unused icons, the function `icon_count_link` and the `allow(dead_code)` of `src/ui/components.rs` are gone.

### 9.2 Users, account, new user, Services

- **Users.** The accounts stand on the left. The panels **Sign-ins** and **Recent changes** stand on the right. The sign-in log and the audit events are the data of these panels. The store gives the counts of the chips: `LoginPage.total` and `LoginPage.refused`.
- **New user.** The button opens `/auth/settings/new-user`. This is the page of an account in create mode. The login, the password and the groups stand in one row. The editor of grants is under them. `POST /auth/users/add` accepts the grants of the editor with the field `grants=1`. It makes the account and its grants in one request. A request without `grants=1` makes an account with no grant, as before. A refused try returns to the create page with a code. The route answers 404 when `users.allow_creation` is `false` or the grants are read-only. The form that made accounts on the page of the accounts is gone.
- **The account page.** One row of tools: log in as, the password (a popover), the activity (a popover), delete (a confirmation in place), two facts, Save. Under it, the groups **Services** and **Portal features** stand side by side. An account of the file has a page too. Its grants have a lock and the page changes nothing.
- **Services.** The table has a selection by link: `?svc=<id>`. The panel **Opened by** shows who opens the selected service and who does not. The page of one service is gone. Its route answers 404, with no redirect. The route `/auth/settings/give` stays: the plus link of the panel uses it. The route `/auth/users/portal-rights` is gone. The grants of the features post to `/auth/users/update`.

### 9.3 Keys

The list is on the left, with a box and a kind icon in each row. The form of a key is a pane on the right, with four states: closed, new, change and "key made". With the pane closed, the toolbar has **New key**. With the pane open, the toolbar button is absent, and the head of the pane has **Cancel** and the action. `GET /auth/keys/new` and `GET /auth/keys/edit?id=<id>` open the page with the pane. The page of the text of a key is gone. The pane shows the text one time, with **Copy** and **Done**. The pane of an API key has the matrix of the Inbox and the budget. The pane of an MCP key has the services only. The key keeps an address: the pane is not a modal.

### 9.4 Inbox

- **One page.** The tab is one page, `/auth/settings/inbox`, for each account that holds any permission of the Inbox. It has the zone **Sent** on the left and the zones **Phones** and **Connections** on the right. The toolbar has the period, the kinds of message, the book icon and **Send alert**. Each part needs its permission and is absent without it. An account that holds only `inbox.receive` has the zone **My phones** and the plus button.
- **Removed pages.** Seven pages are gone, with their routes, their styles and their tests. They are the overview, the phones, the connections, my phones, the documentation, the page of one alert and the page that shows a code. Each address under `/auth/settings/inbox` other than the page answers `404`. A `GET` of a form route answers `405`. The routes `/auth/inbox/api/*` and the JSON admin API are gone. The functions that the page calls are in `src/inbox/admin.rs`, called in process.
- **Filters are links.** A chip is a link with a query parameter (`p`, `g`, `a`, `ps`, `pp`, `mine`, `cs`, `login`, `open`). The README lists them. The period picker and the row of an alert use links too. Nothing needs script.
- **Alert in place.** The row of an alert links to `?open=<id>#a-<id>`. The page draws the detail under the row: the phones that do not have the alert, then a few that have it. The close link keeps the place in the list.
- **Graph.** One store query counts the alerts for each bucket of time and each group (`alert_counts`, SQLite and PostgreSQL, one test each). The page keeps the groups that the account may read and draws the bars as an inline SVG. The critical groups are on top, in the warning colour.
- **Code.** The plus button is a form (section 5.2). With script, **Done** removes the popover. Without script, **Done** is a link to the page.
- **Impersonation.** A session that the superadmin opened as an account can read the page and make a code. It cannot send an alert and cannot revoke a phone.
- **Revoke.** A phone is revoked with `phone.revoke` and `phone.read`, or by its owner who holds `inbox.receive`. A request for a phone of another account answers like a request for a phone that does not exist.
- **Dedupe of the form.** The form of an alert carries a random key. A double click sends one alert. The source of the alert is `admin:<login>` (see D15).
- **Tests.** The end-to-end tests read the page. The rows of the tables carry `data-*` attributes for them (`data-device`, `data-state`, `data-alert`, `data-reason` and others). The filter `since_ms` of the old alert list is gone, and so is the field `live.ip`. A database account has no permission of the Inbox at the start. The tests give `inbox.receive` on the two groups of the test server. The test `an_api_key_sends_alerts_with_the_permissions_and_the_limit_of_its_owner` blocked the runtime while it waited for the start of a clock minute, and the phone session closed with the code 1001. The test now waits first with an async sleep and then opens the phone session.
- **Lost with the old front.** The embedded front, the single page of the standalone server and the page `/doc` are gone. The book icon with its popover replaces `/doc`.

### 9.5 The matrices

The editor of the account and the pane of a key draw the portal features as plain rows and compact matrices. The parts are **Keys**, **Key kinds**, **Inbox** and **Phones**. **Key kinds** has the longest lifetime of each kind. **Inbox** has a row for each group, with the columns Receive, Send and Read, and the budget of the alerts. The data of the editor has the parts `services` and `features`, and two fields: `needs` for each action of the service `portal`, and `critical` for each node that is a critical group. A feature that the file turns off has no action and no node, so the editor draws nothing for it. The kind API comes before the kind MCP, and the critical groups come last. The editor writes the grant `access on service:portal` only when a window has a value. It removes the grant when no window has one. A warning mark "no access permission" ignores the service `portal`. Any other service has a switch, and a right on it ticks the switch (see "Editor of grants" in the root `README.md`). The matrices need script. Without script, the page lists the grants as text, and the pane of a key has text fields for the grants.

### 9.6 Deviations from the mockups

- **Dates.** A time is `11-02 01:39`, the form that `settings.js` writes in the time zone of the browser. The year comes first (`2025-11-02 01:39`) only when it is not the current year. The mockup shows "Nov 1 02:58".
- **Labels of the changes.** The panel keeps the labels of the audit kinds ("Permissions changed", "Account added"). The mockup has other words. The audit kinds did not change.
- **Request limits.** The row of a service has no fields "Requests". The limit `requests` of `access` shows in the list of the actions of the service, as before.
- **The count of accounts.** "3 of 64" counts the accounts of the database. The table also lists the superadmin and the accounts of the file.
- **Opened by.** The panel has no row for the superadmin. A service that no account opens shows "Superadmin only". The accounts that do not open the service have a plus link. The mockup does not draw them.
- **Services table.** "Actions" counts the actions that the service publishes. The lists of the actions and of the resource types, which were folds in the old table, are gone. The editor of the account shows them. The paths open without sign-in stay as a badge.
- **Filter and selection.** A chip and a row are links. A click loads the page again. The row has an anchor, so the list keeps its place. No script filters in place.
- **Firm confirmation.** The mockup has one kind of confirmation. The account page asks for the login before it deletes an account that has keys.
- **Activity popover.** It also has the latest sessions and the services used. The mockup has three figures.
- **Hybrid and static mode.** The old banners are gone. A grant of the file has a lock in the editor.
- **No new user in static mode.** The button is absent in the mode `static`, and when `users.allow_creation` is `false`. The start already refuses the two together. The page checks both.
- **Narrow windows.** Under 1000 px the panels stack and the page scrolls. A panel is 60 vh high at most.
- **Mine on the phones.** It is a chip with a user icon, as the mockup draws it. The Keys page has the segmented filter Mine and All.
- **Connections.** The table has no column End. The last column shows the reason, or "open". The end time is in the tooltip. The zone is too narrow for two time columns.
- **Origin.** A manual alert shows "account": the alert keeps the source `admin`, not the login. The name of a key comes from the directory. A key that was revoked shows "API key".
- **Sent has a bound.** The list holds the 500 newest alerts of the period, and the count shows `500+` when older ones exist. The state chips and the group counts come from these rows. The graph counts every alert. A shorter period shows the older alerts.
- **Code popover.** It shows the fingerprint and the end of the code. It does not show the count of open codes: the store has no such query. The popover opens by a new page, not in place.
- **Send form.** A critical group comes last in the list, so the form never starts on it. One group gives no list. The form has no character counter.
- **Row of an alert.** A click loads the page again. One detail is open at a time.
- **Kind of a key in the pane.** With one kind, the title names it ("New MCP key") and no selector shows. A change shows the kind as a fact, not as a disabled control. The selector lists API first. A new pane starts with the first kind that the account may make. A pane for an account that may make both kinds starts on MCP.
- **Key list.** "Make a key like this one" has no button in a row. The address `?copy=<id>` of the new pane still works. The column "Can use" of an MCP key shows the services and the count of permissions.
- **Editor without script.** The matrices need script (section 9.5).
- **Budget.** The editor writes the grant `access on service:portal` only when a window has a value.

## 10. Storage

- **Two chains.** The portal and the Inbox have independent chains of migrations, not one chain. The owner chose this for step 3. The portal keeps its version in `PRAGMA user_version` (SQLite) and in `schema_migrations` (PostgreSQL). The Inbox keeps its version in the table `inbox_schema_version`. Each chain refuses a schema that is newer than the binary. The earlier version table of the Inbox was renamed in the rework, and the code makes no migration.
- **One database.** The Inbox uses the SQLite file or the PostgreSQL database of the portal. It has no `postgres` block. `inbox.storage.pool_size` (default 8) sets its own pool.
- **Table names.** The tables of the Inbox keep their old names: `devices`, `subscriptions`, `alerts`, `deliveries`, `connections`, `enrollment_codes` and, with PostgreSQL, `instances`. They do not collide with the tables of the portal. A rename would change every query of the delivery path for no gain.
- **Clients.** The Inbox keeps its asynchronous PostgreSQL client (`tokio-postgres` and `deadpool-postgres`) and its SQLite writer thread. The portal keeps its synchronous client. One connection of the Inbox listens for `NOTIFY`.
- **Old files.** An old SQLite file of the standalone alert server cannot open in place: its `PRAGMA user_version` is the version of the old chain. `portal inbox import-sqlite` copies a file at the current Inbox version into an empty PostgreSQL database. The command also reads a SQLite file of the portal. There is no copy from SQLite to SQLite.
- **Replica.** The snapshot is a copy of the whole SQLite file. It holds the tables of the portal too. A standby instance that takes over gets the accounts and the keys of the portal with the data of the Inbox. An alert, an enrollment and a revocation wait for the snapshot when `sync_alerts` is true.
- **Database of K3s.** The K3s configuration uses the database `portal`. A database `inbox` that exists can stay unused.
- **Retention.** The Inbox deletes an alert after `inbox.storage.alert_retention` and the connection log after `connection_log_retention`. A delivery expires after `delivery_ttl`.

## 11. Configuration and switches

The block `inbox` joins the file of the portal. It holds these items: the key file of the server, the groups and the `api_key` blocks. It also holds the blocks `devices`, `enrollment` and `storage`. The optional blocks are `replica`, `cluster` and `watch`. The delivery values (`heartbeat_interval`, `session_timeout`, `resend_after`, `max_inflight`, `unreachable_after`, `offline_warning_after`, `delivery_ttl`, `alert_retention`, `connection_log_retention`) are mandatory and have no default. Every other key has a documented default, accepts `_env` and `_file` where it is a secret, and shows in `portal check-config`. The README has the generated reference.

- **Blocks without a label.** The HCL parser accepts blocks without a label inside the block `inbox` (`devices`, `enrollment`, `storage`, `replica`, `cluster`, `watch`). It still refuses nested blocks everywhere else.
- **Secrets.** `server_key_file` and the `secret_file` of an `api_key` are secrets: each accepts `_file` and `_env`. The program does not trim the value. A key file that `portal inbox keygen` made has one final line end, and the program removes it.
- **Keys of the old server.** `server.shutdown_grace`, `logging.format`, the block `telemetry`, the block `admin` and `limits.max_request_bytes` do not exist any more. The shutdown of the portal replaces `shutdown_grace`. The JSON log of the portal replaces the log format. `inbox.devices.max_request_bytes` replaces `limits.max_request_bytes`. `inbox.devices.max_auth_failures` and `inbox.devices.auth_failure_window` replace the block `security`. The portal key `server.trusted_proxies` gives the client address for the Inbox.
- **Defaults.** Only the delivery values have no default. The block `inbox.replica` has defaults. `sync_alerts` has the safe default `true`.
- **Names.** The block is `inbox`. The block `inbox.watch` replaces the earlier block of the alerts of the portal. The key `name` of the block does not exist. The key `server.name` replaces it. An old name is an unknown name. `check-config` refuses a file with an old block or an old sub-block, and tests cover both cases. The names of the secrets, the variables, the objects of Compose, K3s and Traefik, and the variables of the Android build use the name `inbox`.
- **Example.** `deploy/config.example.hcl` is a complete configuration of the portal with the Inbox.

### 11.1 Switches and panel-only mode

The portal can run as a panel of services only. A part that is off is absent from the DOM. It is never grey. A route that is off answers 404.

| Switch | Default | When off or absent |
|---|---|---|
| Block `inbox` | absent | No tab Inbox, no routes `/v1/*`, `/api/v1/*`, `/api/version`, no phone endpoint, no group of Inbox permissions, no kind `api`. |
| Block `keys` | absent | No list of keys, no group of key permissions, no key count in the list of accounts. |
| `keys.mcp` | `true` | The key edge refuses `mcp_` keys. No MCP chip, no MCP right. |
| `keys.api` | `false` | The API of the Inbox refuses `api_` keys. No API chip, no API right. |
| `users.allow_creation` | `true` | The create route answers 404. No **New user** button, no form and no entry that makes a user. |

Six contradictions stop the start. The plan lists five. The sixth is a check that the author added.

1. `keys.api = true` and no block `inbox`.
2. `keys.mcp = true` and no block `identity` or no block `pdp`.
3. A block `keys` with `mcp = false` and `api = false`.
4. A block `api_key` and `keys.api = false` (or no block `keys`).
5. `access.grants_mode = "static"` and `users.allow_creation = true`.
6. A block `key` of a block `user` (an MCP key of the file) and `keys.mcp` not `true`.

The earlier rule that the block `keys` needs the blocks `identity` and `pdp` now applies to the kind `mcp` only. The matrix of the acceptance tests has five cases: panel only, panel and keys, panel and Inbox, all on, and no creation. The shipped configurations of `deploy/` and `demo/` use `access.grants_mode = "hybrid"` and `keys { mcp = false api = true }`. The demo has both kinds on.

## 12. Alerts of the portal (watch)

Decision D12 gives the scope. Step 6 made these choices where the owner left them open. Batch A renamed the feature to "watch" (module `src/watch.rs`, metrics `portal_watch_*`).

- **Block.** `inbox.watch` is optional. Without it, nothing runs and the metrics have no series. Its keys and defaults are in the README: `group` (mandatory), `service_unhealthy`, `unhealthy_after` (`2m`), `sign_in_burst`, `sign_in_failures` (10), `sign_in_window` (`5m`), `repeat_after` (`1h`) and `recovered`.
- **Checks at start.** `group` must be the label of a block `group`. The message names the key, the groups that exist and the value. The start also stops when `service_unhealthy` and `sign_in_burst` are both `false`: the block would send nothing.
- **Internal sender.** The task calls `accept_alert` of the Inbox directly with the source `portal`. The path is the path of an API alert. It has one transaction, the idempotency key, the copy of the replica and the spool of the degraded mode. There is no key and no HTTP request. The source `portal` does not collide with the sources of the API keys, the webhooks, the notify route and the form.
- **Detectors.** Two small state machines in `src/watch.rs` decide. They take a monotonic time from the caller, so a test moves the clock. A task reads the catalogue and the counter of refused sign-ins once each second. The durations use the monotonic clock, so a jump of the wall clock does not start or stop an alert.
- **State of a service.** The rules are the rules of the tile: `down` and `unhealthy` are unhealthy, `starting` is not unhealthy, and a service with no state is never unhealthy. A service that starts, or that has no state, breaks the unhealthy time. The owner did not say this. The reason is that a restart must not look like an outage.
- **Recovery.** The portal sends `healthy again` only after it sent an unhealthy alert, and only once. A service that only starts again does not count as healthy. A service that leaves the catalogue is forgotten with no alert.
- **Sign-in burst.** The task reads the sum of three counters that exist. They count a wrong password or unknown login, a refusal by the limiter of the address, and a refusal by the global limiter. The sign-in path has no new code. After an alert, the count restarts from zero. The same refusals never give a second alert when `repeat_after` is shorter than `sign_in_window`.
- **Title.** The title has the values of the configuration, not the measured time: `Service Notes is unhealthy for 2 minutes`. The Inbox answers `409` when two alerts have the same key and two titles. The sender counts a `409` as a duplicate. Two instances with two configurations, during an update, then do not try again without end. A name of a service is cut to 64 characters and has no control character. A login, an address and a password are never in a title.
- **Idempotency key.** The keys are `portal:unhealthy:<service id>:<bucket>`, `portal:recovered:<service id>:<bucket>` and `portal:burst:<bucket>`. The bucket is the Unix time divided by `repeat_after`. An id that makes the key longer than 128 bytes gives the first 16 bytes of its SHA-256 digest in hexadecimal. Two instances build the same key from the same event, so the Inbox accepts one alert.
- **Limits of the key.** Two instances that read one event on two sides of a bucket limit send two alerts. A wall clock that goes back can give a key that the Inbox already used, and the Inbox then drops the alert as a duplicate.
- **Per instance.** Each instance counts the refused sign-ins that it sees. A burst that is spread on several instances can stay under the threshold on each of them. A shared counter would need a table and a write for each refusal. The owner can ask for it later.
- **Memory.** The state of the detectors is in memory. A restart forgets it. The portal then waits `unhealthy_after` again, and the key stops a second alert in the same bucket. It does not send `healthy again` for an alert of an earlier process.
- **Failure.** If the Inbox refuses an alert or does not answer in 20 seconds, the portal logs `watch_failed`. It counts the failure in `portal_watch_failed_total{kind}`. It tries again after 10 seconds. The detectors keep the alert due until the Inbox accepts it. The login path and the launcher do not wait for the task. The task never panics on a send error.
- **Metrics.** `portal_watch_sent_total{kind}` counts the alerts that the Inbox accepted. A duplicate that another instance already stored counts as sent, and the log event `watch_sent` has the field `duplicate`.
- **Start.** The task starts after the portal has a complete catalogue, in the same list as the listeners, and it ends with them.
- **Test.** The end-to-end tests use a fake Docker socket for the unhealthy service. The smallest `unhealthy_after` is `10s`, so these tests take about one minute each. The test with two instances needs PostgreSQL (`INBOX_TEST_PG`).
- **Not in this version.** An alert when a key comes near its budget.

## 13. Process

- **Start.** The Inbox starts before the portal waits for its catalogue. An alert does not wait for the discovery.
- **Stop.** At the signal, the Inbox answers `503` at `/readyz` and tells the phones to reconnect (close code 1012) at once. It serves 5 more seconds like the portal. Then it closes the listeners. It sends the close code again to the sessions that started in the meantime. It waits for the sessions (10 seconds at most). It writes what its memory holds to the database.
- **Lease loss.** A replica that loses its lease stops the whole process, because the SQLite file is shared with the portal. The process ends with an error after the last snapshot.
- **Standby wait.** Before the portal opens the SQLite file, a standby instance waits for the lease. A thread with its own small runtime watches SIGTERM and SIGINT during this wait. It replaces the signal handlers of the old code and needs no unsafe code.
- **Host name.** The Inbox does not read `HOSTNAME`. It reads the host name from `/proc/sys/kernel/hostname`, for the table of the instances only.
- **Telemetry.** The Inbox has no OpenTelemetry export. Its metrics are in the Prometheus text of the portal with the same names. The crates `opentelemetry*`, `tracing` and `tracing-subscriber` are not in the build. The Inbox writes its log with small macros that give the JSON lines of the portal. The spans are gone. The events `device_connected` and `device_disconnected` keep the fields that the old span gave (`device.id`, `device.name`, `user.name`, `client.address`). The task of the statistics sets the Prometheus gauges every 15 seconds, as before.
- **Comments.** The comments of the moved code are the old comments. The new modules and the changed interfaces have comments in Simplified Technical English. A full rewrite of the old comments is not done. A few comments still use the earlier word for a sender.

## 14. The Android app

The app has the name **Portal Inbox**. The launcher label, the notification texts and the strings use this name. The application id is `app.portalinbox`. No name or id holds an older name.

- **Code names.** The Kotlin package is `app.portalinbox`. The JNI symbols are `Java_app_portalinbox_core_Native_*`. A unit test of `inbox-android` compares each `external fun` of `Native.kt` with the exported symbols. A name that does not match crashes the app at load. The library is `libinbox_android.so`.
- **Protocol constants.** The Noise prologues are `portal-inbox/v1/session` and `portal-inbox/v1/enroll`. The hash of the cluster peers uses `portal-inbox-peer-v1`. The deep link is `portal-inbox://enroll?...`.
- **The field `kind`.** `AlertFrame.parse` reads the alert frame. The field `kind` never makes the parser fail. A valid name (`[a-z][a-z0-9_]{0,31}`) stays as it is. An absent value, a value that is not a string and a bad name become `unknown`. The other fields of the delivery fail as before. The table `alerts` has the column `kind`, with the default `alert`. The version of the database is 7. The upgrade uses the same removal and creation of the tables as version 6, because no phone holds data.
- **Display.** A message of the kind `alert` shows as before. Any other kind shows as a generic message. The notification text is "Message in <group>". The detail sheet has the row Kind ("Message" or "Message (<kind>)"). The app never drops a message because of its kind. The level of the group (Off, Notification, Ring, Alarm) applies to a message of any kind. The app stores the message, sends the receipt, removes duplicates and shows it as before.
- **Strict core.** The core crate still refuses a kind other than `alert`, for the server and for the tests of the core. The app reads the frame in Kotlin after the core decrypts it. The core does not read the frame. A later protocol version changes the core and the app together.
- **Strings.** The top bar and the first menu item of the app say "Inbox". The lists say "messages". The words "alert" and "alarm" stay where the item is an alert or an alarm. The names of the notification channels and their descriptions are in `strings.xml`. The server name (`server.name` of the portal, field `server_name`) shows in five places. They are the server list, the server screen, the alarm screen, the detail sheet and the notifications of the connection. The login is read-only. The app has no screen "About" or "Help".
- **No TLS code.** The app accepts an `http://` or an `https://` base URL. For `https://` it uses the trust store of the phone. The app has no certificate code and no network security configuration. The manifest keeps `usesCleartextTraffic="true"`: the portal is often on a private network over plain HTTP.
- **Log files.** Three log files of the Kotlin compiler (`.kotlin/errors`) were tracked by mistake. They are removed from the index and ignored.
- **Gradle unit tests.** The module had no unit test. The tests are under `android/app/src/test`. They run on the JVM with the real `org.json` library. `tools/android/build-apk.sh` runs them (`testReleaseUnitTest`).
- **Check in the emulator.** The release APK was built with a throwaway key and run in the Android 14 emulator against a portal container built from this tree. The deep link opened the form with the address and the fingerprint. A malformed link gave "This is not a valid enrollment code." A used code gave "The code is not valid. Ask for a new one." The enrollment and the subscription to two groups worked. One notification showed for an alert that was sent two times. An alarm worked too. The phone showed the server name.
- **Commits of the rename.** The commits split the mechanical rename by area. An intermediate commit can fail to build, because the rename crosses the areas. The last commit builds and passes the tests. Two tests need the old names to prove that the program refuses them. The code builds each name from two parts, so that no copy of the old name stays in the repository.

## 15. Build and release

The commands are in the section "Build and release" of the README. The files are in `deploy/README.md`. Step 7 made these choices where the owner left them open.

- **One version.** The root package and the workspace packages have the version 5.0.0. `tools/version.sh` reads the root package and stops when the workspace version differs. `tools/build.sh`, `tools/android/build-apk.sh`, `tools/release/local-release.sh` and the release workflow call it. The Gradle `versionName` is this version. The `versionCode` is `major * 10000 + minor * 100 + patch`, so 5.0.0 gives 50000. It is larger than every code that the old app used.
- **One image.** `docker/Dockerfile` builds on `rust:1.98-alpine3.24`. Alpine is musl, so the build needs no target and no cross compiler. The packages `musl-dev gcc make perl binutils` are enough for SQLite and for `aws-lc-sys`. CMake is not needed. There is no `rust-toolchain.toml`. The Dockerfile builds natively for each architecture. A `buildx` build for another architecture runs the compiler under QEMU or on a native builder.
- **Layers.** A layer holds the dependencies only. It uses the manifests and stub sources. A change of the sources rebuilds the crates of the repository, not the dependencies. `RUSTFLAGS` has only `--remap-path-prefix`. An explicit `crt-static` would break the proc macros on a musl host.
- **Runtime.** The image has `/tmp` (mode 1777) and `/var/lib/portal` (owner 10001). The process writes nowhere else. The image has no icon library. The pages show initials when `ui.icons_dir` is absent. A deployment that wants the icons mounts the library at `/icons`.
- **Image of the standalone server removed.** Its Dockerfile and its ignore file are gone. A root `.dockerignore` limits the context to `src`, `crates`, `web` and the manifests. `docker/Dockerfile` copies `crates/`, because the portal depends on `inbox-core`.
- **Compose.** `docker-compose.yml` at the root has the portal and PostgreSQL. It builds the image when it is absent. The secrets are files in a directory that `deploy/compose/init-secrets.sh` makes (`PORTAL_SECRETS_DIR` changes the directory). The script uses the image for `portal inbox keygen` and `portal inbox token`. There is no shared device password and no credential file. The file `.env.example` is gone, because the stack has no environment secrets.
- **Two instances.** `deploy/compose/docker-compose.ha.yml` adds a second instance and Traefik. It is a test aid. It needs no new image.
- **K3s.** `deploy/k3s/` has one Deployment with two replicas in the namespace `edge`, where the portal runs in the homelab. The Inbox has no namespace of its own any more. The Services are `portal`, `portal-pdp` and `portal-peers`. The IngressRoute `portal-inbox` sends `/v1/`, `/api/v1/` and `/api/version` to the Service `portal` without ForwardAuth, with a retry middleware. The startup probe reads `/healthz`, and the readiness probe reads `/readyz` of the portal port.
- **CI.** `ci.yml` has four jobs: the policy, the Rust checks, the image with the configuration files and the Compose smoke test, and the Android compile check. `release.yml` replaces both release workflows. It builds each architecture on a native runner. It has no step that touches a registry.
- **Version of the release.** A tag `vX.Y.Z` starts the release. The old manual start with an independent version is gone.
- **No push.** No command and no workflow pushes an image to a registry. A release holds binaries, the signed APK and `SHA256SUMS`.

## 16. Open points

- **Licence.** The licence of the merged code is open. The root package says `MIT OR Apache-2.0`. The shared crates say `LicenseRef-Proprietary`. The files `LICENSE-MIT` and `LICENSE-APACHE` come from the earlier portal.
- **QR code.** A QR code for enrollment needs a camera library in the app. It is not in this version. The code is text or a link.
- **Arm64 image.** The release builds the arm64 binary on a native runner. The arm64 image is not built or tested here.
- **K3s files.** The tests of the manifests are a render with kustomize and `check-config`. No cluster was used. The SQLite variant of the manifests is a comment only.
- **Group-wide permissions.** `inbox.receive`, `alert.send` and `alert.read` list only the type `inbox_group`. An account needs one permission for each group. The owner can add the type `service` to the three actions.
- **Upgrade hazard of `inbox.receive`.** An account that has no `inbox.receive` loses its subscriptions at the first check with a loaded directory. The enrollment code also needs this permission. Give the permission before the Inbox goes live. A decision record of 2026-11-01 holds this point.
- **Impersonation.** A session that the superadmin opens as an account can make an enrollment code for that account. The phone then belongs to that account. The owner has not reviewed this point.
- **Network security configuration of the app.** The manifest allows clear text for each address. A configuration that allows clear text only for the addresses that need it, and uses the trust store for `https://` addresses, is not made.
- **State filter counts.** The counts of the state chips and of the groups in the zone **Sent** come from the 500 newest alerts of the period. The graph counts every alert.
- **Matrices need script.** Without script, the editor lists the grants as text.
- **Shared counter of refused sign-ins.** Each instance counts alone. A burst spread on several instances can stay under the threshold.
- **Alert near a budget.** A key that comes near its budget raises no alert in this version.
- **Physical phone.** The tests use the emulator only. The owner chose no physical phone.
- **Archive of the standalone project.** D8 says that the standalone repository is archived at the end. This is not done.
- **Old comments.** The comments of the moved code are not rewritten in Simplified Technical English.
- **Source names.** The source text of an alert from an API key and one value of the metric label `source` still use the earlier word for a sender. The rename did not touch these values. The pages and the documents do not show them as a name.
- **Reserved prefix.** `whk_` is reserved and not implemented.
- **Roadmap.** Steps 2 and 3 are not built (section 17).
- **Documents of the demo.** `demo/README.md` needs the changes that the report of batch F lists.

## 17. Roadmap: steps 2 and 3

| Step | Who sends | Kinds of message | Key kind | What changes |
|---|---|---|---|---|
| 1 (now) | Services, scripts, Grafana webhooks | Alert | `api_` | The pages and the rename (batches A to F). |
| 2 | Agents | Alert | `mcp_` | The agent sends alerts through the REST route first. The MCP side comes with the tools map. |
| 3 | Agents | Question, approval | `api_` and `mcp_` | New actions, a new protocol version, answers and an audit. |

### 17.1 What step 1 does now to leave room

- **Name.** "Inbox" and "Sent" hold three kinds. No tab or path says "alerts".
- **Permissions.** `inbox.receive` is the same for all kinds. A new kind adds actions. The matrix of the editor builds its columns from a list, so Ask and Approve are two more columns, not a new editor.
- **Resource type.** `inbox_group` is a group of recipients. A user as a target is a second type, `inbox_user`, added at step 3. It has no effect now.
- **Wire and database.** A field `kind` in the frames and in the message table, with the value `alert`. The metrics carry the label `kind`.
- **Keys.** Both kinds exist from step 1. The prefix `whk_` is reserved for a later kind.
- **Page.** A kind of message that does not exist has no chip. A later step adds the chip with the kind.

### 17.2 Step 2

- An agent sends alerts through the REST route first, with an `api_` key of its own.
- The MCP side for alerts comes with the tools map: a tool `send_alert` maps to `alert.send`.
- The sender cell of "Sent" shows a webhook icon for an `api_` key and a robot icon for an `mcp_` key.

### 17.3 Step 3: the changed decision on one-way alerts

The decision of 2026-10-02 says that alerts are one way, with no human acknowledgement. This holds for steps 1 and 2. It changes at step 3: a question and an approval need an answer from a person. The owner agreed. This is a flagged decision, with a new protocol version at step 3.

- **Actions (a sketch, not built).** `question.ask` and `question.read` apply to `inbox_group` and `inbox_user`. `ask` has a budget, as `alert.send` has. `approval.request` and `approval.read` apply to the same types. `approval.decide` applies to `inbox_group`. A user that receives the request can answer. This action can limit who may decide.
- **Protocol.** A new version with the frames `question`, `answer`, `approval` and `decision`. The phone sends an answer. The server answers once and refuses a second answer.
- **Server.** A state machine for each item: sent, open, answered, expired, cancelled. A timeout. For an approval the timeout means deny.
- **Audit.** Each answer and each decision is a record. It holds who, which phone, when, the choice and the key of the requester. An approval also holds the hash of the command. The retention follows `inbox.storage`.
- **App.** A screen with the choices and a screen to approve or deny. The level Ring or Alarm applies to an approval.
- **Delivery.** The delivery guarantee holds for the item. The answer is a second guarantee: exactly one answer. Alerts stay one way.
- **Page.** The kind chips filter "Sent". A kind icon column appears when two kinds are on. The state "unanswered" is a chip and an icon. The row detail shows the choices, the answers and the audit line. A panel "Open items" above Phones shows only when something waits. The matrix gets the columns Ask, Request and Decide.

## 18. Index of the deviations

The first plan had one chain of migrations, one admin front without a password, OPAQUE, and an OpenTelemetry export. The table lists the points where the work did something else. The second column gives the section that has the details.

| Deviation | Section |
|---|---|
| Two independent chains of migrations, not one. | 10 |
| The tables of the Inbox keep their old names. | 10 |
| An old SQLite file of the standalone alert server cannot open in place. | 10, 5.2 |
| The snapshot of the replica holds the tables of the portal too. | 10 |
| The Inbox keeps its asynchronous PostgreSQL client. | 10 |
| The HCL parser accepts blocks without a label inside `inbox`. | 11 |
| The secrets `server_key_file` and the secret of an `api_key`. Keys of the old server removed. | 11 |
| Only the delivery values have no default. | 11 |
| The Inbox starts before the catalogue. | 4.1, 13 |
| The phone sessions have a budget that is separate from the pages. | 4.1 |
| Shutdown, lease loss and standby wait. | 13 |
| No OpenTelemetry export. The log uses small macros. | 13 |
| Enrollment uses the pinned key, not OPAQUE. Protocol 4 server choices. | 5 |
| Alerts of the portal: internal sender, bucketed keys, per-instance count. | 12 |
| The limit is the budget `alerts`, not `max_alerts`. | 6.1 |
| Three actions list the type `inbox_group` only. | 6.1 |
| The API checks the keys of the file before it checks a key of the database. | 7 |
| Subscriptions follow permissions with a safety rule. | 8 |
| The page is under `/auth/settings/inbox`, and the JSON admin API is gone. | 9.4 |
| One image, one version, K3s files only. | 15 |
| The metrics have no listener of their own. | 4 |
| The label `kind` of the failed-authentication metric names the failure. | 5.3 |
| The old names exist in two tests that prove their refusal. | 14 |
| The Android build: the labels, the strings and the themes only had a plain replacement. | 14 |
| The service id `inbox` was a placeholder. Batch C made the scope `portal`. The module `src/inbox_actions.rs` moved to `src/portal_actions.rs`. | 6 |
| A file key decides its groups at start, has no budget and is not in the list of keys. | 7 |
| An API key holds no grant on a service. | 7 |
| The kind of a new key is in the address. | 7 |
| The sixth contradiction of the switches. | 11.1 |
| The summary row of a revoked key has no kind. | 7 |
| The migration of the column `kind` of the keys, although the plan said no old data. | 7 |
| No migration of the grants with the old names. | 6.1 |
| The Android app reads the field `kind` in Kotlin. The core stays strict. | 14 |
| No TLS code in the app. | 14 |
| The tab Activity is gone. | 9.1 |
| Mockups of the Users and Services pages: dates, labels, request limits, count of accounts, Opened by, table, filter. | 9.6 |
| Mockups of the account page: firm confirmation, the Activity popover, banners, new user in static mode, narrow windows. | 9.6 |
| Mockups of the Inbox page: Mine on the phones, connections, origin, bound of Sent, code popover, send form, row of an alert. | 9.6 |
| Mockups of the Keys page: kind of a key in the pane, key list, warning mark, editor without script, order, budget. | 9.6 |
