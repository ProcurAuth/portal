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

Set `PORTAL_SLOT=<name>` to keep the build output of a worktree apart from the others. To regenerate the configuration reference, run `portal config-reference` and put the output between the markers of the root `README.md`. A test fails when the text is not the generated text.

## Test layout

The helpers of the end-to-end tests are in `tests/common/mod.rs`. They start the test server, open the sessions of the accounts, make the HTTP calls and play the phone side of protocol 4. Each file of `tests/` that needs them declares `mod common;`.

The tests of the inbox are in `tests/inbox_e2e.rs`. The tests of items are in `tests/items_e2e.rs`. The conformance series D and E are in `src/http/conformance_tests.rs`. They use the harness of `src/http/cluster_tests.rs`. Add a new helper to `tests/common/mod.rs`, not to a test file.

The tests of the approval API of the services are in `src/http/approval_api_tests.rs`. They have a recorder that keeps each answer that the service gets. Add a new answer to the check of `check_what_the_service_learned`, not a new way to read it. The tests of the approval gate of the PDP are in `src/http/pdp/approval_tests.rs`. The file `src/http/pdp_rig.rs` starts the Inbox for them and for the cases with approvals. It also writes approvals in any state.

## Pull requests

Describe what changed, why, and how you checked it.
