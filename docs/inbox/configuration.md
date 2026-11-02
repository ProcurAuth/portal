# Configuration of the Inbox

The Inbox reads its settings from the block `inbox` of the configuration file of the portal. There is no second file.

The reference of every key is in the root [`README.md`](../../README.md), in the section "Configuration reference". It comes from the table of keys in `src/config/keys.rs`. It gives the type, the range, the default and the use of each key. This page does not copy it. This page gives only the rules that the table cannot show.

Check a file with `portal check-config --config FILE`. The command prints the value that applies to each key and its source. The annotated example is [`deploy/config.example.hcl`](../../deploy/config.example.hcl). The file of the Compose stack is [`deploy/compose/config.hcl`](../../deploy/compose/config.hcl).

## Where the blocks go

- The block `inbox` is optional. Without it, the portal has no Inbox and opens no route for it. With it, the senders, the webhooks and the phones use `server.listen_address`, the port of the pages. The Inbox has no listener for them.
- `inbox.devices.public_address` is the public base URL of the portal as the phones reach it. The portal has no base path. A proxy under a prefix removes the prefix.
- Inside `inbox`, the blocks `devices` and `storage` are mandatory. The blocks `enrollment`, `replica`, `cluster` and `watch` are optional.
- The blocks `group "<id>"` and `api_key "<name>"` can occur more than once. The block `inbox` needs one group at least.
- The Inbox has no `postgres` block. It uses the block `postgres` of the portal. With no `postgres` block, it uses the SQLite file of the block `storage` of the portal.

## Delivery values have no default

These keys have no default: `heartbeat_interval`, `session_timeout`, `resend_after`, `max_inflight`, `unreachable_after` and `offline_warning_after` of `inbox.devices`, and `delivery_ttl`, `alert_retention` and `connection_log_retention` of `inbox.storage`. The program stops at start when one is absent. A wrong default could hide an alert, so the operator chooses each value.

The examples use these values: `heartbeat_interval = "20s"`, `session_timeout = "60s"`, `resend_after = "15s"`, `max_inflight = 32`, `unreachable_after = "2m"`, `offline_warning_after = "3m"`, `delivery_ttl = "7d"`, `alert_retention = "30d"` and `connection_log_retention = "90d"`.

## Rules between keys

Two keys that contradict each other stop the program at start. The message names the key, the range and the value.

- `inbox.devices.session_timeout` is at least two times `heartbeat_interval`.
- `inbox.devices.unreachable_after` is at least `session_timeout`.
- `inbox.devices.offline_warning_after` is at least two times `heartbeat_interval`.
- `inbox.storage.alert_retention` is at least `delivery_ttl`.
- `inbox.storage.pool_size` applies only with the block `postgres`.
- `inbox.replica` needs SQLite. It must not be present with the block `postgres`. Its `dir` must not be the directory of `storage.database`.
- `inbox.cluster` needs the block `postgres`. Its `listen` must not use the port of `server.listen_address` or of `pdp.listen_address`.
- `inbox.watch.group` must be the label of a block `group`. The block must send at least one kind of alert.

## Keys of the senders

A block `api_key "<name>"` is an API key of the file. These rules apply. The error names the key and never shows the secret.

- The switch `keys.api` is `true`. This needs the block `keys`.
- `owner` is the login of an account of the file (a block `user`). A block `user` needs `access.grants_mode` to be `static` or `hybrid`.
- The owner holds `keys.own.use` on `service:portal/key_kind:api` and `alert.send` on each group of `groups`.
- `secret` is an API key of 53 characters with the prefix `api_` and a good checksum. Two keys must not have the same text.
- `groups` lists one group at least. Each group is a block `group`.
- `webhook_send_resolved` is `false` by default. With `true`, the notifications "resolved" of Alertmanager and Grafana become alerts.

## Switches that touch the Inbox

- Without the block `inbox`, no grant can name an action of the Inbox. A grant that does so stops the start.
- `keys.api = true` needs the block `inbox`.
- `inbox.watch` needs the Inbox only. It does not need `keys`.

## Secrets

`inbox.server_key` and the `secret` of each `api_key` accept only `_env` and `_file`. A clear text value stops the program.

`portal inbox keygen --out FILE` makes the key file. It holds one line: the Noise key pair of the server. The command never overwrites a file. The program removes the final line end when it reads the file. A new key makes every phone and every code invalid.

`portal inbox token --out FILE` makes the text of an API key.

## Default values of the optional keys

The optional keys have documented defaults. Examples: `server.name` is `"Portal"`, `inbox.devices.max_devices` is 1000, `inbox.enrollment.code_ttl` is `"10m"`, and `inbox.storage.pool_size` is 8. The reference has the full list.

## Keys that do not exist

An old name is an unknown name. The program has no alias. `portal check-config` stops with the name of the key. This table lists the keys of the earlier standalone alert server.

| Old key | Now |
|---|---|
| `server.listen` | None. The senders and the phones use `server.listen_address` of the portal. |
| `server.name` of the old server | `server.name` of the portal. The key is the name of the instance, and the phones show it. |
| `server.admin_listen` | None. The page is in the portal. |
| `server.trusted_proxies` | `server.trusted_proxies` of the portal. The Inbox has no list of its own. The routes of the phones use it too. |
| `server.shutdown_grace` | None. The shutdown of the portal applies. |
| `server.server_key_file` | `inbox.server_key_file` |
| `admin.allowed_hosts` | None. The admin front is gone. |
| `devices.credential_file` | None. A phone enrolls with a code. |
| `devices.listen`, `inbox.devices.listen` | None. The phones use the port of the portal. |
| `security.*` | `inbox.devices.max_auth_failures` and `inbox.devices.auth_failure_window` |
| `limits.max_request_bytes` | `inbox.devices.max_request_bytes` |
| `storage.path` | `storage.database` of the portal |
| `telemetry` | None. The metrics are in the Prometheus text of the portal. |
| Block `producer` | Block `api_key` in the block `inbox`. |
| `inbox.devices.trusted_proxies` | `server.trusted_proxies` |

The configuration is not read from the environment, except where the file names a variable with `_env`. The `OTEL_*` variables are never read.
