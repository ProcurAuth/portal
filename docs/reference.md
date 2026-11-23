# Reference

This page lists the HTTP endpoints of the three listeners of portal. It lists the error codes, the cookies and the headers. It also gives the rules of the command line and the exit codes.

The tables come from the code of version 5.6.0: the route tables in `src/http.rs`, `src/http/pdp.rs`, `src/http/approval_api.rs`, `src/inbox/http.rs` and `src/inbox/peer.rs`. A path that this page does not list is an unknown path. The section [Command line](#command-line) also gives changes after version 5.6.0: see the section `[Unreleased]` of [CHANGELOG.md](../CHANGELOG.md).

## How to read this page

portal has three listeners. Each listener has its own configuration key. See [Architecture](architecture.md) for the flows.

| Listener | Key | Callers |
|---|---|---|
| Portal | `server.listen_address` | Browsers, Traefik, phones, senders, agents and Prometheus |
| PDP | `pdp.listen_address` | Services (a `pdp_client` block gives each service a token) |
| Cluster | `inbox.cluster.listen` | Other instances of portal |

The column **Authentication** uses these words.

| Word | Description |
|---|---|
| none | The route needs no credential. |
| session | The request needs the session cookie of a signed-in account. |
| superadmin | The request needs the session of the superadmin in its own right. An impersonated session does not count. |
| form token | The form carries the field `csrf`. The token is bound to the session and to one purpose. The `Origin` header must match the public origin. See [Security model](security.md). |
| `api_` key | The header `Authorization: Bearer api_...`. See [Keys and limits](keys-and-limits.md). |
| `mcp_` key | The header `Authorization: Bearer mcp_...`. |
| PDP token | The header `Authorization: Bearer <token>` of a `pdp_client` block. |
| peer MAC | The headers `X-Peer-Time` and `X-Peer-Auth` of the cluster channel. |

A route that needs a block of the configuration answers as an unknown path when the block is absent. The text bodies of the pages are in "Plain-text answers of the pages".

## Portal listener

The Inbox owns the paths `/v1/`, `/api/v1/`, `/readyz` and `/api/version`, and the path `inbox.mcp.path`. It owns them only when the file has the block `inbox`. No session and no other handler applies to these paths. With the block `inbox`, the listener serves them while the instance still starts. Every other route answers `503` with the text `starting` until the catalogue is complete. See [Operations](operations.md).

### Health, version and metrics

| Method | Path | Authentication | Success | Main errors |
|---|---|---|---|---|
| GET | `/healthz` | none | `200` with `ok` | `503` `starting` |
| GET | `/readyz` | none | `200` with `ready` | See the note below. |
| GET | `/api/version` | none | `200` JSON `{version, git_sha, protocol}` | Unknown path without the block `inbox`. |
| GET | `metrics.path` (default `/metrics`) | none | `200` in the Prometheus text format | Unknown path when `metrics.enabled` is `false`. |

The route `/healthz` never reads the database. Without the block `inbox`, `/readyz` answers `200` with `ready` after the start. With the block `inbox`, the Inbox answers `/readyz`:

- SQLite gives `200` with `ready`, or `503` with `storage unavailable`.
- PostgreSQL gives `200` with `ready`, or `200` with `degraded` when the database has no primary.
- During a stop, the answer is `503` with `draining`.

Do not route `metrics.path` through a public router. See [Operations](operations.md).

### Gate for Traefik

Traefik calls these two routes with ForwardAuth. They accept any method. They read headers and never read a body.

| Method | Path | Authentication | Success | Main errors |
|---|---|---|---|---|
| any | `/auth/verify` | session cookie of the forwarded request | `204`, with the identity token in the header of `identity.header` (default `X-Identity-Token`) when a service owns the path | See below. |
| any | `/auth/verify-key` | `mcp_` key | `204` with the identity token of the key | `401`, empty body |

`/auth/verify` reads `X-Forwarded-Uri` (mandatory, one value), `X-Forwarded-Method`, `Cookie` and `User-Agent`. It reads `X-Forwarded-For` only from a proxy in `server.trusted_proxies`. The proxy must set `X-Forwarded-Proto` and `X-Forwarded-Host`, because the portal reads them as they come. It answers:

| Status | Case |
|---|---|
| `204` | The path is public (`/auth/login`, `/auth/logout`, `/auth/login/code`, `/auth/login/enrol`, `/auth/style.css`, `/auth/assets/theme.js` and the hashed assets), or a service excludes it with `portal.exclude`, or the session may open the path. |
| `302` | There is no session. The `Location` is `/auth/login`, with the signed return address in `rd` and `sig`. |
| `302` | The session has no grant for the path, and the method is `GET` or `HEAD`. The `Location` is `/auth/denied`. |
| `403` | The session has no grant for the path, and the method is another one. The body is `Forbidden`. |
| `400` | The header `X-Forwarded-Uri` is absent, repeated, too long or not a valid address. The body is `invalid forwarded uri`. |
| `500` | The portal cannot sign the return address or the identity token. |

`/auth/verify-key` answers `401` with an empty body for each failure. The header `WWW-Authenticate` is `Bearer` when the request has no key. It is `Bearer error="invalid_token"` for any other failure. The answer never shows the cause. The cause is in the log line `key_check_failed` and in the metric `portal_key_checks_total{result}`.

The causes are `no_key`, `bad_format`, `wrong_kind`, `unknown_key`, `expired`, `key_too_old`, `owner_unknown`, `owner_cannot_use_keys`, `no_service`, `bad_forwarded_header`, `no_access`, `keys_disabled` and `directory_too_old`. A key of the kind `api` gets `wrong_kind` on this route. See [Grants and the gate](grants-and-gate.md) and [Identity tokens and the PDP](identity-and-pdp.md).

### Sign-in and sign-out

| Method | Path | Authentication | Success | Main errors |
|---|---|---|---|---|
| GET | `/auth/login` | none | `200` with the form. `303` to the return address when a session exists. | `400` for a bad `rd` or `sig` query. |
| POST | `/auth/login` | none. The form carries `csrf` (cookie of double submit) and the field `token`. | `303` to the return address. `200` with the page of the second step, or with the recovery codes. | See below. |
| POST | `/auth/login/code` | the signed token of the first step | `303` to the return address | `401` wrong code, `403`, `429`, `503` |
| POST | `/auth/login/enrol` | the signed token of the first step | `200` with the recovery codes | `401`, `403`, `429`, `503` |
| GET | `/auth/success` | session | `200` | `303` to `/auth/login` |
| GET | `/auth/denied` | session | `403` with the page "access denied" | `303` to `/auth/login` |
| POST | `/auth/logout` | form token (purpose `logout`) | `303` to `/` | `403` `CSRF validation failed`, `400` |
| GET | `/auth/logout` | none | `303` to `/`. It ends no session. | |
| GET | `/` | session | `200` with the launcher | `302` to `/auth/login` |

`POST /auth/login` has these answers:

- `400` for a bad field, a bad return address or a password that is too long.
- `401` with the form for a wrong login or password. A form that fails the anti-bot check gets the same answer.
- `403` with the sentence "The page was open too long" when the CSRF cookie or the form is stale.
- `415` for a body that is not a form.
- `429` with `Retry-After` when a limiter refuses.
- `503` when the session store is away or the session capacity is full.

See [Authentication and sessions](authentication.md).

### Pages of the launcher and the settings

All these routes use `GET`. A request with no session gets a `302` to `/auth/login`.

| Path | Who may open it | Success | Other answers |
|---|---|---|---|
| `/auth/settings` | The superadmin, or an account that can see the list of keys | `200` tab Access | `404` page "No settings page" |
| `/auth/settings/users/{login}` | The superadmin | `200` page of the account | `404` for an unknown account |
| `/auth/settings/new-user` | The superadmin | `200` | `404` when `users.allow_creation` is `false` or `access.grants_mode` is `static` |
| `/auth/settings/services` | The superadmin | `200` tab Services | |
| `/auth/settings/activity` | The superadmin | `200` tab Activity | |
| `/auth/settings/give?user=&service=` | The superadmin | `303` to the editor of grants | `404` |
| `/auth/settings/security` | An account, when `authentication.mfa` is not `off` | `200` | `404` |
| `/auth/options?service=&type=&parent=&q=&after=` | An account that can fill the forms of grants | `200` JSON `{items, next, unavailable}` | `401`, `403`, `400` for an unknown, repeated or long parameter |

`/auth/options` asks the service for the children of a node, with the identity token of the person. `unavailable` is `true` when the service gives no list. An impersonated session can read these pages. See [Settings pages and history](settings-and-history.md).

### Forms of the accounts and of the services

Each route is a `POST` with a form body. Each answer is a `303` back to the page with the code of the outcome in the query `s`, unless the table says another status.

| Path | Authentication | Use | Refusals |
|---|---|---|---|
| `/auth/users/add` | superadmin, form token (purpose `users`) | Add an account. | `422` page "New user" with the reason |
| `/auth/users/update` | superadmin, form token | Change the grants of an account, or its password. | |
| `/auth/users/delete` | superadmin, form token | Delete an account. | |
| `/auth/users/impersonate` | superadmin, form token | Open a session as another account (the button `Log in as`). | |
| `/auth/users/impersonate-end` | form token, in an impersonated session | Return to the session of the superadmin. | `403` in another session |
| `/auth/users/end-sessions` | superadmin, form token, field `confirm=end-all-sessions` | End all sessions. | |
| `/auth/users/reset-two-factor` | superadmin, form token | Remove the second factor of an account. | `404` when `authentication.mfa` is `off` |
| `/auth/services/add` | superadmin, form token | Add an external service. | `404` when `access.grants_mode` is `static` |
| `/auth/services/delete` | superadmin, form token | Delete an external service. | |
| `/auth/security/enable` | session, form token (purpose `security`) | Confirm the second factor with the first code. | `200` with the recovery codes. A wrong code gives `200` with the page of the enrolment. |
| `/auth/security/disable` | session, form token, password and code | Disable the second factor. | |
| `/auth/security/recovery` | session, form token, password and code | Make new recovery codes. | `200` with the recovery codes |

Each such route makes the checks below, in this order:

1. The `Origin` header (`403` `Origin validation failed`).
2. The content type (`415`).
3. The session (`303` to `/auth/login`).
4. The size of the body (`413` or `408`).
5. A field that the route does not know (`400`).
6. The token (`403` `CSRF validation failed`).

A route that needs the superadmin answers `403` `Forbidden` to any other session. See [Security model](security.md).

### Keys

The list of the keys is a panel of the tab Access. The path `/auth/keys` does not exist. Each route needs a session. An impersonated session can read. It can never make, change or revoke a key.

| Method | Path | Authentication | Success | Main errors |
|---|---|---|---|---|
| GET | `/auth/keys/new` | session | `200` tab Access with the pane of a new key | `404`, `403` |
| GET | `/auth/keys/edit` | session | `200` with the pane of a change | `404`, `403` |
| POST | `/auth/keys/create` | form token (purpose `keys`) | `200` with the text of the key, one time, with `Cache-Control: no-store` | `422` when a right needs `access`, `400`, `403`, `413` |
| POST | `/auth/keys/edit` | form token | `303` to the tab Access | `400`, `403`, `404` |
| POST | `/auth/keys/revoke` | form token | `303` to the tab Access | `400`, `403` |

The right to make, change and revoke comes from the permissions `keys.own.*` and `keys.all.*` of the service `portal`. See [Keys and limits](keys-and-limits.md).

### Pages and forms of the Inbox

The pages need the block `inbox`. Without the block, each route is an unknown path. A session that has no right on the Inbox gets `403`. A request with no session gets a `303` to `/auth/login`.

Each `POST` makes the checks of the previous section, with the purpose `inbox`. A form answers with a `303` to the page and a code in the query `s`. The form `code` shows the enrollment code on the page itself, with `Cache-Control: no-store`.

| Method | Path | Use |
|---|---|---|
| GET | `/auth/settings/inbox` | The tab Inbox: phones, groups, sent alerts, connections. |
| GET | `/auth/settings/inbox/notifications` | The poll of new alerts, as JSON. It does not renew the idle timer. |
| GET | `/auth/settings/inbox/items` | The history of the requests and the questions. |
| GET | `/auth/settings/inbox/items/{id}` and `.../{id}/body` | One request or question, and its body. |
| GET | `/auth/settings/inbox/limits` | The limits of the account. |
| GET | `/auth/settings/inbox/prefs` | The preferences of the account. |
| GET | `/auth/settings/inbox/agents` and `.../agents/{id}` | The agents of the account, and one agent. |
| POST | `/auth/settings/inbox/send` | Send an alert to a group or to an account. `403` in an impersonated session. `422` with the reason when the server refuses. |
| POST | `/auth/settings/inbox/code` | Make an enrollment code for a phone. |
| POST | `/auth/settings/inbox/revoke` | Revoke a phone. |
| POST | `/auth/settings/inbox/read` | Move the marker "read up to". |
| POST | `/auth/settings/inbox/notify` | Set the level of the notifications in the browser. |
| POST | `/auth/settings/inbox/items/answer` | Answer an approval. |
| POST | `/auth/settings/inbox/items/question` | Answer a question. |
| POST | `/auth/settings/inbox/items/reply` | Write a message in the thread of an item. |
| POST | `/auth/settings/inbox/items/cancel` | Cancel an open item. |
| POST | `/auth/settings/inbox/prefs` | Save the preferences. |
| POST | `/auth/settings/inbox/agents/rename`, `/allow`, `/block`, `/unblock`, `/merge`, `/enroll`, `/reset` | Manage an agent. |

A `GET` on the path of a form answers `405`. A `POST` on any other path, and a `GET` on any other path under `/auth/settings/inbox`, answer `404`. See [Inbox](inbox/README.md).

### Assets and icons

| Method | Path | Authentication | Content |
|---|---|---|---|
| GET | `/auth/a/{hash}.css`, `/auth/a/{hash}.js` | none | The stylesheet and the scripts of the pages. The hash follows the content. `Cache-Control: public, max-age=31536000, immutable`. |
| GET | `/auth/a/{hash}.svg` | session | The icon of the tab, when `ui.favicon` is `"default"`. A visitor gets the answer of an unknown path. |
| GET | `/auth/style.css`, `/auth/settings.css` | none | The plain stylesheets. `Cache-Control: public, max-age=3600`. |
| GET | `/auth/assets/theme.js`, `/auth/assets/settings.js`, `/auth/assets/notify.js` | none | The plain scripts. |
| GET | `/auth/icons/{set}/{name}.svg` | none | An icon of a bundled set. `404` for an unknown icon. |
| GET | `/auth/ext-icon/{id}` | session that may open the service | The image of an external service. The answer has an `ETag` and `Cache-Control: private, no-cache`. |

### API for senders and webhooks

The routes need the block `inbox`. A sender uses a key of the kind `api`, from a block `api_key` of the file or from the database. The key is in the header `Authorization: Bearer`. The portal never reads a key from the query. The key comes before the body, so a request with no valid key reads no body. A body has at most `inbox.devices.max_request_bytes` bytes.

Each JSON answer has `Cache-Control: no-store`.

| Method | Path | Right of the key | Success |
|---|---|---|---|
| POST | `/api/v1/alerts` | `alert.send` on the group, or `alert.send_user` for an account | `201` `{alert_id, duplicate: false, targets}`. `200` with `duplicate: true` for a repeat. |
| GET | `/api/v1/alerts/{id}` | `alert.read` on the group. For an alert to an account, the key that sent it. | `200` with the status and the deliveries of the alert |
| POST | `/api/v1/notify/{group}` | `alert.send` on the group | `201` or `200`, as for `/api/v1/alerts` |
| POST | `/api/v1/webhooks/alertmanager/{group}` | `alert.send` on the group | `200` `{created, duplicates, skipped, refused}` |

- The body of `POST /api/v1/alerts` is a JSON object with `title` and exactly one of `group` and `user`. An unknown member gives `400` `invalid_json`.
- The header `Idempotency-Key` has 1 to 128 printable ASCII characters. Without it, the portal derives a key from the content and the current minute.
- `/api/v1/notify/{group}` takes the title from the query `title`, from the header `X-Title`, from the member `title` of a JSON body, or from the body. It cuts a long title.
- The webhook takes the Alertmanager and Grafana payload. It makes one alert for each entry of `alerts`. It skips a `resolved` entry unless the key is a key of the file with `webhook_send_resolved`.
- Any other method on these paths gives `405`. Any other path under `/api/v1/` gives `404` with an empty body.

The error codes are in "Errors of the API for senders".

### Phones

The two routes are `GET` requests that upgrade to a WebSocket. The content is Noise frames in binary messages. See [Phone protocol](inbox/protocol.md).

| Path | Use | Authentication | Answers before the upgrade |
|---|---|---|---|
| `/v1/enroll` | Enroll a phone with an enrollment code. | The code, inside the Noise handshake | `101`, `400`, `405`, `503` with `Retry-After: 5` when the budget of sessions is full |
| `/v1/session` | The session of an enrolled phone. | The key of the phone, inside the Noise handshake | The same |

A refusal after the upgrade is a close code. See "Close codes of the phone".

### Agent channel

The channel needs the block `inbox.items`. The MCP endpoint needs the block `inbox.mcp`. The questions need the block `inbox.questions`. The approvals need the block `inbox.approvals`. A REST route of a block that the file does not enable answers `404` `not_found`.

Without `inbox.mcp`, the MCP path is an unknown path. Read [Agents](inbox/agents.md) for the tools and the rules.

| Method | Path | Key | Use | Success |
|---|---|---|---|---|
| POST | `inbox.mcp.path` (default `/mcp`) | `mcp_` | JSON-RPC 2.0 over HTTP. The tools are `approval_intent`, `ask`, `get_answer`, `keepalive`, `say`, `supersede` and `cancel`. | `200` JSON-RPC result. `202` for a notification. |
| POST | `/api/v1/approval-intents` | `api_` | Tool `approval_intent`. | `201` |
| POST | `/api/v1/questions` | `api_` | Tool `ask`. | `201` for a new question. `200` for a repeat. |
| GET | `/api/v1/items/{id}?wait=&after_seq=` | `api_` | Tool `get_answer`. `wait` renews the lease. | `200` |
| DELETE | `/api/v1/items/{id}` | `api_` | Tool `cancel`. | `200` |
| POST | `/api/v1/items/{id}/keepalive` | `api_` | Tool `keepalive`. | `200` |
| POST | `/api/v1/items/{id}/turns` | `api_` | Tool `say`. | `201` |
| POST | `/api/v1/items/{id}/supersede` | `api_` | Tool `supersede`. | `200` for a question. `422` `use_service_tool` for an approval. |

The routes under `/api/v1/questions/{id}` are the routes of `/api/v1/items/{id}`, when the block `inbox.questions` exists. The agent token goes in the header `Agent-Token` or in the member `agent_token` of the body. The query never holds it: a request with `agent_token` or `agent-token` in the query gets `400` `invalid_query`. An `mcp_` key does not open the REST routes, and an `api_` key does not open the MCP endpoint. Each gets `401`.

The MCP endpoint answers these statuses before it reads the message:

| Status | Cause |
|---|---|
| `401` | No key, an unknown or expired key, or a key of the other kind. The header `WWW-Authenticate` is `Bearer realm="portal"`. |
| `403` | The `Origin` is not in `inbox.mcp.allowed_origins`, or the client address is outside `inbox.mcp.allowed_client_cidrs`. |
| `405` | The method is not `POST`. The header `Allow` is `POST`. |
| `413`, `415`, `408` | The body is too large, is not `application/json`, or comes too slowly. |
| `429` | The key, or the address with no valid key, is over its limit. `Retry-After` gives the seconds. |
| `503` | The directory of keys is not ready. `Retry-After` is `2`. |
| `400` | The body is not valid JSON-RPC, or the protocol headers disagree with the message. The body is a JSON-RPC error. |

The JSON-RPC error codes are:

- `-32700`: parse error.
- `-32600`: invalid request.
- `-32601`: unknown method.
- `-32602`: invalid parameters. A tool that the key cannot see gives it too.
- `-32020`: a protocol header disagrees with the message.
- `-32022`: unknown protocol version, with `data.supported`.

Each answer has `Cache-Control: no-store`.

## PDP listener

The PDP listener answers JSON. A body of an error is `{"error": "<text>"}`, except in the approval API. Each route, except the metadata and `/healthz`, needs the bearer token of a `pdp_client` block. A request with no valid token gets `401` with `WWW-Authenticate: Bearer realm="portal-pdp"`.

The PDP answers only for the resources of the service that owns the token. It copies the header `X-Request-ID` (1 to 128 printable ASCII characters) to the answer. A bad value gives `400`. See [Identity tokens and the PDP](identity-and-pdp.md).

| Method | Path | Authentication | Use |
|---|---|---|---|
| POST | `/access/v1/evaluation` | PDP token | One decision. |
| POST | `/access/v1/evaluations` | PDP token | A batch of decisions. `options.evaluations_semantic` is `execute_all`, `deny_on_first_deny` or `permit_on_first_permit`. |
| POST | `/access/v1/search/action` | PDP token | The actions that the subject may do on the resource. It takes no `context.usage`. |
| GET | `/.well-known/authzen-configuration` | none | The metadata: the endpoints under `pdp.url`, the limits of the listener, the age bound of the directory and, with approvals, the members `procurauth_approval_*`. |
| GET | `/healthz` | none | `200` with `ok`. |
| POST | `/approval/v1/requests` | PDP token | A service submits an approval request. |
| GET | `/approval/v1/requests/{id}?subject=&wait=&renew=` | PDP token | Read the verdict. `wait` is in seconds, never more than `inbox.items.wait_max`. `renew` is `true` or `false`. |
| POST | `/approval/v1/requests/{id}/keepalive` | PDP token | Renew the lease. The body is `{"subject": "..."}` and no other member. |

A decision answer is `{"decision": true|false, "context": {...}}`. A `true` answer can have `context.limits` and `context.usage.units`. A `false` answer can have `context.reason`, `context.retry_after` and `context.evaluation_id`. The reasons are in "Reasons of a refused decision".

| Status | Case |
|---|---|
| `200` | A decision, also a `false` one. |
| `400` | The body is too large, comes too slowly, is not a JSON object, has a bad member, or has a bad `context.usage`. |
| `401` | No valid token. |
| `404` | Unknown path. The body is `{"error": "not found"}`. |
| `405` | A wrong method on a known path. |
| `503` | The count of a use did not work. The PDP never answers a decision in this case. |

The approval API answers `{"error": "<code>"}`, with `retry_after` for a limit. The codes are in "Errors of the approval API". A body of a verdict is `{"verdict": "pending|approved|denied|void"}`. A new request answers `201` with `{request_id, code, verdict}`. A request that still waits answers `200` with the same members.

The body of `POST /approval/v1/requests` is a JSON object (`Content-Type: application/json`). It has `evaluation_id`, `effect`, `op`, `view` and exactly one of `approval_ref`, `lease_s` and `until`. It can also have `supersedes`. An unknown member gives `422` `invalid_argument`.

## Cluster listener

The cluster listener exists when the file has `inbox.cluster.listen`. Only the instances of the same service must reach it. Each request carries `X-Peer-Time` (milliseconds) and `X-Peer-Auth` (hexadecimal HMAC-SHA-256 over the time, the path and the body, with a key derived from the server key). The portal refuses a request whose time differs by more than 60 seconds. A bad or absent MAC gives `401`.

| Method | Path | Use | Success | Other answers |
|---|---|---|---|---|
| GET | `/internal/v1/hello` | Ask who an address is. | `200` `{instance}` | |
| POST | `/internal/v1/spool` | Copy an alert that waits for the database. | `200` `{instance, stored: true}` | `400`, `404` (no spool), `409`, `422`, `503` `spool full` |
| POST | `/internal/v1/ack` | Copy a receipt of a phone. | `200` | `400` |
| POST | `/internal/v1/replaced` | Tell that another instance holds the session of a phone. | `200` | `400` |

The body is at most 64 KiB. A wrong method gives `405`, and any other path gives `404`, with an empty body.

## Error codes

### Plain-text answers of the pages

The pages and the forms of the portal listener answer a short text for a refusal.

| Status | Text | Case |
|---|---|---|
| `400` | `Bad Request` | A form with a bad or unknown field, or a bad query. |
| `400` | `invalid forwarded uri` | `/auth/verify` has no usable `X-Forwarded-Uri`. |
| `403` | `Forbidden` | The session has no right for this route. |
| `403` | `CSRF validation failed` | The form token is absent or wrong. |
| `403` | `Origin validation failed` | The `Origin` header (or the `Referer`) does not match the public origin. |
| `404` | `Not Found` | An unknown path or an unknown object. |
| `408` | `Request Timeout` | The body comes too slowly. |
| `413` | `Payload Too Large` | The body is larger than the limit of the route. |
| `415` | `Unsupported Media Type` | The body of a form is not `application/x-www-form-urlencoded`. |
| `500` | `authentication error` | The portal cannot make a token or a cookie. |
| `503` | `starting` | The catalogue is not complete. |
| `503` | `Service Unavailable` | The sign-in has no room now. The header `Retry-After` is `1`. |
| `503` | `session capacity reached`, `session store unavailable` | The sign-in cannot open a session. |

### Errors of the API for senders

The JSON body is `{"error": {"code": "<code>", "message": "<text>"}}`. A message never holds a secret.

| Code | Status | Cause |
|---|---|---|
| `invalid_json` | `400` | The body is not a JSON object of the expected form. |
| `invalid_target` | `400` | The body has both `group` and `user`, or none. |
| `invalid_idempotency_key` | `400` | The key is not 1 to 128 printable characters. |
| `invalid_query` | `400` | The query has a repeated or badly encoded parameter. |
| `invalid_payload` | `400` | A webhook payload has no `alerts` array. |
| `invalid_body` | `400` | The body cannot be read. |
| `unauthorized` | `401` | No key, a bad key, an unknown key, or a key of another kind. |
| `forbidden_group` | `403` | The key lacks the right on the group. |
| `forbidden_user` | `403` | The key lacks `alert.send_user`. |
| `not_found` | `404` | No such alert, or the key may not read it. |
| `unknown_user` | `404` | The account does not exist. |
| `body_timeout` | `408` | The body comes too slowly. |
| `idempotency_conflict` | `409` | The key names another alert. |
| `no_phone` | `409` | The account has no phone that can receive the alert. |
| `too_large` | `413` | The title or the body is too large. |
| `unknown_group` | `422` | The group is not in the file. |
| `missing_title` | `422` | The title is empty. |
| `invalid_text` | `422` | The title has a control character. |
| `rate_limited` | `429` | The budget of the key is spent. `Retry-After` gives the seconds. |
| `directory_not_ready` | `503` | The portal cannot check keys yet. |
| `rights_unavailable` | `503` | The portal cannot count the use now. |
| `storage_unavailable` | `503` | The database is away. |
| `replication_unavailable` | `503` | The copy to the replica failed. |
| `spool_full` | `503` | The database is away and too many alerts wait. |

### Errors of the agent channel

The MCP door gives these as a tool error: `isError` is `true`, and `structuredContent.error` has `code`, `message` and `retryable`. The REST door gives `{"error": {"code", "message"}}` with the status. An error is retryable for `408`, `429`, `503` and `504`. A limit adds `retry_after` in whole seconds. Read [Agents](inbox/agents.md) for the cause of each code.

| Status | Codes |
|---|---|
| `400` | `invalid_query` (REST door) |
| `401` | `unauthorized` |
| `403` | `forbidden`, `agent_blocked`, `agent_mismatch` |
| `404` | `not_found` |
| `409` | `idempotency_conflict`, `no_device`, `chain_full`, `ask_back_not_allowed`, `stale_thread`, `item_closed`, `too_many_turns` |
| `410` | `item_abandoned` |
| `422` | `invalid_argument`, `invalid_flags`, `invalid_command_chars`, `command_too_large`, `body_too_large`, `text_too_long`, `lease_required`, `invalid_lease`, `explanation_required`, `explanation_too_long`, `interaction_not_allowed`, `use_service_tool`, `unsupported`, `agent_unknown`, `agent_token_required`, `agent_token_conflict`, `agent_token_in_text`, `invalid_agent_label` |
| `429` | `rate_limited`, `too_fast` |
| `503` | `directory_not_ready`, `rights_unavailable`, `storage_degraded`, `replication_unavailable`, `unavailable` |

### Errors of the approval API

The body is `{"error": "<code>"}`. The codes `401` and `403` have no `retry_after`.

| Status | Code | Cause |
|---|---|---|
| `401` | `unauthorized` | No valid token. `WWW-Authenticate: Bearer realm="portal-pdp"`. |
| `403` | `forbidden` | The file has no block `inbox.approvals`. |
| `404` | `not_found` | Unknown request, request of another service, or request of another subject. |
| `405` | `method_not_allowed` | Wrong method. The header `Allow` gives the right one. |
| `409` | `no_device` | The owner has no phone and `web_approval` is `false`. |
| `409` | `cannot_supersede` | The request to replace is consumed. |
| `413` | `command_too_large` | `op` and `view` are larger than `inbox.approvals.max_command_bytes`. |
| `415` | `unsupported_media_type` | The body is not `application/json`. |
| `422` | `invalid_argument` | A bad member, a bad `approval_ref`, or an unknown request to replace. |
| `422` | `invalid_lease`, `lease_required` | The wait is absent, given twice, or out of range. |
| `422` | `invalid_op`, `invalid_view`, `effect_mismatch`, `invalid_evaluation`, `explanation_required` | A mistake of the service. |
| `429` | `rate_limited` | A limit, a mute or a deny time. `retry_after` gives the seconds. |
| `503` | `storage_degraded`, `unavailable` | The Inbox cannot write. |

### Reasons of a refused decision

A `false` answer of the PDP has `context.reason` in these cases only. No other refusal has a reason, so an answer never shows a grant.

| Reason | Case |
|---|---|
| `unknown_subject` | The person or the key does not exist, or the key is revoked, expired or too old. The owner of the key cannot use keys. The subject has another type or an unknown prefix. |
| `invalid_resource` | The resource is not a correct node of the service. |
| `invalid_request` | An item of a batch has a bad `context.match` or bad ancestors. |
| `budget_exceeded` | A budget has no room. The answer has `retry_after`. |
| `approval_required` | The grant needs an approval. The answer has `evaluation_id`. |
| `approval_pending` | An approval waits for the owner. |
| `approval_denied` | The owner denied the approval. |
| `approval_expired` | The approval is no longer valid. |

### Close codes of the phone

A WebSocket of a phone closes with one of these codes. See [Phone protocol](inbox/protocol.md).

| Code | Name | Description |
|---|---|---|
| `4401` | unknown device | The device id is unknown, or its key does not match. Enroll again. |
| `4403` | revoked | The phone was revoked. Enroll again. |
| `4409` | replaced | A newer session of the same phone replaced it. |
| `4410` | enrollment refused | The code is unknown, used or expired, or the account is revoked. |
| `4426` | protocol | The protocol version is not compatible. Update the app. |
| `4429` | rate limited | Too many failed attempts from this address. |

## Cookies and response headers

| Name | Value | Attributes |
|---|---|---|
| Session cookie | `session.cookie_name` (default `__Host-portal`) over HTTPS. The name without `__Host-` over plain HTTP. | `Path=/`, `HttpOnly`, `SameSite` from `session.same_site` (default `Strict`), `Secure` over HTTPS. `Max-Age` only for a remembered session. |
| Sign-in cookie | `__Host-lc` over HTTPS, `lc` over plain HTTP. | The same attributes, with `Max-Age=43200` (12 hours). |

The portal decides that a request is HTTPS only when it has exactly one `X-Forwarded-Proto` header with the value `https`. The same headers, with `X-Forwarded-Host`, give the expected origin of a form. Without both headers, the portal uses `http` and the `Host` header.

The pages, the redirects and the JSON of the portal listener have these headers. The answers of the Inbox API, of the phones and of the agent channel set `Cache-Control: no-store` (JSON) and no other security header.

| Header | Value |
|---|---|
| `Cache-Control` | `no-store, max-age=0`, with `Pragma: no-cache`, for pages and forms. Assets have `public`; see "Assets and icons". |
| `Content-Security-Policy` | `default-src 'none'; style-src 'self'; script-src 'self'; img-src 'self' data:; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'` |
| `X-Content-Type-Options` | `nosniff` |
| `X-Frame-Options` | `DENY` |
| `Referrer-Policy` | `same-origin` |

An icon (`/auth/icons/`, `/auth/ext-icon/`) has the policy `default-src 'none'; style-src 'unsafe-inline'; sandbox`. The metrics answer has `Cache-Control: no-store, max-age=0` and `X-Content-Type-Options: nosniff`.

## Command line

The list of the commands and their use is in [Operations](operations.md#native-commands). This section gives the rules of the options and of the output. The source is `src/main.rs`.

The command `print-config`, the short option `-c` and the form `--NAME=VALUE` come after version 5.6.0. Version 5.6.0 accepts only `--NAME VALUE`, in the order of the usage.

### Options

The commands that take `--config`, outside `portal inbox`, read their options with one parser. These commands are `run`, `check-config`, `print-config`, `identity-public-key`, `healthcheck`, `end-sessions`, `revoke-keys`, `reset-two-factor` and `import-sqlite`. These rules apply:

- The order of the options is free.
- An option comes one time at most. A repeated option is an error.
- `-c FILE` is the short form of `--config FILE`.
- An option with a value also accepts the form `--NAME=VALUE`, for example `--config=FILE`.
- In the form `--NAME VALUE`, the value is the next argument as it is, also when it starts with `-`.
- These are errors: an unknown option, an option without its value, an empty value of `--config`, and a free argument.

The other commands outside `portal inbox` do not use this parser. `hash-password`, `version` and `help` do not read the arguments that follow the command. `render-demo` takes free arguments. `config-reference` refuses each argument.

These four lines are equal:

```sh
portal revoke-keys --config /etc/portal/config.hcl --user alice
portal revoke-keys --config=/etc/portal/config.hcl --user=alice
portal revoke-keys --user alice -c /etc/portal/config.hcl
portal revoke-keys -c /etc/portal/config.hcl --user alice
```

The commands under `portal inbox` accept only the form `--NAME VALUE`. The order of their options is also free, and each option comes one time at most.

### Effective configuration

The commands `check-config` and `print-config` make the same checks. They read the file, the variables and the files that it names. They check these secrets: the session secret, the login and the password of the superadmin, the identity key and the secrets of the inbox. They do not start the server.

The two commands print one line for each key that applies, in the form `key = value  # source`. A secret shows `<hidden>`. See [Configuration](configuration.md#effective-configuration).

| Command | Lines after the lines of the keys |
|---|---|
| `portal check-config --config FILE` | The line `identity.key_id` when the file has the block `identity`. Then the line `configuration ok`. |
| `portal print-config --config FILE` | No line. Use this command in a script that reads the values. |

### Help

The commands `portal help`, `portal --help` and `portal -h` print the usage. The program without a command also prints the usage. The text ends with the exit status.

## Exit codes

The program exits with `0` when the command succeeds. It exits with `1` for any error and writes `portal: <message>` to the standard error. An error of the command line also gives `1`. A configuration error stops the program before it serves. `portal healthcheck` exits with `0` only when each answer has the status `200`. It asks `/healthz`, and also `/readyz` when the file has the block `inbox`.
