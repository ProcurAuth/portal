# Conformance run of the service notes

This directory runs the ProcurAuth conformance series U against the demo service `notes`. The series U tests a PEP: the service side of the protocol. The runner is `access-conformance` of the ProcurAuth repository. Nothing in this directory changes the demo that runs, and nothing calls a real portal.

## Files

| File | Content |
| --- | --- |
| `notes-profile.json` | The profile of `notes` for the runner. It gives the operations that `notes` has. |
| `notes-refusal-profile.json` | A second profile for the refusal run (see below). |
| `run-conformance-u.sh` | The script that starts `notes` and the runner in Docker. |

## Run the series U

1. Put the ProcurAuth checkout next to the portal checkout, or set the variable `PROCURAUTH` to its path.
2. Run `demo/conformance/run-conformance-u.sh` from the portal checkout.
3. Read one line for each case and the summary line on the standard output.
4. To run the refusal cases, add the option `--refusal-cases`.
5. To skip the build of the runner, add the option `--no-build`.

The exit code is the code of the runner: 0 when no case failed, 1 when a case failed, and 2 for a usage error. The option `--format json` gives one JSON document. The option `--cases U04,U62` runs only these cases.

## How the run works

The script uses the image `portal-demo-notes` of the demo. It builds this image with `demo/docker-compose.yml` only when the image is absent. The container mounts `demo/notes/app.py` of the checkout, so a change of the code needs no new image.

`notes` runs in a container with no network (`--network none`). The runner runs in the network namespace of this container. Thus the two programs share only the loopback address. The runner has three roles:

- It is the client. It sends the requests of the operations of the profile to `127.0.0.1:8080`.
- It is the edge. It signs the identity tokens with the test key of ProcurAuth, `crates/access-conformance/examples/test-edge-ed25519.pem`.
- It is the PDP. It listens on `127.0.0.1:8181`, and it answers with the script of each case.

The script gets the public key of the test edge from the private key at each run. It gives this key to `notes` in a temporary file. It gives the credential `crates/access-conformance/examples/pdp-credential.txt` to the two sides. The profile reads the ProcurAuth files at `../procurauth/`, the path that the script uses in the container of the runner.

The values of the profile agree with `notes`: the service `notes`, the header `X-Identity-Token`, a maximum age of 300 seconds and a clock skew of 30 seconds. The `kid` is the RFC 7638 thumbprint of the test key, as the portal writes it.

## Result

With `notes-profile.json`, the runner gave 34 passed, 0 failed and 65 skipped cases of the 99 cases that existed then. This result is out of date. The series U has 117 cases now (U100 to U117 for section 17). The demo service `notes` has not run them. Run the profile again before you use a count. The runner skips a case for these reasons:

| Reason | Cases |
| --- | --- |
| `notes` has no MCP endpoint. | 24 |
| `notes` has no use that needs two permissions. | 2 |
| `notes` has no action without a budget (operation `use_plain`). | 36 |
| `notes` has no use on the service itself (operation `use_service`). | 2 |
| `notes` trusts one Ed25519 key only, so no token can use ES256. | 1 |

## The refusal run

Each action of `notes` lists the budget `requests`. Thus the profile cannot give the operation `use_plain`, and 36 cases skip. These cases hold most checks of the identity token (protocol 9.4).

The profile `notes-refusal-profile.json` maps `use_plain` to the read of a notebook, which is a use with a budget. The option `--refusal-cases` runs only the 21 cases whose steps expect a refusal: HTTP 401 before any question, HTTP 403, or HTTP 503. A budget does not change the result of such a step. The run gives 21 passed cases.

This run does not replace a true action without a budget. The 15 other cases with `use_plain` expect a use that runs, so they stay untested.

## Limits of the result

The series U does not test these points of `notes`:

- The routes `/health` and `/notes/` answer without a token. Protocol 10.0 (step 2) refuses each route except the action document. The portal reads `/health` directly, without the edge.
- The route `/notes/api/whoami` asks the PDP no question (protocol 10.1).
- The playground `/notes/api/pdp` takes the action and the chain from the client (protocol 4.5). It runs no use, and it does not show `context.reason`.
- `notes` opens a new connection for each question (protocol 10.7, a SHOULD rule).
