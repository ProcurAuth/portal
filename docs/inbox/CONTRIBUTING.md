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

Set `PORTAL_SLOT=<name>` to keep the build output of a worktree apart from the others. To regenerate the configuration reference, run `portal config-reference` and put the output between the markers of the root `README.md`. A test fails when the text is not the generated text.

## Test layout

The helpers of the end-to-end tests are in `tests/common/mod.rs`. They start the test server, open the sessions of the accounts, make the HTTP calls and play the phone side of protocol 4. Each file of `tests/` that needs them declares `mod common;`.

The tests of the inbox are in `tests/inbox_e2e.rs`. The tests of items are in `tests/items_e2e.rs`. The conformance series D and E are in `src/http/conformance_tests.rs`. They use the harness of `src/http/cluster_tests.rs`. Add a new helper to `tests/common/mod.rs`, not to a test file.

The tests of the preferences are in three places. The file `tests/prefs_e2e.rs` tests the page, `set_prefs` and the quiet flag with a real phone. The file `src/inbox/items_tests.rs` tests the digest for both databases, with a clock that the test sets. The file `src/http/prefs_tests.rs` tests the frames that the session builds. The pure rules of the time zones and of the due times are in `src/inbox/prefs.rs`. A test of the digest never waits: it sets the clock.

The tests of the agents are in two places. The file `src/inbox/agents_tests.rs` tests the resolution, the limits, the actions of the owner, the lock and the purge for both databases. It uses the rig of `src/inbox/items_tests.rs`. The file `src/http/agent_binding_tests.rs` tests the path of the PEP: the intent gives the agent, and a submission with no intent goes to the bucket. The pure rules (the token, the names, the hints and the binding of a call to an item) are in `src/inbox/agents.rs`.

The tests of the agent channel are in `src/http/agent_who_tests.rs`. They run on both databases with the rig of `src/http/agent_channel_tests.rs`. They test the token, the header, the label, the hints and the six codes of the agents. The black-box cases A01 to A10 are in `tests/mcp_contract.rs`.

The tests of the approval API of the services are in `src/http/approval_api_tests.rs`. They have a recorder that keeps each answer that the service gets. Add a new answer to the check of `check_what_the_service_learned`, not a new way to read it. The tests of the approval gate of the PDP are in `src/http/pdp/approval_tests.rs`. The file `src/http/pdp_rig.rs` starts the Inbox for them and for the cases with approvals. It also writes approvals in any state.

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
| S1 | The first call gives `awaiting_approval`, an id and a code. A second call gives the same request. The phone approves, and the tool runs one time. A call after the use asks again. |
| S2 | A decline with a comment reaches the agent at the portal. The service reads `denied` and `approval_denied`, and never the comment. A changed command has another effect and no deny time. |
| S3 | `get_action_status` renews the lease. When the reads stop, the request is `abandoned`. A late answer of the phone is refused. A read inside the revive window revives the request. |
| S4 | A message of the agent in the thread goes to the portal only. |
| S5 | The tool ran one time for each approval, and no more. A used `approval_ref` is refused. A call with `wait_s` runs the tool in the same call. |
| S6 | The tap shows that the service received only `request_id`, `code`, `verdict`, `evaluation_id` and the reasons of the PDP. The canary strings of the explanation, the hint, the comment and the thread are in no exchange and in no log. |

After the scenarios, the script reads the logs of each container for the canary strings. It then runs the series U of the ProcurAuth runner against the same `reference-service` binary.

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

### Run it

Run `tools/dev/pep-e2e.sh` from the root of the checkout. The script needs Docker with Compose, `openssl` and `curl`. It finds the ProcurAuth checkout in `../procurauth` or `../../procurauth`. Set `PEP_E2E_PROCURAUTH` for another place.

Set `PORTAL_SLOT` to keep the build volume of a worktree apart. Set `PEP_E2E_KEEP_BUILD=1` to keep the images and the build volume for the next run. Without it, the script removes them. The first build takes several minutes.

The header of the script lists the other variables: the port base, the backend, `PEP_E2E_KEEP`, `PEP_E2E_SKIP_UNIT`, `PEP_E2E_SKIP_RUNNER` and `PEP_E2E_TEST_ARGS`. The run takes about two minutes when the images are in the cache. The test waits for real leases of 5 seconds, so it is slow by design.

## Pull requests

Describe what changed, why, and how you checked it.
