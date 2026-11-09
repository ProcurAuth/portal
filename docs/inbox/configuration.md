# Configuration of the Inbox

The Inbox reads its settings from the block `inbox` of the configuration file of the portal. There is no second file.

The reference of every key is in the root [`README.md`](../../README.md), in the section "Configuration reference". It comes from the table of keys in `src/config/keys.rs`. It gives the type, the range, the default and the use of each key. This page does not copy it. This page gives only the rules that the table cannot show.

Check a file with `portal check-config --config FILE`. The command prints the value that applies to each key and its source. The annotated example is [`deploy/config.example.hcl`](../../deploy/config.example.hcl). The file of the Compose stack is [`deploy/compose/config.hcl`](../../deploy/compose/config.hcl).

## Where the blocks go

- The block `inbox` is optional. Without it, the portal has no Inbox and opens no route for it. With it, the senders, the webhooks and the phones use `server.listen_address`, the port of the pages. The Inbox has no listener for them.
- `inbox.devices.public_address` is the public base URL of the portal as the phones reach it. The portal has no base path. A proxy under a prefix removes the prefix.
- Inside `inbox`, the blocks `devices` and `storage` are mandatory. The blocks `enrollment`, `replica`, `cluster` and `watch` are optional.
- The blocks `items` and `at_rest` are optional, but they go together. Each one needs the other. The block `items` holds the blocks `lease` and `digest`. The block `at_rest` holds one block `key "<id>"` for each content key.
- The block `approvals` is optional. It starts the approval of an action at the PDP. See "Approvals".
- The block `mcp` is optional. It starts the MCP endpoint of the agents. See "The MCP endpoint".
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
- `inbox.items` needs `inbox.at_rest`, and `inbox.at_rest` needs `inbox.items`. See "Items" and "Content keys".
- `inbox.approvals` needs `inbox.items`, `inbox.at_rest`, the block `pdp` and `keys.mcp = true`. A block `pdp_client` with `approvals = true` needs the block `inbox.approvals`. See "Approvals".
- `inbox.mcp` needs `inbox.items` and `keys.mcp = true`. Its `max_wait` is at most `inbox.items.wait_max`. Its `path` is not `metrics.path`. See "The MCP endpoint".

## Keys of the senders

A block `api_key "<name>"` is an API key of the file. These rules apply. The error names the key and never shows the secret.

- The switch `keys.api` is `true`. This needs the block `keys`.
- `owner` is the login of an account of the file (a block `user`). A block `user` needs `access.grants_mode` to be `static` or `hybrid`.
- The owner holds `keys.own.use` on `service:portal/key_kind:api` and `alert.send` on each group of `groups`. With `direct_alerts = true`, the owner holds `alert.send_user` on `service:portal` too.
- `secret` is an API key of 53 characters with the prefix `api_` and a good checksum. Two keys must not have the same text.
- `groups` lists one group at least, except when `direct_alerts` is `true`. Each group is a block `group`.
- `direct_alerts` is `false` by default. With `true`, the key can send an alert to any account (`POST /api/v1/alerts` with the field `user`).
- `webhook_send_resolved` is `false` by default. With `true`, the notifications "resolved" of Alertmanager and Grafana become alerts.

## Items

The block `inbox.items` sets the limits of the items. An item is an approval or a question that an agent sends to a person. The block has no default for a limit that could hide an item or keep it too long. The program stops at start when one of these keys is absent.

- `max_lifetime`, `max_open_per_key`, `max_open_per_owner` and `max_open_total`.
- `retention`, `body_purge_after` and `audit_retention`.
- `lease.min`, `lease.max`, `lease.grace` and `lease.revive_window`.
- `digest.mode`, and `digest.at` and `digest.timezone` with the mode `daily`, or `digest.every` with the mode `every`.

The examples use these values: `"90d"`, 5, 20, 1000, `"180d"`, `"30d"`, `"365d"`, `"30s"`, `"7d"`, `"2m"`, `"7d"` and `"daily"`. The other keys have a default: `lease.max_revivals` is 3, `max_chain` is 10, `max_turns` is 40, `stale_after` is `"24h"`, `max_body_bytes` is `"16KiB"` and `wait_max` is `"25s"`.

These limits contradict each other, and the program stops at start. The message names the two keys and their values.

- `lease.min` is above `lease.max`.
- `lease.max` or `lease.revive_window` is above `max_lifetime`.
- `retention` is below `max_lifetime` plus `lease.revive_window`.
- `audit_retention` is below `retention`.
- `body_purge_after` is above `retention`.
- `digest.at` or `digest.timezone` is present and the mode is not `daily`.
- `digest.every` is present and the mode is not `every`.

The program checks `digest.timezone` against the time zone database that is inside the binary. The name needs its exact capital letters: `Europe/Paris` is valid and `europe/paris` is not. The program does not read the zone files of the host. No key of this block is editable from the web.

The keys of `digest` are the defaults of every account. An account can replace them on the page **Inbox > Preferences** or with the frame `set_prefs` of the phone. The values of the account live in the database, not in the file. They never go past a limit of the file: the period of an account has the range of `digest.every`, from 1 hour to 7 days.

## Content keys

The block `inbox.at_rest` holds the keys that seal the contents of the items. The portal never writes a content in clear text. A copy of the database, a backup or a copy of the replica holds no readable content.

- `active_key` is the id of the key that seals the new contents. It is mandatory. It must be the label of a block `key`.
- The block has 1 to 8 blocks `key "<id>"`. An id has 1 to 16 characters from `a-z`, `0-9`, `-` and `_`.
- A key that is not the `active_key` stays readable and seals nothing. Use this for a rotation.
- Each block `key` has exactly one of `secret_env` and `secret_file`. A `secret` in clear text stops the start.
- A secret is the base64 text of 32 random bytes. It has 44 characters. A secret with all bytes equal stops the start.
- Two keys must not have the same bytes. All instances of the portal must hold the same keys.
- `admin_body_read` is `false` by default. With `true`, the superadmin can read the contents. The portal then writes one audit event for each read, never the content.

Make a key with `portal inbox content-key --out FILE`. The command writes one line. It never overwrites a file. 

Keep the key file outside the data volume and outside the backups of the database. Keep a copy of it in a separate place. A lost key makes the sealed contents unreadable. The metadata stays readable.

At start, the portal compares each key with the check value that the database keeps. A key that changed under the same id stops the start. The message names the id. To change a key, add a new key with a new id, make it the `active_key`, and restart each instance. A database from a version before 5.1.0 has no table for the check values. The portal stops at start and says so (see the README, "Upgrades").

`portal check-config` shows for each key its id, whether it is `active` or `read-only`, and its source as `env:VAR` or `file:/path`. It never shows the secret.

## Approvals

The block `inbox.approvals` sets how the PDP gates an action that a grant marks with `confirm`. It also sets the limits of the approval API of the services. Only `consume_within` is mandatory.

- `consume_within` is the time after the decision of the owner in which the approval can be used. The range is `5s` to `1h`. The time that applies is the larger of this value and `lease_s` of the request.
- `evaluation_ttl` is the time in which an `evaluation_id` is valid. The range is `10s` to `1h`. The default is `5m`.
- `deny_cooldown` is the time after an explicit refusal in which the PDP answers `approval_denied` for the same effect. The range is `0s` to `24h`. The default is `5m`. With `0s`, the PDP never answers `approval_denied`.
- `max_command_bytes` is the largest command (`op` and `view`, as compact JSON). The range is `1KiB` to `32KiB`. The default is `16KiB`. It must not be above `pdp.max_body_bytes`. A larger command gets `413 command_too_large`.
- `max_explanation_chars` is the longest explanation of an agent. The range is `0` to `8000`. The default is `2000`. With `0`, no explanation is accepted. The portal never cuts a text.
- `require_explanation` is `false` by default. With `true`, a request without an explanation gets `422 explanation_required`. It must not be `true` with `max_explanation_chars = 0`.
- `max_comment_chars` is the longest text that a person joins to a refusal. The range is `1` to `8000`. The default is `2000`.
- `intent_ttl` is the time in which an `approval_ref` works. The range is `30s` to `1h`. The default is `10m`.
- `max_requests_per_key_per_hour` is the most requests that one key creates in one hour. The range is `1` to `10000`. The default is `20`.
- `mute_after_refusals` is the number of explicit refusals in a row after which a key is muted. The range is `0` to `100`. The default is `3`. With `0`, a key is never muted.
- `mute_for` is the length of the mute. The range is `1m` to `7d`. The default is `1h`.
- `web_approval` is `true` by default. With `false`, the web page lists the requests and does not decide them, so an owner with no phone gets `409 no_device`.

A block `pdp_client` has the key `approvals`. The default is `false`. A server without the flag gets a plain `false` for an action with `confirm`. See the README, "The approval gate" and "The approval API for a service".

## The MCP endpoint

The block `inbox.mcp` starts the endpoint of the agents. All its keys have a default, so the block can be empty. [`agents.md`](agents.md) tells how an agent uses the endpoint.

- `path` is the URL path of the endpoint. The default is `/mcp`. It must not be `/`, `/healthz`, `/readyz` or `/api/version`. It must not be under `/auth/`, `/v1/` or `/api/v1/`. It must not be the path of the metrics. Make the proxy send this path to the portal without ForwardAuth.
- `max_wait` is the longest wait of one call of `get_answer`. The range is `0s` to `60s`. The default is `25s`. It must not be above `inbox.items.wait_max`. A real client stops a call near 60 seconds, so keep the value low.
- `allowed_origins` lists the values of the header `Origin` that pass. A request with no `Origin` header comes from a program and passes. A request with another origin gets `403`. Without this key, no browser origin passes.
- `allowed_client_cidrs` lists the networks of the client address. The client address is the address that `server.trusted_proxies` finds. A client in no network gets `403`. Without this key, each address passes.
- `rate_limit_per_minute` is the bucket of one key. The range is `1` to `100000`. The default is `120`. Each request counts, also `tools/list` and `ping`.
- `max_unauthenticated_per_minute` is the number of requests with no valid key that one client address can send. The range is `1` to `10000`. The default is `10`. The portal counts a bad key only, never a good one.
- `max_body_bytes` is the largest body. The range is `1KiB` to `1MiB`. The default is `64KiB`.

Each instance keeps its limits in memory. With N instances, a key can send N times `rate_limit_per_minute`. A client address that sends too many requests with no valid key gets `429` before the portal checks the key.

## Switches that touch the Inbox

- Without the block `inbox`, no grant can name an action of the Inbox. A grant that does so stops the start.
- `keys.api = true` needs the block `inbox`.
- `inbox.watch` needs the Inbox only. It does not need `keys`.

## Secrets

`inbox.server_key` and the `secret` of each `api_key` accept only `_env` and `_file`. A clear text value stops the program.

`portal inbox keygen --out FILE` makes the key file. It holds one line: the Noise key pair of the server. The command never overwrites a file. The program removes the final line end when it reads the file. A new key makes every phone and every code invalid.

`portal inbox token --out FILE` makes the text of an API key.

The `secret` of each block `inbox.at_rest.key` accepts only `_env` and `_file`, like the other secrets. `portal inbox content-key --out FILE` makes the key file.

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
| The block of the tokens of the earlier version | Block `api_key` in the block `inbox`. |
| `inbox.devices.trusted_proxies` | `server.trusted_proxies` |

The configuration is not read from the environment, except where the file names a variable with `_env`. The `OTEL_*` variables are never read.
