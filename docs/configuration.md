# Configuration

The portal reads one HCL file. The file is the only source of the configuration, and the program checks it before it serves. This page gives the rules of the file, the secrets, a complete example and the generated reference of each key.

## The file

The program reads one file in a small, strict subset of HCL. A block holds lines of the form `key = value`. A block has a name (`server { ... }`) and can have a label (`user "alice" { ... }`). A comment starts with `#`.

A value is a number, a boolean, a quoted text, a list of quoted texts (`["a", "b"]`) or, for a grant, an object. The program reads an environment variable or a file only when the file names it.

Some blocks hold other blocks. The block `inbox` holds the blocks `devices`, `enrollment`, `storage`, `replica`, `cluster`, `watch`, `items`, `at_rest`, `approvals`, `agents`, `questions` and `mcp`. It also holds the blocks `group "<id>"` and `api_key "<name>"`. The block `inbox.items` holds `lease` and `digest`. The block `inbox.at_rest` holds `key "<id>"`.

The block `user "<login>"` holds `key "<name>"`. The [reference](#configuration-reference) lists each block.

The rules of the Inbox that the table of keys cannot show are in [Inbox: configuration rules](inbox/configuration.md).

## Rules

- **The schema is closed.** An unknown block, an unknown key, a duplicate block or a duplicate key stops the program at start.
- **An error stops the program before it serves.** The message gives the key, the values that the key accepts, and the value that the file gave. The message never shows a secret.
- **The program does not change a value.** It does not clamp a number. It does not trim a text. It does not change the case of a text.
- **The types are strict.** Write a number without quotes (`workers = 4`). Write a boolean without quotes, in lower case (`true` or `false`). Write every other value in quotes. A number in quotes or a word without quotes stops the program.
- **A unit is mandatory.** A duration ends with `ms`, `s`, `m`, `h` or `d`. A size ends with `B`, `KiB` or `MiB`.
- **A name of a choice has the exact case of the reference.** `Strict` is valid and `strict` is not. `warning` is not a log level.
- **A key that does not apply must not be in the file.** When `security.login_rate_limit_enabled` is `false`, the keys `login_attempts_per_ip`, `login_window` and `login_lockout` stop the program. The same rule applies to the global limiter. It also applies to `metrics.path` when `metrics.enabled` is `false`. It also applies to `storage.database` with the `postgres` block.
- **Two keys that contradict each other stop the program.** For example, `session.idle_timeout` is longer than `session.ttl`, or `session.remember_ttl` is shorter than `session.ttl`. A key of `keys.mcp = true` without the blocks `identity` and `pdp` is another example.
- **The program parses the file one time.** It builds a typed structure at start. The rest of the code reads only this structure.

## Secrets: `_env` and `_file`

These attributes hold a secret:

- `authentication.superadmin_login`, `authentication.superadmin_password` and `authentication.totp_key`
- `session.secret`
- `postgres.password`
- `identity.signing_key`
- `inbox.server_key`
- the `secret` of each `api_key` block and of each `inbox.at_rest.key` block
- the `token` of each `pdp_client` block
- the `password_hash` of each `user` block
- the `key` and the `key_sha256` of each `key` block

The file never holds the value. Give exactly one of these two forms:

| Form | The value is |
|---|---|
| `<name>_env = "VAR"` | the content of the environment variable `VAR` |
| `<name>_file = "/path"` | the content of the file, without its last line end (`\n` or `\r\n`) |

The program stops at start in these cases:

- The file gives none of the forms, or both of them.
- The file gives the secret in clear text (`<name> = "..."`).
- The variable does not exist, or its value is empty.
- The file does not exist, cannot be read, or is empty. The path is not absolute, or it holds `..`.
- The value is longer than the limit of the key.

The error names the attribute, and the variable or the path. It never shows the value. The reference tells which attribute accepts `_env` and `_file`. Only these attributes accept them.

## Kubernetes API

The Kubernetes source needs the host and the port of the API of the cluster. The configuration names the two environment variables that hold them. The program reads no other variable. At start, both variables must exist.

```hcl
discovery {
  kubernetes          = true
  kubernetes_host_env = "KUBERNETES_SERVICE_HOST"
  kubernetes_port_env = "KUBERNETES_SERVICE_PORT_HTTPS"
}
```

The two `_env` keys are mandatory when `kubernetes` is `true`. They are not valid when `kubernetes` is `false`.

## Effective configuration

The command `portal check-config --config FILE` checks the file. It checks the variables and the files that the file names. It also checks that the identity key and the keys of the Inbox decode. Then it prints the value that applies to each key, with its source. It ends with `configuration ok`.

```text
server.workers = 4  # default
server.max_body_bytes = "8KiB"  # file
authentication.superadmin_login = <hidden>  # env:PORTAL_SUPERADMIN_LOGIN
session.secret = <hidden>  # file:/run/secrets/portal_session_secret
```

The source is `default`, `file`, `env:VAR` or `file:/path`. A secret always shows `<hidden>`. The superadmin login also shows `<hidden>`. The command prints only the keys that apply. With the block `identity`, it adds a line `identity.key_id` that the program derives from `identity.signing_key`.

The server writes the same values to its log at start. The event name is `config`. The server writes one line for each key, with the fields `key`, `value` and `source`.

## Complete example

This file uses the files of a container: secrets in `/run/secrets`, data in `/var/lib/portal`. The values are examples. The command `portal check-config` accepts the file when the secret files exist.

```hcl
server {
  listen_address     = "0.0.0.0:4181"
  name               = "Portal"                       # default; the app shows it as the name of the server
  workers            = 4
  max_body_bytes     = "32KiB"
  max_connections    = 1024
  max_headers        = 64
  max_header_bytes   = "32KiB"
  header_read_timeout = "10s"
  body_read_timeout   = "10s"
  max_login_body_reads = 64
  # The pod network of the cluster, where Traefik runs. [] trusts no proxy.
  trusted_proxies     = ["10.42.0.0/16"]
}

authentication {
  superadmin_login_file    = "/run/secrets/portal_superadmin_login"
  superadmin_password_file = "/run/secrets/portal_superadmin_password"
  # The key of the TOTP secrets (64 hexadecimal characters). The policy `authentication.mfa` is "optional" by default.
  totp_key_file            = "/run/secrets/portal_totp_key"
}

users {
  max_users      = 64
  allow_creation = true
}

access {
  grants_mode = "dynamic"
}

# An external service: a tile that opens a link. href is mandatory. The description and the icon are optional.
# The icon is a data URI (icon) or the path of an image file (icon_path), never both.
external_service "Team wiki" {
  href        = "https://wiki.example.org/"
  description = "The wiki of the team"
  # icon_path = "/run/secrets/wiki_icon.svg"
}

identity {
  issuer           = "https://portal.example.org"
  signing_key_file = "/run/secrets/portal_identity_key"
}

pdp {
  listen_address = "0.0.0.0:4182"
  url            = "http://portal:4182"
  # The link to the PDP is plain HTTP on a private network. The operator states that other means secure it.
  plain_http     = true
}

storage {
  database       = "/var/lib/portal/portal.db"
  retention_days = 90
}

session {
  secret_mode         = "ephemeral"
  cookie_name         = "__Host-portal"
  ttl                 = "24h"
  same_site           = "Strict"
  max_active_sessions = 4096
}

security {
  max_password_bytes      = 1024
  max_forwarded_uri_bytes = 4096
  max_cookie_header_bytes = 8192

  login_rate_limit_enabled = true
  login_attempts_per_ip    = 5
  login_window             = "1m"
  login_lockout            = "5m"

  global_login_rate_limit_enabled = true
  global_login_attempts            = 100
  global_login_window              = "1m"
  global_login_lockout             = "30s"

  max_rate_limit_entries         = 4096
  max_concurrent_password_checks = 1
}

metrics {
  enabled = true
  path    = "/metrics"
}

logging {
  level = "info"
}
```


The directory `deploy/` has complete files for Docker Compose and for K3s: see [deploy/README.md](../deploy/README.md). The file [deploy/config.example.hcl](../deploy/config.example.hcl) has the keys of the Inbox.

## Configuration reference

This reference comes from the table of keys in [`src/config/keys.rs`](../src/config/keys.rs). Run `portal config-reference` to generate it again. A test fails when this text is not the generated text. Do not edit the text between the markers by hand.

<!-- config-reference:begin -->
The column Forms gives the forms that a key accepts. `V`: the file gives the value, as `<name> = value`. `E`: the file gives `<name>_env`, the name of an environment variable. `F`: the file gives `<name>_file`, the absolute path of a file. A secret accepts only `E` and `F`, and never the two forms for one key. A required secret needs one of the two forms.

### Block `server`

The HTTP server.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `listen_address` | text: an IP address and a port, such as 0.0.0.0:4181 | required | V | The address and the port that the server listens on. |
| `name` | text: 1 to 64 bytes, not blank, no control character | `"Portal"` | V | The name of this instance. The phones show it as the name of the server, and the welcome message uses it. The browser tab shows it at the end of the title of the pages after sign-in. No page body shows it. |
| `workers` | number, 1 to 64 | `4` | V | The number of worker threads. |
| `max_body_bytes` | size, 1KiB to 1MiB | `"32KiB"` | V | The largest request body that the server reads. The form of a user holds one field for each grant that it shows. |
| `max_connections` | number, 1 to 100000 | `1024` | V | The largest number of open TCP connections of the pages and the API. A connection that upgrades to a phone session gives its permit back and takes a permit of the session budget: `server.max_connections` plus two times `inbox.devices.max_devices`. This budget exists only with the block `inbox`. |
| `max_headers` | number, 8 to 256 | `64` | V | The largest number of headers in a request. |
| `max_header_bytes` | size, 8KiB to 1MiB | `"32KiB"` | V | The largest size of the headers of a request. |
| `header_read_timeout` | duration, 1s to 1m | `"10s"` | V | The time that a client has to send the request headers. On the PDP listener, it is also the longest time that a connection stays open with no request. The metadata publishes it as `procurauth_idle_max_seconds`, and a service closes an idle connection before this time. |
| `body_read_timeout` | duration, 1s to 1m | `"10s"` | V | The time that a client has to send the request body. A sign-in must finish its Argon2 check within this time, or the server answers `503`. |
| `max_login_body_reads` | number, 1 to 100000 | `64` | V | The largest number of sign-in requests that the server reads, or that wait for their Argon2 check, at the same time. The server answers `503` at once to a sign-in request above this number. Slow sign-in bodies and a queue of sign-ins thus cannot hold all the connections that ForwardAuth needs. It must be less than `server.max_connections`. |
| `trusted_proxies` | list of at most 64 quoted strings: IP networks such as "10.42.0.0/16" or "127.0.0.1/32"; the address is the first address of the network | required | V | The networks of the proxies (Traefik) that set `X-Forwarded-For`. The server reads this header only from a TCP peer in these networks. It then takes the first address from the right that is not in these networks. From any other peer, the client address is the address of the peer. `[]` trusts no proxy. The session binding, the login limiter and the limiter of the enrollment attempts of the phones use this client address. |

### Block `authentication`

The superadmin: the one account that is not in the database. The second factor of the sign-in.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `superadmin_login` | secret, at most 256 bytes | required | E, F | The login of the superadmin. It has 1 to 64 characters, and no upper-case letter, no space, no control character and no colon. The portal keeps the colon for the ids of keys. The server does not change it. A new login ends the sessions of the superadmin and the impersonated sessions. An account of the database or a block `user` with this login stops the start. |
| `superadmin_password` | secret, at most 16384 bytes | required | E, F | The password of the superadmin. At start, the server hashes it with Argon2id and erases it from memory. It has at most `security.max_password_bytes` bytes. The server refuses the example values of `.env.example` and of the documentation. A new password ends the sessions of the superadmin and the impersonated sessions. |
| `mfa` | `off`, `optional`, `required` | `"optional"` | V | The second factor of the sign-in: a TOTP code from an authenticator app (RFC 6238). `off`: the portal never asks for a code. `optional`: each account chooses to enable it, and an account without it authenticates with the password only, so scripts that authenticate need no change. `required`: each account must enrol after the password step. The superadmin has no separate switch: each value applies to it as to the other accounts. A value other than `off` needs `authentication.totp_key_env` or `authentication.totp_key_file`. |
| `totp_key` | secret, at most 128 bytes | required | E, F | The key that encrypts the TOTP secrets in the database (AES-256-GCM). All instances use the same key of 64 hexadecimal characters (32 random bytes), made with `openssl rand -hex 32`. At start, a key that does not match the check value in the database stops the server, and no secret changes. The server never replaces a secret under another key. To use a new key, run `portal reset-two-factor --all` first. Applies only when `authentication.mfa` is `optional` or `required`. |

### Block `users`

The accounts in the database.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `max_users` | number, 1 to 1024 | `64` | V | The largest number of accounts in the database. |
| `allow_creation` | `true` or `false` | `true` | V | `true`: the superadmin can make accounts in the database. `false`: the route that makes an account answers `404`, and the pages show no button and no entry that makes an account. The accounts that exist stay editable, as `access.grants_mode` says. The value `true` contradicts `access.grants_mode = "static"`: in this mode the database holds no account. |
| `max_grants` | number, 1 to 30000 | `512` | V | The largest number of grants of one user. A key holds at most 512 grants, at any value of this key. For example, a user has 3 grants for each service and the 8 actions of the service `portal`. This user needs 3 x the number of services + 8 grants. |

### Block `access`

The grants of the users, and the actions that the services publish.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `grants_mode` | `static`, `dynamic`, `hybrid` | `"dynamic"` | V | Where the grants come from. `static`: only the `user` blocks; the settings pages show the grants and do not change them. `dynamic`: only the settings pages; a `user` block stops the server. `hybrid`: both; the grants of a `user` block are read-only in the settings pages. |
| `actions_timeout` | duration, 100ms to 30s | `"2s"` | V | The time that the server waits for the `/.well-known/procurauth/actions` document of one service. |
| `actions_refresh` | duration, 10s to 1d | `"5m"` | V | The time between two reads of the `/.well-known/procurauth/actions` documents. It is also the shortest time between two early reads of the document of one service, after an `invalid_resource` of the PDP. |
| `actions_max_age` | duration, 1m to 7d | `"1h"` | V | The longest time that the server keeps the last good document of a service after its last good read. After this time, the server acts as if the service had no good document (protocol 5.3a, 11.1). The PDP then answers `false` with no reason for each question of the service, except `access` on the whole service. The edge never refuses a person or a key because of the age of the document (protocol 8.3). `access` stays `true`, and the service then gets `false` for each other question. The value must be more than `access.actions_refresh`. |
| `max_actions` | number, 64 to 4096 | `1024` | V | The largest number of actions in the document of one service. A larger document is bad. Protocol 5.3c asks a reader to accept 64 at least. |
| `max_resources` | number, 256 to 65536 | `4096` | V | The largest number of resources in the document of one service. A larger document is bad. Protocol 5.3c asks a reader to accept 256 at least. |
| `max_document_bytes` | number, 65536 to 4194304 | `262144` | V | The largest size, in bytes, of the document of one service. The server stops the read at this size, and the document is bad. Protocol 5.3c asks a reader to accept 64 KiB at least. The server keeps the last good document of each service in memory. |
| `start_read_deadline` | duration, 1s to 5m | `"5s"` | V | The longest time that the server waits, at start, for the first read of the documents of the services. After this time, the server opens its ports and reads the other documents in the background. |

### Block `identity`

This block is optional. If it is present, the server gives an identity token to each service that it lets a user open.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `header` | text: an HTTP header name of 1 to 64 characters: letters, digits and '-' | `"X-Identity-Token"` | V | The response header of `/auth/verify` that holds the identity token. Traefik copies it to the request with `authResponseHeaders`. |
| `issuer` | text: 1 to 256 visible ASCII characters, for example https://portal.example.internal | required | V | The claim `iss` of each identity token. Each service compares it with its `identity.issuer`. |
| `token_ttl` | duration, 10s to 1h | `"5m"` | V | The life of an identity token (`exp - iat`). It must not be more than the `identity.max_age` of the services. The server gives the same token again until half of its life is gone. |
| `signing_key` | secret, at most 4096 bytes | required | E, F | The Ed25519 private key that signs the identity tokens (algorithm `Ed25519`). The value is a PKCS#8 key in PEM, or the base64 text of the PEM on one line. `portal identity-public-key` prints the public key for the services, as a JWK Set. |

### Block `pdp`

This block is optional. If it is present, the server is an AuthZEN PDP on a second, internal listener.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `listen_address` | text: an IP address and a port, such as 0.0.0.0:4182 | required | V | The address and the port of the PDP listener. Do not expose it through Traefik: only the services use it. |
| `url` | text: an http or https URL without a path, a query or a fragment | required | V | The URL at which the services reach the PDP. `/.well-known/authzen-configuration` gives the endpoints under this URL. |
| `max_body_bytes` | size, 1KiB to 1MiB | `"64KiB"` | V | The largest request body that the PDP reads. The metadata publishes it as `procurauth_evaluations_max_bytes`. With the approval API, the body of a submission holds the command and some more members. This value is then at least `inbox.approvals.max_command_bytes` and 2 KiB more. |
| `max_evaluations` | number, 1 to 1024 | `128` | V | The largest number of evaluations in one request to `/access/v1/evaluations`. The metadata publishes it as `procurauth_evaluations_max_items`, and a service splits its questions to fit. ClickHouse Dash sends up to 100 questions in one request, so keep the value at 100 or more. |
| `plain_http` | `true` or `false` | `false` | V | Set it to `true` to accept an `http` value of `pdp.url`. Then the operator states that other means secure the link to the PDP. With `false`, an `http` URL stops the server. With `true`, an `https` URL also stops the server. With `true`, the start log has a warning. |
| `counter_connections` | number, 1 to 64 | `2` | V | The number of database connections that count the uses of the budgets. Each connection counts approximately 1,000 uses each second. A use that waits more than 500 ms gets the HTTP status 503. |

### Block `keys`

This block is optional. If it is present, the server accepts keys: a user makes keys in the tab Access. A key has one of two kinds. The kind `mcp` is for an agent: the key edge and the PDP verify it. The kind `api` is for a service: the API of the inbox verifies it.

The keys `mcp` and `api` enable or disable each kind. The kind `mcp` needs the blocks `identity` and `pdp`. The kind `api` needs the block `inbox`. At least one kind must be on.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `mcp` | `true` or `false` | `true` | V | `true`: the kind `mcp` is on. A user can make MCP keys, with the prefix `mcp_`, and the key edge and the PDP accept them. `false`: the kind is off. The key edge refuses these keys, and the pages show no MCP key, no MCP right and no MCP chip. The value `true` needs the blocks `identity` and `pdp`. |
| `api` | `true` or `false` | `false` | V | `true`: the kind `api` is on. A user can make API keys, with the prefix `api_`, and the API of the inbox accepts them. `false`: the kind is off. The API of the inbox refuses these keys, and the pages show no API key, no API right and no API chip. The value `true` needs the block `inbox`. A block `api_key` needs the value `true`. |
| `default_lifetime` | duration, 1d to 365d | `"30d"` | V | The lifetime that the form of a new key proposes. The form uses the lower of this value and the longest lifetime of the owner (`max_key_lifetime`). |
| `directory_max_age` | duration, 15s to 5m | `"30s"` | V | The longest age of the directory of the users and the keys. When the last good read of the database is older, the PDP answers `false` for each key and the edge refuses each key. The directory is read again every 5 seconds, so the minimum is 3 rounds. With PostgreSQL, the metadata of the PDP gives this value in whole seconds as `procurauth_directory_max_age_seconds` (30 without the block `keys`). With SQLite, the PDP has no copy of the directory, and the metadata gives 1. |
| `max_per_user` | number, 1 to 10000 | `100` | V | The largest number of active keys of one user. An expired key and a key that is too old do not count. |
| `max_counter_rows` | number, 1 to 100000000 | `2000000` | V | The largest number of counter rows. The server refuses a write that makes more rows. A file that needs more rows stops the server at start. One row is approximately 300 bytes. |
| `max_directory_grants` | number, 1 to 100000000 | `4000000` | V | The largest number of grants of the directory: the grants of the users and the keys. The server refuses a write that makes more grants. A file that needs more grants stops the server at start. One grant takes approximately 200 bytes of memory. |
| `history_days` | number, 1 to 3650 | `90` | V | The number of days that the server keeps the summary row of a revoked key. |

### Block `storage`

The history, and the SQLite file.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `database` | text: an absolute path without '..' | `"/var/lib/portal/portal.db"` | V | The path of the SQLite file. Applies only without the `postgres` block. |
| `retention_days` | number, 1 to 3650 | `90` | V | The number of days that the history keeps the sessions, the service accesses, the audit events and the sign-in attempts. |

### Block `postgres`

This block is optional. If it is present, the server uses a shared PostgreSQL database in place of the SQLite file.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `host` | text: a host name or an address of at most 253 characters | required | V | The host name or the address of the PostgreSQL server. |
| `port` | number, 1 to 65535 | `5432` | V | The TCP port of the PostgreSQL server. |
| `database` | text: 1 to 63 letters, digits, '.', '-' or '_' | required | V | The name of the database. |
| `user` | text: 1 to 63 letters, digits, '.', '-' or '_' | required | V | The name of the database user. |
| `password` | secret, at most 1024 bytes | required | E, F | The password of the database user. It is valid UTF-8. |
| `pool_size` | number, 1 to 64 | `4` | V | The number of connections that the server keeps to the database. |
| `connect_timeout` | duration, 1s to 1m | `"5s"` | V | The time that the server waits for a new connection. |

### Block `session`

The sessions and the session key.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `cookie_name` | text: a cookie name that starts with __Host- | `"__Host-portal"` | V | The name of the session cookie over HTTPS. Over plain HTTP the server uses the same name without the prefix, because browsers refuse `__Host-` cookies there. |
| `ttl` | duration, 1m to 7d | `"24h"` | V | The lifetime of a session from the sign-in. The session ends at this time, even when it is in use. The cookie of this session is a browser-session cookie. |
| `idle_timeout` | duration, 1m to 7d | `"30m"` | V | A session that nobody uses for this time ends. The value is at most `session.ttl`. A remembered session uses `session.remember_idle_timeout`. |
| `remember` | `true` or `false` | `true` | V | The sign-in form shows a box "Remember me". A person who ticks it gets a remembered session with a persistent cookie. With `false`, the form has no box and the server ignores the field. |
| `remember_ttl` | duration, 1m to 90d | `"30d"` | V | The lifetime of a remembered session from the sign-in. The cookie has the same life. The value is at least `session.ttl`. An impersonated session is never remembered. Applies only when `session.remember` is `true`. |
| `remember_idle_timeout` | duration, 1m to 90d | `"7d"` | V | A remembered session that nobody uses for this time ends. The value is at least `session.idle_timeout` and at most `session.remember_ttl`. Applies only when `session.remember` is `true`. |
| `bind` | `ip_user_agent`, `user_agent`, `none` | `"ip_user_agent"` | V | What a session cookie is tied to: the client address and the User-Agent, the User-Agent only, or nothing. |
| `same_site` | `Strict`, `Lax` | `"Strict"` | V | The SameSite attribute of the session cookie. |
| `secret_mode` | `ephemeral`, `provided` | `"ephemeral"` | V | `ephemeral`: the server makes a new random key at each start. `provided`: the key comes from `session.secret_env` or `session.secret_file`. The `postgres` block needs `provided`, because all instances use the same key. |
| `secret` | secret, at most 4096 bytes | required | E, F | The key that signs the login forms, the return links and the anti-CSRF tokens. It has at least 32 bytes. The server refuses the example value of `.env.example`. A new value ends the sessions of the superadmin and the impersonated sessions. Applies only when `session.secret_mode` is `provided`. |
| `max_active_sessions` | number, 1 to 1000000 | `4096` | V | The largest number of open sessions. A login above this number gets the status 503. |
| `max_sessions_per_account` | number, 1 to 1000000 | `16` | V | The largest number of open sessions of one account, the superadmin included. A new sign-in of an account at this number ends the session of that account that was used least recently. Thus one account cannot fill `session.max_active_sessions`. An impersonated session does not count. The value is at most `session.max_active_sessions`. |

### Block `security`

The limits and the login limiters.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `max_password_bytes` | number, 8 to 16384 | `1024` | V | The largest size of a password. |
| `max_forwarded_uri_bytes` | number, 128 to 65536 | `4096` | V | The largest size of the forwarded URI. |
| `max_cookie_header_bytes` | number, 256 to 65536 | `8192` | V | The largest size of the Cookie header. |
| `login_rate_limit_enabled` | `true` or `false` | `true` | V | Turn the limiter of failed logins for each client address on or off. |
| `login_attempts_per_ip` | number, 1 to 1000000 | `5` | V | The number of failed logins for each client address in one window before the lockout. Applies only when `security.login_rate_limit_enabled` is `true`. |
| `login_window` | duration, 1s to 1d | `"1m"` | V | The window in which the failed logins of one client address count. Applies only when `security.login_rate_limit_enabled` is `true`. |
| `login_lockout` | duration, 1s to 1d | `"5m"` | V | The time that a client address stays locked out. Applies only when `security.login_rate_limit_enabled` is `true`. |
| `global_login_rate_limit_enabled` | `true` or `false` | `true` | V | Turn the global limiter of failed logins on or off. A good login does not clear it. |
| `global_login_attempts` | number, 1 to 1000000 | `100` | V | The number of failed logins from all clients in one window before the lockout. The superadmin login has a separate budget of the same size. Failures against other logins never lock out the superadmin. Applies only when `security.global_login_rate_limit_enabled` is `true`. |
| `global_login_window` | duration, 1s to 1d | `"1m"` | V | The window in which the failed logins of all clients count. Applies only when `security.global_login_rate_limit_enabled` is `true`. |
| `global_login_lockout` | duration, 1s to 1d | `"30s"` | V | The time that the logins of the budget stay locked out: all other logins, or the superadmin login. Applies only when `security.global_login_rate_limit_enabled` is `true`. |
| `max_rate_limit_entries` | number, 64 to 1000000 | `4096` | V | The largest number of client addresses that the limiter remembers. Applies only when at least one login limiter is on. |
| `max_concurrent_password_checks` | number, 1 to 64 | `1` | V | The largest number of Argon2 checks that run at the same time. |

### Block `metrics`

The Prometheus endpoint.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `enabled` | `true` or `false` | `true` | V | Turn the Prometheus endpoint on or off. |
| `path` | text: an absolute URL path of at most 128 bytes, not /healthz, /readyz or /api/version, and not under /auth/, /v1/ or /api/v1/ | `"/metrics"` | V | The URL path of the Prometheus endpoint. Applies only when `metrics.enabled` is `true`. |

### Block `discovery`

The sources of the service catalogue.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `interval` | duration, 2s to 1h | `"15s"` | V | The time between two reads of the sources. |
| `probe_timeout` | duration, 200ms to 30s | `"3s"` | V | The time that a health probe and a source request wait for an answer. |
| `kubernetes` | `true` or `false` | `false` | V | Read the IngressRoute annotations through the API of the cluster. The server needs the ServiceAccount of the pod. |
| `kubernetes_host_env` | text: the name of an environment variable | required | V | The environment variable that holds the host of the API of the cluster, for example `KUBERNETES_SERVICE_HOST`. At start, the variable must exist and must not be empty. Applies only when `discovery.kubernetes` is `true`. |
| `kubernetes_port_env` | text: the name of an environment variable | required | V | The environment variable that holds the HTTPS port of the API of the cluster, for example `KUBERNETES_SERVICE_PORT_HTTPS`. At start, the variable must exist and must hold a port number. Applies only when `discovery.kubernetes` is `true`. |
| `docker_socket` | text: an absolute path without '..' | none | V | The Docker socket that the server reads the container labels from. Without this key, the Docker source is off. |

### Block `ui`

The pages.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `icons_dir` | text: an absolute path without '..' | `"/icons"` | V | The directory of the offline icon library. If the directory does not exist, the pages show initials. |
| `favicon` | `default`, `none` | `"default"` | V | The icon of the browser tab. With `default`, the tab shows the built-in logo on the pages after sign-in only, and the server sends the icon to a signed-in browser only. With `none`, no page after sign-in links an icon, and the icon route gives the answer of an unknown path. A page before sign-in never has an icon: it has the empty icon. |
| `services_new_tab` | `true` or `false` | `true` | V | `true`: a tile of the launcher opens its service in a new browser tab. `false`: the tile opens the service in the same tab. The value applies to every service. A service cannot choose. |
| `max_personal_links` | number, 1 to 256 | `32` | V | The largest number of personal links that one account can hold. A personal link is a tile that an account adds for itself in the page Links, with the action `links.own.edit`. Only its owner sees it. An account that holds this number gets a refusal for a new link. |

### Block `logging`

The log.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `level` | `error`, `warn`, `info`, `debug` | `"info"` | V | The lowest level that the server logs. |

### Block `inbox`

This block is optional. If it is present, this server runs the inbox: it sends alerts to phones. The inbox uses the database of the portal.

The blocks `inbox.devices` and `inbox.storage` are mandatory inside this block. The blocks `inbox.replica`, `inbox.cluster`, `inbox.watch`, `inbox.items` and `inbox.at_rest` are optional. The block `inbox.items` has the blocks `lease` and `digest`. The block `inbox.at_rest` has the blocks `key "<id>"`. The blocks `group "<id>"` and `api_key "<name>"` are inside this block.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `server_key` | secret, at most 4096 bytes | required | E, F | The key of the server for the phones. Make it one time with `portal inbox keygen`. A new key makes each enrolled phone and each enrollment code invalid. Keep a copy of it. |

### Block `inbox.devices`

This block is inside the block `inbox`. It sets the public address that phones use, the limits of the phones, and the delivery values. The phones use the listener of the portal, as the senders do. The routes of the phones have no session: phones prove themselves.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `public_address` | text: the public base URL of the portal as the phones reach it: `host:port`, a domain, or an http(s) URL with an optional path prefix; 1 to 512 bytes, no white space, no control character | required | V | The public base URL of the portal, as the phones reach it from outside. The value is `host:port`, a domain, or an http(s) URL `http(s)://host[:port][/prefix]`, for example `http://203.0.113.10:8088`. It is the `s` of the enrollment code, and the phones add `/v1/enroll` and `/v1/session` to it. The portal serves these routes on `server.listen_address`, with the pages. The portal has no base path, so a proxy that publishes it under a prefix must remove the prefix before it sends the request. The inbox does not open a connection to it. |
| `max_devices` | number, 1 to 100000 | `1000` | V | The largest number of enrolled phones that are not revoked. |
| `max_per_account` | number, 1 to 100000 | `10` | V | The largest number of enrolled phones of one account that are not revoked. The inbox refuses the enrollment of one more phone of the account, and the code stays unused. Thus one account cannot use all the places of `inbox.devices.max_devices`. The value is at most `inbox.devices.max_devices`. |
| `heartbeat_interval` | duration, 5s to 1h | required | V | The app sends a ping at this interval. |
| `session_timeout` | duration, 10s to 1d | required | V | A session without a frame for this time ends. The value is at least two times `inbox.devices.heartbeat_interval`. |
| `resend_after` | duration, 1s to 1d | required | V | The inbox sends an alert again to a phone that did not confirm the receipt after this time. |
| `max_inflight` | number, 1 to 1024 | required | V | The largest number of alerts that the inbox sends to one session before the phone confirms them. |
| `unreachable_after` | duration, 10s to 30d | required | V | The inbox reports a subscribed phone as unreachable when it did not see the phone for this time. The value is at least `inbox.devices.session_timeout`. |
| `offline_warning_after` | duration, 10s to 30d | required | V | The app warns its user after it was disconnected for this time. The value is at least two times `inbox.devices.heartbeat_interval`. |
| `max_auth_failures` | number, 1 to 1000 | `5` | V | The number of failed attempts of one client address in the window `inbox.devices.auth_failure_window`. Each started enrollment counts until it succeeds. A session handshake that proves no enrolled phone counts too, also when it ends at its time limit. At this number, the address cannot start an enrollment, and such a handshake gets the close code 4429. A phone that proves its key always gets its session. |
| `auth_failure_window` | duration, 1s to 1d | `"10m"` | V | The window of `inbox.devices.max_auth_failures`. |
| `max_handshakes_per_address` | number, 1 to 100000 | `16` | V | The largest number of phone WebSockets of one client address that did not finish the handshake. The routes `/v1/enroll` and `/v1/session` count together. A new WebSocket of an address at this number ends the oldest handshake of the address with the close code 1013. Thus one client holds at most this part of the session budget. The phones behind one NAT address share this number. |
| `max_request_bytes` | size, 4KiB to 1MiB | `"64KiB"` | V | The largest request body that the listener reads. |

### Block `inbox.enrollment`

This block is optional and is inside the block `inbox`. It sets the enrollment codes: an account of the portal makes a code, and a phone uses it one time to enroll. Every key has a default.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `code_ttl` | duration, 1m to 1d | `"10m"` | V | An enrollment code works for this time after an account made it. A code works one time. |
| `max_active_per_account` | number, 1 to 100 | `5` | V | The largest number of enrollment codes of one account that are not used and not expired. The inbox refuses a new code above this number. |

### Block `inbox.storage`

This block is inside the block `inbox`. It sets the lifetime of alerts and of the connection log. The inbox keeps its tables in the database of the portal: the SQLite file of the block `storage`, or the database of the block `postgres`.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `delivery_ttl` | duration, 1m to 3650d | required | V | The inbox does not send an alert again to a phone that did not receive it within this time. |
| `alert_retention` | duration, 1h to 3650d | required | V | The inbox deletes the alerts, delivered or expired, that are older than this. The value is at least `inbox.storage.delivery_ttl`. |
| `connection_log_retention` | duration, 1h to 3650d | required | V | The inbox deletes the connection records that are older than this. |
| `pool_size` | number, 2 to 64 | `8` | V | The number of connections that the inbox uses for requests and sessions. The inbox keeps one more connection for the notifications of the other instances. Applies only with the `postgres` block. |

### Block `inbox.replica`

This block is optional and is inside the block `inbox`. If it is present, the inbox copies the SQLite file to a directory that the standby instances share. A standby instance becomes the owner when the owner is lost. The block needs the SQLite file: it must not be present with the block `postgres`. The copy covers the whole file, with the tables of the portal.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `dir` | text: an absolute path without '..' | required | V | The directory that every instance that can run the inbox shares. It is a volume of plain files. It must not be the directory of `storage.database`. |
| `snapshot_interval` | duration, 1s to 1d | `"10s"` | V | The inbox copies a changed database at most this often. An accepted alert, an enrollment, a revocation and a form of the portal that changes the database do not wait for this time. |
| `lease_ttl` | duration, 5s to 1h | `"30s"` | V | The other instances consider an instance lost when its lease did not change for this time. One of them then becomes the owner. |
| `sync_timeout` | duration, 1s to 1m | `"5s"` | V | The time that an accepted alert, or the answer to a form of the portal, waits for the copy that holds the change. |
| `sync_alerts` | `true` or `false` | `true` | V | `true`: the inbox confirms an alert only after the copy holds it. If the copy fails, the inbox answers `503` and the sender sends the alert again. `false`: the inbox confirms an alert after the local commit and copies it as soon as possible. |

### Block `inbox.cluster`

This block is optional and is inside the block `inbox`. If it is present, the instances copy to each other the alerts that they accept while PostgreSQL has no writable primary. The block needs the block `postgres`.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `listen` | text: an IP address and a port, such as 0.0.0.0:8082 | required | V | The address and the port where this instance receives the requests of the other instances. Only the pods of the service can reach this port. |
| `peers` | list of at most 16 quoted strings: names with a port such as "inbox-peers.apps.svc.cluster.local:8082" | required | V | The names that list the instances, for example a headless Service, with the port of `inbox.cluster.listen`. The inbox asks each address that the names resolve to who it is. It skips its own address. The list has at least one name. |

### Block `inbox.watch`

This block is optional and is inside the block `inbox`. If it is present, the portal sends alerts about itself to one group. The alerts tell that a service of the launcher is unhealthy, or that sign-ins fail in a burst. Without this block, the portal sends no alert about itself.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `group` | text: the id of a block `group` of the block `inbox` | required | V | The group that receives the alerts of the portal. It must be the label of a block `group` of the block `inbox`. |
| `service_unhealthy` | `true` or `false` | `true` | V | `true`: the portal sends an alert when a service of the launcher stays unhealthy for `inbox.watch.unhealthy_after`. A service is unhealthy when its tile shows `down` or `unhealthy`. A service with no health check is never unhealthy. |
| `unhealthy_after` | duration, 10s to 1d | `"2m"` | V | How long a service stays unhealthy before the alert. A service that is healthy in this time, or that is in the state `starting`, does not raise the alert. |
| `sign_in_burst` | `true` or `false` | `true` | V | `true`: the portal sends an alert when `inbox.watch.sign_in_failures` sign-ins are refused in the window `inbox.watch.sign_in_window`. A wrong password, an unknown login and a sign-in that a limiter blocks count. Each instance counts the refusals that it sees. |
| `sign_in_failures` | number, 2 to 10000 | `10` | V | The number of refused sign-ins in the window that raises the alert. |
| `sign_in_window` | duration, 10s to 1d | `"5m"` | V | The window of `inbox.watch.sign_in_failures`. It slides: the portal counts the refusals of the last window. |
| `repeat_after` | duration, 1m to 7d | `"1h"` | V | The shortest time between two alerts of the same kind and the same subject. A service that stays unhealthy raises the alert again after this time. A burst that continues does the same. The alerts of this time have one shared identity, so two instances send one alert. |
| `recovered` | `true` or `false` | `true` | V | `true`: the portal sends a short alert when a service is healthy again after the portal sent an alert about it. |

### Block `inbox.items`

This block is optional and is inside the block `inbox`. It sets the limits of the items: the approvals and the questions that an agent sends to a person. It has the blocks `lease` and `digest`. Each limit that the file does not give a default for is mandatory: a wrong default would hide an item or keep it too long. The block needs the block `inbox.at_rest`, and the block `inbox.at_rest` needs this block.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `max_lifetime` | duration, 1d to 365d | required | V | The longest life of a chain of requests, from the start of the thread. At this age the item is `expired`, which is a neutral end. A replacement does not extend it. It does not set how long a request waits. Recommended: `"90d"`. |
| `max_open_per_key` | number, 1 to 1000 | required | V | The most open items of one key. Recommended: `5`. |
| `max_open_per_owner` | number, 1 to 10000 | required | V | The most open items of one owner. Recommended: `20`. |
| `max_open_total` | number, 1 to 1000000 | required | V | The most open items in all. A new item above this number is refused. |
| `max_chain` | number, 1 to 50 | `10` | V | The most replacements in one thread. |
| `max_turns` | number, 2 to 200 | `40` | V | The most turns in one thread. |
| `stale_after` | duration, 1m to 365d | `"24h"` | V | The age from which a screen shows the banner "old request" and asks for one more confirmation. |
| `retention` | duration, 1d to 3650d | required | V | How long a finished item stays, with its metadata only. It must be at least `max_lifetime` plus `lease.revive_window`. |
| `body_purge_after` | duration, 0s to 3650d | required | V | The age from which the sealed contents are erased. The metadata and the effect hash stay. It must not be above `retention`. Recommended: `"30d"`. |
| `audit_retention` | duration, 1d to 3650d | required | V | How long the audit events stay. They hold metadata only. It must be at least `retention`. |
| `max_body_bytes` | size, 1KiB to 32KiB | `"16KiB"` | V | The largest content of an item of a question. An approval has its own limits. |
| `wait_max` | duration, 0s to 1m | `"25s"` | V | The longest `wait` that the API accepts for a long poll. |

### Block `inbox.items.lease`

This block is inside the block `inbox.items`. The lease is the time that an item stays open while its agent shows that it is alive. An agent that sends no keepalive loses the item.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `min` | duration, 5s to 1h | required | V | The smallest `lease_s` that the portal accepts. Recommended: `"30s"`. |
| `max` | duration, 5s to 30d | required | V | The largest `lease_s`. The lease that applies is three times `lease_s`. It must be at least `lease.min` and at most `max_lifetime`. Recommended: `"7d"`. |
| `grace` | duration, 0s to 1h | required | V | The time after the end of a lease before the item is `abandoned`. Recommended: `"2m"`. |
| `revive_window` | duration, 0s to 365d | required | V | The time after an abandon in which a keepalive of the same key opens the item again. `"0s"` means never. It must not be above `max_lifetime`. Recommended: `"7d"`. |
| `max_revivals` | number, 0 to 10 | `3` | V | The most times that one item opens again. |

### Block `inbox.items.digest`

This block is inside the block `inbox.items`. It sets the default summary of the open items that a phone gets. An account can change its own summary, but never above a limit of this file.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `mode` | `off`, `daily`, `every` | required | V | The default summary of the open items. `off`: no summary. `daily`: one summary each day at `at`, in `timezone`. `every`: one summary at each interval `every`. Recommended: `"daily"`. |
| `at` | text: a time of the day as HH:MM, from 00:00 to 23:59 | required | V | The time of the daily summary, as `HH:MM` on a 24-hour clock. Applies only when `inbox.items.digest.mode` is `daily`. |
| `timezone` | text: an IANA time zone name, such as Europe/Paris or UTC | required | V | The time zone of `at`, as an IANA name. The start checks the name in the time zone database that the binary embeds. A name with other capital letters is refused. Applies only when `inbox.items.digest.mode` is `daily`. |
| `every` | duration, 1h to 7d | required | V | The interval between two summaries. Applies only when `inbox.items.digest.mode` is `every`. |

### Block `inbox.at_rest`

This block is optional and is inside the block `inbox`. The portal seals the contents of the items with a key of the server before it writes them. It never writes a content in clear text. The block has one block `key "<id>"` for each key. The block needs the block `inbox.items`, and the block `inbox.items` needs this block.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `active_key` | text: the label of a block `key` of this block: 1 to 16 characters of a-z, 0-9, '-' and '_' | required | V | The key that seals the new contents. The other keys of the block stay readable: use them for a rotation. No switch turns the seal off. |
| `admin_body_read` | `true` or `false` | `false` | V | `true`: the superadmin can read the contents of the items. The portal writes one audit event and counts each read, never the content. `false`: no route lets an administrator read a content. Only the owner reads it. |

### Block `inbox.mcp`

This block is optional and is inside the block `inbox`. Its presence starts the MCP endpoint of the portal for the agents. The endpoint gives the tools `approval_intent`, `get_answer`, `keepalive`, `say`, `supersede` and `cancel`. The agent sends its explanation and its talk to the portal here, and the service never sees them.

The endpoint takes the keys of the kind `mcp` only. The block needs the block `inbox.items` and `keys.mcp = true`. A call checks the origin, the client address, the key, the rate and the size of the body, in this order. Each instance keeps its own limits.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `path` | text: an absolute URL path of at most 128 bytes, not `/`, not /healthz, /readyz or /api/version, and not under /auth/, /v1/ or /api/v1/ | `"/mcp"` | V | The URL path of the MCP endpoint. The endpoint answers `POST` only. It must not be the path of the metrics endpoint. |
| `max_wait` | duration, 0s to 1m | `"25s"` | V | The longest time that one call of `get_answer` waits for an answer. A longer `wait_s` of the agent is cut to this value. A real client stops a call near 60 seconds, so keep the value lower. It must not be above `inbox.items.wait_max`. `0s`: a call never waits. |
| `allowed_origins` | list of at most 32 quoted strings: origins such as "https://app.example": a scheme, a host and an optional port, with no path | none | V | The values of the header `Origin` that the endpoint accepts. A request with no `Origin` header comes from a program and passes. A request with another `Origin` gets `403`. A web page then cannot use the key of a visitor against the endpoint. A DNS attack can cause such a request. Without this key, no browser origin passes. |
| `allowed_client_cidrs` | list of at most 64 quoted strings: IP networks such as "10.42.0.0/16" or "127.0.0.1/32"; the address is the first address of the network | none | V | The client networks that the endpoint accepts. The client address is the address that `server.trusted_proxies` finds. A client in no network gets `403`. Without this key, each address passes. |
| `rate_limit_per_minute` | number, 1 to 100000 | `120` | V | The most requests of one key in one minute on this instance. Each request counts, also `tools/list` and `ping`. A key over the limit gets `429` with `Retry-After`. The limit is a bucket in the memory of the instance: with N instances, a key can send N times this value. |
| `max_unauthenticated_per_minute` | number, 1 to 10000 | `10` | V | The most requests with no valid key that one client address can send in one minute on this instance. An address over the limit gets `429` with `Retry-After` before the portal checks the key. |
| `max_body_bytes` | size, 1KiB to 1MiB | `"64KiB"` | V | The largest body of a request. A larger body gets `413`. |

### Block `inbox.agents`

This block is optional and is inside the block `inbox`. Its presence starts the agents of a key: the portal gives each agent of one key a token, and it groups the items by agent. Without this block, each key is one agent. By default, the owner can accept the items of a new agent at once, and gets a notice. The block needs the blocks `inbox.items` and `inbox.at_rest`. The mode of each key (`key`, `discover` or `enrolled`) is an attribute of the key, not of the file.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `max_per_key` | number, 1 to 1000 | `16` | V | The most agents of one key that are `new`, `known` or `blocked`. The next call that would make an agent goes to the bucket `Unattributed` of the key, and the owner gets one alert each hour. The call does not fail. |
| `new_per_hour` | number, 1 to 1000 | `6` | V | The most agents that the calls of one key can make in one hour. The next call goes to the bucket `Unattributed`, as when `max_per_key` is reached. Agents that the owner makes by hand do not count. |
| `lock_new` | `true` or `false` | `false` | V | `true`: the owner cannot accept an approval or an answer of a `new` agent before the action `allow` on the agent. A refusal and a talk stay open. `false`: the owner can accept the items of a new agent at once, and still gets a notice. |
| `idle_after` | duration, 1h to 365d | `"14d"` | V | An agent with no open item and no call for this time leaves the active views. It still works, and a call makes it active again. It must not be above `purge_after`. |
| `purge_after` | duration, 7d to 3650d | `"365d"` | V | The sweeper erases an agent that has no item, no live intent and no call for this time. The token of an erased agent is unknown. It must not be below `idle_after`. |

### Block `inbox.questions`

This block is optional and is inside the block `inbox`. Its presence starts the questions. An agent asks the owner of its key with the tool `ask` or with `POST /api/v1/questions`. The owner answers on a phone or on the web page. The portal seals each question at rest. A service never sees a question.

The block needs the blocks `inbox.items` and `inbox.at_rest`. The block can be empty. A key needs the permission `question.ask` to ask.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `max_options` | number, 2 to 8 | `8` | V | The most options of a question with an input of the type `choice`, and of each choice question of a form. The protocol of the phones accepts 8 at most. A question with more options gets `422 invalid_argument`: the portal never cuts the list. |

### Block `inbox.approvals`

This block is optional and is inside the block `inbox`. Its presence starts the approval of an action: the PDP answers `approval_required` for a grant with `confirm`, and it consumes an approval once. The block needs the blocks `inbox.items` and `inbox.at_rest`, the block `pdp` and `keys.mcp = true`. Every service that asks the PDP can use it: the service has no flag and no declaration for it.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `consume_within` | duration, 5s to 1h | required | V | The time after the decision in which the approval can be used. It never limits the wait for the decision. The window that applies is the larger of this value and `lease_s` of the request. It must not be above `inbox.items.max_lifetime`. Recommended: `"5m"`. |
| `evaluation_ttl` | duration, 10s to 1h | `"5m"` | V | The time in which an `evaluation_id` of the PDP is valid. A service submits its request before this time ends. |
| `deny_cooldown` | duration, 0s to 1d | `"5m"` | V | The time after an explicit refusal in which the PDP answers `approval_denied` for the same key and the same effect. Another effect, and each other end of a request, start no such time. `0s`: the PDP never answers `approval_denied`. |
| `max_command_bytes` | size, 1KiB to 32KiB | `"16KiB"` | V | The largest command (`op` and `view`, as compact JSON) that the approval API accepts. A larger command gets `413 command_too_large`, and the service refuses the action. The PDP publishes the value as `procurauth_approval_max_command_bytes`. The body of a submission also holds other members (`evaluation_id`, `effect`, the wait). This value plus 2048 bytes must not be above `pdp.max_body_bytes`. |
| `max_explanation_chars` | number, 0 to 8000 | `2000` | V | The longest explanation of an agent, in characters. A longer text gets `422 explanation_too_long`, and the portal never cuts it. `0`: no explanation is accepted. |
| `require_explanation` | `true` or `false` | `false` | V | `true`: a request without an explanation gets `422 explanation_required`. It must not be `true` with `max_explanation_chars = 0`. |
| `max_comment_chars` | number, 1 to 8000 | `2000` | V | The longest text that a person joins to a refusal, in characters. |
| `intent_ttl` | duration, 30s to 1h | `"10m"` | V | How long an `approval_ref` stays valid. A reference works once, for the key that made it. A service submits its request before this time ends. The PDP publishes the value as `procurauth_approval_ref_ttl_seconds`. |
| `max_requests_per_key_per_hour` | number, 1 to 10000 | `20` | V | The most requests that one key creates in one hour. A request that replaces another counts too. A repeat of an open request counts never. A key over the limit gets `429 rate_limited` with `retry_after`. |
| `mute_after_refusals` | number, 0 to 100 | `3` | V | The number of explicit refusals in a row after which the portal mutes the key for `mute_for` and sends an alert to its owner. A muted key gets `429 rate_limited`. `0`: a key is never muted. |
| `mute_for` | duration, 1m to 7d | `"1h"` | V | How long a key stays muted after `mute_after_refusals` refusals. |
| `web_approval` | `true` or `false` | `true` | V | `true`: a person can approve from the web page. `false`: the page lists the requests and does not decide them, so an owner with no phone cannot receive a request (`409 no_device`). |

### Block `user "<login>"`

This block has a label and can occur more than one time. The grants of one user that the configuration gives (modes `static` and `hybrid`). The label is the login, as the server keeps it: 1 to 32 characters of a-z, 0-9, '.', '-' and '_'. The account itself is in the database. The superadmin has all grants and has no `user` block.

A grant names a node of the resource tree of a service: `<action> on service:<id>`, then 0 to 4 segments `/<type>:<id>`. An example is `query.run.read on service:clickhouse-dash/host:local/database:analytics`. The action `access` lets the user open the service and has grants on the service only. The other actions are the names that the service publishes at `/.well-known/procurauth/actions`. A grant covers its node and each node below it.

A type starts with a-z, then has a-z, 0-9 and '_', 1 to 32 characters, and is not `service`. Each id is percent-encoded: each byte outside A-Z a-z 0-9 - . _ ~ becomes %XX with upper-case hex digits. For example, `bucket:a%2Fb` is the id `a/b`. Any other form of the same id stops the start. An id has 1 to 256 bytes after the decode, without a control character.

A grant can be an object: `{ grant = "access on service:datastore", limits = { requests = [ { amount = 300, window = "minute" } ] } }`. The member `confirm` is `true` or `false`, and the default is `false`. With `confirm = true`, each use of the action needs the approval of a person. A grant of the service `portal` or of the action `access` with `confirm = true` stops the start.

The `limits` object holds one member for each limit. A ceiling is a whole number. A budget is a list of objects with `amount` and `window` (`second`, `minute`, `hour` or `day`). A budget is valid only on the grant `access` on a service.

A grant has at most 8 limit values. A value is at most 1,000,000,000. The limit values take at most 1024 bytes as compact JSON.

At start, the server checks only the form of each grant: the services and their trees of types are known at run time. The Services page of the settings shows a grant that does not follow the tree of its service. The PDP does not apply this grant. A block `user` stops the server when it gives `keys.own.create`, or an action that needs another action that it does not give.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `grants` | list of at most 30000 grants, each a quoted text or an object with `grant`, `limits` and `confirm`: a grant such as "access on service:grafana" or "query.run.write on service:clickhouse-dash/host:local/database:analytics": an action, the word on, service: and the id of the service, then 0 to 4 segments /type:id with each id percent-encoded. To give limit values, write { grant = "access on service:datastore", limits = { requests = [ { amount = 300, window = "minute" } ] } }. To ask for an approval, write { grant = "query.run.write on service:clickhouse-dash/host:local", confirm = true } | required | V | The grants of the user. The user is an account of the file, with no row in the database. Each grant is a quoted text or an object. See the notes of this block for the form of a grant, the limit values and the approval. There are at most `users.max_grants` grants. |
| `password_hash` | secret, at most 512 bytes | none | E, F | The Argon2id digest of the password of the user, in the PHC string form, as the database keeps it. Without it, the account opens no session, and only its keys act for it. |

### Block `key "<name>"`

This block has a label and can occur more than one time. One key of a user of the file. The block is inside a block `user`. The label is the name of the key: 1 to 64 characters, with no control character. Two keys of one user have two names.

The subject id of the key starts with `key:`. It ends with the first 16 hexadecimal characters of the SHA-256 digest of the login, a zero byte and the name. A key of the file has no expiry date. To stop it, remove the block and restart each instance. The block needs the block `keys`.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `grants` | list of at most 512 grants, each a quoted text or an object with `grant`, `limits` and `confirm`: the same form as `user.grants` | required | V | The grants of the key, in the form of `user.grants`. A key holds at most 512 grants. A grant of an action of the service `portal` stops the server. The decision for a key never gives more than the grants of its owner. The key needs an approval when a grant of the key or a grant of its owner has `confirm = true`. |
| `interaction` | `conversational`, `one_shot` | `"conversational"` | V | What the key can do when the server asks a person. `conversational`: an agent that can discuss and read free text. `one_shot`: a system that decides without free text and gets closed questions only. The agent cannot change this value. |
| `key` | secret, at most 64 bytes | none | E, F | The text of the key: `mcp_`, 43 characters and 6 characters of checksum. Give `key_env` or `key_file`, or give `key_sha256_env` or `key_sha256_file`: exactly one of the four. A bad form stops the server. |
| `key_sha256` | secret, at most 128 bytes | none | E, F | The SHA-256 digest of the text of the key: 64 hexadecimal characters, in upper or lower case. A digest that the database or another block also holds stops the server. It must be the digest of a text of the form `mcp_`, 43 characters and 6 characters of checksum. The server cannot check this at start. |

### Block `group "<id>"`

This block has a label and can occur more than one time. One group of alerts. The block is inside the block `inbox`. The label is the id of the group: 1 to 64 characters of a-z, 0-9, '-' and '_'. A phone subscribes to a group, and a key sends an alert to a group.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `name` | text: 1 to 128 bytes, not blank | required | V | The name of the group that the phones show. |
| `description` | text: at most 512 bytes | `""` | V | The description of the group. |
| `critical` | `true` or `false` | `false` | V | `true`: the group is critical. A limit on the action `alert.send` never applies to a critical group. `false`: the limit of a key applies to the group. |

### Block `api_key "<name>"`

This block has a label and can occur more than one time. One API key of the file, inside the block `inbox`: the key of a tool, a script or a webhook that sends alerts. The label is the name of the key: 1 to 64 characters of a-z, 0-9, '-' and '_'. The key follows its owner, as each key does: it can do only what the key and its owner both have the right to do. The owner is an account of the file (a block `user`), so the key and its account have one source.

The rights of a file account stay fixed while the server runs. So the server decides at start which groups the key can send to.

It also keeps the groups of the key where the owner holds `alert.read`: only these give the status of an alert (`GET /api/v1/alerts/{id}`). This decision does not wait for the database. The block needs `keys.api = true`. The key is not in the list of keys.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `owner` | text: the login of an account of the file (a block `user`) | required | V | The login of the owner of the key. The owner must be an account of the file: a block `user`. The owner must hold `keys.own.use` on `service:portal/key_kind:api`, `alert.send` on each group of the key, and `alert.send_user` on `service:portal` when `direct_alerts` is `true`. |
| `secret` | secret, at most 64 bytes | required | E, F | The text of the key: `api_`, 43 characters and 6 characters of checksum, 53 characters in all. Make it with `portal inbox token`. Two keys must not have the same text. A bad form stops the server. |
| `groups` | list of at most 64 quoted strings: ids of groups that the block `inbox` defines | required | V | The groups that the key can send alerts to. The list has at least one group, except when `direct_alerts` is `true`: then it can be empty. Each group must exist, and the owner must hold `alert.send` on it. |
| `direct_alerts` | `true` or `false` | `false` | V | `true`: the key can send an alert to any account (`POST /api/v1/alerts` with the field `user`). The owner must hold `alert.send_user` on `service:portal`. `false`: the key sends to groups only. |
| `interaction` | `conversational`, `one_shot` | `"one_shot"` | V | What the key can do when the server asks a person. `one_shot`: a system that decides without free text and gets closed questions only. `conversational`: an agent that can discuss and read free text. The service cannot change this value. |
| `webhook_send_resolved` | `true` or `false` | `false` | V | `true`: the notifications "resolved" of Alertmanager and Grafana become alerts. `false`: the inbox ignores them. |

### Block `pdp_client "<server>"`

This block has a label and can occur more than one time. One server that asks the PDP for decisions. The label is the name of the server: the id of its service in the catalogue, and the audience of its identity tokens. The PDP answers only for the resources of this server.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `token` | secret, at most 1024 bytes | required | E, F | The bearer token of the server. It has at least 32 bytes. Two servers must not have the same token. |

### Block `external_service "<name>"`

This block has a label and can occur more than one time. One external service: a tile of the launcher that opens a link. The label is the display name: 1 to 32 characters, with no control character and at least one letter or digit. The id of the service is `ext-` and the slug of the label, so `Team wiki` gives `ext-team-wiki`. The id is the resource id of the grant `access`, which a block `user` gives.

The service needs no container and no route, and the server never requests its link. Two blocks with the same id stop the start.

A service of the file wins over a database service with the same id. The server ignores the row and does not delete it. A block `key` cannot name the service, and a block `pdp_client` cannot have its id as its name.

The Settings page shows the service of the file with the badge `file`, and it cannot change or delete it. A discovered service with the same id wins over both. A block of this kind works in each mode of `access.grants_mode`. At most 256 blocks.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `href` | text: an absolute http or https URL of 1 to 2048 bytes, with a valid host: a DNS name (ASCII labels of RFC 1123), an IPv4 address, or an IPv6 address in square brackets; no user information, no space, no control character and no backslash; a port from 1 to 65535 | required | V | The link that the tile opens. The path, the query and the fragment are free. The server never requests the link. A link that does not follow the rule stops the start. The server removes no space and changes nothing: the value in the file is the value that applies. `ui.services_new_tab` applies to the tile. |
| `description` | text: at most 200 characters, with no control character | `""` | V | The text under the name on the tile, and in the panel of the Services page. An empty text shows nothing. |
| `icon` | text: a data URI in the form data:<type>;base64,<data>, with the type image/png, image/jpeg, image/webp, image/gif or image/svg+xml, and at most 32 KiB when decoded | none | V | The own icon of the service, as a data URI. Give at most one of `icon` and `icon_path`. The server finds the type from the bytes, and it refuses a declared type that does not match. It serves the image itself, to a signed-in session, from its own address. An SVG with a script, an event attribute, a `foreignObject` or an external reference stops the start. Without an icon, the tile uses the icon of the library that has the name of the service, then the first letter of the name. |
| `icon_path` | text: an absolute path without '..' of a PNG, JPEG, WebP, GIF or SVG file of at most 32 KiB | none | V | The path of an image file that the server reads at start. The rules of `icon` apply to the bytes. This key is not the `_file` form of `icon`, on purpose. The `_file` form removes the last line end of a text, but the bytes of an image stay exact. A file that the server cannot read, or that is empty or too large, stops the start. Give at most one of `icon` and `icon_path`. |

### Block `inbox.at_rest.key "<id>"`

This block has a label and can occur more than one time. One key that seals the contents of the items. The block is inside the block `inbox.at_rest`, which has 1 to 8 of these blocks. The label is the id of the key. A key that is not the `active_key` stays readable and seals nothing.

| Key | Type | Default | Forms | Description |
|---|---|---|---|---|
| `secret` | secret, at most 128 bytes | required | E, F | The key: the base64 text of 32 random bytes, 44 characters. Make it with `portal inbox content-key`. The bytes must not all be equal. Two keys must not have the same text. The file never holds the text: give `secret_env` or `secret_file`. |
<!-- config-reference:end -->
