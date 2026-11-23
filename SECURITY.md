# Security policy

This policy tells you how to report a vulnerability in portal, which versions get fixes, and what to expect from the maintainers. The security model of the code is in [`docs/security.md`](docs/security.md).

## Report a vulnerability

Use the private vulnerability reports of GitHub. Do not open a public issue, a pull request or a discussion for a vulnerability.

1. Open <https://github.com/ProcurAuth/portal/security/advisories/new>.
2. Describe the problem, the version, the impact and the steps to reproduce it.
3. Add a proof of concept if you have one. A short one is best.
4. Tell us if you plan to publish your own report, and when.

The project has no e-mail address for reports. If you cannot use private reports, open a public issue that says only "I need a private channel for a security report". Do not put details in the issue.

## What to include

- The version (`portal version`) or the commit.
- The configuration that you used, with the secrets removed.
- The deployment: Docker Compose or Kubernetes, the proxy and its version, and SQLite or PostgreSQL.
- The request or the steps that show the problem, and what you expected to happen.
- The impact as you see it: who can do what that they must not do.

## Supported versions

The latest release of the 5.x series gets security fixes. An older release does not. The project has no long-term support branch. The [changelog](CHANGELOG.md) lists the releases. A fix ships as a new release, and the release notes say that it is a security fix. The Android app Portal Inbox has the same version as the server and follows the same rule.

## Scope

In scope:

- The `portal` binary: the gate (`/auth/verify` and `/auth/verify-key`), the sign-in, the sessions, the second factor, the accounts, the grants and the settings pages.
- The keys and their limits, the identity tokens and the AuthZEN PDP, with the approval API.
- The Inbox: the routes of the phones, the API of the senders, the agent channel, the approvals and the storage of the contents.
- The Android app Portal Inbox and the crates `inbox-core` and `inbox-android`.
- The build and release scripts, if a flaw there can put hostile code in a release.

Out of scope:

- A flaw in a service behind the portal. Report it to the project of that service.
- A deployment that skips a duty that [Traefik and TLS](docs/traefik.md) or [Security model](docs/security.md) describes. The known limits in these pages are not vulnerabilities. Examples are a session cookie that reaches a service on the same origin and plain HTTP on an untrusted network.
- A denial of service that needs the superadmin account or a valid administrator action.
- An attack that needs control of the host, of the database with its keys, of the browser or of the phone of the victim.
- Reports from automatic scanners that show no impact on a real deployment.
- A security header that is absent on a page that holds no secret and no state.
- A weakness in a dependency that the portal does not reach. If you think that it does, tell us how.

If you are not sure that a problem is in scope, report it. We prefer a question to a missed flaw.

## What to expect

These are targets. They are not guarantees. The project has a small team.

| Step | Target |
|---|---|
| We acknowledge your report. | 3 business days |
| We give a first assessment: accepted, needs more information, or not a vulnerability. | 10 business days |
| We fix a critical or high problem. | 30 days after the assessment |
| We fix a medium problem. | 90 days after the assessment |
| We fix a low problem. | The next regular release |

We keep you informed of the progress. We credit you in the advisory and in the changelog if you want it. Tell us the name that you want to use.

## Coordinated disclosure

We ask you to keep the report private until a fix is available. We publish a GitHub security advisory with the fix. We ask for at most 90 days between your report and the public disclosure. We agree on another time with you when the case needs it. If a problem is already under active attack, we ship a fix and an advisory as fast as we can.

## Safe harbour

We consider good-faith research under this policy as authorised. We will not start legal action against you for it. Good faith means these rules:

- Test only against your own deployment, or a deployment whose owner agrees.
- Do not read, change or delete data of other persons. Stop when you reach it, and tell us.
- Do not degrade a service for other persons. Do not run a denial of service test against a shared deployment.
- Do not use deceit of persons, physical attacks or the accounts of other persons.
- Give us reasonable time to fix the problem before you publish it.

## The model of the code

[`docs/security.md`](docs/security.md) states what the portal defends and what it does not, the invariants that the code keeps, and the measures that a deployment must apply. [`docs/inbox/SECURITY.md`](docs/inbox/SECURITY.md) has the model of the Inbox.
