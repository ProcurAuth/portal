# Inbox specification

This document states what the inbox does and why. The inbox is a module of the portal. The code is the reference for details. The design of the merge is in [`../design-inbox-merge.md`](../design-inbox-merge.md).

The words MUST, MUST NOT, SHOULD and MAY are the key words of RFC 2119 and RFC 8174.

## 1. Purpose

1.1 The inbox sends alerts to Android phones. An alert can be vital. Each subscribed phone MUST get each alert, and MUST show it one time.

1.2 This holds when the phone was off, in airplane mode or disconnected when the alert came. It also holds when the app was stopped.

1.3 The inbox is part of the portal. One account of the portal gives one identity and one set of permissions for the launcher, the keys and the inbox.

## 2. Scope

| In scope | Out of scope |
|---|---|
| The module `inbox` of the binary `portal`. | A standalone inbox binary. |
| The Android app. It only receives alerts. | iOS. It cannot keep a connection in the background without the push service of Apple. |
| The producer API, the webhooks for Alertmanager and Grafana, and the pages of the tab Inbox. | A push service of a third party (Firebase or Apple). |
| Alerts of the portal about itself. | An acknowledgement of an alert by a person. An escalation between persons. |
| Docker Compose and K3s files. | A Grafana plugin. Grafana contact points cannot be extended. |
| One database with SQLite or PostgreSQL. A standby replica for SQLite. | An OpenTelemetry export. The metrics are in the Prometheus text of the portal. |

## 3. Functional requirements

### 3.1 Groups and alerts

3.1.1 The configuration file MUST define the groups. A group has an id, a name and an optional description. A group MAY be `critical`.

3.1.2 An alert has one group and one title. The title MUST have one line and 128 characters at most. An alert MUST NOT have a body or a severity.

3.1.3 The producer decides the group. The subscriber decides the reaction of the phone, group by group.

3.1.4 An alert has no human acknowledgement. The phone sends only an automatic receipt after it stores the alert. Without the receipt, the server cannot send the alert again, and the guarantee of delivery fails.

### 3.2 Phones

3.2.1 A phone MUST enroll with a one-time code that an account of the portal makes. The login of the account is the user name of the phone. The phone MUST NOT change it.

3.2.2 The account MUST have the permission `group.subscribe` on at least one group to make a code.

3.2.3 A phone MUST subscribe only to a group for which its account has `group.subscribe`. When the account loses the permission, the inbox MUST remove the subscription and tell the phone.

3.2.4 For each group, the user chooses an action on the phone: `alarm`, `ring` or `notify`. The server MUST NOT know this choice.

3.2.5 The app MUST NOT send an alert. The server has no send route for the identity of a phone.

3.2.6 One app MAY enroll in several servers. It keeps one connection and its own subscriptions for each server.

3.2.7 A person MUST be able to revoke a phone. An account revokes its own phones. An account with `device.revoke` and `device.read` revokes any phone.

### 3.3 Producers

3.3.1 A producer MUST send an alert with a bearer token. Two kinds of token work at the same time: a token of the file (a block `producer`), and a key of the portal.

3.3.2 A token of the file MUST NOT depend on the state of the portal. It works when the portal is not ready.

3.3.3 A key of the portal MUST have `alert.send` on the group, and its owner MUST have it too. The inbox asks the PDP of the portal in process.

3.3.4 The inbox MUST offer these routes: a JSON alert, a generic webhook, and a webhook with the payload of Alertmanager and Grafana. A producer MAY read the delivery status of an alert.

3.3.5 A producer SHOULD send an `Idempotency-Key`. Two requests with the same key MUST make one alert. A key that belongs to a different alert MUST get `409`.

3.3.6 The action `alert.send` MAY have a budget (`alerts`). A critical group MUST NOT have a limit.

### 3.4 Pages

3.4.1 The pages of the inbox are in the tab Inbox of the settings of the portal. They need a session of the portal and the permissions of the service `inbox`.

3.4.2 The pages MUST work without script. The script only gives a Copy button.

3.4.3 The pages show the phones, their state, the alerts and their delivery, and the connection log. They let an account send an alert and revoke a phone.

3.4.4 A session that the superadmin opens as another account MUST NOT send an alert and MUST NOT revoke a phone.

### 3.5 Alerts of the portal

3.5.1 The portal MAY send an alert in three cases. A service is unhealthy. A service is healthy again. Refused sign-ins come in a burst. The block `inbox.portal_alerts` turns this on.

3.5.2 These alerts MUST use the path of an alert of the API. This gives one transaction, the idempotency key and the copy to the replica.

## 4. Delivery guarantee

The transport is at least once. The phone ignores a repeat. The result is that the user sees each alert exactly once.

4.1 The inbox MUST answer `201` only after two things are on disk. They are the alert and one delivery for each phone that is subscribed at that moment. This is one transaction.

4.2 The phones that an alert targets are the phones that are subscribed when the inbox accepts it. They can be online or offline.

4.3 The inbox MUST send again each delivery that the phone did not confirm. It does this during the session and at each reconnection.

4.4 The phone MUST store the alert durably before it confirms it. It MUST ignore an alert id that it already has. At restart, it MUST show what it stored and did not show.

4.5 The inbox keeps an alert for a phone for `inbox.storage.delivery_ttl`. After this time, the delivery expires, and the pages show it as expired. The inbox deletes the history after `inbox.storage.alert_retention`. This value MUST be at least `delivery_ttl`.

4.6 A gap in coverage MUST be visible. A subscribed phone that the inbox does not see for `inbox.devices.unreachable_after` is reported as unreachable. The app shows its connection state and warns after `inbox.devices.offline_warning_after`.

4.7 A phone MUST reconnect at once when the network returns, at boot and after a restart of the server (close code 1012).

4.8 These limits are known. "Force stop" and some battery managers of vendors stop the app. The app guides the user to the battery exemption. The inbox reports the phone as unreachable.

### 4.9 Several instances

4.9.1 With PostgreSQL, any number of instances MAY be active. An alert and its deliveries are one transaction. The idempotency key is a unique index.

4.9.2 When PostgreSQL has no writable primary, the instances MUST go on. They keep alerts in a spool in memory and send them to the connected phones. They write the spool when the primary returns. The section "Storage and failure modes" of [`README.md`](README.md) gives the limits.

### 4.10 Standby with SQLite

4.10.1 With the block `inbox.replica`, the inbox copies the whole database to a shared directory. With `sync_alerts = true`, it confirms an alert, an enrollment and a revocation only after the copy holds them.

4.10.2 A lease with an epoch that increases names the active instance. An instance that loses its lease MUST stop the whole process.

## 5. Security

5.1 Enrollment MUST be safe on plain HTTP. The code holds the address, the public key of the server and a secret of 128 bits. The phone pins the key. It runs a Noise IK handshake to this key and sends the secret inside it.

5.2 The secret MUST work one time and MUST expire. The server MUST keep only its digest.

5.3 A session MUST use Noise IK between the key of the phone and the pinned key of the server. This gives mutual authentication, encryption and forward secrecy.

5.4 The server MUST limit the enrollment attempts for each client address. It MUST compare tokens in constant time.

5.5 The pages MUST need a session of the portal. A form MUST carry an anti-CSRF token and the same-origin check.

5.6 The key file of the server is a secret. A new key file makes every phone and every code invalid. The operator MUST back it up.

5.7 The listener of the phones and producers has no session. It MUST NOT serve any page of the portal.

5.8 A secret MUST NOT be in clear text in the configuration. It comes from `_env` or `_file`.

The details are in [`SECURITY.md`](SECURITY.md) and [`protocol.md`](protocol.md).

## 6. Technical requirements

6.1 The inbox is a module of the one Rust binary `portal`. It uses Hyper for HTTP. It has no embedded front.

6.2 The configuration follows the rules of the portal. It is one HCL file with a closed schema. An error stops the program at start. The program does not coerce a value. A secret uses `_env` or `_file`. The command `portal check-config` checks the file. The delivery values (`heartbeat_interval`, `session_timeout`, `resend_after`, `max_inflight`, `unreachable_after`, `offline_warning_after`, `delivery_ttl`, `alert_retention`, `connection_log_retention`) MUST have no default. A wrong default could hide an alert.

6.3 The inbox uses the database of the portal. It has its own chain of migrations. Each chain MUST refuse a schema that is newer than the binary.

6.4 The protocol and the cryptography of the phones are in the crate `inbox-core`, with no I/O. The server and the app share it. The cryptography exists once.

6.5 The metrics of the inbox have the prefix `inbox_`. They are in the Prometheus text of the portal. The log is the JSON log of the portal.

6.6 The build uses Docker only. There is one static musl image. No command publishes an image to a registry. A release holds binaries, the signed APK and `SHA256SUMS`.

6.7 The tests use the emulator and the end-to-end tests of the binary. No physical phone is needed.

## 7. Integration

7.1 Grafana: a contact point of the type Webhook sends to `POST /api/v1/webhooks/alertmanager/<group>` with a bearer token. [`grafana/contact-point.yaml`](grafana/contact-point.yaml) is an example.

7.2 Alertmanager: a `webhook_configs` receiver does the same. [`alertmanager.yml`](alertmanager.yml) is an example.
