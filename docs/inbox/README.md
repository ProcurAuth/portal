# Inbox

The Inbox is a feature of the portal. It sends alerts to Android phones. It makes sure that each subscribed phone shows each alert one time. The senders, the pages and the phones use the same port of the portal.

This chapter is for operators and for people who send alerts. The root [`README.md`](../../README.md) has the generated reference of the configuration. [`SPEC.md`](SPEC.md) has the numbered requirements. [`protocol.md`](protocol.md) has the wire protocol of the phones. [`SECURITY.md`](SECURITY.md) has the security model. [`configuration.md`](configuration.md) has the rules that the reference table cannot show. [`../design-inbox.md`](../design-inbox.md) has the design and the decisions.

## Concepts

| Word | Use |
|---|---|
| Alert | One group and one title. The title has one line of 128 characters at most. An alert has no body and no severity. It has a field `kind`, and the only value is `alert`. |
| Group | A named list of phones that follow it. The configuration file defines the groups. A group can be `critical`. |
| Sender | A program that sends alerts: a script, Grafana, Alertmanager, a service or the portal itself. It uses an API key. |
| API key | A key with the prefix `api_`. The routes of the Inbox verify it. |
| Phone | An Android phone that runs the app Portal Inbox and enrolled with an account of the portal. |
| Account | An account of the portal. It owns its phones. Its login is the user name of each phone. |
| Subscription | The link between one phone and one group. The phone chooses it. The account must have the permission `inbox.receive` on the group. |
| Delivery | The record that one alert must reach one phone. The Inbox makes one delivery for each phone that is subscribed when it accepts the alert. |
| Enrollment code | A text that the account makes in the Inbox page. It lets one phone enroll one time. |

The phone decides how it reacts to an alert. The server never knows this choice. The choices are `alarm`, `ring` and `notify` (see "The Android app").

## What the Inbox guarantees

Each subscribed phone gets each alert, and shows it one time. This holds also when the phone was off, in airplane mode or disconnected when the alert came.

1. The API answers `201` only after the alert is on disk. The alert and one delivery for each subscribed phone are in one transaction.
2. The Inbox sends again each delivery that the phone did not confirm. It sends again during the session (`inbox.devices.resend_after`) and at each reconnection.
3. The phone stores the alert before it confirms it. It ignores an alert id that it already has.
4. If the phone stores an alert and the app stops before it shows it, the app shows it at the next start.
5. The Inbox keeps an alert for a phone until `inbox.storage.delivery_ttl`. After this time, the delivery expires, and the page shows it as expired.
6. The Inbox deletes the history after `inbox.storage.alert_retention`.

A gap in coverage is visible. A subscribed phone that the Inbox does not see for `inbox.devices.unreachable_after` is `unreachable` in the page and in the metric `portal_inbox_devices_unreachable`. The app shows its connection state all the time. It warns the user after `inbox.devices.offline_warning_after`.

If two instances or a retry send the same alert, the idempotency key makes one alert. A lost receipt can cause a second send to a phone. The app ignores the second copy, so the user sees one alert.

## Groups

A group is a block `group "<id>"` in the block `inbox`. The id has 1 to 64 characters of `a-z`, `0-9`, `-` and `_`.

```hcl
inbox {
  group "infra" {
    name        = "Infra prod"
    description = "Production outages"
    critical    = false
  }
}
```

The permissions of the portal features (the service `portal`) control each group:

- `inbox.receive` lets the phones of an account subscribe to the group.
- `alert.send` lets an account or a key send an alert to the group.
- `alert.read` lets an account or a key read the alerts of the group and their delivery.
- `alert.send_user` lets an account or a key send an alert to any account. This permission is on the whole service `portal`, with no group.

Give these permissions in the editor of permissions of the account or of the key. The editor lists each group by name. A permission on the whole service is not valid for `inbox.receive`, `alert.send` and `alert.read`. Give one permission for each group. `alert.send_user` is the other way: it has no group, so give it on `service:portal`.

The budget `alerts` limits `alert.send` and `alert.send_user`. The two actions use one count. It counts alerts in a window of a second, a minute, an hour or a day. A group with `critical = true` has no limit. The limit never stops an alert to this group. An alert to an account is never critical, so the limit always applies to it.

### An alert to one account

An alert has one target: a group or one account. An alert to an account goes to each phone of the account that is not revoked. The phone needs no subscription. The alert has the same life as an alert to a group: the same spool, time to live, resend, receipt, rate limit and budget.

- An account with no phone, or with only revoked phones, gets no alert. The Inbox answers `409` with the code `no_phone` and stores nothing. The refused alert uses no budget.
- An account that does not exist gets `404` with the code `unknown_user`. A login that is not an account gets the same answer in each case. An account is an account of the database or of the configuration file (a block `user`), and the superadmin. The file account has the same rules as the database account: its phones, its notifications in the browser and its rights.
- The level of an alert to an account is Ring. It is never critical.
- The protocol of the phones does not change. The frame of the alert has the group id `@direct` and the group name `Direct`. A group of the file cannot have this id.
- The account that is the target, the sender and the superadmin can read the alert. A right on a group does not apply, because the alert has no group.

## Keys of the senders

A key has one kind. The prefix of the text gives the kind.

| Prefix | Kind | Who verifies it | Use |
|---|---|---|---|
| `api_` | API key | The routes of the Inbox, in the portal process | Webhooks, services, scripts, Grafana and Alertmanager. |
| `mcp_` | MCP key | The key edge of the portal, `/auth/verify-key` | Agents that call services. |
| `whk_` | Reserved | Nobody | Not in use. |

The routes of the Inbox accept `api_` keys only. They answer `401` with no detail to an `mcp_` key, to a text with a bad form and to an unknown prefix. The key edge does the same for an `api_` key. A key follows its owner: it never has more rights than its owner has now. When the account is deleted, its keys go with it.

Two sources of `api_` key work at the same time.

<<<<<<< ours
- **A key of the file.** It is a block `api_key` in the block `inbox`. The block names its owner, which is an account of the file (a block `user`), and the groups of the key. At start, the portal keeps the groups that the owner may send to. This key does not depend on the portal at run time. It works when the portal is not ready and when PostgreSQL is away. It has no budget. The list of keys does not list it. Make its text with `portal inbox token --out FILE`.
- **A key of the database.** An account that holds `keys.own.create` on the kind `api` makes it in the tab **Access**. The page shows the text one time. The Inbox checks the key and asks the portal for the decision on each request. The owner needs `keys.own.use` on the kind `api` and `alert.send` on the group.
=======
- **A key of the file.** It is a block `api_key` in the block `inbox`. The block names its owner, which is an account of the file (a block `user`), and the groups of the key. At start, the portal keeps the groups that the owner may send to. This key does not depend on the portal at run time. It works when the portal is not ready and when PostgreSQL is away. It has no budget. With `direct_alerts = true`, the key can send an alert to an account too: the owner must hold `alert.send_user` on `service:portal`, and `groups` can be empty. The list of keys does not list it. Make its text with `portal inbox token --out FILE`.
- **A key of the database.** An account that holds `keys.own.create` on the kind `api` makes it in the tab **Access**. The page shows the text one time. The Inbox checks the key and asks the portal for the decision on each request. The owner needs `keys.own.use` on the kind `api`, and `alert.send` on the group or `alert.send_user` on the service.
>>>>>>> theirs

The switch `keys.api` turns the kind on. The default is `false`. The kind needs the block `inbox`. The root `README.md` has the example of a file key and of the account that owns it.

## Send an alert

A sender sends a request to the portal, on the same host and port as the pages. These routes need no session of the portal. The proxy does not apply ForwardAuth to them. The sender proves itself with an API key as a bearer token. The phones use the same port and the same proxy rule (`/v1/enroll` and `/v1/session`).

| Route | Use |
|---|---|
| `POST /api/v1/alerts` | Body: `{"group": "<id>", "title": "<text>"}` or `{"user": "<login>", "title": "<text>"}`. Give exactly one of `group` and `user`. The answer is `201` with `alert_id`, `duplicate` and `targets` (the number of phones). |
| `POST /api/v1/notify/{group}` | A webhook for any tool. The title comes from `?title=`, from the header `X-Title`, from the field `title` of a JSON body, or from the body. The Inbox cuts a long title. |
| `POST /api/v1/webhooks/alertmanager/{group}` | The payload of Alertmanager and of Grafana. Each alert with the status `firing` makes one alert. The answer has the counts `created`, `duplicates` and `skipped`. |
| `GET /api/v1/alerts/{id}` | The delivery status of an alert: sent and received, with the number of attempts. The field `target` has the `kind` (`group` or `user`) and the `id`. An alert to a group needs `alert.read` on the group. An alert to an account can be read by the key that sent it. |
| `GET /readyz`, `GET /api/version` | Readiness and version. No key. |

Send the key in the header `Authorization: Bearer <key>`. A tool that cannot set a header can use the query `?token=<key>`. A proxy can write an address with a query in its log, so use the header when you can.

Example. The portal is at `https://portal.example.org`.

```sh
curl -X POST https://portal.example.org/api/v1/alerts \
  -H "Authorization: Bearer $KEY" \
  -H "Idempotency-Key: deploy-4711" \
  -d '{"group":"infra","title":"Disk full on db-1"}'

curl -X POST https://portal.example.org/api/v1/alerts \
  -H "Authorization: Bearer $KEY" \
  -d '{"user":"alice","title":"Call the on-call lead"}'

curl -X POST "https://portal.example.org/api/v1/notify/infra?title=Backup%20done" \
  -H "Authorization: Bearer $KEY"
```

The header `Idempotency-Key` is optional. It has 1 to 128 printable ASCII characters. Two requests with the same key make one alert. The second answer is `200` with `"duplicate": true`. A key that two different alerts use gets `409`. Without the header, the Inbox derives a key from the content and the minute.

| Status | Use |
|---|---|
| `400` | The JSON or the query is not valid. The code `invalid_target` means that the body has both `group` and `user`, or none of them. |
| `401` | The key is absent or not valid. The key can have a bad form or the wrong kind. |
| `403` | The key cannot send to this group (`forbidden_group`), or cannot send to an account (`forbidden_user`). |
| `404` | The alert does not exist, or the key may not read it. For a send, the code `unknown_user` means that the account does not exist. |
| `409` | The idempotency key belongs to a different alert, or the account has no phone to use (`no_phone`). |
| `413` | The title has more than 128 characters, or the request body is too large. |
| `422` | The group does not exist, or the title is empty or has a control character. |
| `429` | The budget `alerts` of the key is empty. `Retry-After` gives the seconds to wait. |
| `503` | The Inbox cannot take the alert now. Send it again. See the codes below. |

The code of a `503` tells the cause. `directory_not_ready`: the portal cannot check a key yet. `rights_unavailable`: the count of the budget failed. `replication_unavailable`: the copy to the replica failed. `spool_full`: the memory buffer of the degraded mode is full. An error body has the form `{"error": {"code": "...", "message": "..."}}`.

The book icon of the tab Inbox opens a popover with the routes, the groups and the header of the key.

### Alertmanager and Grafana

Grafana has a fixed list of contact point types. A plugin cannot add one. The Inbox accepts the payload that Grafana and Alertmanager already send. Both tools send the key as a bearer token. Use an `api_` key. The key and its owner need `alert.send` on the group that ends the URL.

- **Alertmanager.** Use a `webhook_configs` receiver with a bearer token. The example is [`alertmanager.yml`](alertmanager.yml).
- **Grafana.** Make a contact point of the type Webhook with the authorization scheme Bearer. The example is [`grafana/contact-point.yaml`](grafana/contact-point.yaml).

The Inbox ignores the notifications `resolved`. A block `api_key` can set `webhook_send_resolved = true` to turn them into alerts. A key of the database never sends them. The Inbox merges a repeat of the same alert. It compares the fingerprint, the status and `startsAt`.

### Alerts of the portal

The portal can send alerts about itself, in process, with the source `portal`. It needs the block `inbox.watch`. The alerts are: a service is unhealthy, a service is healthy again, and a burst of refused sign-ins. The root `README.md` describes them.

## Enroll a phone

An account makes the code, and the phone uses it one time.

1. Sign in to the portal with the account that owns the phone.
2. Open the tab Inbox. The account needs `inbox.receive` on one group at least.
3. Press the plus button in the head of the zone **My phones**. An account that has `phone.read` sees the zone **Phones**, and the plus button is in that zone. The plus button makes the code. A popover shows the code one time. It stays open until you press **Done**.
4. On the phone, install the app and open **Add a server**.
5. Type the code, or tap the link `portal-inbox://enroll?...` on the phone.
6. Compare the fingerprint that the app shows with the fingerprint on the page.
7. Confirm. The app connects, enrolls and shows the groups.
8. Choose a group and an action for each group that the app lists.

The code holds the address of the server, the public key of the server and a secret of 128 bits. It works one time. It lives `inbox.enrollment.code_ttl` (10 minutes by default). An account can have `inbox.enrollment.max_active_per_account` codes that are not used and not expired. The code is a bearer secret until its first use. Show it only to its owner. There is no QR code in this version.

The Inbox checks the secret in a Noise IK handshake to the pinned key of the server. Only the holder of the server key can read the secret. The wire details are in [`protocol.md`](protocol.md).

If the enrollment fails, the app shows one of these texts:

| Text in the app | Cause |
|---|---|
| The code is not valid. Ask for a new one. | The code is unknown, used or expired. The account was deleted. The limit `inbox.devices.max_devices` is reached. |
| Update the app. | The app does not speak protocol 4. |
| Too many attempts. | The client address sent too many attempts. Wait. |

To remove a phone, tick its box in the zone **My phones** and press **Revoke (N)**. A question "Revoke N phones?" opens in the same place. An account revokes its own phones only. An account with `phone.revoke` and `phone.read` can revoke any phone of the zone **Phones**. The phone gets the close code 4403. It must enroll again.

## The page of the tab Inbox

The page is `/auth/settings/inbox`. It has no sub-page. The tab shows for an account that has a permission of the Inbox features. The launcher has no tile for the Inbox. The gear of the launcher opens the page. The page shows only the parts that the permissions of the account give.

| Part | Needs | Use |
|---|---|---|
| Zone **Sent** | `alert.read` on one group at least | The alerts of the period, the graph, the chips of the groups and of the states. A row opens its deliveries in place. |
| Zone **Phones** | `phone.read` | All phones, with state, owner and pending alerts. **Revoke (N)** needs `phone.revoke`. |
| Zone **My phones** | `inbox.receive` on one group | The phones of the account, the plus button and **Revoke (N)**. |
| Zone **Connections** | `phone.log` | The connection log, with a search on the login. |
| **Send alert** | `alert.send` on one group at least | The form of a manual alert. The field `to` has the value `group:<id>` or `user:<login>`. A target `user:<login>` needs `alert.send_user`. |
| The book icon | `alert.send` or `alert.read` | The routes, the groups and the header of the key. |

The zone **Sent** holds the 500 newest alerts of the period. The count shows `500+` when older alerts exist. The counts of the state chips come from these alerts. The graph counts every alert. The chips Question, Approval and Unanswered are dashed and do nothing. They show the place for a later step (see "Roadmap").

A session that the superadmin opens with **Log in as** can read the page and make a code. It cannot send an alert and it cannot revoke a phone.

## Notifications in the browser

A person that is signed in gets a notification when an alert arrives for a group of the person or for the account. The person needs a permission of the Inbox features. The account receives the alerts for each group where it holds `inbox.receive`, and the alerts for the account itself.

The person sees three things. A badge with the unread count is on the tab Inbox. A toast shows each new alert in the open page. An inbox icon is on the launcher, next to the gear, and it opens a list of the newest 8 notifications. The toast of a critical group stays until the person closes it. The other toasts go after 8 seconds.

Each account has a level: **All**, **Alerts** (Alert and Critical only) or **Off**. An alert for an account is level Ring. With **Alerts**, a Ring alert gives no toast, but the list and the badge keep it. With **Off**, the browser sends no request. The store keeps the level and the marker "read up to" of each account. The marker moves when the person opens the tab Inbox or uses **Mark all read**.

The browser asks the portal every 10 to 15 seconds. This is 4 to 6 requests each minute for an open tab. A hidden tab sends none. The portal reads the shared store for each request, so any instance gives the same answer. The request writes nothing and does not renew the idle timer of the session. The README of the portal gives the details, the forms and the migration.

## The Android app

The app has the name Portal Inbox. Its application id is `app.portalinbox`. It only receives alerts. It does not send alerts. It does not use Firebase or any push service of a third party. It keeps one encrypted connection for each server in a foreground service. One app can enroll in several servers. The app shows the name of the server (`server.name` of the portal) in its lists and screens.

For each group, the user chooses one action. This choice stays on the phone.

| Action | Behaviour |
|---|---|
| `alarm` | A loud alarm, in a loop, until the user stops it on the phone. It opens full screen. It overrides silent mode, vibrate mode and do-not-disturb. |
| `ring` | One notification sound, also in silent or vibrate mode. |
| `notify` | A standard notification. It follows the mode of the phone. |

The app lists one more entry, **Direct**, under the groups of the server. It is not a group of the server. An alert that the server sends to one account arrives with the group id `@direct` and the group name `Direct`. The app always lists this entry, also before the first direct alert. Its default action is `ring`. The default is never `alarm`. The user can choose `notify`, `ring` or `alarm`, or turn it off. With **Off**, the app stores the alert in its list and makes no sound and no notification. This choice stays on the phone. The app never subscribes to `@direct` and never unsubscribes from it. The server does not know this action. The notification of a direct alert uses the same channel as the action: Rings, Notifications or Alarms.

The app asks for these permissions on its first screen.

| Permission | Reason |
|---|---|
| Notifications | Android 13 and later: show alerts. |
| Full-screen alarms | Show the alarm over the lock screen. |
| Ignore battery optimization | Keep the connection alive in Doze. Without it, Android can delay delivery. |
| Do Not Disturb access | Let `alarm` and `ring` override silent mode and do-not-disturb. |
| Display over other apps | Let an alarm open full screen when the phone is in use. |

The app also starts again after a reboot. On some vendor ROMs (Xiaomi, Huawei, Samsung and others), allow background activity for Portal Inbox. "Force stop" stops the app until the user opens it again. The Inbox then shows the phone as unreachable.

A message of a kind other than `alert` shows as a generic message. The app never drops a message because of its kind. A person cannot change the login of a phone. The app shows it read-only. The source drawings of the app icon are in `assets/brand/`.

The app accepts an address with `http://` or `https://`. For `https://` it uses the trust store of the phone. The manifest allows clear text, because the portal is often on a private network.

### Test with the emulator

No physical phone is needed for the tests of the app. `tools/android/emulator.sh` runs an Android 14 emulator in Docker. It needs `/dev/kvm`.

1. Build the app: `tools/build.sh apk`. The script `tools/android/build-apk.sh` lists the variables for the key of the APK.
2. Start the emulator: `tools/android/emulator.sh start`. The script installs `dist/portal-inbox-<version>.apk`, grants the permissions and prints the address of the screen. Open <http://localhost:6080> in a browser.
3. Start the stack (see `deploy/README.md`). Publish the port of the portal to the network: `PORTAL_BIND=0.0.0.0`.
4. Set `inbox.devices.public_address` to `http://172.17.0.1:<port>`. This address is the host as the emulator sees it through the Docker bridge.
5. Make a code in the tab Inbox. Open its link on the phone: `adb shell am start -a android.intent.action.VIEW -d '<link>'`.

`emulator.sh logs` shows the logs of the app. `emulator.sh install` installs a new build. `emulator.sh stop` removes the emulator. Sound, vibration and the battery savers of vendors can be judged only on a real phone. If the noVNC page shows a JavaScript error from a `chrome-extension://` script, open the page in a private window. The demo in `demo/` can also be used. See `demo/README.md`.

## Configuration

The block `inbox` of the portal file holds all the settings. [`configuration.md`](configuration.md) gives the rules between the keys. The root `README.md` has the generated table with the type, the range and the default of each key. Check a file with `portal check-config --config FILE`.

## Storage and failure modes

The Inbox uses the database of the portal. With SQLite, it uses the same file. With PostgreSQL, it uses the same database. It keeps its own chain of migrations. See the root `README.md`.

### SQLite: one active instance and a standby

SQLite needs a local file system. Two instances must never open the same file. To survive the loss of the node, use the block `inbox.replica`.

The block names a directory that every instance shares. It must be a volume of plain files. The directory holds:

- `snapshot-<epoch>.db`, a consistent copy of the whole database. The Inbox writes it when the database changed, at most every `snapshot_interval`. It writes it also before it confirms an alert, an enrollment or a revocation.
- `lease.json`, the name of the active instance. The owner renews it every `lease_ttl / 3`.
- `claim-<epoch>`, a file that the Inbox creates with exclusive access. Two standby instances cannot take the same epoch.

An instance that starts waits until the lease is released or has not changed for `lease_ttl`. It takes the next epoch. It restores the newest snapshot that passes `PRAGMA quick_check`. Then it serves. An instance that loses its lease stops the whole process. The orchestrator starts it again as a standby.

With `sync_alerts = true`, the Inbox confirms an alert only after the snapshot that holds it exists. A sender that gets no answer sends the alert again, and the idempotency key makes one alert. If the shared directory fails, the sender gets `503 replication_unavailable`. With `sync_alerts = false`, the Inbox confirms after the local commit, and a failed copy is only logged (`portal_inbox_replica_sync_failures_total`).

Receipts and last-seen times travel with the next snapshot. After a takeover, a phone can get an alert again when its receipt was lost. The app ignores it.

Run this as a Deployment with one replica and the strategy `Recreate`. Do not use a StatefulSet: Kubernetes does not move its pod from a node that it cannot reach. Use a short `tolerationSeconds` for `node.kubernetes.io/unreachable` and `not-ready`. Give the startup probe enough time for the wait of a standby and the restore.

### PostgreSQL: several active instances

With the block `postgres`, any number of instances can be active. They are behind a load balancer. A phone connects to any instance. Any instance accepts alerts.

- An alert and its deliveries are one transaction. The idempotency key is a unique index. A sender that retries on another instance gets `200`.
- The instances tell each other with `LISTEN` and `NOTIFY`. A new alert wakes a phone that another instance holds. A session also reads the database at the interval `min(resend_after, 5s)`. The notifications are a speed-up, not a dependency.
- Each instance writes a heartbeat in the table `instances` every 5 seconds. The other instances close the sessions of an instance that is silent for 30 seconds, with the reason `instance_lost`.
- The limiter of enrollment attempts is in memory in each instance. N instances give N times the attempts.
- For the same guarantees as SQLite, use a PostgreSQL that is replicated with synchronous commit.

To move from SQLite to PostgreSQL, stop the SQLite instance. Run `portal inbox import-sqlite --config FILE --from SQLITE_FILE` with a configuration that has a block `postgres`. The target database must be empty. Then start the PostgreSQL instances. The command copies phones, subscriptions, alerts, deliveries, enrollment codes, the connection log and the settings of the notifications. The source can be a file of the portal.

### Degraded mode: the database has no primary

A PostgreSQL failover takes seconds to minutes. In this time, the instances still accept alerts and still deliver them. The phones keep their sessions. `/readyz` answers `200 degraded`.

- Each call to the database has a limit of 2 seconds. When a call fails, the instance switches to memory. A probe checks each second for a writable primary. The log has `storage_degraded` and `storage_recovered`. The gauges `portal_inbox_storage_degraded` and `portal_inbox_spool_alerts` show the state.
- The Inbox validates an alert as usual and keeps it in a spool. The spool holds 10 000 alerts at most. When it is full, the sender gets `503 spool_full` and sends the alert again. The spool holds one delivery for each phone that is subscribed. The id of the alert comes from the source and the idempotency key. Two instances that accept the same alert make the same id.
- With the block `inbox.cluster`, the Inbox copies the alert to the other instances before it answers the sender. The alert then survives the loss of the instance that accepted it. The log has `spool_unreplicated` when no other instance had the copy.
- When the primary is back, each instance writes its spool, oldest first. The write ignores an alert that exists (`ON CONFLICT DO NOTHING`). A crash in the middle gives one alert row and one delivery row for each phone.

Some actions still need the database: enrollment, a change of subscription, the revocation of a phone and the history in the page. A phone that is revoked in the outage can connect until the database is back.

An alert that the Inbox accepts while the database is away exists only in memory. If every instance that holds it stops before the database is back, the Inbox loses the alert. The sender already got `201`. A request to the database that timed out can still commit. If the sender retries it in the outage and no instance remembers it, a phone can get it twice with two ids. A pod that starts while the database is away waits 60 seconds for it, then exits.

## Roadmap

The Inbox has three steps. Only step 1 exists. The numbered requirements in [`SPEC.md`](SPEC.md) mark steps 2 and 3 as future.

| Step | Who sends | Kinds of message | Key kind |
|---|---|---|---|
| 1 (now) | Services, scripts, Grafana and Alertmanager | Alert | `api_` |
| 2 | Agents | Alert | `mcp_` |
| 3 | Agents | Question, approval | `api_` and `mcp_` |

At step 2, an agent first sends alerts through the same REST route with an `api_` key of its own. The MCP side comes later with the tools map. At step 3, an agent asks a question or asks for an approval, and a person answers on the phone. This ends the rule that alerts are one way. Step 3 needs a new protocol version, a state machine for each item, a timeout and an audit record.

## Troubleshoot

| Symptom | Check |
|---|---|
| The app says the server is unreachable. | Open the address of `inbox.devices.public_address` from the phone. Check the port, the firewall and the proxy. The proxy must send `/v1/` to the portal without ForwardAuth. The answer `503` means that the budget of sessions is full. Run `portal inbox healthcheck --config FILE` on the server. |
| The app says "The code is not valid". | The code is used or expired. Make a new code. Check `inbox.devices.max_devices`. The log has the event `enroll_refused` with a reason. |
| The app says "Update the app". | The app is older than protocol 4. Install the current APK. |
| A phone is `unreachable`. | The app is stopped, the phone is off, or the battery saver blocks the app. Check the permissions of the app. |
| An alert does not come. | Open the alert in the zone **Sent**. Check that the phone subscribed to the group and that the account has `inbox.receive`. |
| A sender gets `401`. | The key is not an `api_` key, or it has a bad form, or it is revoked or expired. Check that `keys.api` is `true`. |
| A sender gets `403`. | The key cannot send to the group. Check the block `api_key`, or the permission `alert.send` of the key and of its owner. For an account, check `alert.send_user`, or `direct_alerts` of the block `api_key`. |
| A sender gets `409 no_phone`. | The account has no phone that is not revoked. The owner of the account must enroll a phone. |
| A sender gets `429`. | The budget `alerts` is empty. Wait for `Retry-After`, or send to a critical group. |
| A sender gets `503 directory_not_ready`. | The portal has no fresh directory. A key of the file still works. Check `portal_directory_age_seconds`. |
| After a rights change, a phone lost a group. | The account lost `inbox.receive`. The Inbox removed the subscription. Give the permission again. |
| Every phone is invalid after a restart. | The key file of the server changed. Restore the old file. Else, every phone must enroll again. |
| The tab Inbox is absent. | The file has no block `inbox`, or the account has no permission of the Inbox. |
| The tab Access has no kind API for a new key. | `keys.api` is `false`, or the account has no `keys.own.create` on `key_kind:api`. |

The log of the Inbox is in the JSON log of the portal. Each line has the field `event`.
