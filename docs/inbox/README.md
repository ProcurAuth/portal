# Inbox

The inbox is a feature of the portal. It sends alerts to Android phones. It makes sure that each subscribed phone shows each alert one time. The producers use the port of the pages. Only the phones have a port of their own.

This chapter is for operators and for people who send alerts. The root `README.md` has the reference of the configuration. [`SPEC.md`](SPEC.md) has the requirements. [`protocol.md`](protocol.md) has the wire protocol of the phones. [`SECURITY.md`](SECURITY.md) has the security model.

## Concepts

| Word | Use |
|---|---|
| Alert | One group and one title. The title has one line of 128 characters at most. An alert has no body and no severity. |
| Group | A named list of phones that follow it. The configuration file defines the groups. A group can be `critical`. |
| Producer | A program that sends alerts: a script, Grafana, Alertmanager or the portal itself. |
| Phone | An Android phone that runs the app and enrolled with an account of the portal. |
| Account | An account of the portal. It owns its phones. Its login is the user name of each phone. |
| Subscription | The link between one phone and one group. The phone chooses it. The account must have the permission `group.subscribe` on the group. |
| Delivery | The record that one alert must reach one phone. The inbox makes one delivery for each phone that is subscribed when it accepts the alert. |
| Enrollment code | A text that the account makes in the inbox pages. It lets one phone enroll one time. |

The phone decides how it reacts to an alert. The server never knows this choice. The choices are `alarm`, `ring` and `notify` (see "The Android app").

## What the inbox guarantees

Each subscribed phone gets each alert, and shows it one time. This holds also when the phone was off, in airplane mode or disconnected when the alert came.

1. The API answers `201` only after the alert is on disk. The alert and one delivery for each subscribed phone are in one transaction.
2. The inbox sends again each delivery that the phone did not confirm. It sends again during the session (`inbox.devices.resend_after`) and at each reconnection.
3. The phone stores the alert before it confirms it. It ignores an alert id that it already has.
4. If the phone stores an alert and the app stops before it shows it, the app shows it at the next start.
5. The inbox keeps an alert for a phone until `inbox.storage.delivery_ttl`. After this time, the delivery expires, and the pages show it as expired.
6. The inbox deletes the history after `inbox.storage.alert_retention`.

A gap in coverage is visible. A subscribed phone that the inbox does not see for `inbox.devices.unreachable_after` is `unreachable` in the pages and in the metric `inbox_devices_unreachable`. The app shows its connection state all the time. It warns the user after `inbox.devices.offline_warning_after`.

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

The permissions of the service `inbox` control each group:

- `group.subscribe` lets the phones of an account subscribe to the group.
- `alert.send` lets an account or a key send an alert to the group.
- `alert.read` lets an account or a key read the alerts of the group and their delivery.

Give these permissions in the editor of permissions of the account or of the key. The editor lists each group by name. A permission on the whole service is not valid for these three actions. Give one permission for each group.

The budget `alerts` limits `alert.send`. It counts alerts in a window of a second, a minute, an hour or a day. A group with `critical = true` has no limit. The limit never stops an alert to this group.

## Send an alert (producers)

A producer sends a request to the portal, on the same host and port as the pages. These routes need no session of the portal, and the proxy does not apply ForwardAuth to them. The producer proves itself with a bearer token or a key. The phones use the same port and the same proxy rule (`/v1/enroll` and `/v1/session`).

| Route | Use |
|---|---|
| `POST /api/v1/alerts` | Body: `{"group": "<id>", "title": "<text>"}`. The answer is `201` with `alert_id`, `duplicate` and `targets`. |
| `POST /api/v1/notify/{group}` | A webhook for any tool. The title comes from `?title=`, from the header `X-Title`, from the field `title` of a JSON body, or from the body. The inbox cuts a long title. |
| `POST /api/v1/webhooks/alertmanager/{group}` | The payload of Alertmanager and of Grafana. Each alert with the status `firing` makes one alert. |
| `GET /api/v1/alerts/{id}` | The delivery status of an alert: sent and received, with the number of attempts. |
| `GET /healthz`, `GET /readyz`, `GET /api/version` | Health and version. No token. |

Send the token in the header `Authorization: Bearer <token>`. A tool that cannot set a header can use the query `?token=<token>`. A proxy can write an address with a query in its log, so use the header when you can.

Two kinds of token work at the same time.

- **A token of the file.** It is the value of a block `producer`. The block lists the groups of the token. This token does not depend on the portal. It works when the portal is not ready.
- **A key of the portal.** It starts with `mcp_`. The key and its owner must both have `alert.send` on the group. A key can read the status of an alert with `alert.read` on the group of the alert.

Make a token of the file with `portal inbox token --out FILE`. Make a key on the page **Keys** of the portal. Add the permission `alert.send` of the service `inbox` for each group.

The header `Idempotency-Key` is optional. It has 1 to 128 printable ASCII characters. Two requests with the same key make one alert. The second answer is `200` with `"duplicate": true`. A key that two different alerts use gets `409`. Without the header, the inbox derives a key from the content and the minute.

| Status | Use |
|---|---|
| `401` | The token is absent or not valid. |
| `403` | The token cannot send to this group. The code is `forbidden_group`. |
| `409` | The idempotency key belongs to a different alert. |
| `413` | The title has more than 128 characters, or the request body is too large. |
| `422` | The group does not exist, or the title is empty or has a control character. |
| `429` | The budget `alerts` of the key is empty. `Retry-After` gives the seconds to wait. |
| `503` | The inbox cannot take the alert now. Send it again. See the codes below. |

The code of a `503` tells the cause. `directory_not_ready`: the portal cannot check a key yet. `rights_unavailable`: the count of the budget failed. `replication_unavailable`: the copy to the replica failed. `spool_full`: the memory buffer of the degraded mode is full. An error body has the form `{"error": {"code": "...", "message": "..."}}`.

The page **Documentation** of the tab Inbox shows these routes with the real address and ready recipes.

### Alertmanager and Grafana

Grafana has a fixed list of contact point types. A plugin cannot add one. The inbox accepts the payload that Grafana and Alertmanager already send.

- **Alertmanager.** Use a `webhook_configs` receiver with a bearer token. The example is [`alertmanager.yml`](alertmanager.yml).
- **Grafana.** Make a contact point of the type Webhook. The example is [`grafana/contact-point.yaml`](grafana/contact-point.yaml).

The inbox ignores the notifications `resolved`. A producer of the file can set `webhook_send_resolved = true` to turn them into alerts. A key never sends them. The inbox merges a repeat of the same alert. It compares the fingerprint, the status and `startsAt`.

### Alerts of the portal

The portal can send alerts about itself, in process, with the source `portal`. It needs the block `inbox.portal_alerts`. The alerts are: a service is unhealthy, a service is healthy again, and a burst of refused sign-ins. The root `README.md` describes them.

## Enroll a phone

An account makes the code, and the phone uses it one time.

1. Sign in to the portal with the account that owns the phone.
2. Open the page **My phones** of the tab Inbox. The account needs `group.subscribe` on one group at least.
3. Press **Make a code**. The page shows the code one time.
4. On the phone, install the app and open **Add a server**.
5. Type the code, or tap the link `inbox://enroll?...` on the phone.
6. Compare the fingerprint that the app shows with the fingerprint on the page.
7. Confirm. The app connects, enrolls and shows the groups.
8. Choose a group and an action for each group that the app lists.

The code holds the address of the server, the public key of the server and a secret of 128 bits. It works one time. It lives `inbox.enrollment.code_ttl` (10 minutes by default). An account can have `inbox.enrollment.max_active_per_account` codes that are not used and not expired. The code is a bearer secret until its first use. Show it only to its owner.

The inbox checks the secret in a Noise IK handshake to the pinned key of the server. Only the holder of the server key can read the secret. The wire details are in [`protocol.md`](protocol.md).

If the enrollment fails, the app shows one of these texts:

| Text in the app | Cause |
|---|---|
| The code is not valid. Ask for a new one. | The code is unknown, used or expired. The account was deleted. The limit `inbox.devices.max_devices` is reached. |
| Update the app. | The app does not speak protocol 4. |
| Too many attempts. | The client address sent too many attempts. Wait. |

To remove a phone, open **My phones** and press **Revoke**. An account revokes its own phones only. An account with `device.revoke` and `device.read` can revoke any phone on the page **Phones**. The phone gets the close code 4403. It must enroll again.

## The pages of the tab Inbox

The pages are under `/auth/settings/inbox`. The tab shows for an account that has a permission of the service `inbox`. The launcher has no tile for the inbox. The gear of the launcher opens the pages.

| Page | Needs | Use |
|---|---|---|
| Overview | `alert.read`, `device.read`, `connection.read` or `alert.send` | The figures of the delivery, the groups, the form **Send an alert**, the last alerts. |
| Alert | `alert.read` on the group | The delivery of one alert to each phone. |
| Phones | `device.read` | All phones, with state, owner and groups. **Revoke** needs `device.revoke`. |
| Connections | `connection.read` | The connection log, with a filter on the login. |
| My phones | `group.subscribe` on one group | The phones of the account, **Make a code** and **Revoke**. |
| Documentation | any page of the tab | The producer API with the real address. |

A session that the superadmin opens with **Log in as** can read the pages and make a code. It cannot send an alert and it cannot revoke a phone.

## The Android app

The app only receives alerts. It does not send alerts. It does not use Firebase or any push service of a third party. It keeps one encrypted connection for each server in a foreground service. One app can enroll in several servers.

For each group, the user chooses one action. This choice stays on the phone.

| Action | Behaviour |
|---|---|
| `alarm` | A loud alarm, in a loop, until the user stops it on the phone. It opens full screen. It overrides silent mode, vibrate mode and do-not-disturb. |
| `ring` | One notification sound, also in silent or vibrate mode. |
| `notify` | A standard notification. It follows the mode of the phone. |

The app asks for these permissions on its first screen.

| Permission | Reason |
|---|---|
| Notifications | Android 13 and later: show alerts. |
| Full-screen alarms | Show the alarm over the lock screen. |
| Ignore battery optimization | Keep the connection alive in Doze. Without it, Android can delay delivery. |
| Do Not Disturb access | Let `alarm` and `ring` override silent mode and do-not-disturb. |
| Display over other apps | Let an alarm open full screen when the phone is in use. |

The app also starts again after a reboot. On some vendor ROMs (Xiaomi, Huawei, Samsung and others), allow background activity for Inbox. "Force stop" stops the app until the user opens it again. The inbox then shows the phone as unreachable.

A person cannot change the login of a phone. The app shows it read-only. The source drawings of the app icon are in `assets/brand/`.

### Test with the emulator

No physical phone is needed for the tests of the app. `tools/android/emulator.sh` runs an Android 14 emulator in Docker. It needs `/dev/kvm`.

1. Build the app: `tools/build.sh apk`. The script `tools/android/build-apk.sh` lists the variables for the key of the APK.
2. Start the emulator: `tools/android/emulator.sh start`. The script installs `dist/inbox-<version>.apk`, grants the permissions and prints the address of the screen. Open <http://localhost:6080> in a browser.
3. Start the stack (see `deploy/README.md`). Publish the port of the portal to the network: `PORTAL_BIND=0.0.0.0`.
4. Set `inbox.devices.public_address` to `172.17.0.1:<port>`. This address is the host as the emulator sees it through the Docker bridge.
5. Make a code on the page "My phones". Open its link on the phone: `adb shell am start -a android.intent.action.VIEW -d '<link>'`.

`emulator.sh logs` shows the logs of the app. `emulator.sh install` installs a new build. `emulator.sh stop` removes the emulator. Sound, vibration and the battery savers of vendors can be judged only on a real phone. If the noVNC page shows a JavaScript error from a `chrome-extension://` script, open the page in a private window.

The demo in `demo/` can also be used. See `demo/README.md`.

## Storage and failure modes

The inbox uses the database of the portal. With SQLite, it uses the same file. With PostgreSQL, it uses the same database. It keeps its own chain of migrations. See the root `README.md`.

### SQLite: one active instance and a standby

SQLite needs a local file system. Two instances must never open the same file. To survive the loss of the node, use the block `inbox.replica`.

The block names a directory that every instance shares. It must be a volume of plain files. The directory holds:

- `snapshot-<epoch>.db`, a consistent copy of the whole database. The inbox writes it when the database changed, at most every `snapshot_interval`. It writes it also before it confirms an alert, an enrollment or a revocation.
- `lease.json`, the name of the active instance. The owner renews it every `lease_ttl / 3`.
- `claim-<epoch>`, a file that the inbox creates with exclusive access. Two standby instances cannot take the same epoch.

An instance that starts waits until the lease is released or has not changed for `lease_ttl`. It takes the next epoch. It restores the newest snapshot that passes `PRAGMA quick_check`. Then it serves. An instance that loses its lease stops the whole process. The orchestrator starts it again as a standby.

With `sync_alerts = true`, the inbox confirms an alert only after the snapshot that holds it exists. A producer that gets no answer sends the alert again, and the idempotency key makes one alert. If the shared directory fails, the producer gets `503 replication_unavailable`. With `sync_alerts = false`, the inbox confirms after the local commit, and a failed copy is only logged (`inbox_replica_sync_failures_total`).

Receipts and last-seen times travel with the next snapshot. After a takeover, a phone can get an alert again when its receipt was lost. The app ignores it.

Run this as a Deployment with one replica and the strategy `Recreate`. Do not use a StatefulSet: Kubernetes does not move its pod from a node that it cannot reach. Use a short `tolerationSeconds` for `node.kubernetes.io/unreachable` and `not-ready`. Give the startup probe enough time for the wait of a standby and the restore.

### PostgreSQL: several active instances

With the block `postgres`, any number of instances can be active. They are behind a load balancer. A phone connects to any instance. Any instance accepts alerts.

- An alert and its deliveries are one transaction. The idempotency key is a unique index. A producer that retries on another instance gets `200`.
- The instances tell each other with `LISTEN` and `NOTIFY`. A new alert wakes a phone that another instance holds. A session also reads the database at the interval `min(resend_after, 5s)`. The notifications are a speed-up, not a dependency.
- Each instance writes a heartbeat in the table `instances` every 5 seconds. The other instances close the sessions of an instance that is silent for 30 seconds, with the reason `instance_lost`.
- The limiter of enrollment attempts is in memory in each instance. N instances give N times the attempts.
- For the same guarantees as SQLite, use a PostgreSQL that is replicated with synchronous commit.

To move from SQLite to PostgreSQL, stop the SQLite instance. Run `portal inbox import-sqlite --config FILE --from SQLITE_FILE` with a configuration that has a block `postgres`. The target database must be empty. Then start the PostgreSQL instances. The command copies phones, subscriptions, alerts, deliveries, enrollment codes and the connection log. The source can be a file of the portal.

### Degraded mode: the database has no primary

A PostgreSQL failover takes seconds to minutes. In this time, the instances still accept alerts and still deliver them. The phones keep their sessions. `/readyz` answers `200 degraded`.

- Each call to the database has a limit of 2 seconds. When a call fails, the instance switches to memory. A probe checks each second for a writable primary. The log has `storage_degraded` and `storage_recovered`. The gauges `inbox_storage_degraded` and `inbox_spool_alerts` show the state.
- The inbox validates an alert as usual and keeps it in a spool. The spool holds 10 000 alerts at most. When it is full, the producer gets `503 spool_full` and sends the alert again. The spool holds one delivery for each phone that is subscribed. The id of the alert comes from the source and the idempotency key. Two instances that accept the same alert make the same id.
- With the block `inbox.cluster`, the inbox copies the alert to the other instances before it answers the producer. The alert then survives the loss of the instance that accepted it. The log has `spool_unreplicated` when no other instance had the copy.
- When the primary is back, each instance writes its spool, oldest first. The write ignores an alert that exists (`ON CONFLICT DO NOTHING`). A crash in the middle gives one alert row and one delivery row for each phone.

Some actions still need the database: enrollment, a change of subscription, the revocation of a phone and the history pages. A phone that is revoked in the outage can connect until the database is back.

An alert that the inbox accepts while the database is away exists only in memory. If every instance that holds it stops before the database is back, the inbox loses the alert. The producer already got `201`. A request to the database that timed out can still commit. If the producer retries it in the outage and no instance remembers it, a phone can get it twice with two ids. A pod that starts while the database is away waits 60 seconds for it, then exits.

## Troubleshoot

| Symptom | Check |
|---|---|
| The app says the server is unreachable. | Open the address of `inbox.devices.public_address` from the phone. Check the port, the firewall and the proxy: it must send `/v1/` to the portal without ForwardAuth. The answer `503` means that the budget of sessions is full. Run `portal inbox healthcheck --config FILE` on the server. |
| The app says "The code is not valid". | The code is used or expired. Make a new code. Check `inbox.devices.max_devices`. The log has the event `enroll_refused` with a reason. |
| The app says "Update the app". | The app is older than protocol 4. Install the current APK. |
| A phone is `unreachable`. | The app is stopped, the phone is off, or the battery saver blocks the app. Check the permissions of the app. |
| An alert does not come. | Open the page of the alert. Check that the phone subscribed to the group and that the account has `group.subscribe`. |
| A producer gets `403`. | The token cannot send to the group. Check the block `producer` or the permission `alert.send` of the key and of its owner. |
| A producer gets `429`. | The budget `alerts` is empty. Wait for `Retry-After`, or send to a critical group. |
| A producer gets `503 directory_not_ready`. | The portal has no fresh directory. A token of the file still works. Check `portal_directory_age_seconds`. |
| After a rights change, a phone lost a group. | The account lost `group.subscribe`. The inbox removed the subscription. Give the permission again. |
| Every phone is invalid after a restart. | The key file of the server changed. Restore the old file. Else, every phone must enroll again. |

The log of the inbox is in the JSON log of the portal. Each line has the field `event`.
