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
- `crates/inbox-server` is the old server of Inbox. It stays until step 3 moves its code to `src/inbox/`.
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
