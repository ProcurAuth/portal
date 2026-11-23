<p align="center">
  <img src="docs/assets/banner.svg" alt="portal: the gate, the keys and the inbox for your services" width="100%">
</p>

<p align="center">
  <a href="https://github.com/ProcurAuth/portal/actions/workflows/ci.yml"><img src="https://github.com/ProcurAuth/portal/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/ProcurAuth/portal/releases/latest"><img src="https://img.shields.io/github/v/release/ProcurAuth/portal?color=4338ca&label=release" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-lightgrey" alt="Licence: MIT OR Apache-2.0">
  <img src="https://img.shields.io/badge/profile%20of-AuthZEN%201.0-0ea5e9" alt="Profile of AuthZEN 1.0">
  <img src="https://img.shields.io/badge/ProcurAuth-1.1-4338ca" alt="ProcurAuth 1.1">
  <img src="https://img.shields.io/badge/binary-static%20(musl)-34d399" alt="Static binary">
</p>

<p align="center">
  <a href="https://procurauth.github.io/portal/"><b>Website</b></a> ·
  <a href="https://procurauth.github.io/portal/documentation.html">Documentation</a> ·
  <a href="https://procurauth.github.io/portal/configuration.html">Configuration</a> ·
  <a href="https://procurauth.github.io/portal/security.html">Security</a> ·
  <a href="https://github.com/ProcurAuth/portal/releases/latest">Downloads</a>
</p>

# portal

portal is one program for the web services of a lab. It opens sessions for people and guards each service. It gives keys to AI agents, answers AuthZEN questions and sends alerts to Android phones.

It is for people who run their own services behind Traefik and want one account system, one gate and one inbox. **A key never has more rights than the person who owns it.** A service does not manage users or keys. It gets a short identity token and asks portal what the key may do.

portal is the backend, the edge and the decision point of [ProcurAuth](https://github.com/ProcurAuth/procurauth). It has one binary, one configuration file and one database (SQLite, or PostgreSQL for several instances).

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/screenshots/launcher-dark.png">
    <img src="docs/assets/screenshots/launcher-light.png" alt="The launcher of portal: a grid of the services that the signed-in user may open" width="90%">
  </picture>
</p>

## How it works

Traefik asks portal about each request. portal answers from memory with the grants of the person. For an agent, portal checks the key and gives the service an identity token. The service asks the decision point one AuthZEN question and applies the limits in the answer.

```mermaid
flowchart LR
    P([Person]) -- session --> T[Traefik]
    A([AI agent<br/>key mcp_...]) -- key --> T
    T -- ForwardAuth --> G["portal :4181<br/>gate, accounts, keys"]
    G -- identity token --> T
    T -- request and token --> S[Service]
    S -- AuthZEN question --> D{"portal PDP :4182"}
    D -- decision and limits --> S
    G --- DB[(SQLite or PostgreSQL)]
    D --- DB
```

<p align="center">
  <img src="docs/assets/architecture.svg" alt="Architecture of portal: clients reach Traefik, Traefik asks portal on port 4181, services ask the PDP on port 4182, one database holds the state" width="100%">
</p>

| Part | What it gives |
|---|---|
| **Launcher** | A page of tiles for the services of the lab. A service lists itself with a label (Docker Compose) or an annotation (Kubernetes). |
| **Gate** | A Traefik ForwardAuth server. A grant is an action on a node of the resource tree of a service. A new user has no grant. |
| **Accounts** | Argon2id passwords, opaque sessions bound to the client, a second factor (TOTP), and a log of sign-ins. |
| **Keys and limits** | `mcp_` keys for agents and `api_` keys for senders. A key follows its owner. Ceilings and budgets limit it. |
| **Decision point** | An AuthZEN PDP on its own port. It signs identity tokens and counts budgets. |
| **Approvals** | A grant can ask a person before each use by a key. The person answers on a phone or on a web page. |
| **Inbox** | Alerts, questions and approvals for the Android app Portal Inbox, on a Noise session. Each phone shows each alert one time. |
| **Configuration** | One strict HCL file. An unknown key stops the program. A secret comes only from `_env` or `_file`. |

## Example

Alice has the grant `query.run.read` on the host `local` of the service `db`. Her agent has a key with a ceiling `max_rows` of 100. The service `db` asks one question:

```json
{ "subject":  { "type": "user", "id": "key:9b1c5d0e" },
  "action":   { "name": "query.run.read" },
  "resource": { "type": "database", "id": "analytics",
                "properties": { "ancestors": [ { "type": "service", "id": "db" },
                                               { "type": "host", "id": "local" } ] } },
  "context":  { "usage": { "units": 1 } } }
```

portal answers that the key may read, with the ceiling and the count of the budget:

```json
{ "decision": true, "context": { "limits": { "max_rows": 100 }, "usage": { "units": 1 } } }
```

When the budget is spent, the answer is `{ "decision": false, "context": { "reason": "budget_exceeded", "retry_after": 12 } }`. The grant on the host covers the database below it. The key holds only what its owner holds.

## See it

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/screenshots/settings-grants-dark.png">
    <img src="docs/assets/screenshots/settings-grants-light.png" alt="The grant editor for one account: the tree of resources of a service and the permissions of the chosen node" width="90%">
  </picture>
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/screenshots/approval-dark.png">
    <img src="docs/assets/screenshots/approval-light.png" alt="The approval of an agent action: the command, its effect hash, the explanation of the agent and the buttons Approve and Decline" width="90%">
  </picture>
</p>

## Quick start

Docker is the only tool that you need.

```sh
git clone https://github.com/ProcurAuth/portal && cd portal
tools/build.sh image                  # builds the image on this machine
deploy/compose/init-secrets.sh        # makes the secret files
docker compose up -d                  # starts portal and PostgreSQL on 127.0.0.1:4181
```

Open <http://127.0.0.1:4181/auth/login>. Use the login and the password from the files `superadmin_login` and `superadmin_password` in `deploy/compose/secrets/`. Stop the stack and delete its data with `docker compose down -v`.

For a stack with Traefik, example services, three users and grants, run `./demo/up.sh`. See [Getting started](docs/getting-started.md).

## The ProcurAuth family

portal is a program of the [ProcurAuth](https://github.com/ProcurAuth) organisation. The organisation holds the open specification and the programs that follow it. The [implementations page](https://procurauth.github.io/procurauth/implementations.html) lists them, with the results of the conformance runs.

## Documentation

The [site](https://procurauth.github.io/portal/) and the `docs/` folder have the same pages. The Markdown files are the source.

| Page | Content |
|---|---|
| [Overview](docs/overview.md) | What portal is and how it is built |
| [Getting started](docs/getting-started.md) | Compose stack, demo, first service |
| [Architecture](docs/architecture.md) | Listeners, flows, layout of the repository |
| [Authentication and sessions](docs/authentication.md) | Accounts, sessions, second factor |
| [Grants and the gate](docs/grants-and-gate.md) | The resource tree, grants, the decision of the gate |
| [Identity tokens and the PDP](docs/identity-and-pdp.md) | Tokens, AuthZEN endpoints, approvals |
| [Keys and limits](docs/keys-and-limits.md) | Key kinds, ceilings, budgets |
| [Launcher and service discovery](docs/launcher.md) | Labels, annotations, health |
| [Settings pages and history](docs/settings-and-history.md) | Users, grants, keys, activity |
| [Several instances](docs/several-instances.md) | PostgreSQL and high availability |
| [Traefik and TLS](docs/traefik.md) | The duties of the proxy |
| [Inbox](docs/inbox/README.md) | Alerts, agents, approvals, the Android app |
| [Configuration](docs/configuration.md) | Every key, generated from the code |
| [Security model](docs/security.md) | Trust, invariants, hardening |
| [Operations](docs/operations.md) | Metrics, backups, upgrades |
| [Build and release](docs/build-and-release.md) | Build, test, release |
| [Reference](docs/reference.md) | HTTP endpoints and error codes |
| [Limitations](docs/limitations.md) | Known limits |
| [Changelog](CHANGELOG.md) | History of the versions |

## Build and test

Use Docker. Do not install Rust on your machine.

```sh
tools/dev/lint.sh     # format and clippy
tools/dev/test.sh     # the tests, with a throwaway PostgreSQL
```

## Security

Report a vulnerability in private: <https://github.com/ProcurAuth/portal/security/advisories/new>. Do not open a public issue. Read [SECURITY.md](SECURITY.md) for the scope and the response times, and [Security model](docs/security.md) for the model.

## Contribute

Read [CONTRIBUTING.md](CONTRIBUTING.md) and the [code of conduct](CODE_OF_CONDUCT.md). The tests and the checks run in Docker with `tools/dev/lint.sh` and `tools/dev/test.sh`.

## Licence

portal is licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
