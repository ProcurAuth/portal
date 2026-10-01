# Inbox

Inbox is a self-hosted inbox. A single server receives alerts from your
monitoring and makes subscribed Android phones ring, even when they are on silent.

- **One static binary** (Rust, musl). The admin front is embedded, the database is
  SQLite, and the configuration is one strict HCL file.
- **Standalone.** It needs no Firebase, no Google services and no external
  database. OpenTelemetry export is optional and never blocks delivery.
- **Delivery guarantee.** Every subscribed device gets every alert exactly once,
  including alerts sent while it was off, in airplane mode or disconnected.
- **Secure on plain HTTP.** The password is used only inside OPAQUE, and device
  sessions are Noise IK. A network capture yields neither the password nor any
  material for an offline attack.
- **Receive-only Android app.** For each group, the user chooses *alarm* (loud and
  full-screen until acknowledged, overriding silent, vibrate and do-not-disturb),
  *ring* (one sound, even on silent) or *notify*.

French summary: [docs/SPEC.md](docs/SPEC.md) (cahier des charges).

## Architecture

```text
 Alertmanager / Grafana ──webhook──┐
 scripts, CI ──── POST /api/v1/alerts ─┤        ┌──────────────── inbox ────────────────┐
                                      ├──8080──▶│ producer API   device sessions (Noise) │◀── WebSocket ── Android app
 admin browser ── OPAQUE + signed ───────8081──▶│ admin API + embedded front (wasm)       │      (foreground service)
                  requests                      │ SQLite (WAL, synchronous=FULL)          │
                                                │ OTLP/HTTP ──▶ collector (optional)      │
                                                └─────────────────────────────────────────┘
```

| Path | Contents |
|---|---|
| `crates/inbox-core` | Protocol and cryptography, with no I/O. Shared by the server, the app (JNI) and the front (wasm). |
| `crates/inbox-server` | The `inbox` binary. |
| `crates/inbox-android`, `android/` | JNI bridge and the Android app. |
| `crates/inbox-wasm`, `web/` | Admin front. |
| `deploy/` | Docker Compose and k3s. |
| `docs/` | Specification, configuration, protocol, integrations. |

## Quick start (Docker Compose)

```sh
git clone git@github.com:SCcagg5/Inbox.git && cd Inbox/deploy
./init-secrets.sh            # builds the image, asks for the device and admin passwords
$EDITOR config.hcl           # name, groups, producers
docker compose up -d --build
docker compose ps            # wait for "healthy"
```

1. **Admin front.** It listens on `127.0.0.1:8081`. Reach it with
   `ssh -L 8081:127.0.0.1:8081 <host>` or put it behind a TLS reverse proxy. Log in
   with the admin password.
2. **Phone.** Install the APK from the GitHub release (`inbox-<version>.apk`). Add
   a server with its address (`host:8080`, a domain, or an `https://` URL behind a
   proxy), the device password and a device name. Grant the permissions the app
   asks for, then choose an action for each group.
3. **Test.** Send an alert from the front, or with curl:

```sh
TOKEN=$(cat deploy/secrets/alerting_token)
curl -fsS -X POST http://localhost:8080/api/v1/alerts \
  -H "Authorization: Bearer $TOKEN" \
  -H "Idempotency-Key: test-$(date +%s)" \
  -H "Content-Type: application/json" \
  -d '{"group":"oncall","title":"Disk full","message":"db-1 /var at 98%"}'
```

### Démarrage rapide (FR)

1. `cd deploy && ./init-secrets.sh` crée la clé serveur, les mots de passe (appareils
   et admin) et le token d'émetteur.
2. Adapter `config.hcl` (nom, groupes, émetteurs), puis lancer `docker compose up -d --build`.
3. Ouvrir le front admin (port 8081, accessible en local uniquement, via tunnel SSH
   ou TLS). Installer l'APK, ajouter le serveur (adresse, mot de passe appareils,
   nom), puis choisir l'action de chaque groupe.

Without Compose, the binary needs only a config file and its secret files:

```sh
inbox keygen --out /etc/inbox/server_key
inbox credential --server-key /etc/inbox/server_key --role device --out /etc/inbox/device_credential
inbox credential --server-key /etc/inbox/server_key --role admin  --out /etc/inbox/admin_credential
inbox token --out /etc/inbox/alerting_token
inbox check-config --config /etc/inbox/config.hcl
inbox serve --config /etc/inbox/config.hcl
```

## Delivery guarantees

1. The API answers `201` only after the alert is on disk, together with one
   delivery row per device subscribed at that moment, in a single transaction.
2. The server resends every delivery the device has not confirmed as `received`.
   It resends during the session (`resend_after`) and on every reconnection.
3. The phone stores the alert before confirming it. It deduplicates on the
   128-bit alert id, and on restart it re-posts alerts it stored but never showed.
4. Undelivered alerts never expire. Retention only purges history that has
   already been delivered.
5. Gaps in coverage are visible:
   - A subscribed device that stops reporting becomes *unreachable* in the front and in `inbox_devices_unreachable`.
   - The phone shows its connection state permanently and warns after `offline_warning_after`.

Every producer request requires an `Idempotency-Key`. Retrying with the same key
returns the same `alert_id` (`200`, `"duplicate": true`) and never pages twice.
Reusing a key with different content returns `409`.

## Security model

- **Enrollment** uses OPAQUE (ristretto255, Argon2id) with the shared device
  password.
  - The password never leaves the phone.
  - An attacker without the server's credential record can neither impersonate
    the server nor enroll a device.
  - Credential records are created offline (`inbox credential`), so the running
    server never sees a password.
- **Sessions** use Noise IK between a key generated on the phone and the server
  key the phone pinned at enrollment. This gives mutual authentication,
  encryption and forward secrecy. The phone does not store the password, and
  each device is revocable from the front.
- **Admin front.**
  - It has its own password and logs in with OPAQUE in the browser (WebAssembly).
  - Every later request is HMAC-signed with a timestamp and a nonce, so passive
    capture allows neither forging nor replaying a request.
  - **Caveat:** over plain HTTP, an *active* attacker can tamper with the
    JavaScript the server sends. Keep port 8081 private or behind TLS.
- **Hardening.**
  - Failed attempts are rate-limited per client address.
  - Producer tokens are compared in constant time.
  - The front has a strict CSP.
  - The image is `scratch`, runs as non-root with a read-only root filesystem,
    and drops all capabilities.
- TLS is the reverse proxy's job. The app accepts `https://` addresses.

Details: [docs/protocol.md](docs/protocol.md), [SECURITY.md](SECURITY.md).

## Configuration

Inbox reads one HCL file. Every attribute is mandatory, unknown attributes are
rejected and secrets are only referenced as files. Nothing starts if anything is
missing or invalid. To check a file and the secrets it references, run
`inbox check-config --config <file>`.

- [config.example.hcl](config.example.hcl) is annotated.
- [docs/configuration.md](docs/configuration.md) is the complete reference.

## HTTP API (port 8080)

All producer routes take `Authorization: Bearer <token>`. A token is limited to
the `groups` of its `producer` block.

| Method and path | Purpose |
|---|---|
| `POST /api/v1/alerts` | Body: `{"group": "...", "title"?: "...", "message"?: "..."}`. Header `Idempotency-Key` is required. Returns `201 {alert_id, duplicate, targets}`. |
| `GET /api/v1/alerts/{id}` | Delivery status per device: sent, received, displayed and acknowledged, with attempt counts. Use it to escalate elsewhere. |
| `POST /api/v1/webhooks/alertmanager/{group}` | Alertmanager / Grafana webhook payload. One Inbox alert per alert, deduplicated on fingerprint + status + `startsAt`. |
| `GET /healthz`, `GET /readyz` | Liveness, and readiness (checks the database). |
| `GET /api/version` | Version, git revision, protocol version. |

Errors are JSON, of the form `{"error": {"code": "...", "message": "..."}}`.

```sh
# Delivery status of an alert
curl -fsS -H "Authorization: Bearer $TOKEN" http://localhost:8080/api/v1/alerts/<alert_id>
```

## Grafana and Alertmanager

Grafana contact points are compiled into Grafana: there is no plugin API to add a
new contact point type. A "Inbox plugin" would not be able to receive alerts, so
Inbox speaks the format Grafana already sends instead.

- **Grafana:** create a **Webhook** contact point with:
  - URL `http(s)://<inbox>/api/v1/webhooks/alertmanager/<group>`;
  - *Authorization header* scheme `Bearer` and credentials = the producer token.
  
  The provisioning example is [docs/grafana/contact-point.yaml](docs/grafana/contact-point.yaml).
- **Alertmanager:** use a `webhook_configs` receiver with a bearer token. The
  example is [docs/alertmanager.yml](docs/alertmanager.yml).

Resolved notifications are ignored unless `webhook_send_resolved = true`.
Alertmanager's repeat notifications of the same firing are deduplicated.

## Android app

The app is Android-only and receives alerts only. It keeps one persistent,
encrypted connection per server inside a foreground service. It does not use
Firebase. It asks for these permissions:

| Permission | Why |
|---|---|
| Notifications | Showing alerts (Android 13+). |
| Battery optimization exemption | Keeps the connection alive in Doze. Without it, Android may delay delivery. |
| Do not disturb access | Lets *alarm* and *ring* override do-not-disturb and silent mode. |
| Full-screen intent | Shows the *alarm* screen over the lock screen. |
| Run at startup | Reconnects after a reboot. |

On some vendor ROMs (Xiaomi, Huawei, Samsung…), also allow background activity /
"no restrictions" for Inbox. "Force stop" kills the app until it is opened again.
The server then reports the device as unreachable.

### Testing without a phone

`tools/android/emulator.sh start` boots an Android 14 emulator in Docker (needs
`/dev/kvm`), installs `dist/inbox-<version>.apk`, grants the onboarding
permissions and prints the address of its screen, shown in the browser at
<http://localhost:6080>. With Inbox running on the host (`docker compose up`
in `deploy/`), add the server `172.17.0.1:8080` in the app: that is the host,
seen through the Docker bridge. `emulator.sh logs` follows the app logs,
`emulator.sh install` reinstalls a new build and `emulator.sh stop` removes
the emulator. Sound, vibration and OEM battery savers can only be judged on a
real phone.

## Operations

- **Back up** the SQLite database (`/var/lib/inbox/inbox.db`, use `sqlite3 .backup`
  or stop the service) and **the server key**. Losing the server key means
  re-enrolling every phone.
- **Upgrades** apply schema migrations automatically at start. Downgrading to a
  binary older than the schema is refused.
- **Shutdown** on SIGTERM drains within `server.shutdown_grace`. Phones are told
  1012 (server restarting) and reconnect as soon as the server is back.
- **Logs** are JSON on stderr, one `event` field per line.
- **Metrics** (OTLP) are listed below.

| Metric | Type | Meaning |
|---|---|---|
| `inbox_alerts_accepted_total{source}` | counter | Alerts accepted (`producer`, `webhook`, `admin`) |
| `inbox_alerts_duplicate_total{source}` | counter | Submissions deduplicated by idempotency key |
| `inbox_deliveries_created_total` | counter | Per-device delivery rows |
| `inbox_deliveries_sent_total` | counter | Alert frames sent, including retransmissions |
| `inbox_acks_total{stage}` | counter | Device acknowledgements |
| `inbox_delivery_latency_seconds` | histogram | Acceptance to first receipt |
| `inbox_deliveries_pending` | gauge | Deliveries still waiting for a device |
| `inbox_oldest_pending_delivery_seconds` | gauge | Age of the oldest waiting alert. **Alert on this.** |
| `inbox_devices_unreachable` | gauge | Subscribed devices not seen recently. **Alert on this.** |
| `inbox_devices_enrolled` | gauge | Non-revoked devices |
| `inbox_device_sessions` | up-down counter | Connected devices |
| `inbox_device_session_closes_total{reason}` | counter | Session ends |
| `inbox_auth_failures_total{kind}` | counter | Failed authentications |
| `inbox_enrollments_total` | counter | Devices enrolled |

## Building

Building needs only Docker (BuildKit) and python3.

```sh
tools/build.sh server   # dist/inbox_<version>_linux_<arch>, static, verified
tools/build.sh image    # inbox:<version>
tools/build.sh apk      # dist/inbox-<version>.apk (signing key from INBOX_KEYSTORE*)
tools/build.sh dist     # tarballs + SHA256SUMS
tools/dev/cargo.sh test # cargo in the pinned toolchain image
```

Releases are built by `.github/workflows/release.yml`, on a `vX.Y.Z` tag that
matches the workspace version. They contain:
- static Linux amd64 and arm64 binaries;
- the APK signed with the release key from repository secrets;
- `SHA256SUMS`.

No container image is ever published to a registry (GHCR or other): build it
locally with `tools/build.sh image` or `docker compose up --build`.

## Limitations

- **Single node.** State lives in one SQLite file, so do not run two replicas.
- **Android only.** iOS cannot keep a connection in the background without Apple's push service.
- **TLS** is the reverse proxy's job.
- **Telemetry export** is OTLP/HTTP over `http://` only.

## Kubernetes (k3s)

[deploy/k3s](deploy/k3s) holds a single-replica StatefulSet on a `local-path`
volume, a ConfigMap, a Secret mounted at `/run/secrets`, probes on `/healthz` and
`/readyz`, and Traefik IngressRoutes. It follows the conventions of the Home
infrastructure repository. To export telemetry to the cluster collector, first
allow traffic from `apps` to `observability:4318` in the NetworkPolicies.
