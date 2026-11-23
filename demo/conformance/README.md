# Conformance run of the service notes

This directory runs the ProcurAuth conformance series U against the demo service `notes`. The series U tests a PEP: the service side of the protocol. The runner is `access-conformance` of the ProcurAuth repository. Nothing in this directory changes the demo that runs, and nothing calls a real portal.

The portal itself is a PDP and an edge, not a PEP. Its own conformance runs are the series D and E. [Conformance](../../docs/conformance.md) describes them.

## Files

| File | Content |
| --- | --- |
| `notes-profile.json` | The profile of `notes` for the runner. It gives the operations that `notes` has. |
| `notes-refusal-profile.json` | A second profile for the refusal run (see below). |
| `run-conformance-u.sh` | The script that starts `notes` and the runner in Docker. |

## Run the series U

1. Put the ProcurAuth checkout next to the portal checkout, or set the variable `PROCURAUTH_DIR` to its path.
2. Set the ProcurAuth checkout to the commit `c820b4ac0e37c4769394876105fd549a6c8b38cc` (the tag `v1.1`).
3. Run `demo/conformance/run-conformance-u.sh` from the portal checkout.
4. Read one line for each case and the summary line on the standard output.
5. To run the refusal cases, add the option `--refusal-cases`.
6. To skip the build of the runner, add the option `--no-build`. The script then uses the runner of the last build and does not check its commit.

The old variable `PROCURAUTH` applies when `PROCURAUTH_DIR` is not set. The script does not change the ProcurAuth checkout. It stops with the exit code 2 when the checkout is at another commit. Without `--no-build`, the script builds the runner from this commit, and the runner holds the cases of this commit. Thus, without `--no-build`, a change in the ProcurAuth repository never changes the result by itself. The continuous integration pins the same commit for the series D and E.

The option `--format json` gives one JSON document. The option `--cases U04,U62` runs only these cases.

| Exit code | Result |
| --- | --- |
| 0 | The runner ran, and no case failed. |
| 1 | The runner ran, and a case failed. |
| 2 | A problem of the set-up: a bad option, no ProcurAuth checkout, another commit of ProcurAuth, an absent tool or a bad profile. |
| 124 | The runner did not stop in the time limit (840 seconds in the container, 900 seconds in all). |

A build or a Docker command that fails stops the script with the code of that command.

## How the run works

The script uses the image `portal-demo-notes` of the demo. It builds this image with `demo/docker-compose.yml` only when the image is absent. The container mounts `demo/notes/app.py` of the checkout, so a change of the code needs no new image.

`notes` runs in a container with no network (`--network none`). The runner runs in the network namespace of this container. Thus the two programs share only the loopback address. The runner has three roles:

- It is the client. It sends the requests of the operations of the profile to `127.0.0.1:8080`.
- It is the edge. It signs the identity tokens with the test key of ProcurAuth, `crates/access-conformance/examples/test-edge-ed25519.pem`.
- It is the PDP. It listens on `127.0.0.1:8181`, and it answers with the script of each case.

The script gets the public key of the test edge from the private key at each run. It gives this key to `notes` in a temporary file. It gives the credential `crates/access-conformance/examples/pdp-credential.txt` to the two sides. The profile reads the ProcurAuth files at `../procurauth/`, the path that the script uses in the container of the runner.

The values of the profile agree with `notes`. These are the service `notes`, the header `X-Identity-Token`, a maximum age of 300 seconds and a clock skew of 30 seconds. The `kid` is the RFC 7638 thumbprint of the test key, as the portal writes it.

## Result

This directory records no count of passed cases. A count belongs to one commit of ProcurAuth and to one version of `notes`, so it becomes old. Run the profile to get the count for your checkout. The runner skips a case for these reasons:

| Reason | Skipped cases |
| --- | --- |
| `notes` has no MCP endpoint. | The cases of the MCP binding. |
| `notes` has no use that needs two permissions. | The cases of two permissions. |
| `notes` has no action without a budget (operation `use_plain`). | Most cases of the identity token. |
| `notes` has no use on the service itself (operation `use_service`). | The cases of the service node. |
| `notes` trusts one Ed25519 key only, so no token can use ES256. | The case of ES256. |
| The profile has no operation `use_no_action`: the route `/notes/` asks no `access`, so this step fails (see [Limits of the result](#limits-of-the-result)). The profile also has no operation `read_health`. | The steps of U127: the health probe and a route with no action (protocol 10.1c). |

## The refusal run

Each action of `notes` lists the budget `requests`. Thus the profile cannot give the operation `use_plain`, and many cases skip. These cases hold most checks of the identity token (protocol 9.4).

The profile `notes-refusal-profile.json` maps `use_plain` to the read of a notebook, which is a use with a budget. The option `--refusal-cases` runs only the cases whose steps expect a refusal: HTTP 401 before any question, HTTP 403, or HTTP 503. A budget does not change the result of such a step. The script lists these cases in `run-conformance-u.sh`.

This run does not replace a true action without a budget. The other cases with `use_plain` expect a use that runs, so they stay untested.

## Limits of the result

Protocol 1.1 (10.1c) gives two rules for a route that runs no action of the action document. A health-probe route gives no data of the service. It needs no token, and the PEP asks no question for it. Each other route needs a valid identity token, and the PEP asks the action `access` on the service. A static file follows the second rule too.

The series U does not test these points of `notes`:

- The route `/health` answers `{"ok": true}` with no token and no question. This text gives no data of the service, so `/health` is a health-probe route of 10.1c. The portal reads it directly, without the edge, for the health of the service.
- The route `/notes/` gives the static page with no token and no question. This does not agree with 10.1c: the route must ask `access` on the service.
- The route `/notes/api/whoami` checks the token, but it asks the PDP no question. This does not agree with 10.1c: the route must ask `access` on the service.
- The playground `/notes/api/pdp` takes the action and the chain from the client (protocol 4.5). It runs no use, and it does not show `context.reason`. It asks the question of the client, not the question `access` of 10.1c.
- `notes` opens a new connection for each question (protocol 10.7, a SHOULD rule).

`notes` is a demo of the portal, not a reference PEP. The `reference-service` of the ProcurAuth repository passes the series U. The script `tools/dev/pep-e2e.sh` runs the series U against it.
