# Changelog

This file lists the notable changes of each release of portal. The layout follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The version numbers follow [SemVer](https://semver.org/spec/v2.0.0.html). The date is the date of the tag. [Upgrades](docs/operations.md#upgrades) gives the steps to take for each version.

## [Unreleased]

This version changes no key of the configuration file, no table and no frame of the phone.

### Added

- The route `/livez` gives the same answer as `/healthz`. Its metric label is `live`.
- The probes `/healthz`, `/livez` and `/readyz` answer `HEAD` with the status and the headers of `GET`, without a body. The answer of a probe has `Cache-Control: no-store`.
- The command `print-config` makes the checks of `check-config` and prints only the effective configuration.
- A command with `--config` accepts its options in any order, the short form `-c FILE` and the form `--config=FILE`.
- The usage text gives the exit status: 0 for success, 1 for an error.
- The metric `portal_start_time_seconds` gives the Unix time of the start.
- The log event `request` has two more fields after the four earlier fields: `duration_ms` and `route`.
- Each answer with the security headers has `Cross-Origin-Opener-Policy: same-origin` and a `Permissions-Policy` that refuses the camera, the microphone, the geolocation, the payment and USB features.
- The configuration reference has a column `Forms`. It tells which keys accept `_env` and `_file`.

### Changed

- The comparison of secrets uses `aws_lc_rs::constant_time`.
- The check of the signature of a peer refuses a hex text with a sign or with a character that is not a hex digit.

### Fixed

- `portal inbox import-sqlite` stops with an error that names the table and the column when a row has a value of a wrong type. Before, the command stopped with a panic.

## [5.6.0] - 2026-11-23

This release changes the licence, adds one member to the metadata of the PDP, follows ProcurAuth 1.1 and corrects the documents. It changes no table, no frame of the phone and no key of the configuration file.

### Changed

- The licence of the whole workspace is `MIT OR Apache-2.0`. The file `LICENSE-APACHE` holds the full text of the Apache License 2.0. The local image carries the label `org.opencontainers.image.licenses`.
- A route of a service that runs no action asks the action `access` on the service (ProcurAuth 10.1c). The answer never has a reason of the approval gate, also when the grant of `access` has `confirm`.
- A ceiling has a plural unit, for example `rows`. A budget has a singular unit, for example `request` (ProcurAuth 6.1). The unit of `max_key_lifetime` is plural.
- The approval API answers `401` with the code `unauthorized` and the header `WWW-Authenticate: Bearer realm="portal-pdp"` for an unknown credential. It answers `403` with the code `forbidden` only when the block `inbox.approvals` is absent.
- The conformance run of the series D and E uses the cases of ProcurAuth 1.1. It has 98 cases for a PDP and 20 cases for an edge.

### Added

- The metadata of the PDP has the member `procurauth_directory_max_age_seconds`. With PostgreSQL, its value is `keys.directory_max_age` in whole seconds, rounded up and never 0. With SQLite, the value is 1.
- The test harness of the series D and E reads `skip_if` with a table of the known conditions. A condition that the table does not know makes the run fail.

## [5.5.1] - 2026-11-14

### Changed

- A grant with `confirm` on the action `access`, on the action `mcp.session` or on the service `portal` gets its own refusal. The key form gives the code `bad_confirm`, and the form of an account gives the notice `bad-confirm`.

## [5.5.0] - 2026-11-13

This release aligns the portal with the ProcurAuth specification. It changes no table of the database.

### Added

- A question with `context.permission_only` set to `true` marks a permission that is not the action of the use. The PDP ignores `confirm` for it.
- The metadata of the PDP has the members `procurauth_idle_max_seconds`, `procurauth_evaluations_max_bytes`, `procurauth_evaluations_max_items` and `procurauth_usage_max_seconds`.
- The metric `portal_budget_refunds_total` counts the units that the portal returns when the consumption of an approval fails.
- The script `tools/dev/interop-dash.sh` runs the real portal with the real ClickHouse Dash. The CI runs the series D and E of the conformance cases.
- The start checks that `inbox.approvals.max_command_bytes` plus 2048 bytes is at most `pdp.max_body_bytes`.

### Changed

- The `confirm` of the grant of a person has no effect on the own questions of that person. It applies to the keys and the agents of the person.
- A grant with `confirm` on the action `mcp.session` is refused. A file that has it stops at start.
- A view of an approval can have no title, no field and no body. The phone shows the name of the action as the title. Install the APK of 5.5.0 before a service sends such a view.
- The largest value of a limit is 9007199254740991 (2^53 - 1). It was 1,000,000,000.
- The default of `pdp.max_evaluations` is 128. It was 64.
- The portal counts the use first and then consumes the approval. It never loses an approval and never consumes it two times.
- A right `keys.own.use` that the owner lacks, and a key over `max_key_lifetime`, give a plain `false` with no reason.
- The edge of the keys no longer refuses a key because the document of actions is old.
- The grant editor shows the lock `confirm` only when the file has the block `inbox.approvals`.

## [5.4.0] - 2026-11-12

### Added

- The block `external_service "<name>"` in the configuration file. It has the keys `href`, `description`, `icon` and `icon_path`.
- The route `GET /auth/ext-icon/<id>` serves the own icon of an external service.
- The migration files `deploy/migrations/5.3.0-to-5.4.0.sql` and `deploy/migrations/5.3.0-to-5.4.0.sqlite.sql`. They add three columns to the table `external_services`.

### Changed

- One strict rule of the link applies to the file, the form and the database. A service of the file wins over a row of the database with the same id.
- The start checks each row of `external_services`. A row that breaks the rules stops the start.

## [5.3.0] - 2026-11-11

### Removed

- The key `approvals` of the block `pdp_client`. A file that still has it stops at start.
- The member `approval` of an action in the document of a service. The portal ignores it.

### Changed

- Each grant of a service other than `portal` can have `confirm`, for any action but `access`. The grant editor shows the lock on each such row.
- The PDP answers `approval_required` for a grant with `confirm` on any action of any service with a known document. A service must handle `approval_required` for all its actions.

## [5.2.1] - 2026-11-10

### Fixed

- Android app: the app shows the new state of an agent at once after the owner names or allows it. The line of observed hints no longer marks its own separator.

## [5.2.0] - 2026-11-10

### Added

- Questions. The block `inbox.questions`, the actions `question.ask`, `question.read` and `question.cancel`, the budget `questions`, the MCP tool `ask` and the route `POST /api/v1/questions`. An agent asks a person a question, and the person answers on the phone or on the web.
- The command `portal inbox rekey --config FILE [--from ID]` seals the contents of the old keys again with the active key.
- The log event `content_key_rows_to_rekey` at start.

### Changed

- The answer of a question has a new member `answer` for the agent. A form answer moved from `untrusted.answers` to `answer.untrusted.answers`.

## [5.1.0] - 2026-11-09

### Added

- Items of the Inbox: approvals of agent actions and the discussion with an agent. The blocks `inbox.items` and `inbox.at_rest` start them. The content of an item is sealed at rest with content keys.
- Approvals. A grant can have the property `confirm`, and a key has the attribute `interaction`. The PDP answers `approval_required`, and the approval API lets a service submit a request.
- Agents of a key. The block `inbox.agents` and the key attribute `agents` (`key`, `discover` or `enrolled`). The pages Agents let the owner name, allow, block, merge and enrol agents.
- The Android app shows items, approvals and agents.

### Changed

- The Android app of 5.1.0 and the server of 5.1.0 go together. A phone of an earlier build cannot read the new frames.

## [5.0.5] - 2026-11-08

### Added

- External services: a tile that opens a link, added by the superadmin on the page Services.
- The key `ui.services_new_tab` (default `true`).

### Removed

- The label `portal.target`. A service cannot choose its own behaviour.

## [5.0.4] - 2026-11-08

### Fixed

- The page of an account that another instance made a moment ago opens at once.

### Changed

- The account page and the New user page show the server permissions as the first row of the Services list.

## [5.0.3] - 2026-11-07

### Changed

- The grant editor and the pages have a layout for narrow screens.
- A service row shows an icon when the service publishes its actions.
- The box of the long session in the sign-in form has a new label that names the number of days.

## [5.0.2] - 2026-11-06

### Security

- The PDP refuses a node below the service that has no `resource.properties.ancestors`. It answers `false` with the reason `invalid_resource` (ProcurAuth 4.1).
- The Inbox API reads an API key only in the header `Authorization: Bearer`. The query `?token=` is gone.

## [5.0.1] - 2026-11-06

### Security

- This release closes the findings of a security audit.
- A watch alert about a service reaches only the people who may open that service.
- A key of the file reads the status of an alert only when its owner holds `alert.read` on the group.
- A workload that is not listed but has a path stays closed to everyone except the superadmin.
- The enrollment fingerprint shows 80 bits.

### Added

- The keys `session.max_sessions_per_account` (16), `inbox.devices.max_handshakes_per_address` (16) and `inbox.devices.max_per_account` (10).

## [5.0.0] - 2026-11-06

The first version of the merged product. One program now holds the launcher, the gate, the accounts, the keys, the AuthZEN decision point, the history and the Inbox.

### Added

- The Inbox: alerts to Android phones, with the app Portal Inbox. A phone enrolls with a one-time code of an account. The session is a Noise IK session inside a WebSocket. Senders use `api_` keys.
- Two kinds of key: `mcp_` for agents and `api_` for senders. The rights to make and to use a key apply to the kind.
- One port for the pages, the senders and the phones.

### Changed

- A key of an earlier version becomes an MCP key.
- A phone of the earlier standalone alert server (protocol 3) cannot connect. It gets the close code 4426. Each phone enrolls again with a code.

[Unreleased]: https://github.com/ProcurAuth/portal/compare/v5.6.0...HEAD
[5.6.0]: https://github.com/ProcurAuth/portal/compare/v5.5.1...v5.6.0
[5.5.1]: https://github.com/ProcurAuth/portal/compare/v5.5.0...v5.5.1
[5.5.0]: https://github.com/ProcurAuth/portal/compare/v5.4.0...v5.5.0
[5.4.0]: https://github.com/ProcurAuth/portal/compare/v5.3.0...v5.4.0
[5.3.0]: https://github.com/ProcurAuth/portal/compare/v5.2.1...v5.3.0
[5.2.1]: https://github.com/ProcurAuth/portal/compare/v5.2.0...v5.2.1
[5.2.0]: https://github.com/ProcurAuth/portal/compare/v5.1.0...v5.2.0
[5.1.0]: https://github.com/ProcurAuth/portal/compare/v5.0.5...v5.1.0
[5.0.5]: https://github.com/ProcurAuth/portal/compare/v5.0.4...v5.0.5
[5.0.4]: https://github.com/ProcurAuth/portal/compare/v5.0.3...v5.0.4
[5.0.3]: https://github.com/ProcurAuth/portal/compare/v5.0.2...v5.0.3
[5.0.2]: https://github.com/ProcurAuth/portal/compare/v5.0.1...v5.0.2
[5.0.1]: https://github.com/ProcurAuth/portal/compare/v5.0.0...v5.0.1
[5.0.0]: https://github.com/ProcurAuth/portal/releases/tag/v5.0.0
