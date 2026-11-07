# Deploy

This directory holds the files that run the portal with the Inbox. The portal is one binary. The Inbox is a feature of it.

| Path | Use |
|---|---|
| `config.example.hcl` | Annotated configuration of the portal with the Inbox. Copy it to make your own file. |
| `compose/config.hcl` | Configuration of the local Compose stack. |
| `compose/init-secrets.sh` | Makes the secret files of the Compose stack. |
| `compose/docker-compose.ha.yml` | Optional. Two instances behind a small proxy. |
| `k3s/` | Kustomize manifests for K3s. They are files only. |

The file `docker-compose.yml` at the root of the repository is the local stack. The image is built on this machine. No command of this repository pushes an image to a registry.

## Local stack with Compose

The stack has the portal with the Inbox, and PostgreSQL. It is for development and tests. Docker is the only tool that you need.

1. Build the image: `tools/build.sh image`.
2. Make the secrets: `deploy/compose/init-secrets.sh`. The script writes six files to `deploy/compose/secrets/`. It never replaces a file.
3. Start the stack: `docker compose up -d`.
4. Open `http://127.0.0.1:4181/auth/login`. Sign in with the login and the password in the files `superadmin_login` and `superadmin_password`.
5. Stop the stack and delete the database: `docker compose down -v`.

The portal is on port 4181. This one port serves the pages, the API of the Inbox for the senders and the webhooks (`/api/v1/*`), and the phones (`/v1/enroll` and `/v1/session`). The Inbox is a feature of the portal, so it has no port of its own. The page of the Inbox is in the portal, in the tab Inbox. The portal knows nothing about TLS. No proxy is in front of this stack, so no ForwardAuth and no TLS apply.

A sender sends an alert with the API key in `deploy/compose/secrets/inbox_api_key`. This key is the block `api_key "local"` of `compose/config.hcl`. Its owner is the account of the file `local`, which holds `alert.send` on the group `infra`:

```sh
curl -fsS -X POST http://127.0.0.1:4181/api/v1/alerts \
  -H "Authorization: Bearer $(cat deploy/compose/secrets/inbox_api_key)" \
  -H "Idempotency-Key: test-1" -H "Content-Type: application/json" \
  -d '{"group":"infra","title":"Test"}'
```

A phone enrolls with a code. An account that is signed in makes the code in the tab Inbox, with the plus button. There is no shared device password. The server key in `inbox_server_key` makes each code and each phone valid for this server. Keep a copy of it. A new key invalidates every phone.

The stack publishes its port on `127.0.0.1`. To reach it from a phone or an emulator on another host, set `PORTAL_BIND=0.0.0.0`. Then set `inbox.devices.public_address` in `compose/config.hcl` to the address that the phone uses. This publishes the pages too: use it for tests only.

### Two instances

Run this command from the root of the repository:

```sh
docker compose -f docker-compose.yml -f deploy/compose/docker-compose.ha.yml up -d
```

The file adds a second instance and a Traefik proxy. Both instances share the database and the secrets. Traefik is the only service that publishes a port. It has the middleware `inbox-retry` on `/v1/`, `/api/v1/` and `/api/version`. The portal sees the address of the proxy as the client address. Use this variant to test two instances. It is not a model for production.

## K3s

The manifests in `k3s/` describe the target. This repository does not apply them. No command here needs access to a cluster. Render them with `kubectl kustomize deploy/k3s`.

| File | Content |
|---|---|
| `portal.yaml` | Service account, read-only rights, three Services, the Deployment with two replicas, the PodDisruptionBudget. |
| `ingress.yaml` | IngressRoutes of the pages, of the Inbox (phones, senders and webhooks, no ForwardAuth, with a retry), of the launcher and the fallback route. |
| `network-policy.yaml` | Policies for the portal pods and the database. |
| `config.hcl` | Configuration of the portal. Kustomize makes the ConfigMap `portal-config` from it. |

### What the manifests expect

- The namespace `edge` exists. The namespace `data` has a CloudNativePG cluster with the Service `postgresql-rw`.
- A database `portal` and a role `portal` exist on that cluster. The role has a password.
- The Traefik of the cluster has the middlewares `forward-auth` and `secure-headers` in the namespace `edge`.
- Nodes that run the portal have the label `home.server=true`. Change `nodeSelector` and `tolerations` for other nodes.
- The pod network is `10.42.0.0/16`. Change `trusted_proxies` in `config.hcl` for another network.
- The image `portal:5.0.3` is in the registry of the cluster. Change `images` in `kustomization.yaml` to the name that your registry uses.

### Secret

Create the Secret `portal-secret` in the namespace `edge` before the pods start. Each key becomes a file in `/run/secrets`.

| Key | Content | How to make it |
|---|---|---|
| `superadmin_login` | Login of the superadmin | Choose it. |
| `superadmin_password` | Password of the superadmin | Make a long random value. |
| `session_secret` | Key that signs the forms. At least 32 bytes. | Make a random value. All instances share it. |
| `totp_key` | Key that encrypts the TOTP secrets: 64 hexadecimal characters. All instances share it. | `openssl rand -hex 32` |
| `db_password` | Password of the role `portal` | The same value as in the database. |
| `identity_key` | Ed25519 key of the identity tokens | `openssl genpkey -algorithm ed25519` |
| `inbox_server_key` | Noise key of the inbox server | `portal inbox keygen --out FILE` |
| `inbox_alerting_key` | API key of the block `api_key "alerting"`. The owner is an account of the file. | `portal inbox token --out FILE` |

Add a key `pdp_token_<name>` for each block `pdp_client` that you add to the configuration.

```sh
kubectl -n edge create secret generic portal-secret \
  --from-file=superadmin_login --from-file=superadmin_password --from-file=session_secret --from-file=totp_key \
  --from-file=db_password --from-file=identity_key --from-file=inbox_server_key \
  --from-file=inbox_alerting_key
```

### Configuration

Edit `config.hcl` before you render the manifests. Check it with dummy secrets: `tools/dev/check-configs.sh`. Read the values that you must change in the comment at the top of the file. The delivery values of the inbox have no default. The program stops at start if one is absent.

A new `config.hcl` gives a new name to the ConfigMap. The pods then roll.

### Probes and stop

- The portal opens the port 4181 at once when the file has the block `inbox`. It serves the routes of the Inbox (`/v1/*`, `/api/v1/*`, `/api/version`, `/readyz`) at once, and answers `503` for every other route until the catalogue is complete. An alert and a phone never wait for the catalogue.
- The startup probe reads `/healthz` of the portal and waits up to 5 minutes. It answers `200` when the catalogue is complete.
- The readiness probe reads `/readyz` of port 4181. It answers `200` `ready`, or `200` `degraded` with PostgreSQL and no primary. It answers `503` at SIGTERM. The Services then stop to send requests. The phones get the close code 1012 at once and connect to another instance.
- The liveness probe reads `/healthz` of the portal. It never reads the database.
- `terminationGracePeriodSeconds` is 30. The portal and the inbox serve 5 seconds more, wait at most 10 seconds for the sessions, and then write to the database.
- The update has `maxUnavailable: 0`. The PodDisruptionBudget lets one pod leave at a time.

### Network

The Inbox is a feature of the portal. It has one port and one domain.

- **The IngressRoute `portal-inbox`** sends `/v1/*`, `/api/v1/*` and `/api/version` to the Service `portal` without ForwardAuth. A phone has no session: it proves itself with the Noise handshake inside the WebSocket. A sender has no session: it sends an API key, and the Inbox checks it. Traefik carries a WebSocket upgrade with path routing, so a second port is not needed.
- **The middleware `portal-inbox-retry`** sends a request again (4 attempts, 100 ms apart at first) when a node is lost and the Service still names its pod. Traefik retries only before the first byte of the answer. An upgrade that got `101` is never sent again. The `ServersTransport` `portal-inbox` has a dial timeout of 1 second, so the retry is quick.
- **Phones** use `inbox.devices.public_address`. Set it to the public base URL of the portal: `https://portal.example.org`. A path prefix is possible (`https://host/prefix`), but the portal has no base path. The proxy must remove the prefix before it sends the request.
- **Separate exposure** needs no second port. Restrict `/v1/` by source address or by rate with a Traefik middleware on the route `portal-inbox`. Keep the pages on routes with ForwardAuth.

The portal reads the client address from `X-Forwarded-For` for the pages, the senders and the phones. It trusts the header from the networks in `server.trusted_proxies` only. The middleware `forward-auth` of the cluster must keep `trustForwardHeader: false`. Set `true` only when each hop in front of Traefik overwrites or removes the `X-Forwarded-Uri` and `X-Forwarded-Method` headers of the client. Otherwise a client chooses the request that `/auth/verify` checks (see `SECURITY.md`). The Inbox has no list of its own. The portal has no TLS code: Traefik terminates TLS. Use an HTTPS `inbox.devices.public_address`.

The page of the Inbox is `/auth/settings/inbox` in the portal. ForwardAuth does not protect it. The portal checks the session itself. The launcher has no tile for the Inbox. The gear of the launcher opens the page.

### Variant with SQLite

This variant is not in the manifests. Use it only when no PostgreSQL cluster exists. It has one active instance and standby instances. Replace the blocks `postgres` and `inbox.cluster` with a block `storage`. Add `inbox.replica` and a volume that all instances share. See the sections "Inbox" and "Storage" of the README.

## Check the files

- `tools/dev/check-configs.sh` runs `portal check-config` on every configuration in this directory. It uses dummy secrets.
- `tools/dev/smoke.sh` starts the Compose stack in a temporary directory. It checks the health routes, an alert that an API key of the file sends, and the metrics. It then deletes the stack.
