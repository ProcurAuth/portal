# Security policy

This document tells how to report a vulnerability in portal. It also tells which versions get fixes and what the project covers. For the design of the protections, read the [security model](docs/security.md). For the Inbox, read the [security model of the Inbox](docs/inbox/SECURITY.md).

## Report a vulnerability

Do not open a public issue, a pull request or a discussion for a vulnerability. Use the private reports of GitHub.

1. Open [the page to report a vulnerability](https://github.com/ProcurAuth/portal/security/advisories/new).
2. Describe the problem, the version, the impact and the steps to repeat it.
3. Add a proof of concept if you have one. A short one is best. Remove each real secret and each real host name from it.
4. Tell us if you plan to publish your own report, and when.

Give this information in the report:

- The version (`portal version`) or the commit.
- The configuration that you used, with the secrets removed.
- The deployment: Docker Compose or Kubernetes, the proxy and its version, and SQLite or PostgreSQL.
- The request or the steps that show the problem, and what you expected.
- The impact as you see it: who can do what that they must not do.

The project has no e-mail address for reports. We never ask for a secret. If the page is not available, open a public issue with the title "Security contact needed". Write no detail in it. A maintainer then starts a private channel.

## Response times

These are targets. They are not guarantees. The project has a small team.

| Step | Target |
| --- | --- |
| We acknowledge your report. | 3 business days |
| We give a first assessment: accepted, needs more information, or not a vulnerability. | 10 business days |
| We fix a critical or high problem. | 30 days after the assessment |
| We fix a medium problem. | 90 days after the assessment |
| We fix a low problem. | The next regular release |

We tell you about the progress. We credit you in the advisory and in the changelog if you want it. Tell us the name that you want to use.

## Supported versions

| Version | Supported |
| --- | --- |
| The latest release of the 5.x series | Yes |
| The branch `main` | Yes |
| Older releases | No. Update to the latest release. |

The project has no long-term support branch. The [changelog](CHANGELOG.md) lists the releases. A fix ships as a new release, and the release notes say that it is a security fix. The Android app Portal Inbox has the same version as the server and follows the same rule.

## What the project covers

- **The `portal` binary.** This includes the gate (`/auth/verify` and `/auth/verify-key`), the sign-in, the sessions, the second factor, the accounts, the grants and the settings pages.
- **The keys and the decision point.** This includes the keys and their limits, the identity tokens, the AuthZEN PDP and the approval API.
- **The Inbox.** This includes the routes of the phones, the API of the senders, the agent channel, the approvals and the storage of the contents.
- **The Android app.** This includes the app Portal Inbox and the crates `inbox-core` and `inbox-android`.
- **The release.** A flaw in the build and release scripts is a vulnerability if it can put hostile code in a release.

If you are not sure that a problem is in scope, report it. We prefer a question to a missed flaw.

## What the project does not cover

- A flaw in a service behind the portal. Report it to the project of that service.
- A deployment that does not do a duty that [Traefik and TLS](docs/traefik.md) or the [security model](docs/security.md) describes. The known limits in these pages are not vulnerabilities. Examples are a session cookie that reaches a service on the same origin, and plain HTTP on an untrusted network.
- A denial of service that needs the superadmin account or a valid administrator action.
- An attack that needs control of the host, of the database with its keys, of the browser or of the phone of the victim.
- Reports from automatic scanners that show no impact on a real deployment.
- A security header that is absent on a page that holds no secret and no state.
- A weakness in a dependency that the portal does not reach. If you think that it does, tell us how.

## Test values

The repository holds fixed keys and credentials for the tests and the demo. They are public on purpose and they protect nothing. A report about one of them is not a vulnerability.

| Path | Test value |
| --- | --- |
| `src/config/tests.rs`, `src/identity.rs`, `tests/mcp_contract.rs`, `tests/common/approvals.rs`, `tests/inbox_e2e.rs` | The Ed25519 private key in PKCS#8 form whose seed is the bytes 1 to 32. |
| `src/config/tests.rs` | The variables of the configuration tests: a password, the session secret, a database password, two PDP tokens, the key of the second factor and an all-zero key hash. |
| `src/config/tests.rs` | The password `hunter2hunter2` of a test that refuses a secret in clear text. |
| `src/http/cluster_tests.rs` | The password `correct-horse-battery-staple`, the session secret (32 bytes `k`), the key of the second factor and the two PDP tokens of the cluster tests. |
| `src/mfa.rs` | The secret that signs the tokens in the unit tests of the second factor. |
| `src/totp.rs` | The secret of the test vectors of RFC 6238. |
| `src/inbox/mcp_tools.rs`, `src/ui/agents.rs` | The agent tokens (prefix `agt_`) of the unit tests. |
| `tests/common/approvals.rs` | The PDP token of the end-to-end tests. |
| `tests/common/mod.rs` | The passwords of the end-to-end tests, and the secret (16 bytes `0x5a`) of an enrollment code that no account made. |
| `demo/` | The passwords and keys of the demo stack, for one machine only. `demo/up.sh` writes them to `demo/.secrets/`, and Git ignores that folder. |

Each other fixed key, token or password in a test file is also a test value.

## Coordinated disclosure

We ask you to keep the report private until a fix is available. We publish a GitHub security advisory with the fix. We ask for at most 90 days between your report and the public disclosure. We agree on another time with you when the case needs it. If attackers already use a problem, we ship a fix and an advisory as fast as we can.

## Safe harbour

We consider good-faith research under this policy as authorised. We will not start legal action against you for it. Good faith means these rules:

- Test only against your own deployment, or a deployment whose owner agrees.
- Do not read, change or delete data of other persons. Stop when you reach it, and tell us.
- Do not degrade a service for other persons. Do not run a denial of service test against a shared deployment.
- Do not use deceit of persons, physical attacks or the accounts of other persons.
- Give us reasonable time to fix the problem before you publish it.
