# Conformance

The [ProcurAuth repository](https://github.com/ProcurAuth/procurauth) holds the conformance cases of the protocol and the runner `access-conformance`. The portal is a PDP and an edge. Its test suite runs the series D (a PDP) and the series E (an edge) of these cases. The continuous integration runs the two series on each push to `main` and on each pull request.

## What the portal tests

| Role | Series | Cases at the pinned commit | Where it runs |
|---|---|---|---|
| PDP | D | 98 | The test `the_cases_of_the_series_d_pass` in `src/http/conformance_tests.rs`. |
| Edge | E | 20 | The test `the_cases_of_the_series_e_pass` in `src/http/conformance_tests.rs`. |
| PEP | U | Does not apply | The portal is not a PEP. |

The portal is not a PEP, so the series U does not apply to it. The series U tests a service: the paragraphs of sections 10 and 17.7 are the duty of each service. [Identity tokens and the PDP](identity-and-pdp.md#what-portal-implements-of-the-protocol) gives the paragraphs that the portal follows. Two scripts run the series U against services that use the portal (see [The series U](#the-series-u)).

## How the tests run

The two tests read the cases of the directory `conformance/` of a ProcurAuth checkout. The variable `PROCURAUTH_CONFORMANCE` gives the path of this directory. Each test reads each JSON file of `cases/D` or `cases/E`, and runs each case on SQLite, then on PostgreSQL. The PostgreSQL side runs only when `PORTAL_TEST_POSTGRES` is set.

- The test of the series D loads the directory of each case in a new portal node. It sends the steps of the case to the routes of the PDP and compares the answers with the case.
- The test of the series E acts as Traefik. It sends the headers of each request to `/auth/verify-key`. It also acts as the service behind the edge, and records the request that arrives.

On PostgreSQL, the cases of the series D with approvals also need `INBOX_TEST_PG`. These are the cases with the condition `no_approval_support` and the cases whose directory has approvals. Without this variable, the test skips them on PostgreSQL. `tools/dev/test.sh` sets the two variables.

A test fails when a case fails. At the end, each test writes one line for each database on the standard error. The line has the form `series D [sqlite]: <n> cases, <n> passed, <n> failed, <n> skipped`. The test then lists each skipped case with its reason. `cargo test` shows these lines only with the option `--nocapture` or for a test that fails. Thus the logs of the continuous integration do not show them when the tests pass.

The test `effect_vectors_match` of `crates/inbox-core` reads `vectors/effect.json` in the same directory (protocol 17.3). When `PROCURAUTH_CONFORMANCE` is not set or the file is absent, it uses its own copy, `crates/inbox-core/tests/fixtures/effect-vectors.json`.

## The continuous integration

The job "Tests of the workspace" of `.github/workflows/nightly.yml` does these steps. The workflow runs every night, by hand and before a release. It does not run on a push or on a pull request.

1. It clones the ProcurAuth repository at the pinned commit, in the directory `procurauth`.
2. It sets `PROCURAUTH_CONFORMANCE` to the directory `procurauth/conformance`.
3. It sets `PROCURAUTH_CONFORMANCE_REQUIRED` to `1`.
4. It runs `tools/dev/test.sh --workspace --no-fail-fast`.

On your machine, clone the ProcurAuth repository next to this repository, or set `PROCURAUTH_CONFORMANCE`. Then run `tools/dev/heavy.sh tests`. The script sets `PROCURAUTH_CONFORMANCE_REQUIRED` to `1`.

With `PROCURAUTH_CONFORMANCE_REQUIRED=1`, `tools/dev/test.sh` stops with an error when the directory has no `cases/D` or no `cases/E`. The two tests then also fail when `PROCURAUTH_CONFORMANCE` is not set. Thus a run never passes without the cases. When the two variables are not set, the two tests do nothing.

## Skipped cases

A case can name conditions in its member `skip_if`. Each of the two tests has a table `SKIP_CONDITIONS` of the conditions that it knows, with a value for the portal. Each value is `false`, so no condition skips a case. A condition that is not in the table makes the case fail. Thus a new condition of the cases needs a decision in the code.

At the pinned commit, the portal skips no case of the series D or E, with one exception. When `PORTAL_TEST_POSTGRES` is set and `INBOX_TEST_PG` is not set, the PostgreSQL side of the series D skips the cases with approvals.

| Series | Condition | Cases with it | Value | Reason |
|---|---|---|---|---|
| D | `no_approval_support` | 20 | `false` | The portal gives approvals. The test runs these cases with the block `inbox.approvals`. |
| D | `no_pdp_credential` | 1 | `false` | The test gives a credential to each service of the case. |
| E | `no_mcp_endpoint` | 3 | `false` | The edge serves the MCP path of the probe service. |
| E | `single_key_set` | 1 | `false` | The key set of the portal holds one key. Each token has the `kid` of this key, so E13 checks the same rule. |
| E | `no_token_cache` | 2 | `false` | The edge keeps the identity tokens. |
| E | `no_approval_support` | 1 | `false` | The edge asks only `access`, and `confirm` has no effect on it (protocol 17.8). |

## Run the series D and E

The tests run in Docker. The host needs Docker and `git`.

1. Clone the ProcurAuth repository next to the portal checkout: `git clone https://github.com/ProcurAuth/procurauth.git ../procurauth`.
2. Set the ProcurAuth checkout to the pinned commit: `git -C ../procurauth checkout c820b4ac0e37c4769394876105fd549a6c8b38cc`.
3. Run `PROCURAUTH_CONFORMANCE_REQUIRED=1 tools/dev/test.sh --bin portal the_cases_of_the_series -- --nocapture`.
4. Read the lines `series D` and `series E` on the standard error.

`tools/dev/test.sh` reads `../procurauth/conformance` when `PROCURAUTH_CONFORMANCE` is not set. The script does not check the commit of the checkout. To run some cases of the series D only, set `PROCURAUTH_CASES`, for example `PROCURAUTH_CASES=D97,D98`. This variable has no effect on the series E. [Development](development.md#the-test-script) gives all the variables of the script.

## The series U

The series U tests a PEP, so it runs against a service and not against the portal. The runner `access-conformance` is the PDP of each case, because it plays a script of answers. Thus a run of the series U proves the service, not the portal.

| Script | Service | Result |
|---|---|---|
| `tools/dev/pep-e2e.sh` | The `reference-service` of the ProcurAuth repository, behind a real portal | The scenarios S0 to S6 prove the same rules with the portal. [Development](development.md#the-series-u-and-the-portal) links each scenario to its cases. |
| `demo/conformance/run-conformance-u.sh` | The demo service `notes` | The script records no count. [Conformance run of the service notes](../demo/conformance/README.md) gives the skipped cases and the limits of `notes`. |

The script `demo/conformance/run-conformance-u.sh` checks the commit of the ProcurAuth checkout. It stops with the exit code 2 when the checkout is at another commit.

## The pinned commit

The continuous integration pins one commit of the ProcurAuth repository: `c820b4ac0e37c4769394876105fd549a6c8b38cc`, the tag `v1.1` (ProcurAuth 1.1). Thus a change of the cases never changes the result by itself. At this commit, the series D has 98 cases and the series E has 20 cases.

To change the commit:

1. Choose a commit on the branch `main` of the ProcurAuth repository.
2. Write its full SHA in the member `ref` of the step that gets the conformance cases, in `.github/workflows/nightly.yml`.
3. Write the same SHA in `procurauth_commit` of `demo/conformance/run-conformance-u.sh`.
4. Write the same SHA in step 2 of [Run the series U](../demo/conformance/README.md#run-the-series-u), in `demo/conformance/README.md`.
5. Set your ProcurAuth checkout to this commit.
6. Run the series D and E (see [Run the series D and E](#run-the-series-d-and-e)).
7. Add each new condition of `skip_if` to the table `SKIP_CONDITIONS` of its series in `src/http/conformance_tests.rs`. Give it a value and a reason.
8. Update the numbers and the SHA in this page.

## Published results

The ProcurAuth repository lists the known implementations and their results in [IMPLEMENTATIONS.md](https://github.com/ProcurAuth/procurauth/blob/main/IMPLEMENTATIONS.md). Each project gives its own result of `access-conformance`.
