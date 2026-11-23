# Getting started

This page takes you from a clean machine to a portal that works, in about five minutes. Docker is the only tool that you need. The first path runs the portal alone. The second path runs the portal behind Traefik with example services.

## Path 1: the Compose stack

The stack has the portal with the Inbox, and PostgreSQL. It binds its port to `127.0.0.1`. It is for development and tests.

1. Clone the repository and go to it: `git clone https://github.com/ProcurAuth/portal && cd portal`.
2. Build the image: `tools/build.sh image`.
3. Make the secrets: `deploy/compose/init-secrets.sh`. The script writes the secret files to `deploy/compose/secrets/`. It never replaces a file.
4. Start the stack: `docker compose up -d`.
5. Open `http://127.0.0.1:4181/auth/login`. Use the login and the password from the files `superadmin_login` and `superadmin_password` in `deploy/compose/secrets/`.
6. Stop the stack and delete its database: `docker compose down -v`.

The portal on port 4181 serves the pages, the API of the senders and the routes of the phones. No proxy is in front of it, so no ForwardAuth applies. [`deploy/README.md`](../deploy/README.md) describes the stack, the variant with two instances and the K3s manifests.

### Send a first alert

The stack makes an API key for a sender. Post an alert to the group `infra`:

```sh
curl -fsS -X POST http://127.0.0.1:4181/api/v1/alerts \
  -H "Authorization: Bearer $(cat deploy/compose/secrets/inbox_api_key)" \
  -H "Idempotency-Key: first-1" -H "Content-Type: application/json" \
  -d '{"group":"infra","title":"My first alert"}'
```

The answer is status `201`. The alert shows in the tab Inbox of the portal. A phone that follows the group shows it too. See [Inbox](inbox/README.md) to enroll a phone.

## Path 2: the demo with Traefik

The demo runs the portal behind Traefik with example services, three users and sample grants. It is the closest local setup to a real deployment.

1. Run `./demo/up.sh`. It builds the image, makes the secrets, starts the stack and seeds the data.
2. Open `http://localhost:8088/`.
3. Read the logins and the passwords in `demo/.secrets/credentials.txt`.
4. Run `./demo/smoke.sh` to check the gate, a ceiling, a budget, a key and the Inbox from a terminal.
5. Stop the demo and delete its data: `docker compose -f demo/docker-compose.yml down -v`.

[`demo/README.md`](../demo/README.md) describes the accounts, the services and the checks.

## Your first service

A service lists itself in the launcher with labels. For a Docker Compose service, add these labels. The router rule must match the path of `portal.href`.

```yaml
services:
  wiki:
    image: example/wiki
    labels:
      portal.name: "Wiki"
      portal.href: "/wiki/"
      traefik.http.routers.wiki.rule: "PathPrefix(`/wiki`)"
      traefik.http.routers.wiki.middlewares: "portal-user@file"
```

Then open the settings pages as the superadmin, and give the grant `access on service:wiki` to a user. The user then sees the tile in the launcher and can open the service. A user with no grant sees nothing. See [Launcher and service discovery](launcher.md) and [Grants and the gate](grants-and-gate.md).

## Next steps

- Put portal behind Traefik with TLS: [Traefik and TLS](traefik.md).
- Write your own configuration file: [Configuration](configuration.md). The program checks the file at start and refuses an unknown key.
- Give a key to an agent: [Keys and limits](keys-and-limits.md).
- Run it for real: [Operations](operations.md) and [Security model and hardening](security.md).
