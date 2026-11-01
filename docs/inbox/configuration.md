# Configuration of the inbox

The inbox reads its settings from the block `inbox` of the configuration file of the portal. There is no second file.

The reference of every key is in the root `README.md`, in the section "Configuration reference". It comes from the table of keys in `src/config/keys.rs`. It gives the type, the range, the default and what each key does. This page does not copy it. This page gives only the rules that the table cannot show.

Check a file with `portal check-config --config FILE`. The command prints the value that applies to each key and its source. The annotated example is [`deploy/config.example.hcl`](../../deploy/config.example.hcl). The file of the Compose stack is [`deploy/compose/config.hcl`](../../deploy/compose/config.hcl).

## Where the blocks go

- The block `inbox` is optional. Without it, the portal has no inbox and opens no port for it. With it, the producers, the webhooks and the phones use `server.listen_address`, the port of the pages. The inbox has no listener for them. `inbox.devices.public_address` is the public base URL of the portal as the phones reach it. The portal has no base path: a proxy under a prefix removes the prefix.
- Inside `inbox`, the blocks `devices` and `storage` are mandatory. The blocks `enrollment`, `replica`, `cluster` and `portal_alerts` are optional.
- The blocks `group "<id>"` and `producer "<name>"` can occur more than once.
- The inbox has no `postgres` block. It uses the block `postgres` of the portal. With no `postgres` block, it uses the SQLite file of the block `storage` of the portal.

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
- `inbox.cluster` needs the block `postgres`.
- `inbox.portal_alerts.group` must be the label of a block `group`. The block must send at least one kind of alert.
- Each `producer` names groups that exist, and has a token of 32 characters at least. Two producers must not share a token.

## Secrets

`inbox.server_key` and the `token` of each `producer` accept only `_env` and `_file`. A clear text value stops the program.

`portal inbox keygen --out FILE` makes the key file. It holds one line: the Noise key pair of the server. The command never overwrites a file. The program removes the final line end when it reads the file. A new key makes every phone and every code invalid.

`portal inbox token --out FILE` makes a producer token.

## Default values of the optional keys

The optional keys have documented defaults. Examples: `inbox.name` is `"Inbox"`, `inbox.devices.max_devices` is 1000, `inbox.enrollment.code_ttl` is `"10m"`, and `inbox.storage.pool_size` is 8. The reference has the full list.

## Keys of the standalone Inbox that no longer exist

| Old key | Now |
|---|---|
| `server.listen` | None. The producers and the phones use `server.listen_address` of the portal. |
| `server.name` | `inbox.name` |
| `server.admin_listen` | None. The pages are in the portal. |
| `server.trusted_proxies` | `server.trusted_proxies` of the portal. The inbox has no list of its own. The routes of the phones use it too. |
| `server.shutdown_grace` | None. The shutdown of the portal applies. |
| `server.server_key_file` | `inbox.server_key_file` |
| `admin.allowed_hosts` | None. The admin front is gone. |
| `devices.credential_file` | None. A phone enrolls with a code. |
| `security.*` | `inbox.devices.max_auth_failures` and `inbox.devices.auth_failure_window` |
| `limits.max_request_bytes` | `inbox.devices.max_request_bytes` |
| `storage.path` | `storage.database` of the portal |
| `telemetry` | None. The metrics are in the Prometheus text of the portal. |

The configuration is not read from the environment, except where the file names a variable with `_env`. The `OTEL_*` variables are never read.
