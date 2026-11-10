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
- The block `agents` is optional. It starts the agents of a key. See "Agents".
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
- `inbox.approvals` needs `inbox.items`, `inbox.at_rest`, the block `pdp` and `keys.mcp = true`. See "Approvals".
- `inbox.agents` needs `inbox.items` and `inbox.at_rest`. Its `idle_after` is at most its `purge_after`. See "Agents".
- `inbox.questions` needs `inbox.items` and `inbox.at_rest`. See "Questions".
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
- A key that is not the `active_key` stays readable and seals nothing. Use this for a rotation (see "Rotate a content key").
- Each block `key` has exactly one of `secret_env` and `secret_file`. A `secret` in clear text stops the start.
- A secret is the base64 text of 32 random bytes. It has 44 characters. A secret with all bytes equal stops the start.
- Two keys must not have the same bytes. All instances of the portal must hold the same keys.
- `admin_body_read` is `false` by default. With `true`, the superadmin can read the contents. The portal then writes one audit event for each read, never the content.

Make a key with `portal inbox content-key --out FILE`. The command writes one line. It never overwrites a file. 

Keep the key file outside the data volume and outside the backups of the database. Keep a copy of it in a separate place. A lost key makes the sealed contents unreadable. The metadata stays readable.

At start, the portal compares each key with the check value that the database keeps. A key that changed under the same id stops the start. The message names the id. To change a key, rotate it (see "Rotate a content key"). A database from a version before 5.1.0 has no table for the check values. The portal stops at start and says so (see the README, "Upgrades").

At start, the portal also counts the sealed rows of each key. The start stops when a row names a key id that the file does not hold. The message names the id and the number of rows. It gives two ways out. If you still have the secret, add the block `key` again and run `portal inbox rekey`. If the key is lost, run `portal inbox content-purge`.

A row can name a key that the file holds but that is not the `active_key`. Then the portal starts. It writes the log event `content_key_rows_to_rekey` with the key id and the number of rows. These rows wait for `rekey`.

### Rotate a content key

A rotation has three steps. Do them in this order. Each step is safe to stop and to repeat.

1. Add the new key and make it active.
   1. Run `portal inbox content-key --out FILE`. Keep the file outside the data volume and outside the backups of the database.
   2. Add a block `key` with a new id. Do not remove the block of the old key.
   3. Set `active_key` to the new id.
   4. Restart each instance. Each instance must hold the same keys.

   From now, the new contents use the new key. The old contents stay readable with the old key.

2. Run `portal inbox rekey --config FILE`.
   - The command seals again every sealed value that uses a key that is not the active key. Use `--from ID` to take one old key only.
   - It visits four kinds of value. They are the bodies of the items, the turns of the threads, the intents of the agents and the hints of the agents.
   - It uses batches of 200 rows. It writes each batch in one transaction. It never logs a content.
   - It is idempotent. If you stop it, or if it is killed, run it again. No row is lost, and no row is sealed twice.
   - It keeps the associated data of each value (the request id, the field, the sequence number and the effect). Each check of an approval stays valid.
   - With PostgreSQL, it runs while the portal serves. It writes a row only when the row still holds the value that the command read. A writer that changed the row first keeps its value, and the command reads the row again.
   - With SQLite, it refuses to run while an instance runs on the file. Stop the instance first.
   - It reports the number of rows moved for each table, and the number of rows that still use each old key.
   - A row that does not open stays as it is. The report names its table and id, never its content. The command then ends with an error. Do not remove the old key.

3. Remove the old key when the command reports no row on it.
   1. Run `portal inbox rekey --config FILE` again. It must move 0 rows. It must report no row left on the old key.
   2. When no row uses the old key, the command drops the check value of this key from the database.
   3. Wait at least `inbox.approvals.evaluation_ttl` (5 minutes by default) after the restart of the last instance of step 1.
   4. Remove the block `key` of the old key, and restart each instance.

The wait of step 3 has a reason. An `evaluation_id` that the PDP made before step 1 names the old key. The PDP checks it with that key, for `evaluation_ttl`. If you remove the key sooner, a service that holds such an id gets `invalid_evaluation` and asks the PDP again. No row stores such an id as a value that needs the key: an approval that the owner already decided stays valid.

If you remove the block of the old key too early, the portal stops at start and names the key and the number of rows. Add the block again, start, and run the command.

Backups and restores need care. A backup of the database holds the rows as they were at the time of the backup. Those rows can use an old key. Keep each old key for as long as you keep a backup that uses it.

After a restore, the file must hold every key that the restored rows name. If it does not, the start stops. Add the key that the file lacks, start, and run `portal inbox rekey`. Keep the keys apart from the backups of the database.


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

The block `pdp_client` has no key for the approvals. A service gets them from its document of actions: an action with `"approval": "optional"`. A service without this declaration, or without a known document, gets a plain `false` for an action with `confirm`. The approval API gives it `403`. A file that still has `approvals` in a block `pdp_client` stops at start. See the README, "The approval gate" and "The approval API for a service".

## Agents

Several programs, or several conversations of one model, can share one key. The block `inbox.agents` lets the portal tell them apart. The portal gives each agent a token, and the owner gives it a name. All its keys have a default, so the block can be empty. Without the block, each key is one agent, and the Inbox works as before.

- `max_per_key` is the most agents of one key that are `new`, `known` or `blocked`. The range is `1` to `1000`. The default is `16`.
- `new_per_hour` is the most agents that the calls of one key can make in one hour. The range is `1` to `1000`. The default is `6`.
- `lock_new` is `false` by default. With `false`, a new agent is allowed at once, and its owner gets a notice. With `true`, the owner cannot accept an item of a new agent until the owner allows the agent. A refusal and a talk stay open, and the server decides this, not the screen.
- `idle_after` is the time after which an agent with no open item and no call leaves the active views. The range is `1h` to `365d`. The default is `14d`. The agent still works, and a call makes it active again.
- `purge_after` is the time after which the sweeper erases an agent that has no item, no live intent and no call. The range is `7d` to `3650d`. The default is `365d`. The token of an erased agent is unknown.

The limits are per key, never per agent. An agent that drops its token cannot escape them. A call that would pass `max_per_key` or `new_per_hour` does not fail. It goes to the bucket `Unattributed` of the key, it gets no token, and the owner gets one alert each hour. The bucket has no name, and the owner cannot merge it, block it or lock it.

The mode of a key is an attribute of the key and not of the file. The three modes are `key`, `discover` and `enrolled`. The mode `key` is one agent for the key and has no token. The mode `discover` makes an agent at the first call that makes an item and has no token. The mode `enrolled` makes no agent from a call: the owner makes it. Without the block, each mode acts as `key`.

The portal seals the hints of an agent (what it says about itself, its client and its address range) with the content keys of `inbox.at_rest`. The command `portal inbox content-purge` erases these hints too. The metrics are `portal_agents_created_total`, `portal_agent_refused_total` and `portal_agent_actions_total`.

## Questions

The block `inbox.questions` lets an agent ask a question to the owner of its key. The block can be empty. Without the block, the tool `ask` and the route `POST /api/v1/questions` do not exist. The owner is the only recipient. The portal seals each question at rest, and a service never sees it.

- `max_options` is the most options of a `choice` question, and of each choice question of a form. The range is `2` to `8`. The default is `8`. The protocol of the phones allows 8 at most. A question with more options gets `422 invalid_argument`. The portal never cuts a list.

A key asks only when it holds the permission `question.ask` on the service `portal`. The permission has the budget `questions`. A question, a message of the agent in its thread (`say`) and a replacement (`supersede`) count one unit each. A repeat of a call with the same `idempotency_key`, a renewal of a lease and a read count nothing. The permission `question.cancel` lets a key withdraw its own question. The permission `question.read` is for an account: it shows the metadata of the questions of all the accounts, and never a content.

The attribute `interaction` of a key is `conversational` or `one_shot`. A `one_shot` key asks a `choice` or a `confirm` with no flag. A call with `allow_custom` or `allow_ask_back` gets `422 interaction_not_allowed`.

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

The `secret` of each block `inbox.at_rest.key` accepts only `_env` and `_file`, like the other secrets. `portal inbox content-key --out FILE` makes the key file. `portal inbox rekey --config FILE [--from ID]` seals the old contents again with the active key.

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
