# portal

portal is an authentication service for Traefik `ForwardAuth`. It also contains the launcher page of the lab. portal is written in Rust. It does these functions:

- A superadmin is defined by the environment.
- Users get grants from the settings pages or from the configuration.
- A gate protects each service.
- portal gives an identity token to the services.
- portal is an AuthZEN PDP for the services.
- portal keeps a history of who signed in and what they used.

portal is the access gateway and the PDP of section 11 of the MCP server contract. The launcher (`/`) lists the services that declare themselves by label (Docker Compose) or by annotation (Kubernetes IngressRoute). The launcher groups the services in full-width rows. It shows their health and an offline icon library. See the repository README for the labels.

The runtime image is `FROM scratch`. The HTTP stack is Hyper 1.x on Tokio. Discovery uses `reqwest` (rustls) for the Kubernetes API and for health probes. Pages are rendered in Rust. The only script is the theme selector. The settings pages add one more small script, for the password generator. Accounts and history live in one SQLite file. The SQLite file is bundled in the static binary. To run several active instances, use a shared PostgreSQL database instead (see [Several instances](#several-instances-postgresql)). There is no Redis, no JavaScript framework, no CDN and no runtime shell. Nothing is fetched from the internet at runtime.

## Authentication model

The login page asks for a **login** and a **password**.

The **superadmin** is the one account that is not in the database. Its login and its password come from an environment variable or from a file. The configuration names the source (see [Secrets](#secrets-_env-and-_file)):

```hcl
authentication {
  superadmin_login_file    = "/run/secrets/portal_superadmin_login"
  superadmin_password_file = "/run/secrets/portal_superadmin_password"
}
```

The superadmin login has no upper-case letter and no space. The program refuses a login that breaks this rule at start. The sign-in form compares logins in lower case. The program does not change the login.

At start, the program reads the password with a size limit. The program hashes the password once with Argon2id. The program then erases the plaintext from memory. The superadmin can see and open everything. The superadmin is the only account that can manage the other accounts.

The superadmin adds and deletes **users** from the settings pages (the gear of the launcher). The database stores their Argon2id hashes. The database is the SQLite file `storage.database`, or PostgreSQL. The request path reads an in-memory copy. The request path never waits on the database. A user has a list of grants (see [Grants and the gate](#grants-and-the-gate)). Each login attempt runs exactly one Argon2 check. This applies to any login name. The check uses the superadmin hash, the hash of the account, or a dummy hash. The failure message is the same in all cases. Neither the time nor the wording shows whether a login exists.

A successful login creates a 256-bit random **opaque session ID**. The browser receives only that random ID. With SQLite, the server keeps the active-session set in bounded RAM. Each session has an expiration time. Logout deletes the session at once. A restart of the process clears all sessions. With PostgreSQL, the sessions are in the shared database. Only the SHA-256 of the cookie is stored. The sessions are valid on every instance and survive a restart. A session of the superadmin, and a session that the superadmin opened with **Log in as**, are valid only with the superadmin credentials that opened them. A new superadmin login, password or session secret ends these sessions on every instance (see [Several instances](#several-instances-postgresql)).

## Grants and the gate

The settings pages say "permission" where this document and the protocol say "grant". The two words name the same thing.

### Grants

A grant is an action on a node of the resource tree of a service (contract 11.8). A grant has this form:

```text
<action> on service:<id>[/<type>:<resource id>]...
```

- `access on service:grafana`: the grant applies to the whole service.
- `query.run.read on service:clickhouse-dash/host:local`: the grant applies to the host `local` and to each node below it.
- `query.run.write on service:clickhouse-dash/host:local/database:analytics`: the grant applies to the database `analytics` of the host `local`.

The `<id>` is the id of the service in the catalogue. The id is the label `portal.id`, or the slug of the group and the name. The part after the service has 0 to 4 segments.

- `access` lets the user open the service. portal decides `access` in `/auth/verify`.
- The other actions are the actions that the service publishes at `/.well-known/procurauth/actions` (protocol 5.1). The service decides these actions. The service asks the PDP of portal.
- A grant covers the node that it names and each node below it. portal compares the segments one by one (prefix match). For example, `query.run.read on service:db` covers each host and each database of `db`.
- A grant below the service does not cover its parent, a node of another branch, or the whole service.
- The service gives its tree of resource types. The top-level list `resource_types` of its document declares each type and its parent: `service` or another declared type. A document without this list has one level: each type of an action has the parent `service`.
- Each action of the document has a list `resource_types`: the types on which a grant for this action is valid. The type `service` is the whole service: it is never a segment.
- The types of the segments must follow the parents. With `host` below `service` and `database` below `host`, write `host:<id>/database:<id>`. A grant on `database:<id>` alone does not follow the tree.
- A grant below the service applies only when the last known document of the service follows it. The action must list the type of the last segment, and the segments must follow the parents. Else, the PDP does not apply the grant. If portal never read the document of the service, the PDP does not apply any grant below this service. A grant on the whole service does not need a declaration.
- A type starts with `a-z`, then has `a-z`, `0-9` and `_`, 1 to 32 characters.
- Each resource id is percent-encoded (RFC 3986). Each byte outside `A-Z a-z 0-9 - . _ ~` is `%XX`, with upper-case hex digits. For example, the id `a/b` is `a%2Fb`. Thus a `/` or a `:` in an id never divides a segment.
- The encoding is canonical. portal refuses a different encoding of the same id, for example `%2f` or `%61`.
- A decoded id has 1 to 256 bytes, without a control character. portal uses the id as it is. portal does not change its case. The settings pages show the decoded ids.
- `access` has no grant below the service. `access` opens the whole service.
- A new user has no grant. The user sees no service until the superadmin gives a grant.
- The superadmin has all grants. The superadmin has no `user` block.

portal has no groups of users. Each user has their own grants.

### Where the grants come from

The key `access.grants_mode` gives the source of the grants. The key uses the same modes as the keys in contract 3.5.

| Mode | Grants of the `user` blocks | Grants of the settings pages |
|---|---|---|
| `static` | The server uses them. | The server does not use them. The settings pages show the grants. They do not change them. If the database holds grants, the server writes a warning at start. |
| `dynamic` (default) | A `user` block stops the server at start. | The superadmin gives and removes them. The database keeps them. |
| `hybrid` | The server uses them. They are read-only in the settings pages. | The superadmin gives and removes them. |

```hcl
access {
  grants_mode = "hybrid"
}

user "alice" {
  grants = ["access on service:grafana", "query.run.read on service:db"]
}
```

The server reads the `user` blocks at each start. When you remove a grant from the file, the grant stops at the next start. The server does not write the grants of the configuration to the database.

A `user` block can give a grant below the service, for example `"query.run.write on service:db/host:local/database:analytics"`. A grant with a bad form stops the start: a bad type, a bad encoding, or more than 4 segments. At start, portal does not know the services: the catalogue and the actions documents come at run time. Thus a grant that does not follow the tree of its service does not stop the start. The Services settings page shows this grant under **Permissions to check**. At run time, the PDP does not apply this grant (see "AuthZEN PDP").

### The gate

`/auth/verify` finds the service from the path in `X-Forwarded-Uri`. The service owns the longest path prefix of its `href`. The prefix matches whole segments. portal then decides `access` in its own process. portal uses the grants in memory.

Traefik routes on the clean path, but sends the raw URI. portal therefore reads the path in every way that a proxy or a backend can read it. These ways include `%2e%2e`, `//`, `%2f` as a character and as a separator, and `\`. portal lets the request through only if every way is permitted. For example, portal judges `/grafana/../clickhouse/` as a request for ClickHouse.

- A refused browser navigation goes to `/auth/denied`. Traefik limits the body of a ForwardAuth answer. The answer cannot hold the page. Other requests get a plain `403`.
- The page `/auth/denied` does not give the name of a service. A user does not see a service that is not open to that user.
- `/auth/*` is always open to a signed-in user. A path that no service owns (an absolute asset path) is open to each signed-in user.
- The previous rule needs a complete catalogue. The catalogue is complete when each configured discovery source gave its full list. The instance opens its ports only after this (see "Start and stop"), so a user never reaches the gate with an incomplete catalogue. A source that failed at setup never makes the catalogue complete. The metric `portal_catalog_complete` shows the state. After the first full list, a failed listing keeps the last good list, and the catalogue stays complete.
- A workload with unusable labels is not listed. The gate keeps the path of its `href` closed: only the superadmin passes it. The same rule applies to a workload whose id another workload already uses with another `href`. The catalogue problems name both cases.
- The launcher shows only the services for which the user has `access`. The discovery problems name workloads. Only the superadmin sees these problems.

portal stays a ForwardAuth server. Traefik is the only proxy. portal receives only the headers of a request. portal never receives the body of a request or of a response.

### Identity token

With the `identity` block, `/auth/verify` gives an identity token (protocol section 9) to each service that a user can open. The token is in the response header `identity.header`. The default is `X-Identity-Token`. Traefik copies the token to the request. Add the header to `authResponseHeaders` of the ForwardAuth middleware. Traefik then removes a header of the same name that the client sends.

```hcl
identity {
  issuer           = "https://portal.home.internal"
  header           = "X-Identity-Token"
  token_ttl        = "5m"
  signing_key_file = "/run/secrets/portal_identity_key"
}
```

The token is a JWT. Its algorithm is `Ed25519`. The header has `kid`. `kid` is the RFC 7638 thumbprint of the public key. The claims are:

| Claim | Value |
|---|---|
| `iss` | `identity.issuer` |
| `sub` | the login of the user (the superadmin has its own login) |
| `aud` | the id of the service |
| `iat`, `exp` | the time of issue, and `iat + identity.token_ttl` |

The token contains no permission. The service asks the PDP. portal keeps one token for each session and each service. portal gives the same token again until half of its life is gone. A path that no service owns gets no token.

Make the key with OpenSSL. Give the public key to each service as a JWK Set (protocol 9.4):

```bash
openssl genpkey -algorithm ed25519 -out portal_identity_key
portal identity-public-key --config /etc/portal/config.hcl > portal_identity_public.jwks
```

The key file holds a PKCS#8 key in PEM format. The file can also hold the base64 text of the PEM on one line. `portal check-config` reads the key. It then prints the `kid`.

### AuthZEN PDP

With the `pdp` block, portal is an AuthZEN PDP (contract 11.5) on a second listener. Do not expose this listener through Traefik. Only the services use it.

```hcl
pdp {
  listen_address = "0.0.0.0:4182"
  url            = "http://portal-pdp.edge.svc.cluster.local:4182"
}

pdp_client "db" {
  token_file = "/run/secrets/portal_pdp_token_db"
}
```

| Endpoint | Use |
|---|---|
| `POST /access/v1/evaluation` | One decision: `subject`, `action`, `resource`, `context`. The answer is `{"decision": true}` or `{"decision": false}`. If the subject does not exist or the resource is not valid, the answer has a reason (see the rules below). |
| `POST /access/v1/evaluations` | A batch. The top-level members are the defaults of each item. `options.evaluations_semantic` is `execute_all` (default), `deny_on_first_deny` or `permit_on_first_permit`. The batch has at most `pdp.max_evaluations` items. |
| `POST /access/v1/search/action` | The actions that the subject can do on the resource. The resource has the same form as in an evaluation, with its ancestors. The answer is one page. |
| `GET /.well-known/authzen-configuration` | The endpoints under `pdp.url`. This endpoint needs no token. |

Rules:

- Each server has its own `pdp_client` block and its own token. The label is the name of the server. The label is the id of its service. This id is also the audience of its identity tokens. The server sends `Authorization: Bearer <token>`. A request without a valid token gets status `401`.
- The PDP answers a server only for its own resources. The resource `{"type": "service", "id": "<name of the server>"}` is the whole service. It has no `ancestors`, or an empty list.
- A resource of another type is a node below the service, for example a database. `resource.properties.ancestors` gives the chain from the service to the parent of the node: `[{"type": "service", "id": "db"}, {"type": "host", "id": "local"}]`. The ids are not encoded in a request.
- The first ancestor must be the service of the server that asks: its token tells which server it is. The chain (the ancestors, then the resource) must follow the parents of the last known document of the service. The chain has at most 4 nodes below the service.
- A resource that does not obey these rules gets `{"decision": false, "context": {"reason": "invalid_resource"}}`. The service of another server gets the same answer. This check comes before the subject: the superadmin and an unknown user get the same answer.
- An `invalid_resource` answer also starts an early read of the document of the server that asks (see "Actions of the services"). The server can have a new tree that portal does not know yet.
- Compatibility: a node without `properties.ancestors` is read as `[service of the server, node]`. This is correct only when its type has the parent `service`. Thus the requests of a service with a document of one level do not change.
- If portal never read the document of the service, the decision on a node below the service is `false`, without a reason. The reason in the log is `the document of the service is not known`. A request about the whole service is not affected. After a change of the `portal.actions` URL of a service, a failed read leaves no document.
- A grant covers the node that it names and each node below it. The PDP compares the types and the decoded ids of the chain segment by segment, as exact text. The three endpoints use the same rule.
- `context.match` sets how a grant matches the resource. The default is `"node"`: the rule above. With `"subtree"`, a grant on a node below the resource also gives `true`. Example: a user with a grant on one database of the host `local` gets `true` for the host `local`, so that the service can show this host.
- A service uses a `"subtree"` decision only to show or hide a navigation element. Each operation asks with `"node"` on its exact resource (contract 11.5).
- Another value of `context.match`, or a value that is not a text, gives `{"decision": false, "context": {"reason": "invalid_request"}}`. This check comes first.
- In a batch, the `context` of an item replaces the `context` at the top of the request (AuthZEN 1.0). An item without `context` uses the top-level one. The action search reads `context.match` too.
- A grant below the service applies only when the last known document of the service follows it (see "Grants"). The PDP keeps the last document that it read for each service. A failed read does not remove it. A new document that changes the tree removes the effect of the grants that do not follow it, at once. The reason in the log is `the grant does not follow the document of the service`. A grant on the whole service is not affected.
- The subject type must be `user`. The subject must be an account or the superadmin. If not, the decision is `false`.
- If the subject type is `user` and no account or superadmin has this login, the answer has a reason: `{"decision": false, "context": {"reason": "unknown_subject"}}`. The batch gives the same answer in each item of this kind. A service can use it to tell an unknown user from a user without a grant. An id with a colon names a key, not a person (protocol 3.2). No login has a colon. The portal makes no key yet: such an id also gives `unknown_subject`.
- The reasons `invalid_resource` and `invalid_request` tell that the request of the service is not correct. They do not show the grants of a user.
- No other denial has a reason. The answer is `{"decision": false}`, so that it does not show the grants of a user. A subject type that is not `user` is one of these denials.
- The PDP fails closed. A request that the PDP cannot read gets status `400` and no decision.
- The PDP returns the header `X-Request-ID` of the request.
- The log records each decision (`pdp_decision`). The record has the server, the subject, the action, the resource, the decision and the reason. The resource is the chain in the text form of a grant path, with the ids encoded, for example `service:db/host:local/database:analytics`. The record also has the match: `node`, `subtree` or `invalid`.
- The PDP has no TLS. The deployment must limit the network of the PDP to the services and portal. This differs from AuthZEN, which requires TLS.
- In K3s, the NetworkPolicy `allow-portal-pdp` (`#tools/k3s/network/policies.yaml`) admits to the port 4182 only the pods with the label `portal-pdp-client: "true"`. These pods must be in the namespace `apps`, `observability` or `edge`. Give this label to the pod template of each server that asks the PDP. The policy `allow-edge` does not select the portal pods. The policy `allow-portal-http` opens only the port 4181 of portal, and only to the Traefik pods and the Prometheus pod. portal trusts `X-Forwarded-For` from the pod network (`server.trusted_proxies`). No other pod may reach the port 4181.

A request about a database of the host `local`:

```json
{
  "subject": {"type": "user", "id": "alice"},
  "action": {"name": "query.run.write"},
  "resource": {
    "type": "database",
    "id": "analytics",
    "properties": {"ancestors": [{"type": "service", "id": "db"}, {"type": "host", "id": "local"}]}
  }
}
```

### Actions of the services

portal reads the actions of each service at `/.well-known/procurauth/actions`. The URL comes from the label `portal.actions`. This label is an absolute http or https URL. If the label is missing, the URL is the origin of the health URL (`portal.health`) with this path. A service without these two labels has only `access`.

- The read has a timeout (`access.actions_timeout`) and a size limit (`access.max_document_bytes`, 256 KiB by default). portal reads the document again after `access.actions_refresh`.
- When the PDP gives `invalid_resource` to a service, portal reads the document of this service again at once. The service can have a new tree that portal does not know yet. portal does this at most once in each `access.actions_refresh` interval for each service. The early read has the same timeout, size limit and number of concurrent reads as the other reads. The log has one line `actions_read_early` with the service.
- The field `server` of the document must be the id of the service. Set `portal.id` to the name of the server.
- The top-level list `resource_types` declares the tree of types (protocol 5.2). Each item has `name`, `parent` and `description`. A name starts with `a-z`, then has `a-z`, `0-9` and `_`, 1 to 32 characters. It is not `service`. The parent is `service` or another declared type. The tree has no cycle and at most 4 levels below `service`. The list has at most 16 types.
- When the document has this list, each type in the list of an action is `service` or a declared type. A type occurs one time in the list of an action.
- The document can have an optional list `resources`. This list names the nodes of the tree that the service knows, for example its hosts. Each item has `path` (the text form of a grant, for example `host:prod`), an optional `label`, and an optional list `actions`. A path follows the declared types. A path occurs one time. Each action of an item lists the type of the node. Without `actions`, the item takes each action that lists its type. The settings pages show each named resource apart, with the actions that apply on it only. The list has 256 items at most. The list is not part of AuthZEN: a service that does not give it works as before, and the superadmin types the ids.
- A document that does not obey one of these rules is bad as a whole. portal keeps the last good document, and the Services page shows the problem.

```json
{
  "server": "db",
  "resource_types": [
    {"name": "host", "parent": "service", "description": "One database server."},
    {"name": "database", "parent": "host", "description": "One database of a host."}
  ],
  "actions": [
    {"name": "query.run.read", "resource_types": ["service", "host", "database"], "description": "Run read-only queries."},
    {"name": "explorer.read", "resource_types": ["service", "host", "database"], "description": "Browse the databases."}
  ],
  "resources": [
    {"path": "host:prod", "label": "Production"},
    {"path": "host:replica", "label": "Read replica", "actions": ["query.run.read"]}
  ]
}
```

- A `404` means that the service does not publish actions. The service then has only `access`. `access` is the full permission. portal lets the user in. The service does no other check.
- Any other failure keeps the last good document. The Services settings page shows the problem. The PDP also uses this document: see "AuthZEN PDP".
- The page of a user has one editor of grants, the same editor as the forms of the keys (see "Editor of grants"). The editor shows the tree of the resources of a service and the actions of the chosen node. A person who gives a grant on a resource does not type its id: the editor lists the resources. portal takes them from the member `resources` of the action document and from the options of the service (portal 3.1a, protocol 5.6).
- One **Save access** sends every grant of the account, with its limit values. The form replaces the grants of the account. The grants on the service `portal` (the rights on the keys) have their own form. A grant that the document of the service does not name now stays in the editor. The editor shows it, so a person can remove it. A grant of the configuration shows a lock and cannot change on the page.
- A limit field shows the largest value that the document allows. This is a hint. An empty field gives no value: the limits of the service still apply. The protocol has no default value for a limit.
- While a service has not answered, portal refuses a new grant below it.
- The Services page keeps the section **Grants to check**. It lists each grant below a service that does not follow the document of the service. The PDP does not apply these grants. The section does not list the grants of a service that has not answered yet.
- The log also has one warning `grant_to_check` for each such grant, with the login, the grant and the problem. portal writes it at the first answer of the service after the start, and at each change of its document.

### Migration of the database

At the first start of the version with grants, the server changes the database. SQLite goes to schema version 3. PostgreSQL goes to version 2. The migration deletes the access data of the earlier versions. It adds the table of the grants. The accounts and the history stay. After the migration, no user has a grant. Give the grants again in the settings pages, or in `user` blocks. Until then, only the superadmin can open the services.

The grants on sub-resources need one more change. SQLite goes to schema version 4. PostgreSQL goes to version 3. The table `user_grants` gets the columns `sub_resource_type` and `sub_resource_id`. The migration keeps all grants. A grant of the earlier version applies to the whole service. An earlier portal refuses the new schema: it does not start on it.

The credential generation of the superadmin needs one more change on PostgreSQL. PostgreSQL goes to version 4. The table `sessions` gets the column `admin_generation`. The sessions of the superadmin and the impersonated sessions of version 3 end at the first start. The superadmin signs in again. The sessions of the users stay. An earlier portal refuses version 4: it does not start on it. SQLite does not change, because its sessions are in memory.

The resource tree (portal 4.2.0) needs one more change. SQLite goes to schema version 5. PostgreSQL goes to version 5. The columns `sub_resource_type` and `sub_resource_id` of `user_grants` become one column `resource_path`. It holds the segments after the service in their text form, for example `host:local/database:analytics`. It is empty for a grant on the whole service. The primary key is the user, the action, the service and the path.

- The migration keeps each grant. A grant on one sub-resource `(database, a/b)` becomes the path `database:a%2Fb`.
- The migration does not add a level. A grant `database:analytics` stays at depth 1. When the document of the service puts `database` below `host`, this grant does not follow the tree. The PDP does not apply it. The Services page lists it under **Permissions to check**, and the log has a warning `grant_to_check` for it. Give the grant again on the new tree, for example `host:local/database:analytics`, then remove the old grant.
- A row that does not fit the new rules stops the migration. Examples: a type that does not start with `a-z`, an id of more than 256 bytes. The transaction rolls back. The database stays at the earlier version, and the new portal does not start. The error names `resource_path`.
- An earlier portal refuses version 5: it does not start on it. During a rolling update, an instance of portal 4.1.0 that still operates cannot read or write the grants after the migration. It keeps the grants that it has in memory until it stops.

The keys and limits need one more change. SQLite goes to schema version 6. PostgreSQL goes to version 6. The migration adds the tables `keys`, `key_grants`, `key_history`, `budget_counters` and `directory_version`, and it adds the column `limits` to `user_grants`. A key belongs to one user. The table `keys` holds the SHA-256 digest of a key and a short hint, never the key. When a user is deleted, the database deletes the keys of the user, and with them their grants. Each write of a user or a key adds 1 to `directory_version`, and the other instances read the directory again at the next round. The migration keeps all other data. An earlier portal refuses the new schema: it does not start on it. Therefore, a return to an earlier portal needs a restore of the database from before the migration.

### Paths open without a session (`portal.exclude`)

A caller that cannot sign in (for example, a webhook or a probe) needs a path that portal does not authenticate. The service declares this path with `portal.exclude`. This is an annotation in K3s. Write **one rule per line**. portal ignores blank lines and `#` comments:

```yaml
portal.exclude: |
  POST /dagu/api/v1/webhooks/*
  GET  /dagu/api/v1/health
  PUT,POST /dagu/hooks/**
```

A rule has this form: `[METHOD[,METHOD]] /path`. A segment is written out with the characters `[A-Za-z0-9._~-]`. A segment has up to 64 characters. `*` stands for exactly one segment. `**` stands for one or more segments. `**` is valid only at the end of a rule. There is no regular expression. There is no partial wildcard (`fo*`). A rule that skips authentication must be easy to read. A pattern engine is one more thing that can fail. Without a method, the rule matches any method. With methods, the rule matches only those methods. A request with no method header matches none of them.

The rules are strict by design:

- a service can open only paths under **its own `href`**. It can open a path only while it owns that path. The longest prefix wins, as for the gate. For example, `/dagu/admin` declared by another service stays closed to Dagu's rules;
- portal reads the path every way that a proxy or a backend might read it (`%2e%2e`, `//`, `%2f` as a character and as a separator, `\`). portal lets the request through only if **every** reading belongs to the same service and matches one of its rules. `/dagu/api/v1/webhooks/..%2f..%2fadmin` is not a webhook;
- a wildcard never stands for a segment that hides a separator. `/auth/*` is never open;
- a rule that cannot be read makes the **whole label** unusable. Examples are a regular expression, a bad method, a path outside the `href`, or more than 8 rules. In this case, nothing is opened. The service is reported in the catalogue problems. The service is not listed until it is fixed. The gate keeps the path of its `href` closed: only the superadmin can open it;
- portal ignores the query string. portal records nothing in a session, because there is no session. The request is counted in `portal_forward_auth_requests_total{result="excluded"}`.

The Services settings page lists the rules of every service. The whole exposure can be audited in one place. A path that a rule opens is protected only by what the service does itself, for example by a webhook token. portal does not ask anything for these paths.

## Keys and limits

A key lets a program call a service for a user. Without the block `keys {}`, the keys are off: each address of the key pages answers 404.

- **Key text:** `mcp_`, 43 base62 characters and 6 characters of CRC32, 53 characters in all. The portal shows the text one time, when it makes the key. It keeps only the SHA-256 digest and the last 4 characters.
- **Key pages:** `GET /auth/keys` shows the keys. A user with the right `keys.own.create` makes a key on the page `GET /auth/keys/new`, with a name, an expiry date and grants. The grants of a key never go beyond the grants of its owner. The page of a new key has the editor of grants (see "Editor of grants"). The limit fields of a grant stand in the table of the ticked grants. A key has its own page of change, `GET /auth/keys/edit?id=<id>`, with the same editor. A refused form returns to the page of the new key, with the reason in a notice.
- **Edge:** `GET /auth/verify-key` is the ForwardAuth address for the key routes. It reads only memory. A good key gives status 200 and an identity token. A bad key gives status 401, the header `WWW-Authenticate: Bearer error="invalid_token"` and an empty body.
- **Rights of the portal:** the service `portal` has 8 actions: `keys.own.read`, `keys.own.create`, `keys.own.edit`, `keys.own.revoke`, `keys.own.use`, `keys.all.read`, `keys.all.revoke` and `users.read`. The ceiling `max_key_lifetime` limits the age of a key.
- **Limits:** a grant can hold limit values. A ceiling is a value that the service applies. A budget is a count in a window (`second`, `minute`, `hour` or `day`) that the PDP counts. The PDP counts one use for the key and for its owner, and it takes the unit from both counters or from none. When a budget is empty, the PDP answers `false` with the reason `budget_exceeded` and `retry_after`.
- **Counters:** the table `budget_counters` holds the counters. PostgreSQL uses a second pool of connections (`pdp.counter_connections`). `keys.max_counter_rows` limits the number of rows.
- **Revoke:** the command `portal revoke-keys --config FILE (--user LOGIN | --all)` revokes keys without the web page. With SQLite, it refuses to run when an instance runs.
- **Accounts of the file:** a block `user "<login>"` defines an account of the file, with optional blocks `key "<name>"`. A login comes from the file or from the database, never from both.

## Start and stop

The instance opens its ports only when these tasks end:

1. The directory is loaded and the counter rows exist.
2. The catalogue has the full list of the services.
3. The first read of the document of each service ended, or `access.start_read_deadline` passed.

The startup probe of the pod MUST wait longer than the sum of these times. The liveness probe reads `/healthz`, which never reads the database.

On SIGTERM, the instance serves 5 seconds more. Then it closes its listeners and tells each connection to close. It waits at most 10 seconds for the answers in progress. Set `terminationGracePeriodSeconds` to 20.

The instance reads the action documents at equal intervals, 64 at the same time at most. The catalogue reads the IngressRoutes and the pods in two lists, page by page, and it probes 64 services at the same time at most.

## Launcher

The launcher (`/`) shows one row for each group and one tile for each service. The launcher shows only the services that the viewer can open.

### State of a service

portal finds two states for each service:

- The health state: up or down. A declared `portal.health` URL gives it (HTTP probe). Otherwise, the runtime gives it: the state of the container (Docker), or the number of ready pods of `portal.pods` (K3s). One ready pod gives up.
- The runtime health: healthy, unhealthy or starting. The health check of the runtime gives it, when the runtime has one. It adds detail to the health state. It does not change it. The metric `portal_catalog_services_down` counts only the health state.

Each tile shows one state word. The word always tells the state, so the colour is never the only sign.

| Word | Meaning | Look |
|---|---|---|
| `down` | The health state is down. This word has priority over the runtime health. | Red pill. The whole tile is red. |
| `unhealthy` | The health check of the runtime fails. | Warning pill. |
| `starting` | The health check of the runtime has not passed yet. | Neutral pill. |
| `healthy` | The health check of the runtime passes. | Green dot, the same as `up`. |
| `up` | The health state is up. The runtime has no health check for the service. | Green dot. |
| no word | No check applies, or no check has answered yet. | None. |

The runtime health for each runtime:

- **Docker, Compose and Swarm.** The result of the `HEALTHCHECK` of the container. portal reads it from the list of the containers, the same request as the labels. A recent Engine API gives the member `Health.Status`. An older API gives the end of the `Status` text: `(healthy)`, `(unhealthy)` or `(health: starting)`. A container without `HEALTHCHECK` has no runtime health. A stopped container has no runtime health. A `portal.health` URL does not remove the runtime health.
- **Swarm.** A task container gets a new name for each task. portal uses the label `com.docker.swarm.service.name` as the origin when the label `com.docker.compose.service` is not there. Thus the origin of the service does not change from one task to the next.
- **K3s.** The readiness probes of the containers of the pods of `portal.pods`. portal reads them from the same pod list as the number of ready pods. This list holds the spec and the status of each pod. Thus the runtime health needs no other request and no other RBAC verb: `get` and `list` on `pods` are sufficient.
  - portal uses only the pods that have a container with a readiness probe. A terminating pod does not count. Without such a pod, the service has no runtime health.
  - A ready pod is healthy. A pod that is not ready is starting when its phase is `Pending`, when a container has `started` at `false`, or when a container waits. A container that waits with a reason other than `ContainerCreating` or `PodInitializing`, for example `CrashLoopBackOff`, makes its pod unhealthy. Another pod that is not ready is unhealthy.
  - One unhealthy pod makes the service unhealthy. The service is starting when a pod starts and no probed pod is ready. Otherwise, the service is healthy.
  - With a `portal.health` URL, portal does not read the pods. The service then has no runtime health.

### Group heads

Each row has a head. The head shows the name of the group, the number of services, and the badges `N down` (red) and `N unhealthy` (warning). When the viewer sees one group only, the head gives no useful data. The launcher then does not show it, unless the group has a down or an unhealthy service. The row keeps the name of the group as its accessible name. With two groups or more, each row has its head.

## Settings pages and history

The superadmin gets a gear on the launcher. The gear opens the settings. The settings are **one HTML page per tab**. This is the same design as the pages of clickhouse-dash. Each page has a header with **Back to panel**. Below the header is a row of underlined tab links. The current tab has a line under it. The row has no fill. The settings live under `/auth/settings`. Traefik already routes `/auth/` to the portal.

The pages work without script. The pages answer 404 to anyone who is not the superadmin in its own right. This applies to a user, and to the superadmin who browses as a user. A page carries only what it shows. The launcher carries none of it. The first element of each page is a skip link to the content. The settings stylesheet loads on the settings pages only.

Each page is a column of flat parts, with the density of clickhouse-dash. A part has a heading row with a title, a count in muted text and its controls at the right. Data is in hairline tables of 32 px rows. A state shows as a word in a pill, never as a colour alone. A description is the title of its item, or one muted line. Under a coarse pointer (touch), each control is 44 px. These are the pages:

1. **Users** (`/auth/settings`). A table of the accounts. The superadmin is the first row. Each row gives the login, a summary of the access, the last sign-in, the sessions and the refused sign-ins of the last 30 days. The summary tells how many services the account opens, how many actions it has on whole services, how many grants it has on resources, and how many grants to check. The login is a link to the page of the account. A red line above the table counts the refused sign-ins of 30 days, with a link to the refused attempts. **New user** is one link (`?add=1`). It shows the form above the table. The form also shows after a refused attempt. While the add form is open, it is the whole page: the list of the accounts is not shown. The add form has the login and the password on the top row. Under them is the editor of grants. A new account has no grant and sees no service until a grant is ticked. One request makes the account and its grants. In the mode `static`, the form has no editor. After the add, the portal opens the page of the new account. The page ends with **End all sessions**. See "End all sessions" below.
2. **Page of an account** (`/auth/settings/users/<login>`). This page has three parts:
   - The page of an account is one page with no tab. It has the editor of grants first, then the rights on the keys, then **Account**, then **Activity** (folded). The address `?service=<id>` chooses the service that shows first. One form sends every grant: **Save access** is in the head of the part and at its end. In the mode `static`, the page has no form. In the mode `hybrid`, a grant of the configuration has a lock and cannot change.
   - **Activity** (a part that folds under a head of one line) shows the connections per day over 30 days, the latest sessions (time, address, browser, duration, and how each session ended), and the services that the account used. The bars of the graph are neutral. The bar of today uses the accent. The peak day is written out. A graph is one SVG of three paths.
   - **Account** has **Change password** and **Delete** on one row. A new password ends the sessions of the account at once. **Delete** is a two-step action. The head of the page is one line with the way back to **Users**, the login and **Log in as**, then one line of facts: creation, last sign-in, sessions and refused sign-ins of 30 days.
   The **Generate** button fills a password field with a random 20-character password. The browser draws this password. The password is never sent anywhere before the form is submitted. The password generator is the only script of these pages. It loads with the add form and on the page of an account only. The page of the superadmin shows its activity only: the superadmin has every grant, and the configuration file gives its login and password.
3. **Give access** (`/auth/settings/give?user=<login>&service=<id>`). The page of a service uses this address. It redirects to the page of the account, with the service chosen in the editor and the grant `access` ticked. The page saves nothing until the person clicks **Save access**.
4. **Services** (`/auth/settings/services`). **Grants to check** comes first when there is one: the account (a link to its page), the grant and the problem. Then one table lists the services by group: the name and the id, the path and the paths open without sign-in, the accounts that have `access` (links), the number of actions, the resource types and the number of resources. Each row is one line. A list that can be long folds out under its count: the accounts, and the actions as small tags (the description is the title of a tag). A service whose actions document cannot be read has a warning icon, and its cell of actions folds out the message. A service without actions has `access` only: opening the service is then the full permission. The name of a service is a link to the page of the service (`/auth/settings/services/<id>`). That page shows **Who has access**: each account with a grant on the service, its state, its actions and its grants on resources, with **Edit**. **Give access to an account** is one list of the other accounts and one **Add** button. The button opens the editor with the box `access` ticked. A side block, **How a grant reads**, gives the three forms of a grant and the subtree match.
5. **Activity** (`/auth/settings/logins`). One strip gives the figures of the last 24 hours and the connections of 30 days. Then the page shows the sign-in log (see below). A side column shows the graph of the connections of all accounts and a table of the recent changes. A recent change names its account with a link to the page of the account.

A form of an account returns to the page of the account, or to the editor of the service, with the outcome in a notice. The notice is one code from a fixed list (`?s=<code>`). The address never carries text.

### Editor of grants

A service can have hundreds of actions and hundreds of resources. A box for each pair of an action and a node does not fit in a page. The forms of the keys and the forms of the accounts use one editor. The page holds the data of the editor as a JSON block (`<script type="application/json">`, which the content security policy does not run). The script `settings.js` draws the editor from this block.

At the left is the tree of the resources of a service, with a search field and the number of ticked grants of each node. A list of the services shows when the editor has more than one. At the right are the actions of the chosen node, in groups by the first part of the name. Each action has one line. A list of more than 60 actions starts with its groups closed. A filter and the button **Ticked** narrow the list. The box of a group ticks the actions that the list shows. An action that a grant above the node covers shows ticked, with the words "via the whole service" or "via" and the node. An action shows on the whole service only when its document lists the type `service`.

A ticked action shows its limit fields on its own line, with the largest value of the document as a hint. Each service has a row with a switch for the grant `access` and a count of its permissions. On a form of a key, the button **Same as mine** ticks all the permissions that the owner holds on that service.

The service lists its own resources. The editor shows the nodes of the member `resources` of the action document. When a node can have children that the document does not list, the editor asks the portal: `GET /auth/options?service=<id>&type=<type>&parent=<path>&q=<text>&after=<cursor>`. The portal asks the service at `/.well-known/procurauth/resources` (protocol 5.6), with an identity token for the person who fills the form. The portal makes this token only when the person can open the service. The service lists only what this person can see. The portal asks for 50 items at a time and keeps each answer for 60 seconds, for at most 5,000 entries and 32 MiB (portal 3.1c). The tree shows **Show more** under a page that has a next cursor. A search asks the service for the children of the chosen node. A service that has no such route gives no option. The page of the superadmin and the page of a user who makes keys use the route. The CSP allows `connect-src 'self'` for it.

The page also writes the ticked grants as hidden fields, with the names that the server reads: `grant<n>` and `limit<n>.<name>[.<window>]`. A browser without script sends these fields as they are, so a change that does not touch the grants keeps them. The form of a key also has a text field for each blank grant in a `<noscript>` element. A key holds 512 grants at most.

The portal accepts a document of 1,024 actions, 4,096 resources and 256 KiB by default. The keys `access.max_actions`, `access.max_resources` and `access.max_document_bytes` set these bounds. The minimum of each key is the number of protocol 5.3c.

### Look and feel

The pages share the look of the launcher: graphite panels with a thin border and a small upper-case title, controls of 34 to 36 px, one accent, a theme selector with three icons (System, Dark, Light) that the browser remembers. The file `web/portal.css` holds the tokens, the header, the buttons, the launcher and the sign-in card. The file `web/settings.css` holds the rules that only the settings pages use. The portal serves them at `/auth/style.css` and `/auth/settings.css`. The content security policy allows requests of the script from the same origin only.

The pages use these component names: `pagePart` (a panel), `dataTable`, `uiField`, `uiInput`, `uiBanner` and `uiState`. A time is a `<time class="when">` element. The server writes the time in UTC. The script `settings.js` shows the time of the browser instead and keeps the UTC text in the tooltip. A state (key, session, sign-in) is one icon with a hidden word for a screen reader. A form with the attribute `data-gate` keeps its submit buttons disabled until its fields are valid.

### Sign-in log

The program records each sign-in attempt that reaches the password check. The record has these items: the time (UTC, to the second); whether the attempt succeeded; the **login as typed**; whether that login names an account; the client address; and the browser. The page shows the figures of the last 24 hours. These figures are the attempts, the refused attempts, and the distinct addresses refused. The log shows the newest entries first. The page shows 50 rows for each page. The *Refused* filter uses `?r=failed`. The page number uses `?p=N`.

The password is never recorded. This applies to the database, the page and the logs. What a visitor types is hostile data. The program never uses this data raw. Control characters (newline, NUL, escape sequences) and characters that hide or reorder text (zero-width characters, right-to-left overrides, line separators) are kept as a visible `\u{..}` escape. The text is cut at 64 characters, with an ellipsis. The text is used only as a bound SQL parameter. The page HTML-escapes the text where it prints it. The CSP forbids inline script and style anyway. In the JSON logs (`login_failed`, `login_success` carry `login`), the text stays inside one escaped string, so it cannot forge a line. An attempt blocked by the rate limiter is counted in the metrics. It is not listed one by one. The log keeps at most 20,000 attempts (newest first). So a siege cannot fill the volume.

A browser that the gate refuses goes to a 403 page. The page does not give the name of the service. The page tells the user to ask the administrator for access. The page has the buttons **Back to panel** and **Sign out**.

**Log in as** gives the superadmin a session as the user. The session is flagged *impersonated*. It is not counted as a connection of that user. The history tags it *by the superadmin*. A banner offers **Back to superadmin**. This button needs no password. The impersonated session itself proves that the superadmin opened it. Account management is refused while the superadmin impersonates a user.

### End all sessions

Use this tool after an incident, for example after a suspected theft of a cookie. It ends every session of every account. The sessions of the superadmin end too. Accounts, permissions and history stay. Each user must sign in again.

There are two ways to start it:

- **Settings page.** Open the tab **Users**. Click **End all sessions** at the bottom of the page. The page shows a confirmation step. Click **Confirm: end all sessions**. Your own session ends. The portal sends the browser to the sign-in page. Only the superadmin in its own right can do this. The route is `POST /auth/users/end-sessions`. It needs the anti-CSRF token of the account routes and the field `confirm=end-all-sessions`. A request without the field, or with an extra field, gets 400 and changes nothing. A user, or the superadmin who browses as a user, gets 403.
- **Command.** Run `portal end-sessions --config FILE`. The command deletes every session in the shared database and exits. It prints the number of sessions that it ended. It needs a `postgres` block in the file. With SQLite the sessions are in the memory of the portal process, and a different process cannot end them. The command then fails and changes nothing. Restart the portal to end the sessions of a SQLite configuration. In K3s, run the command in a portal pod with the file `/etc/portal/config.hcl`.

Each use writes one audit event, `sessions_ended`. The page **Activity** shows it as "All sessions ended". The actor is the superadmin login for the page, and `cli` for the command. The history closes each session with the reason `revoked`. The metric `portal_user_admin_total{action="end_sessions"}` counts the page calls. A running instance stops accepting a session of a shared database within 5 seconds, because it caches a session for that time.

The history is stored in the database (SQLite or PostgreSQL). The program keeps the history for `storage.retention_days` days. The default is 90 days. The history contains these records:

- sessions: login, address, user agent, start, last seen, end, and the reason for the end;
- the services that each session used. The history holds counts per service. It never holds a path or a query;
- administrative events: users added or deleted, impersonation, all sessions ended, and failed sign-ins;
- the sign-in attempts above.

The per-account failed counters count only logins that name an account. A login that does not exist is counted apart. It appears in the sign-in log, as typed. A person sometimes types a password in the login field. This is the one item in this log that the log must not hold. That is why the log is short-lived. Only the superadmin can see it.

Requests only touch memory. One writer thread flushes every 15 seconds in a single transaction. An event is dropped rather than make a request slower. The program counts a dropped event in `portal_tracking_dropped_total`.

With SQLite, sessions that a previous run left open end with the reason *restart*. With PostgreSQL, sessions outlive a restart. Only a session whose instance stopped before it recorded its end is closed in this way. This happens after the session has been idle for longer than `session.idle_timeout`. Times are UTC.

```hcl
users {
  max_users = 64
}

storage {
  database       = "/var/lib/portal/portal.db"
  retention_days = 90
}
```

## Session signing secret

The key `session.secret_mode` has two values.

```hcl
session {
  secret_mode = "ephemeral"
}
```

`ephemeral` makes a new 32-byte secret from the OS CSPRNG at each start of the process. The file must not give a secret in this mode.

```hcl
session {
  secret_mode = "provided"
  secret_file = "/run/secrets/portal_session_secret"   # or: secret_env = "PORTAL_SESSION_SECRET"
}
```

`provided` takes the secret from `session.secret_env` or from `session.secret_file`. Give exactly one of the two. The secret has at least 32 bytes. The program refuses a secret that is in clear text in the file.

The secret signs the return targets of the authentication. It also signs the anti-CSRF tokens of the forms. With SQLite, the opaque sessions stay in the memory of the process in both modes. A restart therefore ends all sessions. With PostgreSQL, the sessions are in the database. `provided` is **required** with PostgreSQL. All instances must sign with the same key. If they do not, one instance refuses a form that another instance served. The program refuses `ephemeral` with the `postgres` block.

## Several instances (PostgreSQL)

By default, one process owns a SQLite file. The process keeps the sessions in its memory. So only one instance can be active. The optional `postgres` block moves all data to a shared PostgreSQL database. Then any number of instances can run at once. If one instance stops, the only cost is the requests that it served.

```hcl
session {
  secret_mode = "provided"                   # required: every instance signs with the same key
  secret_file = "/run/secrets/portal_session_secret"
}

storage {
  retention_days = 90                        # storage.database must be absent with the postgres block
}

postgres {
  host            = "postgresql-rw.data.svc.cluster.local"
  port            = 5432                     # default
  database        = "portal"
  user            = "portal"
  password_file   = "/run/secrets/portal_db_password"   # or: password_env = "PORTAL_DB_PASSWORD"
  pool_size       = 8                        # default 4, 1 to 64
  connect_timeout = "5s"                     # default, 1s to 60s
}
```

The client is a blocking PostgreSQL client. It uses plain TCP. It has no TLS stack and no C dependency. The connection stays inside the cluster. The server must accept `scram-sha-256` over a non-TLS connection from the pods. At startup, the first connection is retried for about one minute. Then the schema is created or updated under an advisory lock. Several instances that start together are therefore safe. A statement that runs for more than 15 s is cancelled. A broken connection is replaced, and the operation is retried once. This is what a failover of the database looks like.

This is what the instances share, and how:

- **Sessions** are rows of a `sessions` table. A row holds the digest of the cookie, the client binding, the identity, the expiry and the last use. A login writes the session and its line of the history in one transaction. Each instance keeps a copy of what it checked. The copy is kept for **5 seconds**. ForwardAuth asks the database about one session once per 5 s, not once per request. The binding (address and User-Agent), the idle timeout and the absolute lifetime are enforced exactly as with SQLite. The idle timeout is renewed at most every 5 s of activity. A logout, a deleted account or a password change ends the session in the database at once. The session then stops working on the other instances within 5 seconds (the copy). If the database cannot be reached, a session that was checked less than 2 minutes ago is still accepted. A failover therefore does not sign everybody out. New sign-ins fail with `503` in the meantime.
- **Superadmin sessions** also carry the credential generation of the superadmin. This value is an HMAC-SHA-256, under the session secret, of the superadmin login and password. The database never holds a plain hash of the password. An instance accepts a superadmin session or an impersonated session only with its own generation. At start, an instance deletes these sessions when they have another generation. Thus a new superadmin login, password or session secret ends the old superadmin sessions on every instance. Rotate the credential after a suspected theft of a superadmin cookie. During a rolling update, an old instance and a new instance refuse the superadmin sessions of each other. The sessions of the users stay.
- **Accounts** are read from the database every 5 seconds by every instance. An account that is created, edited or deleted on one instance reaches the others within 5 seconds. A login that the instance does not know yet makes it read the accounts again at once. This happens at most once per second. An account that was just created can therefore sign in on any instance. A session that was opened for this account elsewhere is never ended for that reason.
- **History** is written by every instance. The writes are batched every 15 s, as with SQLite. A session that ends by logout, revocation, expiry or idleness is recorded once. The instance whose call ended the session records it. The database gives each ended session to exactly one caller. The start or the stop of an instance never closes the sessions of the other instances.
- **Each instance has its own rate limits.** The failed-login limiters (per IP and global) are in memory. With two instances, an attacker gets up to twice the budget. A lockout on one instance does not lock the other instance. `session.max_active_sessions` is checked against the database. Two instances that race at the limit can each overshoot it by one.

Liveness and readiness are unchanged. `/healthz` does not touch the database. `/metrics` adds `portal_shared_store_errors_total{use="sessions|directory"}` and `portal_directory_age_seconds`. The second value is the time since the database last answered a read of the accounts. It grows when the refresh fails.

The instance that adds an account checks `users.max_users` against its own copy. Two instances that add accounts at the same moment can exceed the limit by one.

### Migration from SQLite to PostgreSQL

The command `portal import-sqlite --config FILE --from SQLITE_FILE` copies data to PostgreSQL. It copies these items: the accounts (with their grants), the sessions of the history, the service accesses, the audit events and the sign-in log. It copies them into the empty PostgreSQL database that the configuration names. The copy is one transaction. The command prints what it copied. The command refuses a target that already holds any of these items. In this case, it changes nothing. The command only reads the SQLite file. The SQLite file can be the live file. Sessions do not move. They were in the memory of the old process. The sessions that the file still shows as open end where they were last seen, with the reason *restart*. Every user signs in again after the switch. Changes made to the SQLite file after the import are not copied. Do not change accounts between the import and the switch.

## Inbox

The portal can run the inbox. The inbox sends alerts to phones. It makes sure that each phone shows each alert one time. The inbox starts when the file has a block `inbox`. Without this block, the portal has no inbox and opens no port for it.

The inbox is a module of the portal. It runs in the same process and in the same Tokio runtime. It starts and stops with the portal. It does not use the sessions or the directory of the portal. It delivers alerts when the rest of the portal is not ready. The wire protocol of the phones is protocol 4: enrollment with a one-time code, and the Noise IK session. The specification is in `docs/inbox/`.

```hcl
inbox {
  name            = "Inbox"                           # default
  server_key_file = "/run/secrets/inbox_server_key"   # or: server_key_env

  devices {
    listen                = "0.0.0.0:8080"
    public_address        = "inbox.example.org:8080"   # the address that phones use; no default
    trusted_proxies       = []
    heartbeat_interval    = "20s"                      # no default
    session_timeout       = "60s"                      # no default
    resend_after          = "15s"                      # no default
    max_inflight          = 32                         # no default
    unreachable_after     = "2m"                       # no default
    offline_warning_after = "3m"                       # no default
  }

  enrollment {                                         # optional block
    code_ttl               = "10m"                     # default
    max_active_per_account = 5                         # default
  }

  storage {
    delivery_ttl             = "7d"                    # no default
    alert_retention          = "30d"                   # no default
    connection_log_retention = "90d"                   # no default
  }

  group "infra" {
    name        = "Infra prod"
    description = "Production outages"
  }

  producer "grafana" {
    token_file = "/run/secrets/inbox_grafana_token"    # or: token_env
    groups     = ["infra"]
  }
}
```

The delivery values have no default. The program stops at start when one of them is missing. They are `heartbeat_interval`, `session_timeout`, `resend_after`, `max_inflight`, `unreachable_after`, `offline_warning_after`, `delivery_ttl`, `alert_retention` and `connection_log_retention`. A wrong default could hide an alert, so the operator chooses each value. The reference below lists every key.

### Enrollment of a phone

A phone enrolls with a one-time code. The code belongs to one account of the portal. The login of the account is the user name of the phone. The phone cannot change it.

1. The account makes a code with `POST /auth/inbox/api/enrollment-codes`. Any account that is signed in can use this call. The code is for the login of the session. The call has no login parameter.
2. The call gives `{"code": "inbox://enroll?s=...&k=...&c=...", "expires_at_ms": ..., "fingerprint": "..."}`. The code holds the key `inbox.devices.public_address` (`s`), the public key of the server (`k`) and a secret of 128 bits (`c`). The fingerprint is the first 8 hexadecimal characters of the SHA-256 digest of the public key.
3. The person types the code in the app, or taps it as a link. The app pins the key `k`. It connects to `GET /v1/enroll` and runs one Noise IK handshake with the prologue `inbox/v1/enroll`. The first message holds the secret and the name of the phone.
4. The inbox looks for the SHA-256 digest of the secret. A code works one time and lives `inbox.enrollment.code_ttl`. The inbox stores the phone and marks the code as used in one transaction. Then it sends message 2 and closes the connection.

The inbox keeps only the digest of the secret. An account has at most `inbox.enrollment.max_active_per_account` codes that are not used and not expired. The inbox refuses another code with the status `429`.

A refusal has no message 2. The inbox closes the WebSocket with the code 4410 when the code is unknown, used or expired, when it cannot read the message, and when the limit `inbox.devices.max_devices` is reached. The log has the reason in the event `enroll_refused`. The inbox closes with 4426 when the protocol is not 4, and with 4429 after too many attempts from one address. The inbox closes a connection that sends no message in 10 seconds. With a replica and `sync_alerts = true`, the inbox sends message 2 only after the copy holds the phone.

When the superadmin deletes an account, the inbox revokes the phones of the account and deletes its codes that are not used. The sessions of these phones close with the code 4403. If the inbox cannot do this, the account stays and the portal shows an error.

### Listeners

The inbox has its own listener, `inbox.devices.listen`. This listener has no session. Phones and producers prove themselves. It serves these routes:

| Route | Use |
|---|---|
| `GET /v1/enroll` | WebSocket: enrollment of a phone with a one-time code. |
| `GET /v1/session` | WebSocket: the session of an enrolled phone. |
| `POST /api/v1/alerts`, `GET /api/v1/alerts/{id}` | Producer API with a bearer token. |
| `POST /api/v1/webhooks/alertmanager/{group}`, `POST /api/v1/notify/{group}` | Webhooks with a bearer token. |
| `GET /healthz`, `GET /readyz`, `GET /api/version` | Health and version. |

`/readyz` answers `200` with PostgreSQL, also when the database has no writable primary (`degraded`). With SQLite it answers `503` when the file does not answer. After SIGTERM it answers `503` with `draining`. The cluster then removes the instance from its Service. `portal healthcheck` checks `/healthz` of the portal and `/readyz` of the inbox. `portal inbox healthcheck --config FILE` checks the inbox only.

With the block `inbox.cluster`, the inbox has a third listener for the other instances. Only the pods of the service must reach it. Each request carries an HMAC that the server key derives.

### Storage

The inbox uses the database of the portal. With SQLite, it uses the same file. With PostgreSQL, it uses the same database and the connection settings of the block `postgres`. There is no second `postgres` block. The key `inbox.storage.pool_size` sets the number of connections of the inbox.

The inbox and the portal have two independent chains of migrations. The portal keeps its version in `PRAGMA user_version` (SQLite) or in `schema_migrations` (PostgreSQL). The inbox keeps its version in the table `inbox_schema_version`. Each chain refuses a schema that is newer than its binary. The tables of the inbox are `devices`, `subscriptions`, `alerts`, `deliveries`, `connections`, `enrollment_codes` and, with PostgreSQL, `instances`.

With PostgreSQL, any number of instances can run. They share the phones, the alerts and the sessions through the database, `LISTEN` and `NOTIFY`. If the database has no writable primary, the inbox goes on: it accepts alerts in memory, it delivers them to the phones that are connected, and it writes them when the database is back. With the block `inbox.cluster`, the instances copy these alerts to each other before the producer gets its answer.

With SQLite, the block `inbox.replica` copies the whole file to a shared directory. The copy holds the tables of the portal too. An instance that owns the lease serves. A standby instance waits for the lease before it opens the file. It restores the newest copy that passes the consistency check. An instance that loses its lease stops the whole process. The metrics `inbox_replica_*` show the state of the copy.

### Commands

| Command | Use |
|---|---|
| `portal inbox keygen --out FILE` | Makes the key of the server. The file holds one line: the Noise key pair of the server. The command never overwrites a file. |
| `portal inbox token --out FILE` | Makes the token of a producer. |
| `portal inbox import-sqlite --config FILE --from SQLITE_FILE` | Copies a SQLite file of the inbox into the empty PostgreSQL database that the configuration names. The file can be an old file of the standalone inbox or a file of the portal. |
| `portal inbox healthcheck --config FILE` | Asks `/readyz` of the listener of the phones. |

### Admin calls

The standalone admin listener and the admin front do not exist any more. The calls of the inbox are temporarily under the `listen` of the portal, in `/auth/inbox/api/`. Only a session of the superadmin can use the admin calls. Any signed-in account can use the two calls that make a code. A request without a session gets `401`. Any other account gets `403` for an admin call. Without the block `inbox`, each call answers `404`.

| Call | Use |
|---|---|
| `GET /auth/inbox/api/csrf` | Gives the anti-CSRF token of the session. Any signed-in account can use it. |
| `POST /auth/inbox/api/enrollment-codes` | Makes an enrollment code for the login of the session. The body is `{}`. Any signed-in account can use it. |
| `GET /auth/inbox/api/overview` | Phones, groups, producers and the delivery statistics. |
| `GET /auth/inbox/api/alerts`, `GET /auth/inbox/api/alerts/{id}` | The alerts, and the delivery state of one alert. The query accepts `limit` and `since_ms`. |
| `GET /auth/inbox/api/connections` | The connection log. The query accepts `limit` and `user`. |
| `POST /auth/inbox/api/alerts` | Sends an alert. The body is `{"group", "title", "idempotency_key"}`. |
| `POST /auth/inbox/api/devices/{id}/revoke` | Revokes a phone and closes its session. |

A POST follows the rules of the other forms of the portal. The request has the `Origin` of the portal, the content type `application/json`, and the header `X-CSRF-Token` with the value of `GET /auth/inbox/api/csrf`. A later version adds rights and pages for these calls.

### Metrics

The metrics of the inbox are in the Prometheus text of the portal, at `metrics.path`. They have the names `inbox_*`. The gauges `inbox_devices_unreachable`, `inbox_oldest_pending_delivery_seconds`, `inbox_replica_last_snapshot_age_seconds` and `inbox_storage_degraded` are the ones to alert on. The inbox has no OTLP export. The log of the inbox is in the JSON lines of the portal, with the field `event`.

### Stop

On SIGTERM, the inbox tells each phone to reconnect with the close code 1012. It still serves for 5 seconds. Then it closes its listeners and waits for the sessions at most 10 seconds. It writes what the memory holds to the database. With the block `inbox.replica`, it takes a last copy and gives the lease back, so a standby instance takes over at once.

## Local test

The Compose file contains the HCL directly through `configs.content`. The superadmin login and password come from the host environment. Compose writes them to secret files. The database is in a named volume.

```bash
export PORTAL_SUPERADMIN_LOGIN='admin'
export PORTAL_SUPERADMIN_PASSWORD="$(head -c 18 /dev/urandom | base64)"
echo "$PORTAL_SUPERADMIN_PASSWORD"
docker compose up -d --build
```

The program refuses the example passwords that the repository publishes (`.env.example`, earlier versions of this section). Give a new random value.

Open this URL:

```text
http://localhost:4181/auth/login
```

Sign in as `admin` with the password that `echo` printed. The gear opens the settings pages.

Stop the stack with:

```bash
docker compose down
```

The local Compose file publishes port 4181 only on loopback. This is intentional. In production, the same binary runs behind Traefik. The port is not published publicly.

## HCL example

```hcl
server {
  listen_address     = "0.0.0.0:4181"
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
}

users {
  max_users = 64
}

access {
  grants_mode = "dynamic"
}

identity {
  issuer           = "https://portal.home.internal"
  signing_key_file = "/run/secrets/portal_identity_key"
}

pdp {
  listen_address = "0.0.0.0:4182"
  url            = "http://portal:4182"
}

storage {
  database       = "/var/lib/portal/portal.db"
  retention_days = 90
}

session {
  secret_mode         = "ephemeral"
  cookie_name         = "__Host-portal"
  ttl                 = "8h"
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

## Configuration

The program reads one HCL file. The HCL parser is a small, strict subset parser. Blocks hold `key = value` lines. A block has no nested block, with one exception: the block `inbox` holds the blocks `devices`, `storage`, `replica` and `cluster`, and the blocks with a label `group` and `producer`. A block can have a label (`user "alice" { ... }`). A value is a scalar or a list of quoted strings (`["a", "b"]`). The file is the only source of the configuration. The program reads an environment variable or a file only when the file names it.

### Rules

- **The schema is closed.** An unknown block, an unknown key, a duplicate block or a duplicate key stops the program at start.
- **An error stops the program before it serves.** The message gives the key, the values that the key accepts, and the value that the file gave. The message never shows a secret.
- **The program does not change a value.** It does not clamp a number. It does not trim a text. It does not change the case of a text.
- **The types are strict.** Write a number without quotes (`workers = 4`). Write a boolean without quotes, in lower case (`true` or `false`). Write every other value in quotes. A number in quotes or a word without quotes stops the program.
- **A unit is mandatory.** A duration ends with `ms`, `s`, `m`, `h` or `d`. A size ends with `B`, `KiB` or `MiB`.
- **A name of a choice has the exact case of the reference.** `Strict` is valid and `strict` is not. `warning` is not a log level.
- **A key that does not apply must not be in the file.** When `security.login_rate_limit_enabled` is `false`, the keys `login_attempts_per_ip`, `login_window` and `login_lockout` stop the program. The same rule applies to the global limiter. It also applies to `metrics.path` when `metrics.enabled` is `false`. It also applies to `storage.database` with the `postgres` block.
- **Two keys that contradict each other stop the program.** For example, `session.idle_timeout` is longer than `session.ttl`.

### Secrets: `_env` and `_file`

These attributes hold a secret: `authentication.superadmin_login`, `authentication.superadmin_password`, `session.secret`, `postgres.password`, `identity.signing_key`, the `token` of each `pdp_client` block, `inbox.server_key` and the `token` of each `producer` block. The file never holds the value. Give exactly one of these two forms:

| Form | The value is |
|---|---|
| `<name>_env = "VAR"` | the content of the environment variable `VAR` |
| `<name>_file = "/path"` | the content of the file, without its last line end |

The program stops at start in these cases:

- The file gives none of the forms, or both of them.
- The file gives the secret in clear text (`<name> = "..."`).
- The variable does not exist, or its value is empty.
- The file does not exist, cannot be read, or is empty. The path is not absolute, or it holds `..`.

The reference tells which attribute accepts `_env` and `_file`. Only these attributes accept them.

### Kubernetes API

The Kubernetes source needs the host and the port of the API of the cluster. The configuration names the two environment variables that hold them. The program reads no other variable. At start, both variables must exist.

```hcl
discovery {
  kubernetes          = true
  kubernetes_host_env = "KUBERNETES_SERVICE_HOST"
  kubernetes_port_env = "KUBERNETES_SERVICE_PORT_HTTPS"
}
```

The two `_env` keys are mandatory when `kubernetes` is `true`. They are not valid when `kubernetes` is `false`.

### Effective configuration

`portal check-config --config FILE` checks the file. It checks the variables and the files that the file names. Then it prints the value that applies to each key. It also prints the source of each value. It ends with `configuration ok`.

```text
server.workers = 4  # default
server.max_body_bytes = "8KiB"  # file
authentication.superadmin_login = <hidden>  # env:PORTAL_SUPERADMIN_LOGIN
session.secret = <hidden>  # file:/run/secrets/portal_session_secret
```

The source is `default`, `file`, `env:VAR` or `file:/path`. A secret always shows `<hidden>`. The superadmin login also shows `<hidden>`. The command prints only the keys that apply. The server writes the same values to its log at start. The event name is `config`. The server writes one line for each key.

### Configuration reference

This reference comes from the table of keys in `src/config/keys.rs`. Run `portal config-reference` to generate it again. A test fails when this text is not the generated text.

<!-- config-reference:begin -->
#### Block `server`

The HTTP server.

| Key | Type | Default | Description |
|---|---|---|---|
| `listen_address` | text: an IP address and a port, such as 0.0.0.0:4181 | required | The address and the port that the server listens on. |
| `workers` | number, 1 to 64 | `4` | The number of worker threads. |
| `max_body_bytes` | size, 1KiB to 1MiB | `"32KiB"` | The largest request body that the server reads. The form of a user holds one field for each grant that it shows. |
| `max_connections` | number, 1 to 100000 | `1024` | The largest number of open TCP connections. |
| `max_headers` | number, 8 to 256 | `64` | The largest number of headers in a request. |
| `max_header_bytes` | size, 8KiB to 1MiB | `"32KiB"` | The largest size of the headers of a request. |
| `header_read_timeout` | duration, 1s to 1m | `"10s"` | The time that a client has to send the request headers. |
| `body_read_timeout` | duration, 1s to 1m | `"10s"` | The time that a client has to send the request body. |
| `max_login_body_reads` | number, 1 to 100000 | `64` | The largest number of sign-in requests whose body the server reads at the same time. The server answers `503` at once to a sign-in request above this number. Slow sign-in bodies thus cannot hold all the connections that ForwardAuth needs. It must be less than `server.max_connections`. |
| `trusted_proxies` | list of at most 64 quoted strings: IP networks such as "10.42.0.0/16" or "127.0.0.1/32"; the address is the first address of the network | required | The networks of the proxies (Traefik) that set `X-Forwarded-For`. The server reads this header only from a TCP peer in these networks. It then takes the first address from the right that is not in these networks. From any other peer, the client address is the address of the peer. `[]` trusts no proxy. The client address keys the session binding and the login limiter. |

#### Block `authentication`

The superadmin: the one account that is not in the database.

| Key | Type | Default | Description |
|---|---|---|---|
| `superadmin_login` | secret: give `superadmin_login_env` (a variable name) or `superadmin_login_file` (an absolute path), not both | required | The login of the superadmin. It has 1 to 64 characters, and no upper-case letter, no space, no control character and no colon. The portal keeps the colon for the ids of keys. The server does not change it. A new login ends the sessions of the superadmin and the impersonated sessions. |
| `superadmin_password` | secret: give `superadmin_password_env` (a variable name) or `superadmin_password_file` (an absolute path), not both | required | The password of the superadmin. At start, the server hashes it with Argon2id and erases it from memory. It has at most `security.max_password_bytes` bytes. The server refuses the example values of `.env.example` and of the README. A new password ends the sessions of the superadmin and the impersonated sessions. |

#### Block `users`

The accounts in the database.

| Key | Type | Default | Description |
|---|---|---|---|
| `max_users` | number, 1 to 1024 | `64` | The largest number of accounts in the database. |
| `max_grants` | number, 1 to 30000 | `512` | The largest number of grants of one user. A key holds at most 512 grants, at any value of this key. A user with 3 grants for each service and the 8 actions of the service `portal` needs 3 x the number of services + 8 grants. |

#### Block `access`

The grants of the users, and the actions that the services publish.

| Key | Type | Default | Description |
|---|---|---|---|
| `grants_mode` | `static`, `dynamic`, `hybrid` | `"dynamic"` | Where the grants come from. `static`: only the `user` blocks; the settings pages show the grants and do not change them. `dynamic`: only the settings pages; a `user` block stops the server. `hybrid`: both; the grants of a `user` block are read-only in the settings pages. |
| `actions_timeout` | duration, 100ms to 30s | `"2s"` | The time that the server waits for the `/.well-known/procurauth/actions` document of one service. |
| `actions_refresh` | duration, 10s to 1d | `"5m"` | The time between two reads of the `/.well-known/procurauth/actions` documents. It is also the shortest time between two early reads of the document of one service, after an `invalid_resource` of the PDP. |
| `actions_max_age` | duration, 1m to 7d | `"1h"` | The longest time that the server keeps the last good document of a service after its last good read. After this time, the server acts as if the service had no good document. The edge also refuses each key for this service. The value must be more than `access.actions_refresh`. |
| `max_actions` | number, 64 to 4096 | `1024` | The largest number of actions in the document of one service. A larger document is bad. Protocol 5.3c asks a reader to accept 64 at least. |
| `max_resources` | number, 256 to 65536 | `4096` | The largest number of resources in the document of one service. A larger document is bad. Protocol 5.3c asks a reader to accept 256 at least. |
| `max_document_bytes` | number, 65536 to 4194304 | `262144` | The largest size, in bytes, of the document of one service. The server stops the read at this size, and the document is bad. Protocol 5.3c asks a reader to accept 64 KiB at least. The server keeps the last good document of each service in memory. |
| `start_read_deadline` | duration, 1s to 5m | `"5s"` | The longest time that the server waits, at start, for the first read of the documents of the services. After this time, the server opens its ports and reads the other documents in the background. |

#### Block `identity`

This block is optional. If it is present, the server gives an identity token to each service that it lets a user open.

| Key | Type | Default | Description |
|---|---|---|---|
| `header` | text: an HTTP header name of 1 to 64 characters: letters, digits and '-' | `"X-Identity-Token"` | The response header of `/auth/verify` that holds the identity token. Traefik copies it to the request with `authResponseHeaders`. |
| `issuer` | text: 1 to 256 visible ASCII characters, for example https://portal.example.internal | required | The claim `iss` of each identity token. Each service compares it with its `identity.issuer`. |
| `token_ttl` | duration, 10s to 1h | `"5m"` | The life of an identity token (`exp - iat`). It must not be more than the `identity.max_age` of the services. The server gives the same token again until half of its life is gone. |
| `signing_key` | secret: give `signing_key_env` (a variable name) or `signing_key_file` (an absolute path), not both | required | The Ed25519 private key that signs the identity tokens (algorithm `Ed25519`): a PKCS#8 key in PEM, or the base64 text of the PEM on one line. `portal identity-public-key` prints the public key for the services, as a JWK Set. |

#### Block `pdp`

This block is optional. If it is present, the server is an AuthZEN PDP on a second, internal listener.

| Key | Type | Default | Description |
|---|---|---|---|
| `listen_address` | text: an IP address and a port, such as 0.0.0.0:4182 | required | The address and the port of the PDP listener. Do not expose it through Traefik: only the services use it. |
| `url` | text: an http or https URL without a path, a query or a fragment | required | The URL at which the services reach the PDP. `/.well-known/authzen-configuration` gives the endpoints under this URL. |
| `max_body_bytes` | size, 1KiB to 1MiB | `"64KiB"` | The largest request body that the PDP reads. |
| `max_evaluations` | number, 1 to 1024 | `64` | The largest number of evaluations in one request to `/access/v1/evaluations`. |
| `plain_http` | `true` or `false` | `false` | Set it to `true` to accept an `http` value of `pdp.url`. Then the operator states that other means secure the link to the PDP. With `false`, an `http` URL stops the server. With `true`, an `https` URL also stops the server. With `true`, the start log has a warning. |
| `counter_connections` | number, 1 to 64 | `2` | The number of database connections that count the uses of the budgets. Each connection counts approximately 1,000 uses each second. A use that waits more than 500 ms gets the HTTP status 503. |

#### Block `keys`

This block is optional. If it is present, the server accepts keys: a user makes keys on the page Keys, and the edge and the PDP accept them. The block needs the blocks `identity` and `pdp`.

| Key | Type | Default | Description |
|---|---|---|---|
| `default_lifetime` | duration, 1d to 365d | `"30d"` | The lifetime that the form of a new key proposes. The form uses the lower of this value and the longest lifetime of the owner (`max_key_lifetime`). |
| `directory_max_age` | duration, 15s to 5m | `"30s"` | The longest age of the directory of the users and the keys. When the last good read of the database is older, the PDP answers `false` for each key and the edge refuses each key. The directory is read again every 5 seconds, so the minimum is 3 rounds. |
| `max_per_user` | number, 1 to 10000 | `100` | The largest number of active keys of one user. An expired key and a key that is too old do not count. |
| `max_counter_rows` | number, 1 to 100000000 | `2000000` | The largest number of counter rows. The server refuses a write that makes more rows. A file that needs more rows stops the server at start. One row is approximately 300 bytes. |
| `max_directory_grants` | number, 1 to 100000000 | `4000000` | The largest number of grants of the directory: the grants of the users and the keys. The server refuses a write that makes more grants. A file that needs more grants stops the server at start. One grant takes approximately 200 bytes of memory. |
| `history_days` | number, 1 to 3650 | `90` | The number of days that the server keeps the summary row of a revoked key. |

#### Block `storage`

The history, and the SQLite file.

| Key | Type | Default | Description |
|---|---|---|---|
| `database` | text: an absolute path without '..' | `"/var/lib/portal/portal.db"` | The path of the SQLite file. Applies only without the `postgres` block. |
| `retention_days` | number, 1 to 3650 | `90` | The number of days that the history keeps the sessions, the service accesses, the audit events and the sign-in attempts. |

#### Block `postgres`

This block is optional. If it is present, the server uses a shared PostgreSQL database in place of the SQLite file.

| Key | Type | Default | Description |
|---|---|---|---|
| `host` | text: a host name or an address of at most 253 characters | required | The host name or the address of the PostgreSQL server. |
| `port` | number, 1 to 65535 | `5432` | The TCP port of the PostgreSQL server. |
| `database` | text: 1 to 63 letters, digits, '.', '-' or '_' | required | The name of the database. |
| `user` | text: 1 to 63 letters, digits, '.', '-' or '_' | required | The name of the database user. |
| `password` | secret: give `password_env` (a variable name) or `password_file` (an absolute path), not both | required | The password of the database user. It is valid UTF-8. |
| `pool_size` | number, 1 to 64 | `4` | The number of connections that the server keeps to the database. |
| `connect_timeout` | duration, 1s to 1m | `"5s"` | The time that the server waits for a new connection. |

#### Block `session`

The sessions and the signing key.

| Key | Type | Default | Description |
|---|---|---|---|
| `cookie_name` | text: a cookie name that starts with __Host- | `"__Host-portal"` | The name of the session cookie over HTTPS. Over plain HTTP the server uses the same name without the prefix, because browsers refuse `__Host-` cookies there. |
| `ttl` | duration, 1m to 7d | `"8h"` | The lifetime of a session. |
| `idle_timeout` | duration, 1m to 7d | `"30m"` | A session that nobody uses for this time ends. The value is at most `session.ttl`. |
| `bind` | `ip_user_agent`, `user_agent`, `none` | `"ip_user_agent"` | What a session cookie is tied to: the client address and the User-Agent, the User-Agent only, or nothing. |
| `same_site` | `Strict`, `Lax` | `"Strict"` | The SameSite attribute of the session cookie. |
| `secret_mode` | `ephemeral`, `provided` | `"ephemeral"` | `ephemeral`: the server makes a new random signing key at each start. `provided`: the key comes from `session.secret_env` or `session.secret_file`. The `postgres` block needs `provided`, because all instances use the same key. |
| `secret` | secret: give `secret_env` (a variable name) or `secret_file` (an absolute path), not both | required | The signing key of the login forms, of the return links and of the anti-CSRF tokens. It has at least 32 bytes. The server refuses the example value of `.env.example`. A new value ends the sessions of the superadmin and the impersonated sessions. Applies only when `session.secret_mode` is `provided`. |
| `max_active_sessions` | number, 1 to 1000000 | `4096` | The largest number of open sessions. A login above this number gets the status 503. |

#### Block `security`

The limits and the login limiters.

| Key | Type | Default | Description |
|---|---|---|---|
| `max_password_bytes` | number, 8 to 16384 | `1024` | The largest size of a password. |
| `max_forwarded_uri_bytes` | number, 128 to 65536 | `4096` | The largest size of the forwarded URI. |
| `max_cookie_header_bytes` | number, 256 to 65536 | `8192` | The largest size of the Cookie header. |
| `login_rate_limit_enabled` | `true` or `false` | `true` | Turn the limiter of failed logins for each client address on or off. |
| `login_attempts_per_ip` | number, 1 to 1000000 | `5` | The number of failed logins for each client address in one window before the lockout. Applies only when `security.login_rate_limit_enabled` is `true`. |
| `login_window` | duration, 1s to 1d | `"1m"` | The window in which the failed logins of one client address count. Applies only when `security.login_rate_limit_enabled` is `true`. |
| `login_lockout` | duration, 1s to 1d | `"5m"` | The time that a client address stays locked out. Applies only when `security.login_rate_limit_enabled` is `true`. |
| `global_login_rate_limit_enabled` | `true` or `false` | `true` | Turn the global limiter of failed logins on or off. A good login does not clear it. |
| `global_login_attempts` | number, 1 to 1000000 | `100` | The number of failed logins from all clients in one window before the lockout. The superadmin login has a separate budget of the same size. Failures against other logins never lock out the superadmin. Applies only when `security.global_login_rate_limit_enabled` is `true`. |
| `global_login_window` | duration, 1s to 1d | `"1m"` | The window in which the failed logins of all clients count. Applies only when `security.global_login_rate_limit_enabled` is `true`. |
| `global_login_lockout` | duration, 1s to 1d | `"30s"` | The time that the logins of the budget stay locked out: all other logins, or the superadmin login. Applies only when `security.global_login_rate_limit_enabled` is `true`. |
| `max_rate_limit_entries` | number, 64 to 1000000 | `4096` | The largest number of client addresses that the limiter remembers. Applies only when at least one login limiter is on. |
| `max_concurrent_password_checks` | number, 1 to 64 | `1` | The largest number of Argon2 checks that run at the same time. |

#### Block `metrics`

The Prometheus endpoint.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | `true` or `false` | `true` | Turn the Prometheus endpoint on or off. |
| `path` | text: an absolute URL path of at most 128 bytes, not /healthz and not under /auth/ | `"/metrics"` | The URL path of the Prometheus endpoint. Applies only when `metrics.enabled` is `true`. |

#### Block `discovery`

The sources of the service catalogue.

| Key | Type | Default | Description |
|---|---|---|---|
| `interval` | duration, 2s to 1h | `"15s"` | The time between two reads of the sources. |
| `probe_timeout` | duration, 200ms to 30s | `"3s"` | The time that a health probe and a source request wait for an answer. |
| `kubernetes` | `true` or `false` | `false` | Read the IngressRoute annotations through the API of the cluster. The server needs the ServiceAccount of the pod. |
| `kubernetes_host_env` | text: the name of an environment variable | required | The environment variable that holds the host of the API of the cluster, for example `KUBERNETES_SERVICE_HOST`. At start, the variable must exist and must not be empty. Applies only when `discovery.kubernetes` is `true`. |
| `kubernetes_port_env` | text: the name of an environment variable | required | The environment variable that holds the HTTPS port of the API of the cluster, for example `KUBERNETES_SERVICE_PORT_HTTPS`. At start, the variable must exist and must hold a port number. Applies only when `discovery.kubernetes` is `true`. |
| `docker_socket` | text: an absolute path without '..' | none | The Docker socket that the server reads the container labels from. Without this key, the Docker source is off. |

#### Block `ui`

The pages.

| Key | Type | Default | Description |
|---|---|---|---|
| `icons_dir` | text: an absolute path without '..' | `"/icons"` | The directory of the offline icon library. If the directory is missing, the pages show initials. |

#### Block `logging`

The log.

| Key | Type | Default | Description |
|---|---|---|---|
| `level` | `error`, `warn`, `info`, `debug` | `"info"` | The lowest level that the server logs. |

#### Block `inbox`

This block is optional. If it is present, this server runs the inbox: it sends alerts to phones. The inbox uses the database of the portal. The blocks `inbox.devices` and `inbox.storage` are mandatory inside this block. The blocks `inbox.replica` and `inbox.cluster` are optional. The blocks `group "<id>"` and `producer "<name>"` are inside this block.

| Key | Type | Default | Description |
|---|---|---|---|
| `name` | text: 1 to 64 bytes, not blank | `"Inbox"` | The name that the phones show for this server. |
| `server_key` | secret: give `server_key_env` (a variable name) or `server_key_file` (an absolute path), not both | required | The key of the server for the phones. Make it one time with `portal inbox keygen`. A new key makes each enrolled phone and each enrollment code invalid. Back it up. |

#### Block `inbox.devices`

This block is inside the block `inbox`. It sets the listener for phones and producers, and the delivery values. This listener has no session: phones and producers prove themselves.

| Key | Type | Default | Description |
|---|---|---|---|
| `listen` | text: an IP address and a port, such as 0.0.0.0:8080 | required | The address and the port of the listener for phones, producers and webhooks. The listener serves `/v1/enroll`, `/v1/session`, `/api/v1/*`, `/healthz`, `/readyz` and `/api/version`. It must not use the port of `server.listen_address`. |
| `public_address` | text: the address that phones use to reach the listener `inbox.devices.listen`: `host:port`, a domain, or an http(s) URL; 1 to 512 bytes, no white space, no control character | required | The address that phones use to reach the listener `inbox.devices.listen`, as the phones see it from outside: `host:port`, a domain, or an http(s) URL. It is the `s` of the enrollment code. The inbox does not open a connection to it. |
| `trusted_proxies` | list of at most 64 quoted strings: IP networks such as "10.42.0.0/16" or "127.0.0.1/32"; the address is the first address of the network | required | The networks of the proxies that set `X-Forwarded-For` or `X-Real-IP` for this listener. The inbox reads these headers only from a TCP peer in these networks. `[]` trusts no proxy. The client address keys the limiter of the enrollment attempts. |
| `max_devices` | number, 1 to 100000 | `1000` | The largest number of enrolled phones that are not revoked. |
| `heartbeat_interval` | duration, 5s to 1h | required | The app sends a ping at this interval. |
| `session_timeout` | duration, 10s to 1d | required | A session without a frame for this time ends. The value is at least two times `inbox.devices.heartbeat_interval`. |
| `resend_after` | duration, 1s to 1d | required | The inbox sends an alert again to a phone that did not confirm the receipt after this time. |
| `max_inflight` | number, 1 to 1024 | required | The largest number of alerts that the inbox sends to one session before the phone confirms them. |
| `unreachable_after` | duration, 10s to 30d | required | The inbox reports a subscribed phone as unreachable when it did not see the phone for this time. The value is at least `inbox.devices.session_timeout`. |
| `offline_warning_after` | duration, 10s to 30d | required | The app warns its user after it was disconnected for this time. The value is at least two times `inbox.devices.heartbeat_interval`. |
| `max_auth_failures` | number, 1 to 1000 | `5` | The number of enrollment attempts of one client address in the window `inbox.devices.auth_failure_window`. Each started enrollment counts until it succeeds. |
| `auth_failure_window` | duration, 1s to 1d | `"10m"` | The window of `inbox.devices.max_auth_failures`. |
| `max_request_bytes` | size, 4KiB to 1MiB | `"64KiB"` | The largest request body that the listener reads. |

#### Block `inbox.enrollment`

This block is optional and is inside the block `inbox`. It sets the enrollment codes: an account of the portal makes a code, and a phone uses it one time to enroll. Every key has a default.

| Key | Type | Default | Description |
|---|---|---|---|
| `code_ttl` | duration, 1m to 1d | `"10m"` | An enrollment code works for this time after an account made it. A code works one time. |
| `max_active_per_account` | number, 1 to 100 | `5` | The largest number of enrollment codes of one account that are not used and not expired. The inbox refuses a new code above this number. |

#### Block `inbox.storage`

This block is inside the block `inbox`. It sets the lifetime of alerts and of the connection log. The inbox keeps its tables in the database of the portal: the SQLite file of the block `storage`, or the database of the block `postgres`.

| Key | Type | Default | Description |
|---|---|---|---|
| `delivery_ttl` | duration, 1m to 3650d | required | The inbox does not send an alert again to a phone that did not receive it within this time. |
| `alert_retention` | duration, 1h to 3650d | required | The inbox deletes the alerts, delivered or expired, that are older than this. The value is at least `inbox.storage.delivery_ttl`. |
| `connection_log_retention` | duration, 1h to 3650d | required | The inbox deletes the connection records that are older than this. |
| `pool_size` | number, 2 to 64 | `8` | The number of connections that the inbox uses for requests and sessions. The inbox keeps one more connection for the notifications of the other instances. Applies only with the `postgres` block. |

#### Block `inbox.replica`

This block is optional and is inside the block `inbox`. If it is present, the inbox copies the SQLite file to a directory that the standby instances share. A standby instance takes over when the owner is lost. The block needs the SQLite file: it must not be present with the block `postgres`. The copy covers the whole file, with the tables of the portal.

| Key | Type | Default | Description |
|---|---|---|---|
| `dir` | text: an absolute path without '..' | required | The directory that every instance that can run the inbox shares. It is a volume of plain files. It must not be the directory of `storage.database`. |
| `snapshot_interval` | duration, 1s to 1d | `"10s"` | The inbox copies a changed database at most this often. An accepted alert, an enrollment and a revocation do not wait for this time. |
| `lease_ttl` | duration, 5s to 1h | `"30s"` | The other instances consider an instance lost when its lease did not change for this time. They then take over. |
| `sync_timeout` | duration, 1s to 1m | `"5s"` | The time that an accepted alert waits for the copy that holds it. |
| `sync_alerts` | `true` or `false` | `true` | `true`: the inbox confirms an alert only after the copy holds it. If the copy fails, the inbox answers `503` and the producer sends the alert again. `false`: the inbox confirms an alert after the local commit and copies it as soon as possible. |

#### Block `inbox.cluster`

This block is optional and is inside the block `inbox`. If it is present, the instances copy to each other the alerts that they accept while PostgreSQL has no writable primary. The block needs the block `postgres`.

| Key | Type | Default | Description |
|---|---|---|---|
| `listen` | text: an IP address and a port, such as 0.0.0.0:8082 | required | The address and the port where this instance receives the requests of the other instances. Only the pods of the service can reach this port. |
| `peers` | list of at most 16 quoted strings: names with a port such as "inbox-peers.apps.svc.cluster.local:8082" | required | The names that list the instances, for example a headless Service, with the port of `inbox.cluster.listen`. The inbox asks each address that the names resolve to who it is. It skips its own address. The list has at least one name. |

#### Block `user "<login>"`

This block has a label and can occur more than one time. The grants of one user that the configuration gives (modes `static` and `hybrid`). The label is the login, as the server keeps it: 1 to 32 characters of a-z, 0-9, '.', '-' and '_'. The account itself is in the database. The superadmin has all grants and has no `user` block.

| Key | Type | Default | Description |
|---|---|---|---|
| `grants` | list of at most 30000 grants, each a quoted text or an object with `grant` and `limits`: a grant such as "access on service:grafana" or "query.run.write on service:clickhouse-dash/host:local/database:analytics": an action, the word on, service: and the id of the service, then 0 to 4 segments /type:id with each id percent-encoded. To give limit values, write { grant = "access on service:datastore", limits = { requests = [ { amount = 300, window = "minute" } ] } } | required | The grants of the user. The user is an account of the file, with no row in the database. `access` lets the user open the service. The other actions are the names that the service publishes at `/.well-known/procurauth/actions`. A grant names a node of the resource tree of the service: `<action> on service:<id>`, then 0 to 4 segments `/<type>:<id>`, for example `query.run.read on service:clickhouse-dash/host:local/database:analytics`. A grant covers its node and each node below it. A type starts with a-z, then has a-z, 0-9 and '_', 1 to 32 characters, and is not `service`. Each id is percent-encoded: each byte outside A-Z a-z 0-9 - . _ ~ is %XX with upper-case hex digits, for example `bucket:a%2Fb` for the id `a/b`. Another encoding of the same id stops the start. A decoded id has 1 to 256 bytes, without a control character. The action `access` has grants on the service only. The `limits` object holds one member for each limit. A ceiling is a whole number. A budget is a list of objects with `amount` and `window` (`second`, `minute`, `hour` or `day`). A budget value is only on the grant `access` on a service. A grant has at most 8 limit values. A value is at most 1,000,000,000. The limit values take at most 1024 bytes as compact JSON. The number of grants is at most `users.max_grants`. At start, the server checks only the form of each grant: the services and their trees of types are known at run time. The Services page of the settings shows a grant that does not follow the tree of its service. The PDP does not apply this grant. A block `user` stops the server when it gives `keys.own.create`, or an action that needs another action that it does not give. |
| `password_hash` | secret: give `password_hash_env` (a variable name) or `password_hash_file` (an absolute path), not both | none | The Argon2id digest of the password of the user, in the PHC string form, as the database keeps it. Without it, the account opens no session, and only its keys act for it. |

#### Block `key "<name>"`

This block has a label and can occur more than one time. One key of a user of the file. The block is inside a block `user`. The label is the name of the key: 1 to 64 characters, with no control character. Two keys of one user have two names. The subject id of the key is `key:` and the first 16 hexadecimal characters of the SHA-256 digest of the login, a zero byte and the name. A key of the file has no expiry date. To stop it, remove the block and restart each instance. The block needs the block `keys`.

| Key | Type | Default | Description |
|---|---|---|---|
| `grants` | list of at most 512 grants, each a quoted text or an object with `grant` and `limits`: the same form as `user.grants` | required | The grants of the key, in the form of `user.grants`. A key holds at most 512 grants. A grant of an action of the service `portal` stops the server. The decision for a key never gives more than the grants of its owner. |
| `key` | secret: give `key_env` (a variable name) or `key_file` (an absolute path), not both | none | The text of the key: `mcp_`, 43 characters and 6 characters of checksum. Give `key_env` or `key_file`, or give `key_sha256_env` or `key_sha256_file`: exactly one of the four. A bad form stops the server. |
| `key_sha256` | secret: give `key_sha256_env` (a variable name) or `key_sha256_file` (an absolute path), not both | none | The SHA-256 digest of the text of the key: 64 hexadecimal characters, in upper or lower case. A digest that the database or another block also holds stops the server. The digest must be the digest of a text of the form `mcp_`, 43 characters and 6 characters of checksum: the server cannot check this at start. |

#### Block `group "<id>"`

This block has a label and can occur more than one time. One group of alerts. The block is inside the block `inbox`. The label is the id of the group: 1 to 64 characters of a-z, 0-9, '-' and '_'. A phone subscribes to a group. A producer sends an alert to a group.

| Key | Type | Default | Description |
|---|---|---|---|
| `name` | text: 1 to 128 bytes, not blank | required | The name of the group that the phones show. |
| `description` | text: at most 512 bytes | `""` | The description of the group. |

#### Block `producer "<name>"`

This block has a label and can occur more than one time. One producer of alerts that has a token: a monitoring tool, a script or a webhook. The block is inside the block `inbox`. The label is the name of the producer: 1 to 64 characters of a-z, 0-9, '-' and '_'.

| Key | Type | Default | Description |
|---|---|---|---|
| `token` | secret: give `token_env` (a variable name) or `token_file` (an absolute path), not both | required | The bearer token of the producer. It has at least 32 printable characters. Make it with `portal inbox token`. Two producers must not have the same token. |
| `groups` | list of at most 64 quoted strings: ids of groups that the block `inbox` defines | required | The groups that the producer can send alerts to. The list has at least one group. Each group must exist. |
| `webhook_send_resolved` | `true` or `false` | `false` | `true`: the notifications "resolved" of Alertmanager and Grafana become alerts. `false`: the inbox ignores them. |

#### Block `pdp_client "<server>"`

This block has a label and can occur more than one time. One server that asks the PDP for decisions. The label is the name of the server: the id of its service in the catalogue, and the audience of its identity tokens. The PDP answers only for the resources of this server.

| Key | Type | Default | Description |
|---|---|---|---|
| `token` | secret: give `token_env` (a variable name) or `token_file` (an absolute path), not both | required | The bearer token of the server. It has at least 32 bytes. Two servers must not have the same token. |
<!-- config-reference:end -->

## HTTP protection

Hyper parses HTTP/1. The application configures bounded HTTP behavior:

- maximum simultaneous TCP connections;
- maximum number of request headers;
- maximum Hyper header buffer;
- header-read timeout;
- application body-size limit;
- body-read timeout;
- bounded concurrent Argon2 checks.

Dynamic `Location`, `Set-Cookie` and `Retry-After` values are converted to typed HTTP header values. An invalid value fails closed. The program does not emit it.

The crate uses `#![forbid(unsafe_code)]` for project code.

## CSRF and Origin/Referer verification

`POST /auth/login`, `POST /auth/logout` and the account routes (`POST /auth/users/add`, `/delete`, `/impersonate`, `/impersonate-end`, `/end-sessions`) require both:

1. a CSRF token. For login, the token is the double-submit cookie. The cookie lives 12 hours. When it is gone, the portal gives the sign-in page again with a new token and the sentence "The page was open too long" (status 403). For the other routes, the token is an HMAC of the session ID and of the purpose (`logout`, `users`). The form of the button carries the HMAC. The HMAC has no cookie that can expire under a page left open. A token for one action cannot be replayed on another action; and
2. a same-origin `Origin` check. The program uses `Referer` only when `Origin` is absent.

The account routes also check on the server that the session belongs to the superadmin. The session must not be an impersonated one. In K3s, these paths do not pass through ForwardAuth. portal therefore cannot rely on the gate.

The Sign out button sends a POST. This POST ends the session at once. `GET /auth/logout` only redirects to `/`.

When requests arrive through Traefik, the program builds the expected public origin from the unique `X-Forwarded-Proto` and `X-Forwarded-Host` values. For direct local access, the program uses `http` and the unique `Host` header. The program rejects these inputs: partial forwarded-origin metadata, duplicate headers, `Origin: null`, foreign origins, userinfo and invalid schemes.

Authentication pages return `Referrer-Policy: same-origin`. The Referer fallback stays available. Paths do not leak across origins.

## Opaque session behavior

Session cookies use:

```text
__Host-portal=<opaque random token>; Path=/; Max-Age=<ttl>; HttpOnly; SameSite=Strict; Secure   (HTTPS)
portal=<opaque random token>; Path=/; Max-Age=<ttl>; HttpOnly; SameSite=Strict                       (plain HTTP)
```

After **Log in as** or **Back to superadmin**, `Max-Age` is the time left: the new session ends when the session of the password sign-in would have ended. A switch never gives a fresh `session.ttl`.

The program reads the scheme for each request from `X-Forwarded-Proto`. A session is bound to the client address and the User-Agent of the login. The client address follows the rule of `server.trusted_proxies` (see "Anti-bruteforce"). The binding is set by `session.bind = "ip_user_agent"`. The other values are `user_agent` and `none`. A session ends after `session.idle_timeout` without use. The default is `30m`.

The token contains no identity, no expiration timestamp, no claims and no signed application data. The server stores only a SHA-256 digest of each token. It also stores the expiration time. With SQLite, this data is in memory. With PostgreSQL, this data is in the shared database.

The session store is bounded by `session.max_active_sessions`. If the store is full, a successful password check cannot create a session. The program returns `503`. It does not remove an arbitrary live session.

## Anti-bruteforce

Two independent controls run before Argon2. Each control is local to the process. With several instances, each instance has its own counters. See above.

- per-IP failed-password limiter;
- global failed-password limiter for distributed attacks.

The global limiter has two budgets of the same size. One budget is for the superadmin login. The other budget is for all other logins. Failures against other logins thus never refuse the sign-in of the superadmin. Failures against the superadmin login lock out only the superadmin. While a budget is locked out, the answer tells whether a typed login is the superadmin login. The superadmin login is not a secret: the identity tokens carry it.

The per-IP limiter uses the client address. The client address comes from `X-Forwarded-For` only when the TCP peer is in `server.trusted_proxies`. For any other peer, the client address is the address of the peer. A client that connects to the port directly thus cannot choose its address.

Only failed password checks use the failure budgets. A successful authentication clears the per-IP state. It does not clear the global failure history. This is intentional.

`max_concurrent_password_checks` separately limits the number of concurrent Argon2 operations.

## Prometheus

When enabled, `/metrics` exports Prometheus metrics with bounded cardinality. The metrics cover: HTTP status and route; login success and failure; CSRF and origin failures; IP and global rate limits; session checks; active sessions; session-capacity rejections; Argon2 concurrency; HTTP connections; sign-in bodies refused at the limit `server.max_login_body_reads` (`portal_login_body_rejected_total`); the state of the catalogue (`portal_catalog_complete`); and uptime.

Do not put client IPs, raw URIs, passwords, cookies or return targets into metric labels.

See `examples/prometheus.yml`.

## Global Traefik ForwardAuth

The project is designed for installation as an entryPoint-level middleware. A request to a protected application first reaches `/auth/verify`. Without a valid session, the service returns a redirect to `/auth/login`. The redirect contains a validated, HMAC-signed path and query as the return target. After a successful login, the service returns the browser to that target.

`/auth/verify` explicitly lets the public auth paths `/auth/login`, `/auth/logout` and `/auth/style.css`. This prevents a loop when the ForwardAuth middleware is global. `/auth/success` still requires a valid session.

Any other path that the portal receives redirects to the login page when there is no session. This applies to `/auth/…` and to the catch-all route. It applies whether or not the path exists. Once the user is signed in, the portal answers 404. In the Traefik routes, the catch-all runs ForwardAuth first. Then Traefik rewrites the path to `/__portal-fallback-404`. This keeps the portal's own endpoints from being exposed on arbitrary paths.

See:

- `examples/traefik-entrypoint-snippet.yaml`
- `examples/traefik-dynamic.yaml`
- `demo/`: a complete local stack with Traefik, the portal, a service with a front, and the routes for keys (`demo/README.md`)

## Absolute JS/CSS paths

The authentication cookie has `Path=/`. So same-origin resources carry the same session cookie. Examples are `/app.js`, `/css/app.css`, `/api/...` and `/images/...`. After login, these resources pass the global ForwardAuth check.

Authentication does not rewrite application paths. If an application mounted under `/testservice` emits `/app.js`, Traefik still needs a routing rule. The rule maps `/app.js` to the intended backend.

## Reproducible dependency resolution

After this Hyper migration, bootstrap `Cargo.lock` once on a development machine with Docker. Then commit it:

```bash
./scripts/update-lockfile.sh
```

From that point, `Cargo.lock` is part of the source tree. Review and commit it whenever dependencies change. Normal build paths never regenerate it:

```bash
cargo fetch --locked
cargo test --frozen
cargo build --release --frozen
```

The steps that compile and test the code use `--frozen`. These steps run after the dependencies are fetched. The Docker build follows the same pattern.

When you change `Cargo.toml` on purpose, run the same maintenance script. Review the lockfile diff that results. Commit the lockfile before CI or release.

## Native commands

```text
portal run --config FILE
portal check-config --config FILE
portal config-reference
portal identity-public-key --config FILE
portal healthcheck --config FILE
portal import-sqlite --config FILE --from SQLITE_FILE
portal end-sessions --config FILE
portal hash-password < password.txt
portal version
```

## Release

Start the GitHub `Release` workflow by hand from the GitHub Actions UI. Its version input is independent of the version in `Cargo.toml`. For example, `2.0.0` builds binaries that report `portal 2.0.0`. It publishes the `v2.0.0` assets.

Supported release targets:

```text
linux/amd64  -> x86_64-unknown-linux-musl
linux/arm64  -> aarch64-unknown-linux-musl
linux/arm/v7 -> armv7-unknown-linux-musleabihf
```
