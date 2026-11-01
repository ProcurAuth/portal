# Deploy

This directory holds the files that run the portal with the inbox. The portal is one binary. The inbox is a module of it.

| Path | Use |
|---|---|
| `config.example.hcl` | Annotated configuration of the portal with the inbox. Copy it to make your own file. |
| `compose/config.hcl` | Configuration of the local Compose stack. |
| `compose/init-secrets.sh` | Makes the secret files of the Compose stack. |
| `compose/docker-compose.ha.yml` | Optional. Two instances behind a small proxy. |
| `k3s/` | Kustomize manifests for K3s. They are files only. |

The file `docker-compose.yml` at the root of the repository is the local stack. The image is built on this machine. No command of this repository pushes an image to a registry.

## Local stack with Compose

The stack has the portal, PostgreSQL and the inbox. It is for development and tests. Docker is the only tool that you need.

1. Build the image: `tools/build.sh image`.
2. Make the secrets: `deploy/compose/init-secrets.sh`. The script writes six files to `deploy/compose/secrets/`. It never replaces a file.
3. Start the stack: `docker compose up -d`.
4. Open `http://127.0.0.1:4181/auth/login`. Sign in with the login and the password in the files `superadmin_login` and `superadmin_password`.
5. Stop the stack and delete the database: `docker compose down -v`.

The pages of the portal are on port 4181. The devices listener of the inbox is on port 8080. Phones, producers and webhooks use it. The admin pages of the inbox are in the portal. There is no admin port.

A producer sends an alert with the token in `deploy/compose/secrets/inbox_producer_token`:

```sh
curl -fsS -X POST http://127.0.0.1:8080/api/v1/alerts \
  -H "Authorization: Bearer $(cat deploy/compose/secrets/inbox_producer_token)" \
  -H "Idempotency-Key: test-1" -H "Content-Type: application/json" \
  -d '{"group":"infra","title":"Test"}'
```

A phone enrolls with a code. An account that is signed in makes the code in the inbox pages. There is no shared device password. The server key in `inbox_server_key` makes each code and each phone valid for this server. Keep a copy of it. A new key invalidates every phone.

The stack publishes its ports on `127.0.0.1`. To reach it from a phone or an emulator on another host, set `PORTAL_DEVICES_BIND=0.0.0.0`. Then set `inbox.devices.public_address` in `compose/config.hcl` to the address that the phone uses.

### Two instances

Run this command from the root of the repository:

```sh
docker compose -f docker-compose.yml -f deploy/compose/docker-compose.ha.yml up -d
```

The file adds a second instance and a Traefik proxy. Both instances share the database and the secrets. Traefik is the only service that publishes ports. The portal sees the address of the proxy as the client address. Use this variant to test two instances. It is not a model for production.

## K3s

The manifests in `k3s/` describe the target. This repository does not apply them. No command here needs access to a cluster. Render them with `kubectl kustomize deploy/k3s`.

| File | Content |
|---|---|
| `portal.yaml` | Service account, read-only rights, four Services, the Deployment with two replicas, the PodDisruptionBudget. |
| `ingress.yaml` | IngressRoutes of the pages, the tile of the launcher and the fallback route. |
| `edge.yaml` | DaemonSet `inbox-edge`: the relay of the devices listener on host port 8087. |
| `edge-dynamic.yaml` | Traefik configuration of the relay. |
| `network-policy.yaml` | Policies for the portal pods, the relay and the database. |
| `config.hcl` | Configuration of the portal. Kustomize makes the ConfigMap `portal-config` from it. |

### What the manifests expect

- The namespace `edge` exists. The namespace `data` has a CloudNativePG cluster with the Service `postgresql-rw`.
- A database `portal` and a role `portal` exist on that cluster. The role has a password.
- The Traefik of the cluster has the middlewares `forward-auth` and `secure-headers` in the namespace `edge`.
- Nodes that run the portal have the label `home.server=true`. Change `nodeSelector` and `tolerations` for other nodes.
- The pod network is `10.42.0.0/16`. Change `trusted_proxies` in `config.hcl` for another network.
- The image `portal:5.0.0` is in the registry of the cluster. Change `images` in `kustomization.yaml` to the name that your registry uses.

### Secret

Create the Secret `portal-secret` in the namespace `edge` before the pods start. Each key becomes a file in `/run/secrets`.

| Key | Content | How to make it |
|---|---|---|
| `superadmin_login` | Login of the superadmin | Choose it. |
| `superadmin_password` | Password of the superadmin | Make a long random value. |
| `session_secret` | Key that signs the forms. At least 32 bytes. | Make a random value. All instances share it. |
| `db_password` | Password of the role `portal` | The same value as in the database. |
| `identity_key` | Ed25519 key of the identity tokens | `openssl genpkey -algorithm ed25519` |
| `inbox_server_key` | Noise key of the inbox server | `portal inbox keygen --out FILE` |
| `inbox_alerting_token` | Token of the producer `alerting` | `portal inbox token --out FILE` |

Add a key `pdp_token_<name>` for each block `pdp_client` that you add to the configuration.

```sh
kubectl -n edge create secret generic portal-secret \
  --from-file=superadmin_login --from-file=superadmin_password --from-file=session_secret \
  --from-file=db_password --from-file=identity_key --from-file=inbox_server_key \
  --from-file=inbox_alerting_token
```

### Configuration

Edit `config.hcl` before you render the manifests. Check it with dummy secrets: `tools/dev/check-configs.sh`. Read the values that you must change in the comment at the top of the file. The delivery values of the inbox have no default. The program stops at start if one is absent.

A new `config.hcl` gives a new name to the ConfigMap. The pods then roll.

### Probes and stop

- The startup probe waits up to 5 minutes for the port 4181. The portal opens it when the catalogue is complete.
- The readiness probe reads `/readyz` of the devices listener. It answers `503` at SIGTERM. The Services then stop to send requests.
- The liveness probe reads `/healthz` of the portal. It never reads the database.
- `terminationGracePeriodSeconds` is 30. The portal and the inbox serve 5 seconds more, wait at most 10 seconds for the sessions, and then write to the database.
- The update has `maxUnavailable: 0`. The PodDisruptionBudget lets one pod leave at a time.

### Network

The devices listener has no session. Phones prove themselves with the Noise session. Producers send a bearer token. The DaemonSet `inbox-edge` relays port 8087 of every server to the Service `portal-devices`. The relay sets `X-Forwarded-For` to the real client address. Phones use `inbox.devices.public_address`, which must name a relay.

The pages of the inbox are under `/auth/settings/inbox` in the portal. ForwardAuth does not protect them. The portal checks the session itself. The portal lists the tile **Inbox** in the launcher by itself, for an account that has a permission of the service `inbox`.

### Variant with SQLite

This variant is not in the manifests. Use it only when no PostgreSQL cluster exists. It has one active instance and standby instances. Replace the blocks `postgres` and `inbox.cluster` with a block `storage`. Add `inbox.replica` and a volume that all instances share. See the sections "Inbox" and "Storage" of the README.

## Check the files

- `tools/dev/check-configs.sh` runs `portal check-config` on every configuration in this directory. It uses dummy secrets.
- `tools/dev/smoke.sh` starts the Compose stack in a temporary directory. It checks the health routes, a producer alert and the metrics. It then deletes the stack.
