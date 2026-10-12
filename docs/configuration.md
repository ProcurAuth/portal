# Configuration reference

Inbox reads one HCL file, given with `--config`. The configuration is declarative
and strict:

- **Every attribute is mandatory.** There are no implicit defaults. The only
  optional elements are the `producer` blocks (zero producers is valid: the admin
  front can always send alerts), the endpoint settings of a disabled
  `telemetry` block and the `replica` block (absent: no replication; present: all
  its attributes are mandatory).
- **Unknown attributes and blocks are rejected**, so a typo stops startup instead
  of being ignored.
- **Semantic checks** (ranges, cross-field rules, references) run before anything
  starts. All errors are reported at once.
- **Secrets are never inline.** The file holds paths to secret files, read at
  startup. Their content never appears in logs or errors.
- Environment variables are not read.

Validate a file, including every secret it references:

```sh
inbox check-config --config /etc/inbox/config.hcl
```

Durations are strings with a unit suffix: `ms`, `s`, `m`, `h`, `d` (`"500ms"`,
`"20s"`, `"30d"`). The annotated example is [`config.example.hcl`](../config.example.hcl);
the Compose variant is [`deploy/config.hcl`](../deploy/config.hcl).

## `server`

| Attribute | Type | Constraint | Meaning |
|---|---|---|---|
| `name` | string | 1..64 bytes | Display name, shown in the app and the admin front. |
| `listen` | `"ip:port"` | ≠ `admin_listen` | Devices (`/v1/enroll`, `/v1/session`), producer API, webhooks, `/healthz`, `/readyz`, `/api/version`. |
| `admin_listen` | `"ip:port"` | ≠ `listen` | Admin front and admin API. Keep it private or behind TLS. |
| `trusted_proxies` | list of CIDR | may be empty | Peers allowed to set `X-Forwarded-For` / `X-Real-IP`. The forwarded chain is walked from the right, skipping trusted hops. |
| `shutdown_grace` | duration | ≥ 1s | Time given to open connections on SIGTERM / SIGINT. |
| `server_key_file` | path | file must exist | Server key from `inbox keygen`. Replacing it invalidates every credential record and every enrolled device. |

## `storage`

| Attribute | Type | Constraint | Meaning |
|---|---|---|---|
| `path` | path | parent directory must exist | SQLite database (WAL, `synchronous = FULL`). |
| `delivery_ttl` | duration | ≥ 1m | An alert a device has not received within this delay is no longer sent to it and shows as expired (7d in the examples). |
| `alert_retention` | duration | ≥ 1h, ≥ `delivery_ttl` | History (delivered and expired alerts) is purged after this. |
| `connection_log_retention` | duration | ≥ 1h | Closed entries of the connection log are purged after this. |

## `devices`

| Attribute | Type | Constraint | Meaning |
|---|---|---|---|
| `credential_file` | path | readable credential record | Shared device password, from `inbox credential`. |
| `heartbeat_interval` | duration | ≥ 5s | The app pings at this interval (sent to the app at session start). |
| `session_timeout` | duration | ≥ 2 × `heartbeat_interval` | A session with no frame for this long is closed. |
| `resend_after` | duration | ≥ 1s | An alert sent without a delivery receipt from the device is sent again after this. |
| `max_inflight` | integer | 1..1024 | Alerts sent and still waiting for their delivery receipt, per session. |
| `unreachable_after` | duration | ≥ `session_timeout` | A subscribed device unseen for this long is reported *unreachable* (front, metric). |
| `offline_warning_after` | duration | ≥ 2 × `heartbeat_interval` | The app warns its user after being disconnected this long. |
| `max_devices` | integer | 1..100000 | Non-revoked devices. Enrollment is refused beyond it. |

## `admin`

| Attribute | Type | Constraint | Meaning |
|---|---|---|---|
| `allowed_hosts` | list of strings | non-empty | Exact `Host` header values accepted on `server.admin_listen` (host, plus `:port` when the browser sends one). Anything else is refused, which blocks DNS rebinding. |

The admin front has **no password**: anyone who can reach `server.admin_listen`
can send alerts and revoke devices. Bind it to localhost or a private network, or
put a reverse proxy that authenticates users in front of it. Requests coming from
a non-loopback peer in `server.trusted_proxies` skip the `allowed_hosts` check (the
proxy routed on its own hostnames), so several proxies, at the root or under any
path prefix, need no per-hostname setting. Loopback peers always keep the check
(SSH tunnels, local browsers); a proxy on the same host must therefore have its
hostnames in `allowed_hosts`. Never put the Docker gateway (x.x.x.1) or a whole
bridge range in `trusted_proxies`: traffic from the host and from SSH tunnels
arrives from the gateway. Every admin `POST` must also carry
`Content-Type: application/json` and `X-Inbox-Request: 1` (CSRF protection), and
requests marked `Sec-Fetch-Site: cross-site` are refused.

## `security`

| Attribute | Type | Constraint | Meaning |
|---|---|---|---|
| `max_auth_failures` | integer | 1..1000 | Device password attempts (counted when they start, cleared on success) and device key failures allowed per client address within the window. |
| `auth_failure_window` | duration | ≥ 1s | Sliding window of the limiter. |

## `limits`

| Attribute | Type | Constraint | Meaning |
|---|---|---|---|
| `max_request_bytes` | integer | 4096..1048576 | HTTP request body limit on both listeners. |

## `logging`

| Attribute | Values | Meaning |
|---|---|---|
| `level` | `error`, `warn`, `info`, `debug`, `trace` | Minimum level, for stderr and OTLP logs. |
| `format` | `json`, `text` | stderr format. JSON lines carry an `event` field. |

## `telemetry`

| Attribute | Type | Meaning |
|---|---|---|
| `enabled` | bool | `false`: no exporter. The other attributes must then be absent. |
| `otlp_endpoint` | string | Required when enabled. OTLP/HTTP protobuf base URL, `http://` only, e.g. `http://otel-collector:4318`. `/v1/traces`, `/v1/metrics` and `/v1/logs` are appended. |
| `service_name` | string | Required when enabled. `service.name` resource attribute. |
| `export_interval` | duration ≥ 1s | Required when enabled. Metric export period. |

Telemetry never gates the service: if the collector is down, exports fail in the
background and alerts keep flowing.

## `replica` (optional)

Standby replication to a directory shared with the other instances that may take over
(see "Standby replication" in the README). Absent: Inbox runs alone.

| Attribute | Type | Meaning |
|---|---|---|
| `dir` | absolute path | Shared directory (ReadWriteMany volume). Must not be the directory of `storage.path`. |
| `snapshot_interval` | duration ≥ 1s | A changed database is copied at most this often. Accepted alerts, enrollments and revocations do not wait for it. |
| `lease_ttl` | duration ≥ 5s | An instance whose lease has not changed for this long is considered lost; a standby then takes over. Also the time after which an instance that cannot renew its lease stops. |
| `sync_timeout` | duration 1s..60s | How long a write waits for the copy that contains it. |
| `sync_alerts` | bool | `true`: an alert is acknowledged and delivered only once copied (`503 replication_unavailable` otherwise). `false`: acknowledged after the local commit. |

```hcl
replica {
  dir               = "/replica"
  snapshot_interval = "10s"
  lease_ttl         = "30s"
  sync_timeout      = "5s"
  sync_alerts       = true
}
```

## `group "<id>"`

At least one group is required. `<id>` matches `[a-z0-9_-]{1,64}`; it is the
identifier used by producers, webhooks and subscriptions.

| Attribute | Constraint | Meaning |
|---|---|---|
| `name` | 1..128 bytes | Display name. |
| `description` | ≤ 512 bytes | Shown to users choosing a subscription. |

Removing a group from the configuration drops the subscriptions to it at the next
start (logged as `subscriptions_pruned`).

## `producer "<name>"` (optional, repeatable)

`<name>` matches `[a-z0-9_-]{1,64}`. Alerts from a producer carry the source
`producer:<name>` (API) or `webhook:<name>` (Alertmanager / Grafana webhook).

| Attribute | Constraint | Meaning |
|---|---|---|
| `token_file` | unique per producer | Bearer token, from `inbox token` (≥ 32 printable characters). |
| `groups` | non-empty, existing group ids | Groups this producer may alert. |
| `webhook_send_resolved` | bool | Whether Alertmanager / Grafana `resolved` notifications become alerts. |

## Secret files

| File | Command | Notes |
|---|---|---|
| server key | `inbox keygen --out <file>` | One line of base64. Back it up. |
| device credential | `inbox credential --server-key <key> --out <file>` | OPAQUE registration record; the password (≥ 8 chars) never reaches the server. |
| producer token | `inbox token --out <file>` | 64 hex characters. |

These commands refuse to overwrite an existing file and create it with mode 0600.
`--password-stdin` reads the password from standard input (scripts).
