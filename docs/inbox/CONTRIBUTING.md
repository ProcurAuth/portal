# Contribute

This page is for the whole repository: the portal, the Inbox, the shared crates and the Android app.

## Rules

- Write code, comments and documents in English.
- Write documents in ASD-STE100 Simplified Technical English. A procedure sentence has 20 words at most. A descriptive sentence has 25 words at most.
- Keep a paragraph to 6 sentences at most. Run `python3 tools/dev/ste-check.py FILE` on each document that you change. The script reports the long sentences, the long paragraphs, the -ing forms and the phrasal verbs. It needs only Python 3.
- Keep each patch small and focused. Do not format code that you do not change.
- Give each new requirement of `docs/inbox/SPEC.md` an id and a testable verb. Put the test name in the table of section 6.12 when you write the test.
- Update the tests and the documents with each change of behavior. The documents are the root `README.md`, `SECURITY.md`, `deploy/README.md`, `docs/design-inbox.md` and the files of `docs/inbox/`.
- Change `protocol.md` when you change the protocol. Raise `PROTOCOL_VERSION` when the change is not compatible.
- The delivery guarantees are not negotiable. A change MUST NOT let an accepted alert be lost or shown two times.
- A new dependency needs a reason. Prefer the standard library and the platform.
- The configuration stays strict. A new key goes in the table of keys in `src/config/keys.rs`. The reference in the root `README.md` comes from this table.
- Do not publish an image to a registry. Do not add a workflow step that does.
- Do not write a real server name or a secret in a file of the repository.

## Checks

All commands run in Docker. The host needs only Docker.

| Check | Command |
|---|---|
| Format and clippy | `tools/dev/lint.sh` |
| Tests | `tools/dev/test.sh --workspace --no-fail-fast` |
| End-to-end tests of the Inbox on PostgreSQL | `INBOX_E2E_BACKEND=postgres tools/dev/test.sh --test inbox_e2e` |
| STE limits of a document | `python3 tools/dev/ste-check.py docs/inbox/SPEC.md docs/design-inbox.md` |
| Configuration files of `deploy/` | `tools/dev/check-configs.sh` |
| Image and Compose stack | `tools/build.sh image` then `tools/dev/smoke.sh` |
| Portal against the real reference PEP of ProcurAuth | `tools/dev/pep-e2e.sh` (see "End-to-end check with the real PEP") |
| Portal against the real ClickHouse Dash | `INTEROP_DASH_BIN=<binary> tools/dev/interop-dash.sh` (see "Joint check with ClickHouse Dash") |

Set `PORTAL_SLOT=<name>` to keep the build output of a worktree apart from the others. To regenerate the configuration reference, run `portal config-reference` and put the output between the markers of the root `README.md`. A test fails when the text is not the generated text.

## Test layout

The helpers of the end-to-end tests are in `tests/common/mod.rs`. They start the test server, open the sessions of the accounts, make the HTTP calls and play the phone side of protocol 4. Each file of `tests/` that needs them declares `mod common;`.

The tests of the inbox are in `tests/inbox_e2e.rs`. The tests of items are in `tests/items_e2e.rs`. The conformance series D and E are in `src/http/conformance_tests.rs`. They use the harness of `src/http/cluster_tests.rs`. Add a new helper to `tests/common/mod.rs`, not to a test file.

The tests of the preferences are in three places. The file `tests/prefs_e2e.rs` tests the page, `set_prefs` and the quiet flag with a real phone. The file `src/inbox/items_tests.rs` tests the digest for both databases, with a clock that the test sets. The file `src/http/prefs_tests.rs` tests the frames that the session builds. The pure rules of the time zones and of the due times are in `src/inbox/prefs.rs`. A test of the digest never waits: it sets the clock.

The tests of the agents are in two places. The file `src/inbox/agents_tests.rs` tests the resolution, the limits, the actions of the owner, the lock and the purge for both databases. It uses the rig of `src/inbox/items_tests.rs`. The file `src/http/agent_binding_tests.rs` tests the path of the PEP: the intent gives the agent, and a submission with no intent goes to the bucket. The pure rules (the token, the names, the hints and the binding of a call to an item) are in `src/inbox/agents.rs`.

The tests of the agent channel are in `src/http/agent_who_tests.rs`. They run on both databases with the rig of `src/http/agent_channel_tests.rs`. They test the token, the header, the label, the hints, the text that holds a token, the flood and the codes of the agents. The black-box cases A01 to A10 are in `tests/mcp_contract.rs`.

The tests of the approval API of the services are in `src/http/approval_api_tests.rs`. They have a recorder that keeps each answer that the service gets. Add a new answer to the check of `check_what_the_service_learned`, not a new way to read it. The tests of the approval gate of the PDP are in `src/http/pdp/approval_tests.rs`. The file `src/http/pdp_rig.rs` starts the Inbox for them and for the cases with approvals, and it writes approvals in any state. A test service needs no declaration: each action can ask.

## End-to-end check with the real PEP

The tests in `src/http/approval_api_tests.rs` use a client that records the answers. The script `tools/dev/pep-e2e.sh` uses the real PEP: the reference service of the ProcurAuth repository. Run it when you change the PDP, the approval API or the agent channel.

### What the script builds

The script builds a stack of Docker containers. It never touches another stack. It opens ports on `127.0.0.1` only. It pushes no image to a registry.

| Part | What it is |
|---|---|
| `portal` | The portal, built from this checkout, with `inbox.items`, `inbox.at_rest`, `inbox.approvals` and `inbox.mcp`. |
| `service` | `reference-service`, built from the ProcurAuth checkout. The checkout is a read-only build context. |
| `tap` | A proxy on the link from the service to the PDP. It records each request and each answer. It never writes the token of the service. |
| `sock` | A fake Docker socket. It lets the portal find the service, its actions and its path. |
| `postgres` | PostgreSQL, only with `PEP_E2E_BACKEND=postgres`. SQLite is the default. |

The portal advertises the tap as its PDP URL. The service checks this URL in the metadata of the PDP. The content key is in its own directory, outside the data volume.

### What it runs

The test `tests/pep_e2e.rs` is `#[ignore]`. The script sets the variables that it needs. The test plays the agent and the phone of the owner. It calls `/auth/verify-key` as Traefik does, and it gives the identity token to the service. The phone speaks Noise, as in `tests/items_e2e.rs`.

| Scenario | What it proves |
|---|---|
| S0 | The static file `/static/about.txt` asks one question: `access` on the service, with no usage and no effect. The PDP answers `true` with no reason. The health probe `/health` needs no token (ProcurAuth 10.1c). |
| S1 | The first call gives `awaiting_approval`, an id and a code. A second call gives the same request. The phone approves, and the tool runs one time. A call after the use asks again. |
| S2 | A decline with a comment reaches the agent at the portal. The service reads `denied` and `approval_denied`, and never the comment. A changed command has another effect and no deny time. |
| S3 | `get_action_status` renews the lease. When the reads stop, the request is `abandoned`. A late answer of the phone is refused. A read inside the revive window revives the request. |
| S4 | A message of the agent in the thread goes to the portal only. |
| S5 | The tool ran one time for each approval, and no more. A used `approval_ref` is refused. A call with `wait_s` runs the tool in the same call. |
| SA | A second agent of the same key asks for the same effect while the request is open. It gets the request of the first agent, and the phone gets no second request. The two agents have two tokens, and the phone gets two frames `agent`. |
| S6 | The tap shows that the service received only `request_id`, `code`, `verdict`, `evaluation_id` and the reasons of the PDP. The canary strings of the explanation, the hint, the comment and the thread are in no exchange and in no log. The same holds for the token, the label, the client and the id of each agent, in the replies of the service and in the identity token too. |

After the scenarios, the script reads the logs of each container for the canary strings. The log of the portal holds the id of an agent. So the script looks for this id in the logs of the service, the tap and the Docker socket only. It then runs the series U of the ProcurAuth runner against the same `reference-service` binary.

### The series U and the portal

The runner `access-conformance` is the PDP of each case. It plays a script, for example `approval_expired` between two questions. A real PDP does not give such an answer on demand. The runner has no key to call another PDP. Its only fallback is the `fake-pdp` of the process.

The script therefore runs the series U with the runner as PDP. It proves the service binary, not the portal. The scenarios S1 to S6 prove the same behavior with the real portal. The table shows the link.

| Case | Scenario with the portal |
|---|---|
| U100, U108, U109 | S1: the submission has `effect`, `op` and `view`, and the effect is the effect of the phone. The PEP sends no explanation and no hint. |
| U101 | S1: a call while the request is pending gives the same id and code. |
| U102, U30 | S1 and S5: one question with `usage` after the approval, and one execution. |
| U103 | S2: a changed argument gives another effect. |
| U104, U105 | S1 and S2: a used approval asks again. `approval_denied` has no comment. |
| U106 | S3: the renewal comes from `get_action_status` only. |
| U107 | S6: the question holds the effect only. |
| U110 | S1 to S3: the service reads `pending`, `approved`, `denied` or `void`. |
| U127 | S0: a route that runs no action asks `access` on the service. A health probe needs no token. |

### Run it

Run `tools/dev/pep-e2e.sh` from the root of the checkout. The script needs Docker with Compose, `openssl` and `curl`. It finds the ProcurAuth checkout in `../procurauth` or `../../procurauth`. Set `PEP_E2E_PROCURAUTH` for another place.

Set `PORTAL_SLOT` to keep the build volume of a worktree apart. Set `PEP_E2E_KEEP_BUILD=1` to keep the images and the build volume for the next run. Without it, the script removes them. The first build takes several minutes.

The header of the script lists the other variables: the port base, the backend, `PEP_E2E_KEEP`, `PEP_E2E_SKIP_UNIT`, `PEP_E2E_SKIP_RUNNER` and `PEP_E2E_TEST_ARGS`. The run takes about two minutes when the images are in the cache. The test waits for real leases of 5 seconds, so it is slow by design.

## Joint check with ClickHouse Dash

The script `tools/dev/interop-dash.sh` runs the real portal and the real ClickHouse Dash together. The portal is the PDP and the gate. Dash is the PEP. The other tests never run the two products together. Run the script when you change the PDP, the approval gate, the action document or the edge for the keys.

### What the script builds

The script builds a throwaway Docker stack in the Compose project `portal-x13`. Each container, network and volume has a name that starts with `x13`. The script never touches another name. It never reads the Docker socket of the host. It opens ports on `127.0.0.1` only. It pushes no image to a registry.

| Part | What it is |
|---|---|
| `portal` | The portal, built from this checkout with `docker/Dockerfile`. The Inbox has `inbox.approvals` with the web approval on, so no phone is needed. |
| `dash` | The static binary of ClickHouse Dash that you name. The script mounts it read-only. It runs in the identity mode of MCP. |
| `clickhouse` | A ClickHouse server with its own users and 1000 rows of data. The profile of one MCP user has `readonly = 1`. |
| `traefik` | The edge. It has a route for the persons (ForwardAuth `/auth/verify`) and a route for the keys (ForwardAuth `/auth/verify-key`). It has no Docker socket. |
| `tap` | A proxy on the PDP link of Dash. It records each question and each answer. It is the `sidecar.py` of `tools/dev/pep-e2e`. |
| `sock` | A fake Docker socket. It lists Dash only, so the portal learns the path and the action document of Dash. |
| `driver` | A Python container on the same network. It opens the sessions of the persons, calls the MCP endpoint as an agent, and prints the table. |

Dash has 32 hosts, so the navigation needs 129 questions. Dash must split them into two batches.

### Scenarios

Each scenario prints its checks. The script prints one line for each scenario at the end. The exit code is 0 when each scenario passes and 1 when one fails. It is 2 when the setup fails.

| Scenario | What it proves |
|---|---|
| 1 | The portal reads the action document of Dash. The editor shows the published actions, and no other. |
| 2 | The portal publishes the metadata members, also `procurauth_directory_max_age_seconds`. Dash logs `access.pdp_metadata`. A batch of more than 64 questions works. |
| 3 | A person opens a session, sees the tile and opens a page through the gate. Dash accepts the identity token. For the page `/`, Dash asks `access` on its service, and the PDP answers `true` with no reason (ProcurAuth 10.1c). |
| 4 | An agent key calls `tools/list` and `run_query` with no `confirm`. |
| 5 | The approval of a call: approve and run once, a second call asks again, deny, void, and a call that waits (`wait_s`). |
| 6 | An operation with two permissions (`export_query`): only `confirm` on `query.export` asks. |
| 7 | The `confirm` of a person never blocks that person. The same grant on the key of that person asks. |
| 8 | The forms and the file refuse `confirm` on `mcp.session`. The tools use their own actions. |
| 9 | A ceiling above 1e9 is accepted and applied. On a host with `readonly = 1`, Dash refuses it. |
| 10 | A page route that needs an approval answers `403` with `approval_cannot_wait`. The page `/` asks only `access`, so it answers `200` for the same key. |
| 11 | The budget `queries` of Dash is counted by the portal. |
| 12 | No exchange between Dash and the PDP gets a 5xx or a 400. |

### Run it

The host needs Docker with Compose, `openssl` and `curl`. Give the path of a static Dash binary:

```sh
INTEROP_DASH_BIN=/path/to/chdash tools/dev/interop-dash.sh
```

The first run builds the image of the portal, and this takes several minutes. A later run takes about 90 seconds when the image is in the cache. The script removes its containers, network, volumes and files at the end. It also removes the image `x13-portal:dev`, unless `INTEROP_KEEP_BUILD=1`.

| Variable | Use |
|---|---|
| `INTEROP_DASH_BIN` | The path of the Dash binary. This variable is required. |
| `INTEROP_PORT_BASE` | The first of two ports on `127.0.0.1`: the edge and the portal. The default is 18500. |
| `INTEROP_CH_IMAGE` | The ClickHouse image. The default is `clickhouse/clickhouse-server:26.8.34.13`. |
| `INTEROP_ONLY` | Run only these scenarios, for example `5,6`. |
| `INTEROP_KEEP` | Set it to 1 to keep the stack up. The script then prints how to remove it. |
| `INTEROP_KEEP_BUILD` | Set it to 1 to keep the image of the portal. |
| `INTEROP_EXTRA_HOSTS` | The number of extra hosts in Dash (default 30). |

Run `tools/dev/interop-dash.sh clean` to remove the stack of an earlier run. The script stops at start when a name of the stack exists already.

### What the check cannot show

Dash retries a call that was approved with a control argument such as `lease_s`. Without it, Dash answers `invalid_argument` before it reads the approval. The agent must send the same arguments again, with the control argument.

The log of Dash has no field for `context.permission_only`. The tap shows the question on the wire, so scenario 6 reads the tap.

## Pull requests

Describe what changed, why, and how you checked it.
