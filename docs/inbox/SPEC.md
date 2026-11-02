# Inbox specification

This document states what the Inbox does and why. The Inbox is a feature of the portal. The code is the reference for details. The design of the merge is in [`../design-inbox.md`](../design-inbox.md). The specification of the portal in its own repository has the same requirements in its section 15.

The words MUST, MUST NOT, SHOULD and MAY are the key words of RFC 2119 and RFC 8174.

A requirement has one of two states. A requirement without a mark is in force in this version (step 1 of the roadmap). A requirement with the mark **FUTURE** is for step 2 or step 3. No code implements a **FUTURE** requirement. Section 8 lists them.

## 1. Purpose

1.1 The Inbox sends alerts to Android phones. An alert can be vital. Each subscribed phone MUST get each alert, and MUST show it one time.

1.2 This holds when the phone was off, in airplane mode or disconnected when the alert came. It also holds when the app was stopped.

1.3 The Inbox is part of the portal. One account of the portal gives one identity and one set of permissions for the launcher, the keys and the Inbox.

## 2. Scope

| In scope | Out of scope |
|---|---|
| The Inbox feature of the binary `portal`. | A standalone alert server binary. |
| The Android app Portal Inbox. It only receives alerts. | iOS. It cannot keep a connection in the background without the push service of Apple. |
| The API for the senders, the webhooks for Alertmanager and Grafana, and the page of the tab Inbox. | A push service of a third party (Firebase or Apple). |
| Alerts of the portal about itself. | An acknowledgement of an alert by a person (step 1). An escalation between persons. |
| Docker Compose and K3s files. | A Grafana plugin. Grafana contact points cannot be extended. |
| One database with SQLite or PostgreSQL. A standby replica for SQLite. | An OpenTelemetry export. The metrics are in the Prometheus text of the portal. |

## 3. Functional requirements

### 3.1 Groups and alerts

3.1.1 The configuration file MUST define the groups. A group has an id, a name and an optional description. A group MAY be `critical`.

3.1.2 An alert has one group and one title. The title MUST have one line and 128 characters at most. An alert MUST NOT have a body or a severity.

3.1.3 The sender decides the group. The subscriber decides the reaction of the phone, group by group.

3.1.4 An alert has no human acknowledgement in step 1. The phone sends only an automatic receipt after it stores the alert. Without the receipt, the server cannot send the alert again, and the guarantee of delivery fails.

3.1.5 Each alert MUST have a kind. The only kind in step 1 is `alert`. The wire frame and the stored alert carry it in the field `kind`. In protocol 4, a parser MUST refuse any other value.

3.1.6 A message of a kind other than `alert` MUST show in the app as a generic message. The app MUST NOT drop a message because of its kind.

### 3.2 Phones

3.2.1 A phone MUST enroll with a one-time code that an account of the portal makes. The login of the account is the user name of the phone. The phone MUST NOT change it.

3.2.2 The account MUST have the permission `inbox.receive` on at least one group to make a code. The code is made in the tab Inbox, with the plus button.

3.2.3 A phone MUST subscribe only to a group for which its account has `inbox.receive`. When the account loses the permission, the Inbox MUST remove the subscription and tell the phone. The Inbox MUST NOT remove a subscription when it does not know the rights.

3.2.4 For each group, the user chooses an action on the phone: `alarm`, `ring` or `notify`. The server MUST NOT know this choice.

3.2.5 The app MUST NOT send an alert. The server has no send route for the identity of a phone.

3.2.6 One app MAY enroll in several servers. It keeps one connection and its own subscriptions for each server. The app shows the name of each server (`server.name`).

3.2.7 A person MUST be able to revoke a phone. An account revokes its own phones. An account with `phone.revoke` and `phone.read` revokes any phone. A revoked phone gets the close code 4403.

### 3.3 Senders and keys

3.3.1 A sender MUST send an alert with an API key (the prefix `api_`) as a bearer token. Two sources of API key work at the same time: a key of the file (a block `api_key`), and a key of the database. A bad form, a bad checksum, an unknown prefix and a key of another kind MUST get `401` with no detail, before any lookup.

3.3.2 A key of the file MUST NOT depend on the state of the portal at run time. It works when the portal is not ready and when PostgreSQL is away. Its owner MUST be an account of the file. The start decides its groups.

3.3.3 A key of the database MUST have `alert.send` on the group, and its owner MUST have it too. A key follows its owner for both kinds. The Inbox asks the PDP of the portal in process.

3.3.4 The Inbox MUST offer these routes: a JSON alert, a generic webhook, and a webhook with the payload of Alertmanager and Grafana. A sender MAY read the delivery status of an alert with `alert.read` on its group.

3.3.5 A sender SHOULD send an `Idempotency-Key`. Two requests with the same key MUST make one alert. A key that belongs to a different alert MUST get `409`.

3.3.6 The action `alert.send` MAY have a budget (`alerts`). A critical group MUST NOT have a limit.

3.3.7 The kind `api` MUST be on only with the block `inbox`. The switch is `keys.api`, and its default is `false`. A file with a block `api_key` and `keys.api` not `true` MUST stop at start.

3.3.8 The routes of the Inbox MUST NOT accept an `mcp_` key, and the key edge MUST NOT accept an `api_` key. The prefix `whk_` is reserved.

### 3.4 Page

3.4.1 The page of the Inbox is the tab Inbox of the settings of the portal, at `/auth/settings/inbox`. It needs a session of the portal and the permissions of the portal features. It has no sub-page.

3.4.2 The page MUST work without script. The script only gives the Copy button, the close of a popover and the count of the bulk action.

3.4.3 The page shows the alerts and their delivery (zone **Sent**), the phones (zones **Phones** and **My phones**) and the connection log (zone **Connections**). It lets an account send an alert and revoke a phone. A part that an account may not use MUST be absent from the page.

3.4.4 A session that the superadmin opens as another account MUST NOT send an alert and MUST NOT revoke a phone.

3.4.5 The zone **Sent** MUST hold the 500 newest alerts of the period at most, and MUST show `500+` when older alerts of the period exist.

3.4.6 The page MUST be absent, and its routes MUST answer `404`, when the file has no block `inbox`.

### 3.5 Alerts of the portal

3.5.1 The portal MAY send an alert in three cases. A service is unhealthy. A service is healthy again. Refused sign-ins come in a burst. The block `inbox.watch` turns this on.

3.5.2 These alerts MUST use the path of an alert of the API. This gives one transaction, the idempotency key and the copy to the replica. The source of the alert is `portal`.

## 4. Delivery guarantee

The transport is at least once. The phone ignores a repeat. The result is that the user sees each alert exactly once.

4.1 The Inbox MUST answer `201` only after two things are on disk. They are the alert and one delivery for each phone that is subscribed at that moment. This is one transaction.

4.2 The phones that an alert targets are the phones that are subscribed when the Inbox accepts it. They can be online or offline.

4.3 The Inbox MUST send again each delivery that the phone did not confirm. It does this during the session and at each reconnection.

4.4 The phone MUST store the alert durably before it confirms it. It MUST ignore an alert id that it already has. At restart, it MUST show what it stored and did not show.

4.5 The Inbox keeps an alert for a phone for `inbox.storage.delivery_ttl`. After this time, the delivery expires, and the page shows it as expired. The Inbox deletes the history after `inbox.storage.alert_retention`. This value MUST be at least `delivery_ttl`.

4.6 A gap in coverage MUST be visible. A subscribed phone that the Inbox does not see for `inbox.devices.unreachable_after` is reported as unreachable. The app shows its connection state and warns after `inbox.devices.offline_warning_after`.

4.7 A phone MUST reconnect at once when the network returns, at boot and after a restart of the server (close code 1012).

4.8 These limits are known. "Force stop" and some battery managers of vendors stop the app. The app guides the user to the battery exemption. The Inbox reports the phone as unreachable.

### 4.9 Several instances

4.9.1 With PostgreSQL, any number of instances MAY be active. An alert and its deliveries are one transaction. The idempotency key is a unique index.

4.9.2 When PostgreSQL has no writable primary, the instances MUST go on. They keep alerts in a spool in memory and send them to the connected phones. They write the spool when the primary returns. The section "Storage and failure modes" of [`README.md`](README.md) gives the limits.

### 4.10 Standby with SQLite

4.10.1 With the block `inbox.replica`, the Inbox copies the whole database to a shared directory. With `sync_alerts = true`, it confirms an alert, an enrollment and a revocation only after the copy holds them.

4.10.2 A lease with an epoch that increases names the active instance. An instance that loses its lease MUST stop the whole process.

## 5. Security

5.1 Enrollment MUST be safe on plain HTTP. The code holds the address, the public key of the server and a secret of 128 bits. The phone pins the key. It runs a Noise IK handshake to this key and sends the secret inside it.

5.2 The secret MUST work one time and MUST expire. The server MUST keep only its digest.

5.3 A session MUST use Noise IK between the key of the phone and the pinned key of the server. This gives mutual authentication, encryption and forward secrecy.

5.4 The server MUST limit the enrollment attempts for each client address. It MUST compare tokens in constant time.

5.5 The page MUST need a session of the portal. A form MUST carry an anti-CSRF token and the same-origin check.

5.6 The key file of the server is a secret. A new key file makes every phone and every code invalid. The operator MUST back it up.

5.7 The routes of the Inbox share the listener of the portal: `/v1/enroll` and `/v1/session` for the phones, `/api/v1/` for the senders, `/api/version` and `/readyz`. They have no session and set no cookie. The portal MUST NOT apply its session, CSRF or origin checks to them. The portal MUST match them before every other route. No page of the portal MUST use a path under `/v1/` or `/api/v1/`.

5.8 A phone session is long-lived. It MUST NOT use the budget of connections of the pages. The portal MUST give the permit of the pages back when a connection upgrades, and take a permit of a separate budget of sessions. A phone that cannot get a permit gets `503` before the upgrade.

5.9 A secret MUST NOT be in clear text in the configuration. It comes from `_env` or `_file`. The log MUST NOT hold a secret.

5.10 The portal has no TLS. A reverse proxy terminates TLS. The portal MUST NOT trust a forwarded header from a peer that is not in `server.trusted_proxies`.

The details are in [`SECURITY.md`](SECURITY.md) and [`protocol.md`](protocol.md).

## 6. Technical requirements

6.1 The Inbox is a module of the one Rust binary `portal`. It uses Hyper for HTTP. It has no embedded front.

6.2 The configuration follows the rules of the portal. It is one HCL file with a closed schema. An error stops the program at start. The program does not coerce a value. A secret uses `_env` or `_file`. The command `portal check-config` checks the file. The delivery values (`heartbeat_interval`, `session_timeout`, `resend_after`, `max_inflight`, `unreachable_after`, `offline_warning_after`, `delivery_ttl`, `alert_retention`, `connection_log_retention`) MUST have no default. A wrong default could hide an alert.

6.3 The Inbox uses the database of the portal. It has its own chain of migrations. Each chain MUST refuse a schema that is newer than the binary.

6.4 The protocol and the cryptography of the phones are in the crate `inbox-core`, with no I/O. The server and the app share it. The cryptography exists once.

6.5 The metrics of the Inbox have the prefix `portal_inbox_`. The metrics of the alerts of the portal have the prefix `portal_watch_`. They are in the Prometheus text of the portal. The log is the JSON log of the portal.

6.6 The build uses Docker only. There is one static musl image. No command publishes an image to a registry. A release holds binaries, the signed APK and `SHA256SUMS`.

6.7 The tests use the emulator and the end-to-end tests of the binary. No physical phone is needed.

6.8 The permissions of the Inbox are on the one internal scope of the portal, the service `portal`. The actions are `inbox.receive`, `alert.send`, `alert.read`, `phone.read`, `phone.revoke` and `phone.log`. The first three apply to the resource type `inbox_group`.

## 7. Integration

7.1 Grafana: a contact point of the type Webhook sends to `POST /api/v1/webhooks/alertmanager/<group>` with a bearer `api_` key. [`grafana/contact-point.yaml`](grafana/contact-point.yaml) is an example.

7.2 Alertmanager: a `webhook_configs` receiver does the same. [`alertmanager.yml`](alertmanager.yml) is an example.

## 8. Roadmap: future requirements (FUTURE)

The roadmap has three steps. Step 1 is this version. The requirements below are not built. They record the decisions so that step 1 leaves room for them.

### 8.1 Step 2: agents send alerts (FUTURE)

8.1.1 **FUTURE.** An agent MUST send alerts through the REST route of 3.3, with an `api_` key of its own.

8.1.2 **FUTURE.** The MCP side MAY accept an alert from an `mcp_` key. It comes with the tools map: a tool `send_alert` maps to the action `alert.send`.

8.1.3 **FUTURE.** The zone **Sent** SHOULD show a webhook icon for an `api_` key and a robot icon for an `mcp_` key.

### 8.2 Step 3: questions and approvals (FUTURE)

8.2.1 **FUTURE.** The decision that alerts are one way (3.1.4) ends at step 3 for the new kinds. An alert stays one way. A question and an approval need an answer from a person.

8.2.2 **FUTURE.** A new protocol version MUST add the frames `question`, `answer`, `approval` and `decision`. The phone sends the answer. The server MUST accept one answer and MUST refuse a second answer.

8.2.3 **FUTURE.** The server MUST keep a state for each item: sent, open, answered, expired or cancelled. A timeout ends an open item. For an approval, the timeout means deny.

8.2.4 **FUTURE.** The Inbox MUST keep an audit record for each answer and each decision. The record holds who, which phone, when, the choice and the key of the requester. An approval record also holds the hash of the command. The retention follows `inbox.storage`.

8.2.5 **FUTURE.** New actions: `question.ask`, `question.read`, `approval.request`, `approval.read` and `approval.decide`. `question.ask` has a budget, as `alert.send` has. A user as a target is a second resource type, `inbox_user`.

8.2.6 **FUTURE.** The app MUST have a screen with the choices of a question and a screen to approve or deny. The level Ring or Alarm of the group applies to an approval.

8.2.7 **FUTURE.** The delivery guarantee MUST hold for each item. The answer is a second guarantee: exactly one answer.

8.2.8 **FUTURE.** The page MUST show the kind chips Question and Approval as active. It MUST show a panel of open items above the phones when an item waits. The matrix MUST have the columns Ask, Request and Decide. The dashed chips of step 1 show the place.
