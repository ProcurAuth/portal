# Operations

This page is for the operator of a portal. It covers start and stop, the probes, the effective configuration, the container and the reverse proxy. It also covers backups, the rotation of a key, the metrics and the events of the log. The last parts give the commands of the binary, the exit status and the notes for each upgrade.

## Start and stop

The instance opens its ports only when these tasks end:

1. The directory is loaded and the counter rows exist.
2. The catalogue has the full list of the services.
3. The first read of the document of each service ended, or `access.start_read_deadline` passed.

The startup probe of a pod MUST wait longer than the sum of these times. The liveness probe reads `/healthz`, which never reads the database (see [Probes](#probes)). While the start runs, the log has the events `start_waits` and `start_read_deadline`.

With the block `inbox`, one exception exists. The portal listener opens at once and serves the routes of the Inbox. These are `/v1/`, `/api/v1/`, `/readyz`, `/api/version` and the path of `inbox.mcp`. An alert and a phone then never wait for the catalogue. Every other route of the portal, `/healthz` included, answers `503` with `starting` until the three tasks end.

The gate never decides with an incomplete catalogue. The PDP listener opens when the three tasks end.

On `SIGTERM`, the instance serves 5 seconds more, so the cluster can remove it from the endpoints of its service. Then it closes its listeners and tells each connection to close. It waits at most 10 seconds for the answers in progress. Set `terminationGracePeriodSeconds` to 20.

The instance reads the action documents of the services at equal intervals, 64 at the same time at most. The catalogue reads the IngressRoutes and the pods in two lists, page by page, and it probes 64 services at the same time at most.

## Probes

The probes are on the listener of the pages. They need no session. Each probe answers `GET` and `HEAD`.

| Path | Answer |
|---|---|
| `/healthz` | `200` with the body `ok`. While the start runs, `503` with the body `starting`. It never reads the database. |
| `/livez` | The same answer as `/healthz`. When `metrics.path` or `inbox.mcp.path` is `/livez`, that route keeps the path. |
| `/readyz` without the block `inbox` | `200` with the body `ready`. While the start runs, `503` with the body `starting`. |
| `/readyz` with the block `inbox` | The Inbox answers, also while the start runs. `200` with `ready`. With PostgreSQL, `200` with `degraded` while the Inbox holds alerts in memory. `503` with `draining` after `SIGTERM`. With SQLite, `503` with `storage unavailable` when the file does not answer in 2 seconds. |

The answer of a `HEAD` has the status and the headers of the `GET`, and no body. Its `Content-Length` gives the size of the body of the `GET`. Each answer of a probe has the header `Cache-Control: no-store`. The event `request` of a probe has the level `debug`, so a log at the level `info` does not show the probes.

Use `/healthz` for the startup probe and for the liveness probe. Use `/readyz` for the readiness probe. The file [deploy/k3s/portal.yaml](../deploy/k3s/portal.yaml) shows the three probes. The command `portal healthcheck --config FILE` asks `/healthz`, and `/readyz` too with the block `inbox`. The image uses this command as its `HEALTHCHECK`.

## Effective configuration

Two commands check the configuration and print the value that applies to each key. They read and check the file and decode the secrets as `run` does. They do not open the database and they do not start the server.

- `portal check-config --config FILE` checks the file, the variables and the files that it names. It also checks that the identity key and the keys of the Inbox decode. It prints one line for each key and then the line `configuration ok`. With the block `identity`, it also prints the line `identity.key_id`, which the portal derives from `identity.signing_key`.
- `portal print-config --config FILE` does the same checks. It prints only the lines of the keys, so a script can read them.

Each line has the form `key = value  # source`:

```text
server.workers = 4  # default
logging.level = "info"  # file
authentication.superadmin_login = <hidden>  # env:PORTAL_SUPERADMIN_LOGIN
session.secret = <hidden>  # file:/run/secrets/portal_session_secret
```

The source is `default`, `file`, `env:VAR` or `file:/path`. A value that the portal derives from another key names that key, for example `derived from inbox.at_rest.active_key`. A secret and the superadmin login always show `<hidden>`. An error stops the command with the exit status 1, and the message names the key.

At start, the server writes the same values to its log at the level `info`. It writes one event `config` for each key, with the fields `key`, `value` and `source`. The event hides the secrets in the same way. The page [Configuration](configuration.md#effective-configuration) gives the rules of the keys.

## Container and read-only root

The image runs as the user `10001`, and it works with a read-only root file system. Mount a tmpfs of 16 MiB on `/tmp`, and mount a volume on `/var/lib/portal` when you use SQLite. The default SQLite file is `/var/lib/portal/portal.db`. The Compose stack and the K3s manifests of the repository use PostgreSQL, so they mount no volume on `/var/lib/portal`.

The service `portal` of [docker-compose.yml](../docker-compose.yml) uses this form:

```yaml
read_only: true
tmpfs:
  - /tmp:size=16m,mode=1777
```

The pod of [deploy/k3s/portal.yaml](../deploy/k3s/portal.yaml) uses this form. The excerpt shows only the keys of the read-only root. The key `volumes` is in the specification of the pod, next to `containers`.

```yaml
spec:
  containers:
  - name: portal
    securityContext:
      readOnlyRootFilesystem: true
    volumeMounts:
    - name: tmp
      mountPath: /tmp
  volumes:
  - name: tmp
    emptyDir:
      medium: Memory
      sizeLimit: 16Mi
```

## Reverse proxy and path prefix

The portal serves plain HTTP and has no TLS server. Put a reverse proxy in front of it for HTTPS. The page [Traefik and TLS](traefik.md) gives the routes, the ForwardAuth middlewares and the rules of the headers.

- Set `server.trusted_proxies` to the network of the proxy. The portal reads `X-Forwarded-For` only from a peer in these networks.
- The scheme comes from `X-Forwarded-Proto`. Only the single value `https` counts as HTTPS. It gives the session cookie its `__Host-` name and its `Secure` attribute. The portal reads this header from every peer, so the proxy MUST set it and replace the value of the client.
- Publish the pages at the root of a host name. The pages use absolute paths, for example `/auth/login`, and the session cookie has `Path=/`.

The portal has no base path. A proxy can publish the routes of the Inbox under a path prefix, but it MUST remove the prefix before it sends the request. These routes are `/v1/` for the phones and `/api/v1/` for the senders. The key `inbox.devices.public_address` can hold the prefix, for example `https://example.com/portal`. The phones add `/v1/enroll` and `/v1/session` to it. The routes of the phones are HTTP upgrades to WebSocket, so the proxy MUST send the headers `Upgrade` and `Connection`.

To publish the routes of the Inbox at `/portal/`, use these nginx rules. Put the `map` in the block `http` and the `location` in the block `server`.

```nginx
map $http_upgrade $connection_upgrade {
    default upgrade;
    ''      close;
}

location ~ ^/portal/(v1|api/v1)/ {
    rewrite ^/portal(/.*)$ $1 break;
    proxy_pass http://127.0.0.1:4181;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection $connection_upgrade;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_read_timeout 300s;
}
```

The `rewrite` removes the prefix `/portal`. Set `proxy_read_timeout` longer than `inbox.devices.session_timeout`. Then the proxy does not close a quiet session of a phone first.

With Traefik, the middleware `stripPrefix` removes the prefix. Traefik carries the WebSocket upgrade and needs no other rule for it.

```yaml
http:
  middlewares:
    portal-prefix:
      stripPrefix:
        prefixes: ["/portal"]
  routers:
    portal-inbox:
      rule: "Host(`example.com`) && (PathPrefix(`/portal/v1/`) || PathPrefix(`/portal/api/v1/`))"
      entryPoints: [websecure]
      middlewares: [portal-prefix]
      service: portal
      tls: {}
  services:
    portal:
      loadBalancer:
        servers:
          - url: "http://127.0.0.1:4181"
```

## Backups

Copy two things: the database and the key file of the Inbox server. With SQLite, copy the file with `sqlite3 .backup`, or stop the instance first. With PostgreSQL, use the backup of the database cluster.

If you lose the key file of the Inbox server, every enrolled phone and every enrollment code becomes invalid. Each person must then enroll again. A new key file has the same effect. Keep the file secret.

The content keys of `inbox.at_rest` must stay outside the data volume and outside the backups of the database. A copy of the database alone then shows no content. Keep each content key as long as you keep a backup that uses it.

## Rotate the content key

A rotation has three steps. The page [Inbox: configuration rules](inbox/configuration.md) gives the rules of the block `inbox.at_rest`.

1. Make a key with `portal inbox content-key --out FILE`. Add a block `key` for it in `inbox.at_rest`, and set `active_key` to its id. Keep the old block. Restart each instance.
2. Run `portal inbox rekey --config FILE`. The command seals again each old content with the active key. With SQLite, stop the instance first: the command refuses to run while an instance runs. With PostgreSQL, the portal can serve while the command runs. If the command stops, run it again.
3. Run the command a second time. When it reports no row on the old key, wait for `inbox.approvals.evaluation_ttl`. Then remove the old block and restart each instance.

If you remove an old block while rows use it, the portal stops at start. The message names the key and the number of rows. Add the block again and run the command. At start, the portal writes the log event `content_key_rows_to_rekey` when rows use a key that is not active.

If the file lost a key and you have no copy of it, run `portal inbox content-purge --config FILE --key ID`. The command erases the sealed contents of that key and keeps the metadata of the items.

## Metrics

Prometheus reads `metrics.path` of the listener of the pages (`metrics.enabled` must be `true`). The route needs no session. Do not publish it through Traefik. The file [examples/prometheus.yml](../examples/prometheus.yml) shows a scrape job. A label never holds a client address, a raw URI, a password, a cookie or a return target.

Alert on the metrics in bold. A metric of the Inbox exists only with the block `inbox`.

### Portal

| Metric | Type | Labels | Use |
|---|---|---|---|
| `portal_build_info` | gauge | `version` | Build information. |
| `portal_uptime_seconds` | gauge | | Process uptime. |
| `portal_http_requests_total` | counter | `route`, `method`, `status` | HTTP requests by route, method and status. |
| `portal_http_request_duration_seconds` | histogram | | Duration of the HTTP requests. |
| `portal_connections_active`, `portal_connections_total` | gauge, counter | | Open and accepted connections. |
| **`portal_connections_rejected_total`** | counter | | Connections refused because `server.max_connections` was reached. |
| `portal_workers` | gauge | | Worker threads that the file sets. |
| `portal_login_attempts_total` | counter | `result` | Results of the sign-in form. A wrong second-factor code has `mfa_failure`. |
| `portal_login_form_check_failures_total` | counter | | Sign-ins that failed the anti-bot check of the form. Each one also counts as a failure in `portal_login_attempts_total`. |
| `portal_login_body_rejected_total` | counter | | Sign-ins refused because `server.max_login_body_reads` bodies were already in the read. |
| `portal_origin_rejections_total` | counter | `route` | POST requests that the same-origin check refused. |
| `portal_forward_auth_requests_total` | counter | `result` | Results of `/auth/verify`. The value `excluded` counts the paths of `portal.exclude`. |
| `portal_session_checks_total` | counter | `result` | Results of the check of a session cookie. |
| `portal_sessions_issued_total`, `portal_logouts_total` | counter | | Sessions issued, and sign-outs. |
| `portal_sessions_active`, `portal_session_capacity` | gauge | | Open sessions and the limit `session.max_active_sessions`. |
| `portal_session_capacity_rejections_total` | counter | | Sessions refused because the store was full. |
| `portal_password_checks_total` | counter | | Argon2 checks run. |
| `portal_password_checks_in_flight`, `portal_password_check_concurrency_limit` | gauge | | Open Argon2 checks, and their limit. |
| `portal_rate_limit_blocks_total` | counter | `scope` | Requests that the sign-in limiter blocked. |
| `portal_rate_limit_entries`, `portal_rate_limit_threshold` | gauge | `scope` | Entries of the limiter and the failure threshold. |
| `portal_users` | gauge | | Accounts in the database. The superadmin is not counted. |
| `portal_user_admin_total` | counter | `action`, `result` | Requests of the superadmin on the accounts. |
| **`portal_tracking_dropped_total`** | counter | | History events lost because the writer queue was full. |
| **`portal_tracking_flush_errors_total`** | counter | | Writes of the history that failed. |

### Discovery and actions

| Metric | Type | Labels | Use |
|---|---|---|---|
| `portal_catalog_services`, `portal_catalog_services_down` | gauge | | Services in the catalogue, and services that are down. |
| **`portal_catalog_complete`** | gauge | | 1 when each source answered at least once. At 0, only the superadmin passes the gate. |
| `portal_catalog_duplicates`, `portal_catalog_items` | gauge | | Workloads ignored because their service id was already listed, and the items of the last lists. |
| `portal_discovery_last_success_timestamp_seconds` | gauge | | Unix time of the last refresh where each source answered. |
| `portal_discovery_refreshes_total` | counter | | Refreshes. |
| **`portal_discovery_errors_total`** | counter | `source` | Source listings that failed. |
| `portal_actions_age_seconds` | gauge | | Age of the oldest action document of a service. |

### Keys, PDP and budgets

The PDP metrics exist only with the block `pdp`. `portal_key_checks_total` and `portal_active_keys` exist only with the block `keys`.

| Metric | Type | Labels | Use |
|---|---|---|---|
| `portal_key_checks_total` | counter | `result` | Key checks of the edge. |
| `portal_active_keys` | gauge | | Keys that are not expired and not too old. |
| `portal_pdp_decisions_total` | counter | `decision`, `reason` | Decisions of the PDP. |
| `portal_pdp_approval_total` | counter | `reason` | Outcomes of the approval gate: the four reasons, `consumed`, `released` and `not_enabled`. |
| `portal_pdp_refused_total` | counter | `reason` | Requests refused before a decision: `unauthorized`, `bad_request`. |
| `portal_usage_rejected_total` | counter | | Requests that got HTTP 400 for `context.usage`. |
| **`portal_pdp_unavailable_total`** | counter | `cause` | Questions that got HTTP 503: `store_error`, `row_missing`. |
| `portal_budget_units_total` | counter | `result` | Uses counted: `counted`, `refused`. |
| `portal_budget_refunds_total` | counter | `result` | Counted uses whose units the portal gave again, because the approval could not be consumed. |
| `portal_counter_statement_seconds` | summary | | Time of the counter statements. |
| **`portal_counter_wait_exceeded_total`** | counter | | Counts that took more than 500 ms. |
| `portal_counter_rows_made_total`, `portal_counter_rows` | counter, gauge | | Counter rows made by the instance, and rows in the table. |
| `portal_limit_values_undeclared` | gauge | | Limit values that the action documents do not declare. |
| `portal_directory_grants` | gauge | | Grants of the users and the keys. |
| **`portal_directory_age_seconds`** | gauge | | Time since the last good read of the directory. It is always 0 with SQLite. It grows when the refresh fails with PostgreSQL. |
| `portal_shared_store_errors_total` | counter | `use` | Calls to the shared database that failed, for `sessions` or `directory`. PostgreSQL only. |

### Inbox

| Metric | Type | Labels | Use |
|---|---|---|---|
| **`portal_inbox_oldest_pending_delivery_seconds`** | gauge | | Age of the oldest alert that still waits for a phone. |
| **`portal_inbox_devices_unreachable`** | gauge | | Subscribed phones that the Inbox did not see for `inbox.devices.unreachable_after`. |
| **`portal_inbox_storage_degraded`** | gauge | | 1 while the database has no usable primary and the Inbox holds alerts in memory. |
| **`portal_inbox_replica_last_snapshot_age_seconds`** | gauge | | Age of the newest snapshot. Only with `inbox.replica`. |
| `portal_inbox_alerts_accepted_total` | counter | `source`, `kind`, `target` | Messages accepted. The source names the route. The target is `group` or `user`. |
| `portal_inbox_alerts_duplicate_total` | counter | `source`, `kind`, `target` | Submissions that the idempotency key merged. |
| `portal_inbox_deliveries_created_total`, `portal_inbox_deliveries_sent_total`, `portal_inbox_delivery_receipts_total` | counter | | Deliveries made, frames sent (with repeats), receipts from phones. |
| `portal_inbox_delivery_latency_seconds` | histogram | | Time from acceptance to the first receipt. |
| `portal_inbox_deliveries_pending` | gauge | | Deliveries that wait for a phone. |
| `portal_inbox_devices_enrolled`, `portal_inbox_device_sessions` | gauge | | Phones that are not revoked, and connected sessions. |
| `portal_inbox_device_session_closes_total` | counter | `reason` | Session ends. |
| `portal_inbox_auth_failures_total` | counter | `kind` | Failed authentications. |
| `portal_inbox_enrollments_total` | counter | | Phones enrolled. |
| `portal_inbox_alerts_spooled_total` | counter | `source`, `kind`, `target` | Messages that the spool of the degraded mode accepted. |
| `portal_inbox_spool_alerts`, `portal_inbox_spool_rejected_total` | gauge, counter | | Alerts in the spool, and alerts refused because the spool was full. |
| `portal_inbox_alerts_refused_total` | counter | `stage` | Alerts refused one by one, without a stop of the others. `webhook`: an alert of a webhook payload. `replay`: a spooled alert that the database refused. |
| `portal_inbox_replica_epoch`, `portal_inbox_replica_snapshots_total`, `portal_inbox_replica_snapshot_failures_total`, `portal_inbox_replica_sync_failures_total` | gauge, counter | | The replica. Only with `inbox.replica`. |
| `portal_item_events_total` | counter | `event` | Transitions of the items. |
| `portal_item_answers_total` | counter | `status` | Answers of owners to the items. |
| `portal_item_lease_writes_total` | counter | | Writes of a lease by a renewal. |
| `portal_item_body_served_total`, `portal_item_body_reads_total` | counter | | Bodies served to their owner, and bodies read by the superadmin. |
| `portal_item_body_effect_mismatch_total` | counter | | Bodies not served because the effect of the opened command differs from the stored effect. |
| `portal_agents_created_total` | counter | `origin` | Agents made. |
| `portal_agent_refused_total` | counter | `reason` | Calls that an agent limit or an agent check refused or sent to the bucket. |
| `portal_agent_actions_total` | counter | `op`, `via` | Actions of owners that changed an agent. |
| `portal_watch_sent_total`, `portal_watch_failed_total` | counter | `kind` | Alerts of the portal about itself that the Inbox accepted or refused. The kinds are `service_unhealthy`, `service_recovered` and `sign_in_burst`. Only with `inbox.watch`. |

## Logs

The portal writes its log to the standard error stream. Each line is one JSON object with the fields `ts` (Unix time in seconds), `level` and `event`, and the fields of the event as strings. One exception exists: the event `postgres_unreachable` of the portal gives `retry_for_s` as a JSON number. The key `logging.level` sets the lowest level: `error`, `warn`, `info` or `debug`. The Inbox writes to the same log.

At start, the event `config` gives each configuration value with its source: see [Effective configuration](#effective-configuration). The log never holds a password, a key text or a secret.

The tables below list each event that the code writes, with its level and its fields. A field can be absent from some lines of an event. When a row gives two levels, the level depends on the case:

- The event `request` has the level `debug` for the probes and for `metrics.path`, and `info` for the other routes. Its field `duration_ms` is the time of the answer in whole milliseconds. Its field `route` is the label `route` of the metrics.
- The event `access_decision` has the level `info` for a refusal and `debug` for an access.
- The events `approval_<event>` and `question_<event>` follow an item of the Inbox. The `<event>` is `requested`, `revived`, `replied`, `resumed`, `answered`, `muted`, `consumed` or `lapsed`. It can also be a final state: `abandoned`, `expired`, `cancelled` or `superseded`.

### Events of the portal

| Event | Level | Fields |
|---|---|---|
| `accept_error` | error | `error` |
| `access_decision` | info, debug | `subject`, `action`, `resource`, `decision`, `path` |
| `access_model` | info | `grants_mode`, `configured_users`, `identity_token`, `identity_key_id`, `pdp` |
| `config` | info | `key`, `value`, `source` |
| `connection_closed_with_error` | debug | `remote_addr`, `error` |
| `counter_rows_failed` | error | `error` |
| `directory_refresh_failed` | error | `error` |
| `end_sessions_failed` | error | `error` |
| `external_delete_failed` | error | `error` |
| `external_icon_refused` | info | `reason` |
| `external_insert_failed` | error | `error` |
| `grant_to_check` | warn | `login`, `grant`, `problem`, `detail` |
| `history_unreadable` | error | `error` |
| `inbox_admin_revoke` | info | `actor`, `device` |
| `inbox_admin_send` | info | `actor`, `target` |
| `inbox_enrollment_code` | info | `actor` |
| `inbox_key_decision` | info | `key`, `action`, `group`, `counted`, `decision` |
| `inbox_notify_level` | info | `actor`, `level` |
| `inbox_web_answer` | info | `actor`, `request`, `action` |
| `inbox_web_cancel` | info | `actor`, `request` |
| `inbox_web_reply` | info | `actor`, `request` |
| `key_check` | info | `key`, `owner`, `service`, `result` |
| `key_check_failed` | info | `source`, `cause`, `not_logged` |
| `key_checks_write_failed` | error | `error` |
| `key_created` | info | `key`, `owner`, `kind`, `expires_at` |
| `key_edited` | info | `key`, `owner` |
| `key_insert_failed` | error | `error` |
| `key_prune_failed` | error | `error` |
| `key_revoked` | info | `key`, `by`, `section` |
| `key_update_failed` | error | `error` |
| `login_body_capacity_reached` | warn | `client_ip`, `max_login_body_reads` |
| `login_failed` | warn | `client_ip`, `login`, `known`, `reason` |
| `login_password_check_wait_timeout` | warn | `client_ip` |
| `login_rate_limited` | warn | `scope`, `client_ip`, `retry_after_seconds` |
| `login_success` | info | `client_ip`, `login`, `second_factor` |
| `mfa_check_failed` | error | `error` |
| `mfa_enrol_failed` | error | `error` |
| `mfa_read_failed` | error | `error` |
| `options_failed` | debug | `service`, `error` |
| `origin_check_failed` | warn | `route`, `client_ip` |
| `postgres_unreachable` | warn | `retry_for_s`, `error` |
| `request` | info, debug | `method`, `path`, `status`, `client_ip`, `duration_ms`, `route` |
| `server_started` | info | `listen`, `workers`, `icons`, `max_connections` |
| `session_capacity_reached` | error | `capacity` |
| `session_check_failed` | error | `error` |
| `session_count_failed` | error | `error` |
| `session_end_all_failed` | error | `error` |
| `session_generation_cleanup_failed` | error | `error` |
| `session_history_cleanup_failed` | error | `error` |
| `session_insert_failed` | error | `error` |
| `session_revoke_failed` | error | `error` |
| `session_revoke_user_failed` | error | `error` |
| `session_sweep_failed` | error | `error` |
| `sessions_ended` | warn | `count` |
| `start_read_deadline` | warn | `detail` |
| `start_waits` | warn | `detail` |
| `stopped` | info | `connections_left` |
| `stopping` | info | `serve_seconds` |
| `stored_grants_not_used` | warn | `users`, `detail` |
| `tracking_flush_failed` | error | `error` |
| `tracking_prune_failed` | error | `error` |
| `user_add_inbox_failed` | error | `error` |
| `user_delete_failed` | error | `error` |
| `user_delete_inbox_failed` | error | `error` |
| `user_insert_failed` | error | `error` |
| `user_insert_undo_failed` | error | `error` |
| `user_update_failed` | error | `error` |
| `watch_failed` | error | `kind`, `subject`, `error`, `message` |
| `watch_sent` | info | `kind`, `subject`, `duplicate` |
| `watch_started` | info | `group`, `service_unhealthy`, `sign_in_burst` |

### Events of the PDP

| Event | Level | Fields |
|---|---|---|
| `actions_read_early` | info | `service`, `request_id` |
| `pdp_accept_error` | error | `error` |
| `pdp_approval_store_error` | warn | `error` |
| `pdp_approval_unavailable` | warn | `detail` |
| `pdp_decision` | info | `client`, `subject`, `action`, `resource`, `match`, `usage`, `effect`, `decision`, `reason`, `request_id` |
| `pdp_evaluation_id_unavailable` | warn | `detail` |
| `pdp_request` | debug | `method`, `path`, `status`, `remote_addr`, `request_id` |
| `pdp_started` | info | `listen`, `clients` |
| `pdp_unavailable` | warn | `cause` |
| `pdp_without_client` | warn | `detail` |

### Events of the Inbox

| Event | Level | Fields |
|---|---|---|
| `absent_account_end_failed` | error, warn | `user.name`, `error` |
| `absent_account_ended` | warn | `user.name`, `devices` |
| `account_inbox_ended` | info | `user.name`, `devices` |
| `agent_action` | info | `op`, `agent.id`, `actor`, `via` |
| `agent_channel_store_error` | error | `error` |
| `agent_enrolled` | info | `agent.id`, `actor`, `via` |
| `agent_frame_refused` | error | `agent.id`, `error` |
| `agent_hints_failed` | warn | `agent.id`, `error` |
| `agent_state_not_sent` | info | `agent.id`, `reason` |
| `alert_accepted` | info | `alert.id`, `targets` |
| `alert_spooled` | warn | `alert.id`, `targets` |
| `alert_without_targets` | warn | `alert.id` |
| `approval_api_store_error` | error | `error` |
| `approval_<event>`, `question_<event>` | info | `request.id`, `actor` |
| `body_effect_mismatch` | error | `request.id`, `reason` |
| `body_read` | info | `request.id`, `actor` |
| `body_served` | info | `request.id` |
| `cache_refresh_failed` | warn | `error` |
| `content_key_loaded` | info | `kid`, `active` |
| `content_key_rows_to_rekey` | warn | `kid`, `count`, `active`, `hint` |
| `database_close_timeout` | warn | none |
| `database_closed` | info | none |
| `deliveries_withdrawn_by_rights` | info | `device.id`, `user.name`, `count` |
| `device_connected` | info | `recorded`, `device.id`, `device.name`, `user.name`, `client.address`, `app.version` |
| `device_disconnected` | info | `reason`, `device.id`, `device.name`, `user.name`, `client.address` |
| `device_enrolled` | info | `device.id`, `device.name`, `user.name`, `client.address` |
| `device_revoked` | info | `device.id` |
| `digest_failed` | warn | `error` |
| `digest_plan_refused` | warn | `owner`, `reason` |
| `digest_queued` | info | `count` |
| `enroll_accept_failed` | error | `device.id`, `error` |
| `enroll_cleanup_failed` | error | `device.id`, `error` |
| `enroll_code_cleanup_failed` | warn | `error` |
| `enroll_code_created` | info | `user.name`, `expires_at_ms` |
| `enroll_refused` | info | `reason`, `client.address` |
| `enroll_store_failed` | error | `error` |
| `handshake_ended_by_newer` | info | `client.address` |
| `heartbeat_failed` | warn | `error` |
| `inbox_started` | info | `public_address`, `groups`, `api_keys`, `replica`, `cluster`, `storage` |
| `inbox_stopped` | info | `sockets_left` |
| `instance_shutdown_failed` | warn | `error` |
| `item_delivery_withdrawn` | info | `device.id`, `request.id`, `kind`, `reason` |
| `item_frame_failed` | error | `request.id`, `error` |
| `item_frame_refused` | error | `error`, `request.id` |
| `item_queue_failed` | error | `error` |
| `item_state_not_sent` | info | `request.id`, `reason` |
| `item_store_failed` | error | `error` |
| `items_cancel_failed` | warn | `user.name`, `error`, `key.id` |
| `items_cancelled` | info | `user.name`, `count`, `key.id` |
| `items_catch_up_failed` | warn | `device.id`, `error` |
| `items_caught_up` | info | `device.id`, `count` |
| `items_retention` | info | `bodies_purged`, `items_deleted`, `events_deleted`, `agents_purged` |
| `items_retention_failed` | warn | `error` |
| `items_sweep_failed` | warn | `error` |
| `last_seen_flush_failed` | warn | `error` |
| `leases_extended` | warn | `count`, `reason` |
| `lost_instance_sessions_closed` | warn | `count` |
| `mcp_call` | info | `tool`, `key`, `agent`, `outcome` |
| `migration_applied` | info | `version` |
| `peer_request_refused` | warn | `path` |
| `peers_changed` | info | `peers` |
| `phone_agent_do` | info | `device.id`, `agent.id`, `status` |
| `phone_answer` | info | `device.id`, `request.id`, `action`, `status` |
| `phone_answer_invalid` | warn | `device.id`, `request.id`, `rule` |
| `postgres_listen_lost` | warn | `error` |
| `postgres_ready` | info | `instance`, `host`, `database` |
| `postgres_unreachable` | warn | `error`, `retry_for_s` |
| `prefs_read_failed` | warn | `error` |
| `replica_fenced` | error | `reason`, `action` |
| `replica_final_snapshot_failed` | error | `error` |
| `replica_first_snapshot` | info | `epoch`, `bytes`, `dir` |
| `replica_lease_contended` | warn | `epoch` |
| `replica_lease_release_failed` | warn | `error` |
| `replica_lease_released` | info | `epoch` |
| `replica_lease_unreadable` | warn | `error` |
| `replica_local_rejected` | error | `error` |
| `replica_ready` | info | `epoch`, `restored_epoch`, `kept_local` |
| `replica_renew_failed` | warn | `error` |
| `replica_snapshot` | debug | `epoch`, `bytes` |
| `replica_snapshot_failed` | warn | `error` |
| `replica_snapshot_rejected` | error | `epoch`, `error` |
| `replica_sync_failed` | warn | `error` |
| `replica_waiting` | info | `reason`, `ttl_ms` |
| `retention_failed` | warn | `error` |
| `retention_purged` | info | `alerts`, `connections`, `codes` |
| `rights_read_failed` | warn | `error` |
| `rights_unsubscribe_failed` | warn | `error` |
| `session_budget_full` | warn | `client.address` |
| `session_disconnect_record_failed` | error | `error` |
| `session_lookup_failed` | error | `error` |
| `session_record_failed` | error | `error` |
| `session_refused` | info | `reason`, `device.id`, `user.name` |
| `session_storage_error` | error | `error` |
| `spool_alert_conflict` | warn | `alert.id`, `alert.source` |
| `spool_alert_refused` | error | `alert.id`, `alert.source`, `error` |
| `spool_full` | error | `capacity` |
| `spool_lost` | error | `alerts`, `message`, `error` |
| `spool_replay_failed` | warn | `error` |
| `spool_replayed` | info | `alerts` |
| `spool_unreplicated` | warn | `alert.id` |
| `stale_connections_closed` | info | `count` |
| `stats_failed` | warn | `error` |
| `storage_degraded` | warn | `reason`, `spooled` |
| `storage_error` | error | `error` |
| `storage_recovered` | info | `down_ms`, `spooled` |
| `subscription_changed` | info | none |
| `subscription_removed` | info | none |
| `subscription_removed_by_rights` | info | `device.id`, `user.name`, `group` |
| `subscriptions_pruned` | warn | `count`, `reason` |
| `subscriptions_removed_by_rights` | warn | `count` |
| `wal_checkpoint_failed` | warn | `error` |
| `watch_key_in_use` | warn | `error` |
| `webhook_alert_refused` | warn | `group`, `error` |

## Native commands

The binary has these commands. Each command that reads the configuration takes `--config FILE`. A command that needs the database opens it as the configuration says.

| Command | What it does |
|---|---|
| `portal run --config FILE` | Starts the server. |
| `portal check-config --config FILE` | Checks the file, the variables and the files that it names. Prints the value and the source of each key, then `configuration ok`. See [Effective configuration](#effective-configuration). |
| `portal print-config --config FILE` | Does the checks of `check-config`. Prints only the value and the source of each key. |
| `portal config-reference` | Prints the configuration reference in Markdown, from the table of keys. |
| `portal identity-public-key --config FILE` | Prints the public key of the identity tokens as a JWK Set. A service uses it. |
| `portal healthcheck --config FILE` | Asks `/healthz` of the listener, and `/readyz` with the block `inbox`. The exit code is 0 when the answer is good. The container image uses it. |
| `portal import-sqlite --config FILE --from SQLITE_FILE` | Copies the accounts, keys, external services, history and sessions of a SQLite file into the empty PostgreSQL database that the file names. See [Several instances](several-instances.md). |
| `portal end-sessions --config FILE` | Ends every session of the shared database, the sessions of the superadmin too. An instance that runs refuses a session within 5 seconds. It needs a `postgres` block: with SQLite, restart the portal. |
| `portal revoke-keys --config FILE (--user LOGIN \| --all)` | Revokes the keys of one user, or all the keys, in the database. It does not revoke the keys of the file. With SQLite, stop the portal first. |
| `portal reset-two-factor --config FILE (--user LOGIN \| --all)` | Removes the second factor of one account, or of each account and the check value of the key. With SQLite, stop the portal first. |
| `portal hash-password < password.txt` | Reads a password on the standard input (16 KiB at most) and prints its Argon2id hash. |
| `portal inbox keygen --out FILE` | Makes the key of the Inbox server. |
| `portal inbox token --out FILE` | Makes the text of an API key. |
| `portal inbox content-key --out FILE` | Makes a content key for `inbox.at_rest`. |
| `portal inbox content-purge --config FILE --key ID` | Erases the sealed contents of a key that the file lost. |
| `portal inbox rekey --config FILE [--from ID]` | Seals again the contents of the old keys with the active key. |
| `portal inbox import-sqlite --config FILE --from SQLITE_FILE` | Copies the data of a SQLite file of the Inbox into the PostgreSQL database. |
| `portal inbox healthcheck --config FILE` | Asks `/readyz` of the listener. |
| `portal render-demo [ICONS_DIR\|-] [PAGE]` | Prints a synthetic page as HTML on the standard output. Use it to look at the design with no session. |
| `portal version` | Prints the version. `portal --version` and `portal -V` do the same. |
| `portal help` | Prints the usage. `portal --help`, `portal -h` and the program without a command do the same. |

The commands `keygen`, `token` and `content-key` refuse to overwrite a file.

## Exit status

Each command exits with the status `0` when it succeeds. It exits with the status `1` for any error, also for an error of the command line or of the configuration. It then writes `portal: ` and the message to the standard error stream. A panic stops the process with the signal `SIGABRT`, so a shell shows the status `134`. `portal healthcheck` exits with `0` only when each answer has the status `200`. The page [Reference](reference.md#exit-codes) also gives the exit codes.

## Upgrades

A new binary changes the schema of the database at start. A change only adds. A binary refuses a schema that is newer than itself. An earlier binary does not start on a new schema. To return to an earlier version, restore the database from before the upgrade.

Some changes of the base schema (version 8) have no migration. A database that an earlier build made lacks a table or a column. The portal then stops at start and names the absent object. Make a new database, or add the object by hand as the notes below say. Roll the instances one at a time. A new instance opens its ports only when its catalogue is complete (see [Start and stop](#start-and-stop)).

The sections go from the newest version to the oldest. Read the sections between your version and the target.

### 5.6.1

This version changes no table, no frame of the phone and no key of the file. Read these points when you upgrade from 5.6.0.

- The probes have the new route `/livez`. They answer `HEAD`, and their answer has `Cache-Control: no-store`.
- Each answer with the security headers has `Cross-Origin-Opener-Policy` and `Permissions-Policy`.
- The command `print-config` is new. A command with `--config` accepts `-c FILE` and `--config=FILE`, with the options in any order.
- `portal inbox import-sqlite` stops with an error, not with a panic, when a row has a value of a wrong type.

### 5.6.0

This version changes the licence and adds one member to the metadata of the PDP. It changes no table, no frame of the phone and no key of the file. Read these points when you upgrade from 5.5.1.

- The licence of the whole workspace is `MIT OR Apache-2.0`. The file `LICENSE-APACHE` holds the full text of the Apache License 2.0. The image has the label `org.opencontainers.image.licenses`.
- The metadata of the PDP has the member `procurauth_directory_max_age_seconds`. With PostgreSQL, its value is `keys.directory_max_age` in whole seconds (default 30). It is rounded up when the duration is not whole, and it is never 0. With SQLite, the value is 1, because the PDP has no copy of its directory. A service that does not know the member ignores it.
- The approval API answers `401` with the code `unauthorized` and the header `WWW-Authenticate: Bearer realm="portal-pdp"` for an unknown credential. It answers `403` with the code `forbidden` only when the block `inbox.approvals` is absent.
- A route of a service that runs no action asks `access` on the service. The answer never has a reason of the approval gate, also when the grant of `access` has `confirm`.
- The demo and the tests give a plural unit to a ceiling, for example `rows`. A budget keeps a singular unit, for example `request`.

### 5.5.1

A grant with `confirm` on the action `access`, on the action `mcp.session` or on the service `portal` gets its own refusal. The key form gives the code `bad_confirm`, and the form of an account gives the notice `bad-confirm`. The text names the lock `confirm`. This version changes no key, no table and no frame of the phone.

### 5.5.0

This version aligns the portal with the ProcurAuth specification of 2026-11-13. It changes no table of the database and needs no migration file. Read these points when you upgrade from 5.4.x.

- A view of an approval can have no title, no fields and no body. The phone shows the name of the action as the title. A phone of 5.4.0 shows such a request as not valid. Install the APK of 5.5.0 before a service sends a view without members.
- The `confirm` of the grant of a person has no effect on the own questions of that person. It applies to the keys and the agents of the person. The PDP never gives an approval to a person.
- A grant with `confirm` on the action `mcp.session` is refused. The forms and the key forms refuse it, and a file that has it stops at start. The PDP ignores it in the data of an older database.
- A question with `context.permission_only` set to `true` marks a permission that is not the action of the use. The PDP ignores `confirm` for it. A marker with `context.usage`, with `context.approval` or with another value gives HTTP 400. Put `confirm` on the action of the use.
- The largest value of a limit is 9007199254740991 (2^53 - 1). Before, it was 1,000,000,000. A value that was valid stays valid.
- The metadata of the PDP has the members `procurauth_idle_max_seconds`, `procurauth_evaluations_max_bytes`, `procurauth_evaluations_max_items` and `procurauth_usage_max_seconds`. The default of `pdp.max_evaluations` is 128. It was 64.
- The portal counts the use first and then consumes the approval. If the consumption fails, the portal gives the units back. The metric `portal_budget_refunds_total` counts these refunds. The portal never loses an approval and never consumes it twice. A failure can leave one unit counted.
- A right `keys.own.use` that the owner lacks, and a key over `max_key_lifetime`, give a plain `false` with no reason. The reason `unknown_subject` is only for an orphan.
- The edge of the keys no longer refuses a key because the actions document is old. The key keeps the right `access`.
- The start checks that `inbox.approvals.max_command_bytes` plus 2048 bytes is at most `pdp.max_body_bytes`.

### 5.4.0

This version gives an external service a description and an icon of its own. It adds the block `external_service` to the file and three columns to the database. It makes the rule of the link stricter. Read these points when you upgrade from 5.3.x.

- The block `external_service "<name>"` is new and optional. The configuration is closed, so a 5.3.x portal does not know the block: add it only after the upgrade.
- The table `external_services` has three new columns: `description`, `icon_type` and `icon`. A 5.4.0 portal stops at start on a database without them. The error names the columns and the migration file. This change of the base schema has no automatic migration.
- Apply the migration file before you start 5.4.0. With PostgreSQL, stop the portal, take a `pg_dump`, then run `psql -v ON_ERROR_STOP=1 -f 5.3.0-to-5.4.0.sql`. The file adds the columns, keeps each row and drops nothing. A second run changes nothing.
- With SQLite, stop the portal, copy the database file, then run `sqlite3 -bail portal.db < 5.3.0-to-5.4.0.sqlite.sql`. SQLite has no `ADD COLUMN IF NOT EXISTS`, so run this file one time only. A second run stops with `duplicate column name`, and nothing changes.
- Both files are in [deploy/migrations/](../deploy/migrations). A new database does not need them.
- One rule now checks the link of an external service, in the file, in the form and in the database. The host must be a valid DNS name, an IPv4 address or a bracketed IPv6 address. A row that 5.3.x accepted can break the rule, for example `http://1.2.3/` or a host with an underscore.
- The start checks each row of `external_services`. A row that breaks the rules stops the start, and the error names the id of the row and the reason. The row stays in the database. Fix it with SQL or delete it, then start again.
- A service of the file wins over a row of the database with the same id. The portal ignores the row and does not delete it. The Services page shows the service of the file with the badge `file`, and it cannot change or delete it.
- The form that adds a service has the fields Description and Icon. The route reads a body of `server.max_body_bytes` plus 144 KiB for the superadmin.
- The route `GET /auth/ext-icon/<id>` serves the own icon to a signed-in session that may open the service.

### 5.3.0

This version makes the approval a duty of each service. It removes the key `approvals` of the block `pdp_client` and the member `approval` of the action document. Read these points when you upgrade from 5.2.x.

- A file that still has the key `approvals` stops at start with an unknown key error that names `pdp_client.<name>.approvals`. Delete the key from each block `pdp_client`. There is no alias.
- The portal ignores the member `approval` of an action, as it ignores each member that it does not know. Remove it when you can.
- Each grant of a service other than `portal` can have `confirm`, for any action but `access`. The service `portal` and the action `access` refuse `confirm`.
- The PDP answers `approval_required` for a grant with `confirm` on any action of any service with a known document. The block `inbox.approvals` must exist. Any token of a block `pdp_client` can use the approval API.
- A service must handle `approval_required` for all its actions. Give no grant with `confirm` on an action of a service that does not handle it.
- The database, the protocol and the Android app have no change.

### 5.2.1

This version changes only the Android app. The app shows the new state of an agent at once after the owner names or allows it. Install the APK of 5.2.1. The server of 5.2.0 still works.

### 5.2.0

This version adds the questions. Read these points when you upgrade from 5.1.0.

- The block `inbox.questions` is new and optional. It needs the blocks `inbox.items` and `inbox.at_rest`. Without it, the tool `ask` and the route `POST /api/v1/questions` do not exist.
- The actions `question.ask`, `question.read` and `question.cancel` and the budget `questions` are new. A key asks only when it holds `question.ask`, and its owner must hold it too.
- The database has no change.
- The answer of a question has the member `answer` for the agent. A form answer moved from `untrusted.answers` to `answer.untrusted.answers`.
- The command `portal inbox rekey` is new (see [Rotate the content key](#rotate-the-content-key)).
- The Android app of 5.2.0 and the server of 5.2.0 go together.

### 5.1.0

This version adds the items of the Inbox, the approvals of grants and the agents of a key. The three parts changed the base schema in place, so a database from 5.0.x needs a new Inbox database or the changes below. Read these points when you upgrade from 5.0.x.

**Items.** The blocks `inbox.items` and `inbox.at_rest` are new and optional, and each one needs the other. Without them, the Inbox runs as before.

- The tables `items`, `turns`, `approvals`, `item_events`, `approval_intents`, `approval_mute`, `account_prefs`, `items_heartbeat` and `content_key_check` are new. The table `alerts` has two new columns, `request_id` and `seq`.
- A database that 5.0.x made lacks the tables. With the block `inbox.at_rest`, the portal stops at start and names the table. Make a new Inbox database.
- A sealed content needs its key. The portal stops at start when sealed rows name a key that the file lost. The error gives the key id and the count. Add the key again, or run `portal inbox content-purge`.
- `portal inbox import-sqlite` refuses a SQLite file that holds items.
- Keep the content keys outside the data volume and outside the backups of the database. The Compose script `init-secrets.sh` makes the file `inbox_content_key`. The K3s Secret `portal-secret` gets the key `inbox_content_key`.

**Approvals.** A grant can have the property `confirm`, and a key has the attribute `interaction`. See [Grants and the gate](grants-and-gate.md) and [Keys and limits](keys-and-limits.md). The PDP answers `false` with `approval_required` for an action with `confirm`, and the agent asks again after the owner approves.

- The columns `user_grants.confirm`, `key_grants.confirm` and `keys.interaction` are new. A database that 5.0.5 made lacks them, and the portal stops at start and says so. Make a new database, or add the columns by hand.
- With SQLite, add `confirm INTEGER NOT NULL DEFAULT 0 CHECK (confirm IN (0, 1))` to `user_grants` and to `key_grants`. Add `interaction TEXT NOT NULL DEFAULT 'conversational' CHECK (interaction IN ('conversational', 'one_shot'))` to `keys`. With PostgreSQL, use `confirm BOOLEAN NOT NULL DEFAULT false`.
- Then run `UPDATE keys SET interaction = 'one_shot' WHERE kind = 'api';`. A start from an empty database makes the columns.
- An item of `grants` in the file can have `confirm = true`. A grant of the service `portal` or of the action `access` with `confirm = true` stops the start.
- The attribute `interaction` is new in the blocks `key` and `api_key`. Its default is `conversational` for `key` and `one_shot` for `api_key`.
- The command `import-sqlite` copies the new columns. A file that lacks them gets the defaults.

**Agents.** The block `inbox.agents` is new and optional. It needs the blocks `inbox.items` and `inbox.at_rest`. Without it, each key is one agent. With it, a key can have the mode `discover`, and the portal gives each agent a token.

- The column `keys.agents` is new. It has the values `key`, `discover` and `enrolled`. A database that an earlier build made lacks it, and the portal stops at start and names the column. Add `agents TEXT NOT NULL DEFAULT 'discover' CHECK (agents IN ('key', 'discover', 'enrolled'))` to `keys`, then run `UPDATE keys SET agents = 'key' WHERE kind = 'api';`.
- The tables `agents` and `agent_events` are new. The columns `items.agent_id` and `approval_intents.agent_id` are new, each with a foreign key to `agents`. A database that an earlier build made lacks the table `agents`. With the block `inbox.at_rest`, the portal stops at start and names the table. Make a new Inbox database.
- The default of `inbox.agents.lock_new` is `false`: a new agent is allowed at once, and its owner gets a notice. Set it to `true` to lock the approval of a new agent until its owner allows it.
- The hints of an agent are sealed with the content keys. The command `portal inbox content-purge` erases them with the other contents of a lost key.
- The metrics `portal_agents_created_total`, `portal_agent_refused_total` and `portal_agent_actions_total` are new.
- The Android app and the server of 5.1.0 go together. The app keeps its portals when it updates from an earlier build, and it drops its local items. They return from the server at the next connection.
- A phone of an earlier build cannot read the new frames of the agents. Update the app and the server in one step.

### 5.0.5

This version changes how the launcher opens a service, and it adds external services. It adds one table to the database. It has no change of the protocol. Read these points when you upgrade from 5.0.4.

- A tile of the launcher opens its service in a new browser tab. The key `ui.services_new_tab` (default `true`) sets this for every service. Set it to `false` to open the service in the same tab.
- The label `portal.target` does not exist. A service cannot choose its own behavior, and the portal ignores the label.
- The superadmin can add an external service: a name and a link, with the badge `external`. The Settings page Services has the button **Add external service**.
- The table `external_services` is new. A database that 5.0.4 made lacks it. The portal stops at start and says so. Make a new database, or add the table by hand.
- With SQLite, run `CREATE TABLE external_services (id TEXT PRIMARY KEY, name TEXT NOT NULL, href TEXT NOT NULL, created_at INTEGER NOT NULL);`. With PostgreSQL, use `created_at BIGINT NOT NULL`. A start from an empty database makes the table.
- The command `import-sqlite` copies the external services too.

### 5.0.4, 5.0.3 and 5.0.2

These versions have no change of the database or of the configuration, except the points below.

- 5.0.4: the page of an account that another instance made a moment ago opens at once. The pages and the actions of the superadmin that name an unknown account read the accounts again from the shared database first. The account page shows the server permissions as the first row of the Services list.
- 5.0.3: the grant editor and the pages have a layout for narrow screens. A service row shows an icon after its name when the service publishes its actions. The sign-in form has a box to keep the session for 30 days.
- 5.0.2 follows protocol 4.1. The PDP refuses a node below the service that has no `resource.properties.ancestors`. It answers `false` with the reason `invalid_resource`. A node of depth 1 has the chain `[{"type": "service", "id": "<service>"}]`. The Inbox API reads an API key only in the header `Authorization: Bearer`. A request with the query `?token=` gets the status 401.

### 5.0.1

This version closes the findings of a security audit. Read these points when you upgrade from 5.0.0.

- The base schema changed in place. A PostgreSQL database needs the column `sessions.credential`, and each database needs the column `alerts.service`. A database without them is refused at start: make a new database.
- A cluster of instances must run one version. The copies of alerts between instances carry the service of a watch alert.
- Three keys are new: `session.max_sessions_per_account` (16), `inbox.devices.max_handshakes_per_address` (16) and `inbox.devices.max_per_account` (10). A file that sets `session.max_active_sessions` below 16 must also set the first key.
- A watch alert about a service reaches only the people who may open that service. Give `access` on the service to the members of `inbox.watch.group` that must get its alerts.
- A file key reads the status of an alert only when its owner holds `alert.read` on the group.
- A workload that is not listed but has a path stays closed to everyone except the superadmin.
- The webhook keys of Alertmanager and Grafana have a new form: an alert that is active during the upgrade is created once more.
- The enrollment fingerprint shows 80 bits. The app and the page show the same value.

### 5.0.0

This version is the first version of the merged product. Read these points when you upgrade from a portal that has grants and keys.

- The Inbox is new. Without the block `inbox`, nothing changes for the portal.
- A key gets a kind. A key of an earlier version becomes an MCP key. The rights to make and to use a key apply to the kinds: give `keys.own.create` and `keys.own.use` on `key_kind:mcp` or `key_kind:api`. These two actions are not valid on the whole service.
- An account that has no permission `inbox.receive` loses its subscriptions at the first check that has a directory. The account also cannot make an enrollment code. Give the permission before you turn the Inbox on for the people that use it.
- A phone of the earlier standalone alert server (protocol 3) cannot connect. It gets the close code 4426. Each phone must enroll again with a code.
- The configuration has no alias for a block, a key or a command of an earlier name. `portal check-config` names the key that it refuses.

## Schema versions

The database has a schema version. SQLite keeps it in `PRAGMA user_version`. PostgreSQL keeps one row for each applied version in the table `schema_migrations`. Both stores apply each absent step at start, in one transaction, in order. A failure rolls the transaction back and leaves the database at the earlier version. Both stores are at version 8 today.

| Change | Version in SQLite | Version in PostgreSQL |
|---|---|---|
| Grants of the users. The migration drops the data of the older access model: the accounts and the history stay, and no user has a grant after it. Give the grants again in the settings pages or in `user` blocks. | 3 | 2 |
| A grant can name a sub-resource of the service. A grant of the earlier version applies to the whole service. | 4 | 3 |
| Credential generation of the superadmin (column `sessions.admin_generation`). The sessions of the superadmin and the impersonated sessions end at the first start. | none: SQLite sessions are in memory | 4 |
| The path of a grant is one column, `resource_path`. A grant on one sub-resource `(database, a/b)` becomes the path `database:a%2Fb`. A grant that does not follow the new tree is listed on the Services page under **Permissions to check**, and the log has the warning `grant_to_check`. A row that does not fit the rules stops the migration. | 5 | 5 |
| Keys and limits: the tables `keys`, `key_grants`, `key_history`, `budget_counters` and `directory_version`, and the column `limits` of `user_grants`. | 6 | 6 |
| The kind of a key (`mcp` or `api`). A key from before is an MCP key. | 7 | 7 |
| The remembered sessions (column `remembered`). | 8 | 8 |

Later features changed version 8 in place and have no migration. These features are the second factor, the external services, the approvals, the agents, and the description and icon of the external services. The second factor adds the tables `totp_credentials`, `totp_recovery` and `totp_key_check` and the column `mfa`.

The portal checks for these objects at start. It stops and names the absent object, as the notes above say. Version 5.4.0 gives migration files for the last change.

An older portal does not start on a newer schema.
