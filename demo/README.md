# Local demo

This demo runs the portal and the inbox with Traefik and example services on one machine. It uses Docker only. Nothing goes to a registry.

## Start and stop

1. Run `./demo/up.sh`. It builds the portal image from `docker/Dockerfile`, makes the secrets, starts the stack and runs the seed.
2. Open `http://localhost:8088/`.
3. Read the logins and the passwords in `demo/.secrets/credentials.txt`. The password of `admin` is `test`. This is for a demo on one machine only.
4. To stop the demo and delete its data, run `docker compose -f demo/docker-compose.yml down -v`. Then delete `demo/.secrets/alice_key`.

The stack has these parts:

| Part | Role |
|---|---|
| `traefik` | The edge. The port 8088 is open on loopback and on the Docker bridge (`172.17.0.1`, for the emulator). |
| `portal` | The portal: sessions, the PDP on port 4182, the edge for keys, and the inbox. |
| `notes` | A service with a front. It asks the PDP for each operation. |
| `files`, `wiki` | Two example services. They show the request headers. |
| `bigdb`, `chdash` | Two services with large action documents. |

The demo publishes these ports:

| Port | Use |
|---|---|
| `127.0.0.1:8088` and `172.17.0.1:8088` | The portal, through Traefik: the pages, the API of the inbox for the senders and the webhooks (`/api/v1/*`, `/api/version`), and the phones (`/v1/enroll`, `/v1/session`). The address `172.17.0.1` is the host as the Android emulator sees it. It is the `inbox.devices.public_address` of `portal.hcl`. |
| `127.0.0.1:8091` | The listener of the portal without Traefik. Use it for `/metrics` and `/healthz`. |

The inbox is a feature of the portal, with one port. Traefik sends `/v1/`, `/api/v1/` and `/api/version` to the portal without ForwardAuth, because phones and senders have no session. They prove themselves. The two routes of the phones are WebSocket upgrades. Traefik carries them with path routing. The middleware `inbox-retry` (4 attempts, 100 ms) sends a request again only when the portal does not answer at all.

To check the main points from a terminal, run `./demo/smoke.sh`. It signs in, tests the gate, a ceiling, a budget, a key and the inbox. Read the rate budgets at the top of the script. The budget of `alice` is 5 uses each minute, so wait one minute before a second run.

## Accounts

All accounts are database accounts. `seed.sh` sets their grants through the pages of the portal.

| Account | Grants |
|---|---|
| `admin` | Superadmin. Every right. No limit. |
| `alice` | `access` on notes, files and wiki. Read of all notebooks, with at most 2 notes in one read. Write and delete in the notebook `work`. A budget of 5 uses each minute on notes. In the inbox: `inbox.receive` on `infra` and `critical`, `alert.send` and `alert.read` on `infra`, and `alert.send_user` (an alert to any account). The rights to make and use the keys of both kinds, to read and to revoke a key. |
| `bob` | `access` on notes and wiki. Read of the notebook `public` only. In the inbox: `inbox.receive` on `infra`. |
| `carol` | No grant. |

The superadmin can add users and change grants in the settings page. The superadmin can also make keys in the tab **Access**. The button **New key** opens a page that fills the window. The page has a selector for the kind of key: API for services, MCP for agents. The service **ClickHouse Dash** looks like ChDash: three hosts, their databases (listed by the service, page by page) and the actions of ChDash. The service **Big database** publishes 424 actions and 220 resources. Use it to try the editor of grants and the tables with a large action document.

## Seed

`up.sh` runs `seed.sh` after the portal is ready. It is safe to run again. It does these steps through the pages of the portal:

1. It signs in as `admin` and adds the accounts `alice`, `bob` and `carol`.
2. It saves the grants of `alice` and `bob` on the page of each account. It saves the rights on keys of `alice` in the group **Server features** of her page.
3. It signs in as `alice` and makes the API key `demo-sender` on `/auth/keys/new?kind=api`. The key has `alert.send` on `infra`. The portal shows the key one time, and the script keeps it in `demo/.secrets/alice_key`.

The inbox has these groups and API keys:

| Item | Value |
|---|---|
| Group `infra` | A normal group. |
| Group `critical` | A group with `critical = true`. A limit never applies to it. |
| API key `local` | A key in `demo/.secrets/inbox_api_key`, owned by the file account `local`. It may send to `infra` and `critical`. |
| `watch` | The portal sends an alert to `infra` when a service stays unhealthy for 10 seconds, and when it is healthy again. |

## Send an alert from a terminal

Use the API key of the file:

```sh
curl -i -X POST http://localhost:8088/api/v1/alerts \
  -H "Authorization: Bearer $(cat demo/.secrets/inbox_api_key)" \
  -H "Idempotency-Key: demo-1" -H "Content-Type: application/json" \
  -d '{"group":"infra","title":"Test"}'
```

Use the key of `alice` in the same way: replace the key with `$(cat demo/.secrets/alice_key)`. The key may send to `infra`. For `critical` the answer is `403`.

## A phone in the emulator

This test needs `/dev/kvm`. The emulator is an Android 14 image in Docker. Its screen is at `http://localhost:6080`.

1. Build the APK with a signing key that is only for this test. Run `tools/android/build-apk.sh` with the variables `INBOX_KEYSTORE`, `INBOX_KEYSTORE_PASSWORD`, `INBOX_KEY_ALIAS` and `INBOX_KEY_PASSWORD` set. Make the keystore with `keytool -genkeypair`. Do not use the keystore of a release.
2. Run `tools/android/emulator.sh start dist/portal-inbox-<version>.apk`. Wait until it prints the address of the screen.
3. Sign in as `alice`. Open **Inbox**, then press the plus button of the zone **Phones**. A popover shows the code one time. It stays open until you press **Done**.
4. Open the link of the code in the emulator: `docker exec inbox-emulator adb shell am start -a android.intent.action.VIEW -d '<link>'`. The app opens the form with the code filled in. Compare the fingerprint with the page.
5. Type a name for the phone and press **Connect**. In the app, subscribe to the groups.
6. Send alerts from a terminal (see above). The phone shows each alert one time.
7. Stop the emulator with `tools/android/emulator.sh stop`. Delete the keystore of the test.

Tests to do with the phone:

1. **Two senders.** Send one alert with the key of the file and one with the key of `alice`. The phone shows each one time.
2. **Critical.** Send an alert to `critical` with the key of the file. The phone shows it.
3. **No network.** Turn on the airplane mode: `adb shell cmd connectivity airplane-mode enable`. Send an alert. Turn the mode off. The alert arrives one time.
4. **Rights.** As `admin`, open the page of `alice`, and remove `inbox.receive` on `critical`. Save. The inbox removes the subscription within a few seconds, and the app shows the group as off. A new alert to `critical` does not arrive. To give the right back, tick the permission again and save.
5. **Revoke.** As `admin`, open **Inbox**, then **Phones**, and revoke the phone. The app tells that it must enroll again.
6. **Portal alert.** Run `docker pause portal-demo-wiki-1`. The container stays in the catalogue, and its health check fails. After about 15 seconds the phone gets the alert that the service Wiki is unhealthy. Run `docker unpause portal-demo-wiki-1`. The phone gets the alert "healthy again". Do not use `docker stop`: a service that leaves the catalogue is forgotten with no alert.

## Tests to do

1. **Launcher.** Sign in as `bob`. The launcher shows notes and wiki, not files. Open `/files/` by hand: the portal refuses.
2. **Grants on nodes.** Open Notes as `bob`. Only the notebook `public` has the button Read. Sign in as `alice`: all notebooks can be read, and only `work` can be written.
3. **Ceiling.** As `alice`, open a notebook that has 3 notes. The list shows 2 notes. The ceiling `max_results` is 2.
4. **Budget.** As `alice`, press "Read 8 times". The first reads give 200, and then the answer is 429 with `Retry-After`. The budget is 5 uses each minute. Wait for the window to end, and read again.
5. **PDP playground.** Ask `notes.read` on `notebook:work/note:1`. Ask `notes.read` on `notebook:work` with `subtree`. Ask a resource that the tree does not have, for example `folder:x`: the answer is `invalid_resource`.
6. **Access tab.** As `admin`, open the settings, then Access. Click **New user**, choose its groups and give permissions with limits. Try the filters by user, group and service. Sign in as this user in a private window and look at the effect.
7. **Keys.** As `admin`, open the tab Access and click **New key**. Choose the kind MCP and make a key with the grants `access on service:notes` and `notes.read on service:notes`. The next page shows the key text one time, with Copy and Done. Copy the text. Paste it in "Use a key" in Notes. The key acts for its owner, and it never gets more than its owner.
8. **Key from a terminal.** Run `curl -i -H "Authorization: Bearer mcp_..." http://localhost:8088/notes/api/notebooks`. A bad key gives 401 with an empty body and the header `WWW-Authenticate`.
9. **Revoke.** Revoke the key on the page. The next call gives 401 within seconds.
10. **Limits on a key.** Make a key with a budget on `access on service:notes`. The counters of the key and of its owner both count each use.
11. **Services page.** As `admin`, open the page Services. It shows the document of notes with its tree, its actions and its limits.
12. **Logs.** Run `docker logs -f portal-demo`. The log has one line for each decision of the PDP and one line for each failed key check.

## Files

- `docker-compose.yml`: the stack.
- `portal.hcl`: the configuration of the portal. It has the block `keys {}`, the PDP and the block `inbox`.
- `traefik-dynamic.yml`: the middlewares for persons and for keys.
- The portal image is `docker/Dockerfile` of the repository. `up.sh` builds it as `local/portal:demo`.
- `up.sh`: builds the image, makes the secrets, starts the stack and runs the seed.
- `seed.sh`: makes the accounts, the grants and the key of `alice` through the pages of the portal.
- `smoke.sh`: the checks from a terminal. `lib.sh` has the functions that `seed.sh` and `smoke.sh` share.
- `notes/`: the service Notes (`app.py`, `index.html`).
