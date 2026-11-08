# Inbox specification

This document states what the Inbox does and why. The Inbox is a feature of the portal. The code is the reference for details. The design of the merge is in [`../design-inbox.md`](../design-inbox.md). The specification of the portal in its own repository has the same requirements in its section 15.

The words MUST, MUST NOT, SHOULD and MAY are the key words of RFC 2119 and RFC 8174.

A requirement has one of three states. A requirement without a mark is in force in version 5.0.x (step 1 of the roadmap). A requirement with the mark **[5.1.0]** or **[5.2.0]** is for that release of step 3. A requirement with the mark **FUTURE** is for a later step. No code implements a marked requirement before its release. Section 8 lists the steps.

Each requirement has an id and a testable verb. Section 6.12 gives the lot and the test for each id of the releases 5.1.0 and 5.2.0. A decision of the owner has the word **Decision** (section 3.10).

## 1. Purpose

1.1 The Inbox sends alerts to Android phones. An alert can be vital. Each subscribed phone MUST get each alert, and MUST show it one time.

1.2 This holds when the phone was off, in airplane mode or disconnected when the alert came. It also holds when the app was stopped.

1.3 The Inbox is part of the portal. One account of the portal gives one identity and one set of permissions for the launcher, the keys and the Inbox.

## 2. Scope

| In scope | Out of scope |
|---|---|
| The Inbox feature of the binary `portal`. | A standalone alert server binary. |
| The Android app Portal Inbox. It only receives alerts. | iOS. It cannot keep a connection in the background without the push service of Apple. |
| The API for the senders, the webhooks for Alertmanager and Grafana, and the page of the tab Inbox. | A push service of a third party (Firebase or Apple). |
| Alerts of the portal about itself. | An acknowledgement of an alert by a person. An alert stays one way. The items of 3.6 need an answer. |
| Docker Compose and K3s files. | A Grafana plugin. Grafana contact points cannot be extended. |
| One database with SQLite or PostgreSQL. A standby replica for SQLite. | An OpenTelemetry export. The metrics are in the Prometheus text of the portal. |
| Approvals and questions for the owner of a key (3.6 to 3.10), from 5.1.0. | Any recipient other than the owner. End-to-end encryption. A signature of the phone. A risk class. See 8.3. |

## 3. Functional requirements

### 3.1 Groups and alerts

3.1.1 The configuration file MUST define the groups. A group has an id, a name and an optional description. A group MAY be `critical`.

3.1.2 An alert has one group and one title. The title MUST have one line and 128 characters at most. An alert MUST NOT have a body or a severity.

3.1.3 The sender decides the group. The subscriber decides the reaction of the phone, group by group.

3.1.4 An alert has no human acknowledgement in step 1. The phone sends only an automatic receipt after it stores the alert. Without the receipt, the server cannot send the alert again, and the guarantee of delivery fails.

3.1.5 Each alert MUST have a kind. The only kind in step 1 is `alert`. The wire frame and the stored alert carry it in the field `kind`. In protocol 4, a parser MUST refuse any other value.

3.1.5a **[5.1.0]** Protocol 5 has the kinds `alert`, `question`, `approval`, `turn` and `digest`. A parser of protocol 5 MUST refuse any other value. The rule of one-way messages (3.1.4) stays true for `alert` only. A phone of protocol 4 MUST get the close code 4426.

3.1.6 A message of a kind other than `alert` MUST show in the app as a generic message. The app MUST NOT drop a message because of its kind.

### 3.2 Phones

3.2.1 A phone MUST enroll with a one-time code that an account of the portal makes. The login of the account is the user name of the phone. The phone MUST NOT change it.

3.2.2 The account MUST have the permission `inbox.receive` on at least one group to make a code. The code is made in the tab Inbox, with the plus button.

3.2.3 A phone MUST subscribe only to a group for which its account has `inbox.receive`. When the account loses the permission, the Inbox MUST remove the subscription and tell the phone. The Inbox MUST NOT remove a subscription when it does not know the rights.

3.2.4 For each group, the user chooses an action on the phone: `alarm`, `ring` or `notify`. The server MUST NOT know this choice.

3.2.5 The app MUST NOT send an alert. The server has no send route for the identity of a phone.

3.2.6 One app MAY enroll in several servers. It keeps one connection and its own subscriptions for each server. The app shows the name of each server (`server.name`).

3.2.7 A person MUST be able to revoke a phone. An account revokes its own phones. An account with `phone.revoke` and `phone.read` revokes any phone. A revoked phone gets the close code 4403.

### 3.3 Senders and keys

3.3.1 A sender MUST send an alert with an API key (the prefix `api_`) as a bearer token in the header `Authorization`. The server MUST NOT read a key in the query of the URL, and MUST NOT log the query. Two sources of API key work at the same time: a key of the file (a block `api_key`), and a key of the database. A bad form, a bad checksum, an unknown prefix and a key of another kind MUST get `401` with no detail, before any lookup.

3.3.2 A key of the file MUST NOT depend on the state of the portal at run time. It works when the portal is not ready and when PostgreSQL is away. Its owner MUST be an account of the file. The start decides its groups.

3.3.3 A key of the database MUST have `alert.send` on the group, and its owner MUST have it too. For an alert to an account, the key and its owner MUST have `alert.send_user` on the service `portal`. A key follows its owner for both kinds. The Inbox asks the PDP of the portal in process.

3.3.4 The Inbox MUST offer these routes: a JSON alert, a generic webhook, and a webhook with the payload of Alertmanager and Grafana. A sender MAY read the delivery status of an alert with `alert.read` on its group. For an alert to an account, only the key that sent it MAY read the status.

3.3.4a An alert has one target: a group or one account. A request MUST give exactly one of `group` and `user`, or it MUST get `400`. An alert to an account MUST go to each phone of the account that is not revoked, and it MUST need no subscription. An account is of the database, of the file, or the superadmin: the Inbox MUST use one check for the three kinds. An account that does not exist MUST get `404`. An account with no phone MUST get `409`, and the Inbox MUST NOT store the alert.

The level of the alert is Ring. It MUST NOT be critical. The protocol of the phones MUST NOT change: the frame has the group id `@direct`.

3.3.5 A sender SHOULD send an `Idempotency-Key`. Two requests with the same key MUST make one alert. A key that belongs to a different alert MUST get `409`.

3.3.6 The actions `alert.send` and `alert.send_user` MAY have a budget (`alerts`). They use one count. A critical group MUST NOT have a limit. An alert to an account is never critical, so the budget MUST count it.

3.3.7 The kind `api` MUST be on only with the block `inbox`. The switch is `keys.api`, and its default is `false`. A file with a block `api_key` and `keys.api` not `true` MUST stop at start.

3.3.8 The routes of the Inbox MUST NOT accept an `mcp_` key, and the key edge MUST NOT accept an `api_` key. The prefix `whk_` is reserved.

3.3.8a **[5.1.0]** The endpoint `/mcp` is an exception to 3.3.8. It MUST accept an `mcp_` key and MUST refuse an `api_` key. The REST routes of the items (3.8.2) MUST accept an `api_` key and MUST refuse an `mcp_` key.

### 3.4 Page

3.4.1 The page of the Inbox is the tab Inbox of the settings of the portal, at `/auth/settings/inbox`. It needs a session of the portal and the permissions of the portal features. It has no sub-page.

3.4.2 The page MUST work without script. The script only gives the Copy button, the close of a popover and the count of the bulk action.

3.4.3 The page shows the alerts and their delivery (zone **Sent**), the phones (zones **Phones** and **My phones**) and the connection log (zone **Connections**). It lets an account send an alert and revoke a phone. A part that an account may not use MUST be absent from the page.

3.4.4 A session that the superadmin opens as another account MUST NOT send an alert and MUST NOT revoke a phone.

3.4.5 The zone **Sent** MUST hold the 500 newest alerts of the period at most, and MUST show `500+` when older alerts of the period exist.

3.4.6 The page MUST be absent, and its routes MUST answer `404`, when the file has no block `inbox`.

### 3.5 Alerts of the portal

3.5.1 The portal MAY send an alert in three cases. A service is unhealthy. A service is healthy again. Refused sign-ins occur in a burst. The block `inbox.watch` enables this.

3.5.2 These alerts MUST use the path of an alert of the API. This gives one transaction, the idempotency key and the copy to the replica. The source of the alert is `portal`.

3.5.3 An alert about a service, and its recovery, MUST reach a reader only when the reader may open the service. The readers are the superadmin and an account with `access` on the service. This applies to the phones, the notifications, the page of the Inbox and the keys. A key reads such an alert only when its owner may open the service. When the portal cannot tell the rights of a reader, the reader MUST NOT see the alert.

### 3.6 Items

An item is a message that waits for an answer from a person. The portal has one engine for all items. Release 5.1.0 has the approvals and the discussion. Release 5.2.0 adds the questions.

3.6.1 **[5.1.0]** The Inbox MUST store each item with one kind. The kind is `approval` or, from 5.2.0, `question`. An item MUST be a row of the table `items`. Its delivery MUST use a row of the table `alerts` with the same kind.

3.6.2 **[5.1.0]** The owner of an item is the owner of the requester key. The owner is the only recipient. A route of the API MUST NOT have a field for another recipient.

3.6.3 **[5.1.0]** An item MUST have one of these states: `pending`, `replied`, `answered`, `abandoned`, `expired`, `cancelled` and `superseded`. The states `pending` and `replied` are open. Each other state is final, but an `abandoned` item MAY revive (3.6.11).

3.6.4 **[5.1.0]** The state `answered` has the action `accept` or `decline`. Only `decline` is a refusal. For an approval, `accept` gives the decision `approve` and `decline` gives the decision `deny`. No other final state MUST read as a refusal.

3.6.5 **[5.1.0]** The server MUST set the states `abandoned` and `expired` with its own clock. It MUST check them at each read, at each answer and in a periodic task. A clock of a phone MUST NOT change a state. The state MUST be in the database, not only in memory.

3.6.6 **[5.1.0]** The agent MUST give the wait time as `lease_s` or as `until`, and never both. A request with both values, or with none, MUST get `422`. A value outside the range of the file MUST get `422`. The portal MUST NOT change a value to fit the range.

3.6.7 **[5.1.0]** `lease_s` MUST be between `inbox.items.lease.min` and `inbox.items.lease.max`. The effective lease is 3 times `lease_s`, from the last renewal. It MUST NOT pass `chain_created_at` plus `max_lifetime`. `until` MUST be a date in RFC 3339 and MUST NOT pass the same limit.

3.6.8 **[5.1.0]** A call renews the lease only when the requester key makes it. These calls renew: `get_answer`, a read with `?wait=`, `keepalive`, `say`, and a repeat of the creation with the same idempotency key. A renewal MUST NOT use a unit of a budget. It MUST NOT create a delivery. The server MUST write `lease_until` at most one time in each `lease_s`.

3.6.9 **[5.1.0]** When the lease and `lease.grace` end, the item MUST become `abandoned`. This is not a refusal. The notification MUST leave the list "To answer". The history MUST say that the agent does not answer any more.

3.6.10 **[5.1.0]** When `until` or `max_lifetime` is reached, the item MUST become `expired`. This is neutral and is not a refusal. A supersede MUST NOT extend `max_lifetime`.

3.6.11 **[5.1.0]** Inside `lease.revive_window`, a `keepalive` from the requester key MUST revive an `abandoned` item to `pending`. The Inbox MUST send one notification, "The agent is back". An item MUST revive at most `lease.max_revivals` times. After the window, `get_answer` MUST return `abandoned` with `revivable` false, and `keepalive` MUST get `410 item_abandoned`. The agent then makes a new request with `previous_id`.

3.6.12 **[5.1.0]** A supersede MUST be one transaction. It makes the old item `superseded`, makes a new item `pending` in the same chain, and cancels the approval of the old item. The new item MUST ask its own approval. The server MUST NOT move an approval. A chain MUST NOT hold more than `max_chain` items.

3.6.13 **[5.1.0]** A `cancel` from the requester key MUST make the item `cancelled`. The same holds when the key is revoked, the owner is deleted, or an administrator with `approval.cancel` cancels the item. The service sees only `void`.

3.6.14 **[5.1.0]** The server MUST accept exactly one answer for an item, from a phone or from the web. The answer MUST be one compare-and-set on the state, the lease, the lifetime and the device or the session. The next answer MUST get `already_answered`. An answer after `abandoned` MUST get `abandoned`.

3.6.15 **[5.1.0]** A `reply` of the owner MUST move an open item to `replied`. The item stays open and keeps its lease. A `say` of the agent MUST move it back to `pending`. Both calls need `allow_ask_back`, or they MUST be refused. A thread MUST NOT have more than `max_turns` turns. The server MUST refuse a turn of the agent less than 2 seconds after the last turn of the agent.

3.6.16 **[5.1.0]** A key has the attribute `interaction`, with the value `conversational` or `one_shot`. The owner sets it when the key is made. The agent MUST NOT set it. The default is `conversational` for an `mcp_` key and `one_shot` for an `api_` key.

3.6.17 **[5.1.0]** The agent MAY set `allow_custom` and `allow_ask_back` for each item. Both default to false. A `one_shot` key that sets one of them MUST get `422 interaction_not_allowed`. A `one_shot` item MUST refuse a `reply` and a `say`.

3.6.18 **[5.1.0]** The Inbox MUST limit the open items by `max_open_per_key`, `max_open_per_owner` and `max_open_total`. A creation above a limit MUST get `429`. The limits have no default.

3.6.19 **[5.1.0]** The Inbox MUST keep the metadata of a final item for `inbox.items.retention` and the audit events for `audit_retention`. After `body_purge_after`, the sealed body MUST be gone. The effect hash and the audit MUST stay.

3.6.20 **[5.1.0]** The task of the server MUST write a heartbeat, `swept_at`. After an outage longer than `lease.grace`, it MUST extend each open lease by the length of the gap. No item MUST become `abandoned` because of an outage. When the database has no writable primary, the creation, the renewal and the answer MUST get `503 storage_degraded`.

3.6.21 **[5.2.0]** A question has a title of one line and 128 characters at most. It MAY have an `explanation`, a `command`, a `command_label` and a `hint`. The input type is `choice`, `confirm`, `text`, `none` or `form`. A choice has 2 to 8 options. A `form` has several questions (3.6.29).

3.6.22 **[5.2.0]** The flags of a question MUST follow these rules, or the Inbox MUST answer `422 invalid_flags`. `text` refuses `allow_custom`. `none` refuses both flags. `multiple` with `allow_custom` makes the free answer exclusive of the boxes. A closed question has no flag.

3.6.23 **[5.2.0]** The Inbox MUST refuse a `command` of a question with a control character, except a line feed or a tab. It MUST also refuse a bidirectional control (U+202A to U+202E, U+2066 to U+2069). The answer is `422 invalid_command_chars`. The free text of the owner MUST NOT have more than 2000 characters. A turn of the agent MUST NOT have more than 4000.

3.6.24 **[5.2.0]** The answer to the agent MUST have the action, the choice, the text of the owner in an `untrusted` object, and the times. A free text answer MUST arrive as an empty choice with the text. `ask` MUST wait at most `wait_s` and then MUST return a handle with the request id and the lease.

3.6.25 **[5.2.0]** `ask`, `say` and `supersede` MUST each use one unit of the budget `questions`. A renewal MUST use none. The actions are `question.ask`, `question.read` and `question.cancel`. A question MUST NOT pass through a service. An `ask` to the owner is not a tool with an effect.

3.6.26 **[5.1.0]** The explanation of an agent has a format in the member `explanation_format`: `plain` (the default) or `markdown`. It exists in the frames `request` and `message` and in the intent of an approval. It applies to the explanation only. Every other text MUST be plain text. A value that is not in the list, or a format without an explanation, MUST get `422`.

3.6.27 **[5.1.0]** The Markdown of an explanation MUST be a closed subset. The subset has paragraphs, headings of level 1 to 3 and emphasis. It has code, block quotes, lists, task items, line breaks and links. Raw HTML, images, tables, footnotes, autolinks and thematic breaks MUST stay plain text. The server and the phone MUST use the same parser. A text over a limit (8000 characters, 2000 nodes, a depth of 4) MUST show as plain text, with no error.

3.6.28 **[5.1.0]** A link MUST NOT be active. A screen MUST show the text of the link and, in parentheses, the host of an `http` or `https` address. The host is the part after the user information. A link with another scheme MUST show its text only. The HTML of the server MUST NOT have a `style` attribute, a script, an event attribute or an `href`. It MUST write only classes that start with `md-` or `tok-`.

3.6.29 **[5.2.0]** A form is a question with `input.type` equal to `form`. It has 1 to 4 questions. Each question has a unique `id`, a `header` of 16 characters at most and a `text` of 600 characters at most. The `type` of a question is `choice` or `text`. A `choice` question has 2 to 8 options, `multiple`, `min` and `max`, with the rules of a `choice` input. A form that breaks one of these rules MUST NOT be a frame.

3.6.30 **[5.2.0]** The flags of a form are the flags of its item. With `allow_custom`, each `choice` question MUST have a free answer "Other" that is exclusive of its ticks. A form with no `choice` question MUST NOT have `allow_custom`. With `allow_ask_back`, the owner MAY ask the agent, and the form stays open. A form on a `one_shot` key MUST be refused.

3.6.31 **[5.2.0]** The `answer` to a form has `answers`: one entry for each question. An entry has `question_id` and exactly one of `choice` and `text`. The frame excludes `choice`, `text` and `intent`, and it needs the action `accept`. The parser MUST check the shape. The server MUST call `validate_form_answer` before it takes the answer. The call checks the ids and the ticks, and it checks that a free text has `allow_custom`.

3.6.32 **[5.2.0]** The agent MUST read the answers of a form in the object `untrusted`, as `untrusted.answers`, under the member `data_notice`. Each entry has `question_id` and `choice` or `text`. The portal cleans the texts as it cleans a comment.

3.6.33 **[5.1.0]** A command MAY have a `language` for the color. The values are `plain`, `sql`, `json`, `yaml`, `toml`, `bash`, `python`, `rust`, `javascript`, `typescript` and `diff`. A value that is not in the list MUST be refused. The language MUST NOT change the text. The tokenizers MUST be in the core crate, with no heavy dependency. They MUST run in linear time and MUST NOT panic on any text.

### 3.7 Approvals

An approval asks the owner of a key to allow one exact action of an agent. The portal is the gate. The service stays without state and learns only the verdict. Release: 5.1.0.

3.7.1 **[5.1.0]** A grant MAY carry `confirm`. The decision is true when a grant of the action on the key side or on the owner side has `confirm`. For an owner who is the superadmin, only the `confirm` of the key counts. A file with `confirm` on a grant of the service `portal` or of `access` MUST stop at start.

3.7.2 **[5.1.0]** When `confirm` applies and no usable approval exists, the PDP MUST answer `false` with the reason `approval_required` and an `evaluation_id`. The answer MUST NOT take a unit of a budget. A service without `approvals = true` MUST get a plain `false`.

3.7.3 **[5.1.0]** The `evaluation_id` MUST be stateless. It MUST hold a key id, the time of issue and an HMAC. The HMAC covers the service, the subject, the action, the resource, the effect and the time. It MUST also hold the subject, the action and the resource, sealed with the content key, because a submission does not have them. It MUST be valid for `inbox.approvals.evaluation_ttl`. A submission without a valid `evaluation_id` for the same subject, action and resource MUST be refused.

3.7.4 **[5.1.0]** A message of approval has two parts. The command is the text of the service, made of `op` and `view`, and is bound to the effect hash. The explanation is the text of the agent, and nothing binds it. The explanation MUST NOT enter `effect`, `shown_effect` or the local hash of the phone.

3.7.5 **[5.1.0]** The effect hash is `"sha256:"` and the hex of the SHA-256 of the RFC 8785 canonical form of `{"v":1,"service","action","resource","op"}`. The portal MUST compute it again at the entry. A different value MUST get `422 effect_mismatch`. A pointer of `view` that does not exist in `op` MUST get `422 invalid_view`.

3.7.6 **[5.1.0]** The value `op` MAY hold objects, arrays, strings, booleans, null and integers between -(2^53-1) and 2^53-1. A float or a larger integer MUST get `422 invalid_op`. The PEP and the PDP MUST apply this rule.

3.7.7 **[5.1.0]** A command larger than `max_command_bytes` MUST get `413 command_too_large`. An explanation longer than `max_explanation_chars` MUST get `422 explanation_too_long`. No limit MUST truncate a text. With `require_explanation`, a request without an explanation MUST get `422 explanation_required`.

3.7.8 **[5.1.0]** Before a service submits, the agent MAY ask the portal for an intent with `approval_intent`. The call MUST return an opaque `approval_ref`. The reference MUST work one time, belong to the key of the agent, and expire after `intent_ttl`. An unknown, used, expired or foreign reference MUST get `invalid_argument`.

3.7.9 **[5.1.0]** The portal MUST allow one open request for each hash of service, subject, action, path and effect. A second submission MUST return the same request id. It MUST NOT renew the lease and MUST NOT replace the body.

3.7.10 **[5.1.0]** An `accept` MUST be refused when `shown_effect` differs from the effect, with the status `effect_mismatch`. An `accept` MUST NOT carry a text. A `decline` MAY carry a text with the intent `comment` or `change_request`, but only when `allow_custom` is true. Otherwise the status is `custom_not_allowed`. The text is at most `max_comment_chars`.

3.7.11 **[5.1.0]** The owner MUST NOT edit the command. A change request refuses the first request. The agent MAY then make a new call with another command. This makes a new effect hash and a new request that links to the old one with `previous_id`.

3.7.12 **[5.1.0]** Only the owner of the requester key MAY answer. The owner answers from a phone that is not revoked, or from a web session when `web_approval` is true. The Inbox MUST refuse an answer from an `mcp_` key, an `api_` key, a session that the superadmin opens as another account, and a service.

3.7.13 **[5.1.0]** The Inbox MUST send a request to all phones of the owner that are not revoked. The first valid answer wins. The other phones MUST get `settled`. An owner with no enrolled phone MUST get `409 no_phone` on the submission.

3.7.14 **[5.1.0]** A resume call with `usage` and the same `effect` MUST consume an approval that is `answered` with `accept`. The consumption is one compare-and-set of `consumed_at`, and it works only up to `consume_before`. The PDP then counts the unit. When the budget refuses or the count fails, the PDP MUST release the approval.

3.7.15 **[5.1.0]** One approval MUST give one execution at most. The value `consume_before` is the time of the decision plus the larger of `consume_within` and `lease_s`. After it, the approval is `lapsed`, and the service gets `approval_expired`. A crash between the consumption and the count MAY lose one approval. It MUST NOT give two executions.

3.7.16 **[5.1.0]** At the consumption, the PDP MUST check again that the key is valid, the owner has the right, the grant exists and `confirm` is current. A key that is revoked, or a grant that is removed, MUST never get `true` again. The open requests of that key MUST become `cancelled`.

3.7.17 **[5.1.0]** After an explicit refusal, the same key and the same effect MUST NOT get a new request for `deny_cooldown`. No other end gives a cooldown. After `mute_after_refusals` refusals in a row, the portal MUST mute the key for `mute_for` and alert the owner. The owner MAY refuse and mute a key for one hour.

3.7.18 **[5.1.0]** A key MUST NOT create more than `max_requests_per_key_per_hour` requests in one hour. A request above the limit MUST get `429` with `retry_after`. The code of the body is `rate_limited` (protocol 17.4), and the PEP shows it as `approval_rate_limited`. The replacements count as creations.

3.7.19 **[5.1.0]** The portal MUST give the service only the verdict: `pending`, `approved`, `denied` or `void`. The verdict `pending` covers `pending` and `replied`. The verdict `approved` or `denied` covers `answered`. Every other state gives `void`. The service MUST NOT learn which end or why.

3.7.20 **[5.1.0]** The service MUST NOT receive the comment, the explanation, the thread or the `hint`. It MUST NOT receive the identity or the device of the approver. It MUST NOT receive the time of the answer, the lease of the agent or the count of reads. The API for the service has three routes only: submit, read with `wait`, and `keepalive`. It has no route for a turn, a comment, an approver or a device.

3.7.21 **[5.1.0]** The relay to renew a lease MUST need `renew=true` and a `subject` equal to the requester key. A read without `renew=true` MUST NOT renew. The service MUST NOT renew a lease on a timer of its own.

3.7.22 **[5.1.0]** The screen MUST show the age of the request and the activity of the agent. After `stale_after`, it MUST show a banner and a mandatory box: "I know this request is N days old". The phone MUST also keep the gate "read to the end" and the local biometric prompt. The web page has no script, so it MUST show the full command.

3.7.23 **[5.1.0]** The Inbox MUST send one notification at the arrival of an approval, at the level Ring at least, and never at the level Alarm. The lock screen MUST NOT show the command or the explanation. The notification MUST have no button that approves.

3.7.24 **[5.1.0]** The service `portal` has the actions `approval.read` and `approval.cancel`. `approval.read` gives metadata and the audit, and no body. The portal has no action `approval.decide` and no action `approval.read_body`. An `mcp_` key holds none of these actions.

3.7.25 **[5.1.0]** The Inbox MUST show a repeat as such. When the same effect was approved and consumed before, the screen says "Same effect approved and consumed 00:20 ago". The portal knows this from the hash. It does not read the text.

3.7.26 **[5.1.0]** The page of the Inbox MUST show a panel "Open items" above the phones only when an item waits. It MUST have a zone "History" with filters. The matrix of the editor MUST have the columns Read and Cancel. It MUST have no column that decides. A lock icon MUST mark an action that has `approval: "optional"`.

3.7.27 **[5.1.0]** The pages of the items MUST work without script. They MUST carry `Cache-Control: no-store`, and MUST check the CSRF token and the origin on an answer and on a reply. An owner MUST see only the items of the owner. With `web_approval` false, the page MUST list the items and MUST NOT let the owner decide.

### 3.8 Agent channel

The agent talks to the portal for everything except the command. The portal has one engine behind two doors, `/mcp` and REST. The text of the agent is untrusted. Release: 5.1.0.

3.8.1 **[5.1.0]** The portal MUST serve the endpoint `inbox.mcp.path` (default `/mcp`) with JSON only, no session and no SSE. It MUST need `keys.mcp = true`. The tools are `approval_intent`, `get_answer`, `keepalive`, `say`, `supersede` and `cancel`. The tool `ask` arrives in 5.2.0.

3.8.2 **[5.1.0]** The REST routes are `POST /api/v1/approval-intents`, `GET` and `DELETE /api/v1/items/{id}`, and `POST` to `/api/v1/items/{id}/keepalive`, `/turns` and `/supersede`. A `GET` with `?wait=` renews the lease. The routes for questions arrive in 5.2.0. The routes have no alias.

3.8.3 **[5.1.0]** The MCP endpoint MUST check the origin, the client address, the key, the rate and the size of the body, in this order. A call with no key MUST get `401` with an empty body. `tools/list` MUST depend on the key `interaction`. A `one_shot` key MUST NOT see `say`.

3.8.4 **[5.1.0]** A call MUST NOT wait more than `inbox.mcp.max_wait`. The value of `max_wait` MUST NOT pass `wait_max`. The value `wait_max` has the default 25 seconds and the maximum 60. A read with no answer SHOULD return fewer than 100 tokens.

3.8.5 **[5.1.0]** A key reads an item and speaks on it only when `items.requester_key_id` is its own id. No grant is needed. A call for another item MUST get `not_found`. An `mcp_` key and an `api_` key MUST NOT use each other's door (3.3.8a).

3.8.6 **[5.1.0]** A text that a third party wrote MUST sit in an `untrusted` object, with a `data_notice`. This holds for the comment, the change request and the text of a question. The result of `get_answer` MUST NOT hold the identity or the device of the approver.

3.8.7 **[5.1.0]** The screen MUST show the line "from" (key name, owner, interaction) that the portal builds. A label that the agent gives is a subtitle. The screen MUST show the fixed banner "written by an agent". The explanation MUST carry the label "Text of the agent, not verified".

3.8.8 **[5.1.0]** An agent with no channel to the portal MUST get the verdict only. It has no explanation, no flag and no thread. It sets the lease with `lease_s` or `until` at the service. The screen then says "No explanation given by the agent".

3.8.9 **[5.1.0]** The block `inbox.mcp` MUST give the path, the maximum wait, the allowed origins and the allowed client addresses. It MUST also give the rate limits and the size of the body. A contradiction in the block MUST stop the start (6.9.6).

### 3.9 Preferences

Each account has two preferences: the digest and the quiet hours. They are in the database. Release: 5.1.0.

3.9.1 **[5.1.0]** The Inbox MUST keep the preferences of an account in the table `account_prefs`. They override the defaults of `inbox.items.digest`. They MUST NOT exceed a limit of the file. Only the account itself MAY change them, never a session that the superadmin opens as the account.

3.9.2 **[5.1.0]** The digest mode is `off`, `daily` or `every`. `daily` needs a time `HH:MM` and a time zone. `every` needs a period from 1 hour to 7 days. A name that is not an IANA time zone MUST get `422`.

3.9.3 **[5.1.0]** The Inbox MUST send one frame `digest` for each owner at each due time, even with several instances. The frame holds the count, the age of the oldest item and the ids. It MUST NOT hold a body. An empty digest MUST NOT be sent.

3.9.4 **[5.1.0]** A phone MAY set quiet hours with `set_prefs`. The server MUST sync them in the frame `welcome`. A request that arrives inside the quiet hours MUST be delivered with the flag `quiet`. The phone does not play a sound in this period, except for the urgency `high` when the user allows it.

3.9.5 **[5.1.0]** The lock screen option is local to the phone. The default is off. With it off, the notification MUST use the private visibility and a generic text.

3.9.6 **[5.1.0]** Delegation, escalation and muted agents are not part of this version. They need a new decision of the owner.

### 3.10 Product decisions on items

Each decision below is final. It has a requirement that a test can check.

3.10.1 **[5.1.0]** **Decision: no signature of the phone.** An `answer` frame MUST NOT have a signature field. The proof of an approval is an authenticated session and one accepted answer that is bound to the effect hash. A parser MUST refuse an unknown field.

3.10.2 **[5.1.0]** **Decision: no risk class.** No key, grant, action document or screen MUST depend on a risk level. The closed schema of the configuration MUST refuse a key `risk`.

3.10.3 **[5.1.0]** **Decision: the owner only.** The owner of the requester key is the only recipient and the only approver. The Inbox MUST NOT have a named account, a group, a delegation, an escalation or a two-person rule for items.

3.10.4 **[5.1.0]** **Decision: the service learns only the verdict.** The service MUST receive nothing that 3.7.20 lists. A test with a service that records its input MUST fail when one of these strings reaches it.

3.10.5 **[5.1.0]** **Decision: no end-to-end encryption.** The portal reads each body. It seals the body at rest (5.11). The specification MUST say this plainly, in 5.11.1.

3.10.6 **[5.1.0]** **Decision: plain text, and safe Markdown for the explanation.** The command, the comments of the owner and the turns of the owner are plain text. The explanation of an agent is plain text unless the agent sets `explanation_format` to `markdown` (3.6.26). No screen MUST render HTML, an image, an active link or a script from any text.

## 4. Delivery guarantee

The transport is at least once. The phone ignores a repeat. The result is that the user sees each alert exactly once.

4.1 The Inbox MUST answer `201` only after two things are on disk. They are the alert and one delivery for each phone that is subscribed at that moment. This is one transaction.

4.2 The phones that an alert targets are the phones that are subscribed when the Inbox accepts it. They can be online or offline. An alert about a service targets only the phones of the readers of 3.5.3. The Inbox MUST NOT send it to a phone whose account lost the access after the acceptance.

4.3 The Inbox MUST send again each delivery that the phone did not confirm. It does this during the session and at each reconnection.

4.4 The phone MUST store the alert durably before it confirms it. It MUST ignore an alert id that it already has. At restart, it MUST show what it stored and did not show.

4.5 The Inbox keeps an alert for a phone for `inbox.storage.delivery_ttl`. After this time, the delivery expires, and the page shows it as expired. The Inbox deletes the history after `inbox.storage.alert_retention`. This value MUST be at least `delivery_ttl`.

4.6 A gap in coverage MUST be visible. A subscribed phone that the Inbox does not see for `inbox.devices.unreachable_after` is reported as unreachable. The app shows its connection state and warns after `inbox.devices.offline_warning_after`.

4.7 A phone MUST reconnect at once when the network returns, at boot and after a restart of the server (close code 1012).

4.8 These limits are known. "Force stop" and some battery managers of vendors stop the app. The app guides the user to the battery exemption. The Inbox reports the phone as unreachable.

### 4.9 Several instances

4.9.1 With PostgreSQL, any number of instances MAY be active. An alert and its deliveries are one transaction. The idempotency key is a unique index.

4.9.2 When PostgreSQL has no writable primary, the instances MUST continue. They keep alerts in a spool in memory and send them to the connected phones. They write the spool when the primary returns. The section "Storage and failure modes" of [`README.md`](README.md) gives the limits.

4.9.2a **[5.1.0]** An item MUST NOT use the spool. When PostgreSQL has no writable primary, the creation, the renewal and the answer of an item MUST get `503 storage_degraded`. See 3.6.20.

### 4.10 Standby with SQLite

4.10.1 With the block `inbox.replica`, the Inbox copies the whole database to a shared directory. With `sync_alerts = true`, it confirms an alert, an enrollment and a revocation only after the copy holds them.

4.10.2 A lease with an epoch that increases names the active instance. An instance that loses its lease MUST stop the whole process.

### 4.11 Delivery of items

The transport of items is at least once, as for alerts. The answer is a second guarantee: the Inbox accepts exactly one answer for each item. Release: 5.1.0.

4.11.1 **[5.1.0]** The Inbox MUST answer `201` to the creation of an item only after two things are on disk. They are the item and one delivery for each phone of the owner that is not revoked. This is one transaction.

4.11.2 **[5.1.0]** A phone that enrolls after the creation MUST get the open items at its first session. The same holds at each reconnection. An item that is `abandoned` MUST NOT be sent to a phone that was off.

4.11.3 **[5.1.0]** The row of the table `alerts` for an item MUST hold metadata only. The title MUST be a fixed text, for example "Approval K7QF". The server MUST build the frame `request` when it unseals the body to send it.

4.11.4 **[5.1.0]** The phone MUST store the item before it sends `ack` with the stage `received`. It MUST ignore an id that it already has. The server MUST send again each delivery that it did not see confirmed.

4.11.5 **[5.1.0]** The phone MUST send `answer` with a `client_msg_id` that is unique in the thread. The outbox of the phone MUST send it again until it gets `answer_result`. The server MUST NOT take the same `client_msg_id` twice.

4.11.6 **[5.1.0]** Several devices and the web MAY answer at the same time. One answer MUST win. Each other device MUST get `settled` with the state, and its notification MUST be withdrawn.

4.11.7 **[5.1.0]** The status of `answer_result` is one of `accepted`, `already_answered`, `closed`, `abandoned`, `superseded`, `expired`, `cancelled`, `effect_mismatch`, `device_revoked`, `custom_not_allowed` and `ask_back_not_allowed`. An answer from a revoked device MUST NOT change the state.

4.11.8 **[5.1.0]** A renewal MUST NOT create a delivery. A change of state MUST create one delivery of the kind `settled` for each phone. The delivery MUST keep its order in the thread.

4.11.9 **[5.1.0]** An open item MUST NOT expire because of `inbox.storage.delivery_ttl`. Only the lease and `max_lifetime` end it. A phone that reconnects MUST send `sync` with its open items. The server MUST answer with the real state of each one.

4.11.10 **[5.1.0]** With `inbox.replica` and `sync_alerts`, the Inbox MUST confirm an answer only after the copy holds it.

## 5. Security

5.1 Enrollment MUST be safe on plain HTTP. The code holds the address, the public key of the server and a secret of 128 bits. The phone pins the key. It runs a Noise IK handshake to this key and sends the secret inside it.

5.2 The secret MUST work one time and MUST expire. The server MUST keep only its digest.

5.3 A session MUST use Noise IK between the key of the phone and the pinned key of the server. This gives mutual authentication, encryption and forward secrecy.

5.4 The server MUST limit the enrollment attempts for each client address. It MUST compare tokens in constant time.

5.5 The page MUST need a session of the portal. A form MUST carry an anti-CSRF token and the same-origin check.

5.6 The key file of the server is a secret. A new key file makes every phone and every code invalid. The operator MUST keep a backup copy of it.

5.7 The routes of the Inbox share the listener of the portal: `/v1/enroll` and `/v1/session` for the phones, `/api/v1/` for the senders, `/api/version` and `/readyz`. They have no session and set no cookie. The portal MUST NOT apply its session, CSRF or origin checks to them. The portal MUST match them before every other route. No page of the portal MUST use a path under `/v1/` or `/api/v1/`.

5.8 A phone session is long-lived. It MUST NOT use the budget of connections of the pages. The portal MUST give the permit of the pages back when a connection upgrades, and take a permit of a separate budget of sessions. A phone that cannot get a permit gets `503` before the upgrade.

5.9 A secret MUST NOT be in clear text in the configuration. It comes from `_env` or `_file`. The log MUST NOT hold a secret.

5.10 The portal has no TLS. A reverse proxy terminates TLS. The portal MUST NOT trust a forwarded header from a peer that is not in `server.trusted_proxies`.

The details are in [`SECURITY.md`](SECURITY.md) and [`protocol.md`](protocol.md).

### 5.11 At rest

The portal seals the body of an item before it writes it. This is not end-to-end encryption. Release: 5.1.0.

5.11.1 **[5.1.0]** The portal reads each body. It receives the text in clear on an authenticated channel, seals it, writes only the sealed value, and unseals it to serve the owner. The specification and the README MUST say that this is not end-to-end encryption.

5.11.2 **[5.1.0]** The portal MUST seal the command, the view, the explanation, the hint, the turns and the comment of the owner. For a question it MUST also seal the title, the options, the flags and the answer. The seal uses AES-256-GCM with a random 12-byte nonce.

5.11.3 **[5.1.0]** A sealed value is `0x01`, the length of the key id, the key id, the nonce, the cipher text and the tag. The sub-key is the HMAC-SHA-256 of the key with the label `portal-content/seal/v1`. The additional data is length-prefixed and holds `portal-content/v1`, the request id, the field, the sequence number and the effect hash if it exists.

5.11.4 **[5.1.0]** A sealed value that moves to another row, field or effect MUST NOT open. A reader that unseals a command MUST compute the effect hash again. On a difference, it MUST serve nothing and MUST write the log line `body_effect_mismatch`.

5.11.5 **[5.1.0]** These data stay in clear in their columns: the request id, the service, the action, the resource path, the subject and the owner. The effect, the code, the state, the dates, the lease and the decision stay in clear too. So do the device or session of the answer, the size of the sealed body and the key id. The portal MUST NOT write a title in clear for an item.

5.11.6 **[5.1.0]** The block `inbox.at_rest` MUST have `active_key` and 1 to 8 blocks `key "<id>"`. An id has 1 to 16 characters from `a-z`, `0-9`, `-` and `_`. A key holds exactly one of `secret_env` and `secret_file`, with 32 random bytes in base64. A secret in clear in the file MUST stop the start.

5.11.7 **[5.1.0]** At start, the portal MUST stop in each of these cases. The error names the id and the count of rows.

- `active_key` names no block.
- Two blocks have the same id or the same secret.
- A secret has a wrong form.
- The check value in the table `content_key_check` differs for the same id.
- A sealed row uses a key id that the file does not hold.

5.11.8 **[5.1.0]** The command `portal inbox content-key` MUST make a new key and MUST NOT overwrite a file. The command `portal inbox content-purge --key <id>` MUST erase the bodies of a lost key and keep the metadata. The operator MUST keep the keys apart from the database and from the backups.

5.11.9 **[5.2.0]** The command `portal inbox rekey` MUST seal again the rows of an old key with the active key. It MUST be idempotent, work in batches and resume after a stop. With SQLite it MUST refuse to run while an instance runs.

5.11.10 **[5.1.0]** After `body_purge_after`, the portal MUST erase the sealed value of a final item. The metadata, the effect hash and the audit MUST stay. The phone keeps its copy until its own `local_retention`. The `welcome` frame carries `body_purge_after_ms` as the default.

5.11.11 **[5.1.0]** The portal MUST NOT write a body in the log, a trace, a metric, the spool, the replica or a backup in clear. A debug print MUST hide the body. A page with a body MUST carry `Cache-Control: no-store`. A canary test MUST check each of these places.

5.11.12 **[5.1.0]** The README and the file `SECURITY.md` MUST say what the protection covers. It covers a leak of the database, a backup, a copy of the replica, a disk image, and a database administrator without the key. It covers an attacker who can write in the database.

5.11.13 **[5.1.0]** The same documents MUST say what the protection does not cover. It does not cover a person who has the key and the process, or a portal that is compromised while it runs. It does not cover a compromised host of the service. It does not cover a stolen phone or browser, or a key that leaves with the database. It does not cover the plain HTTP hop from the proxy to the portal. The operator MUST limit the PDP listener, which has no TLS, with a network policy or a private network.

5.11.14 **[5.1.0]** A portal that is dishonest MAY show one text and consume another effect. The Inbox does not prevent this. It makes it detectable. The phone MUST keep the command and its hash that it showed. The service logs the effect that it ran. The web page gives no independent proof.

5.11.15 **[5.1.0]** The switch `inbox.at_rest.admin_body_read` has the default `false`. With `false`, the portal MUST have no route that reads a body for an administrator, and the route MUST answer `404`. With `true`, only the superadmin MAY read a body, with `GET /auth/settings/inbox/items/{id}/body`.

5.11.16 **[5.1.0]** No separate right reads a body. The right `approval.read` MUST give metadata only. A session that the superadmin opens as another account MUST NOT read a body and MUST NOT answer.

5.11.17 **[5.1.0]** When `admin_body_read` is true, each read MUST write one row `body_read` in `item_events`. The row holds the login, the time and the request id, and never the body. The portal MUST count the reads in `portal_item_body_reads_total`. This trace does not stop a person who has the key and the database, because that person reads outside the application.

### 5.12 Text that comes from an agent

The agent is not trusted. Its text is shown with care. Release: 5.1.0.

5.12.1 **[5.1.0]** The screen MUST show the command first, in a fixed font, with a copy button, the age and the short hash. The explanation MUST come below it, in another style, with the label "Text of the agent, not verified". The explanation MUST NOT be copyable in one gesture, and MUST NOT hold an active link.

5.12.2 **[5.1.0]** The screen MUST show a control character, a bidirectional control and a zero-width character as a visible mark, for example `<U+202E>`. It MUST show the count of non-ASCII characters. The portal MUST NOT remove such a character from the explanation, the comments, the turns or the command of an approval.

5.12.3 **[5.1.0]** The page and the phone MUST show a text as plain text, except an explanation with the format `markdown` (3.6.27). The web page MUST escape every text, and it MUST write only the tags of the closed list of 3.6.28. The explanation longer than 400 characters MUST fold, with an action "Show all". The screen MUST state that the explanation does not change what the service runs.

5.12.4 **[5.2.0]** A screen with a free text field MUST warn: "Do not enter a password or a secret. An agent reads this answer."

## 6. Technical requirements

6.1 The Inbox is a module of the one Rust binary `portal`. It uses Hyper for HTTP. It has no embedded front.

6.2 The configuration follows the rules of the portal. It is one HCL file with a closed schema. An error stops the program at start. The program does not coerce a value.

A secret uses `_env` or `_file`. The command `portal check-config` checks the file. The delivery values (`heartbeat_interval`, `session_timeout`, `resend_after`, `max_inflight`, `unreachable_after`, `offline_warning_after`, `delivery_ttl`, `alert_retention`, `connection_log_retention`) MUST have no default. A wrong default could hide an alert.

6.3 The Inbox uses the database of the portal. It has its own chain of migrations. Each chain MUST refuse a schema that is newer than the binary.

6.4 The protocol and the cryptography of the phones are in the crate `inbox-core`, with no I/O. The server and the app share it. The cryptography exists once.

6.5 The metrics of the Inbox have the prefix `portal_inbox_`. The metrics of the alerts of the portal have the prefix `portal_watch_`. They are in the Prometheus text of the portal. The log is the JSON log of the portal.

6.6 The build uses Docker only. There is one static musl image. No command publishes an image to a registry. A release holds binaries, the signed APK and `SHA256SUMS`.

6.7 The tests use the emulator and the end-to-end tests of the binary. No physical phone is needed.

6.8 The permissions of the Inbox are on the one internal scope of the portal, the service `portal`. The actions are `inbox.receive`, `alert.send`, `alert.send_user`, `alert.read`, `phone.read`, `phone.revoke` and `phone.log`. `inbox.receive`, `alert.send` and `alert.read` apply to the resource type `inbox_group`. The other four apply to the service.

### 6.9 Configuration of items

The blocks `inbox.items`, `inbox.at_rest`, `inbox.approvals` and `inbox.mcp` follow 6.2. The reference of the keys comes from the table of keys in the code. The values below are examples. The file has no default for a key that says "required".

6.9.1 **[5.1.0]** The block `inbox.items` MUST have these required keys: `max_lifetime`, `max_open_per_key`, `max_open_per_owner`, `max_open_total`, `lease.min`, `lease.max`, `lease.grace`, `lease.revive_window`, `retention`, `body_purge_after`, `audit_retention` and `digest.mode`. The examples use 90d, 5, 20, a total, 30s, 7d, 2m, 7d, 180d, 30d, 365d and `daily`.

6.9.2 **[5.1.0]** These keys have a default: `lease.max_revivals` 3, `max_chain` 10, `max_turns` 40, `stale_after` 24h, `max_body_bytes` 16KiB and `wait_max` 25s. The ranges are in the table of keys. A value outside its range MUST stop the start. The error names the key, the range and the value.

6.9.3 **[5.1.0]** The block `inbox.approvals` MUST have `consume_within`. The block has these defaults: `evaluation_ttl` 5m, `intent_ttl` 10m, `max_command_bytes` 16KiB, `max_explanation_chars` 2000 and `require_explanation` false. It has `max_comment_chars` 2000, `max_requests_per_key_per_hour` 20, `deny_cooldown` 5m, `mute_after_refusals` 3, `mute_for` 1h and `web_approval` true. The block MUST need `inbox.items`, `inbox.at_rest`, `pdp` and `keys.mcp = true`.

6.9.4 **[5.1.0]** The key `approvals` of a block `pdp_client` has the default `false`. A value `true` without a block `inbox.approvals` MUST stop the start. A grant object has `confirm`, with the default `false`.

6.9.5 **[5.1.0]** The portal MUST stop at start for each of these contradictions.

- `lease.min` above `lease.max`.
- `lease.max` above `max_lifetime`.
- `lease.revive_window` or `consume_within` above `max_lifetime`.
- `retention` below `max_lifetime` plus `lease.revive_window`.
- `audit_retention` below `retention`.
- `body_purge_after` above `retention`.

6.9.6 **[5.1.0]** The portal MUST also stop for each of these contradictions.

- `max_command_bytes` above `pdp.max_body_bytes`.
- `require_explanation` with `max_explanation_chars` at 0.
- A mode `daily` without `at` or `timezone`.
- A block `inbox.at_rest` without `inbox.items`.
- A block `inbox.mcp` without `keys.mcp = true`.
- `inbox.mcp.max_wait` above `wait_max`.
- A block `inbox.mcp` without `inbox.items`.
- `inbox.mcp.path` equal to `metrics.path`.

6.9.7 **[5.1.0]** A configuration secret of items MUST come from `_env` or `_file`. `portal check-config` MUST show for each key of content its id, whether it is active or read-only, and its source as `env:VAR` or `file:/path`. It MUST hide the secret. No key of this section MUST be editable from the web.

6.9.8 **[5.2.0]** The block `inbox.questions` MUST activate the questions. It needs `inbox.items` and `inbox.at_rest`. It has the key `max_options` with the range 2 to 12 and the default 8.

### 6.10 Protocol 5

6.10.1 **[5.1.0]** `PROTOCOL_VERSION` MUST be 5. The server MUST close the session of an older app with the code 4426. The protocol 5 has no compatibility with protocol 4.

6.10.2 **[5.1.0]** The server sends `request`, `message`, `settled`, `digest`, `answer_result` and `welcome`. The phone sends `answer`, `reply`, `refresh`, `sync`, `set_prefs` and `ack`. The field of `ack` that names the message is `id`. Each parser MUST refuse an unknown field.

6.10.3 **[5.1.0]** A frame MUST fit in one Noise message. The protocol MUST add no new cryptography. The body goes in clear inside the Noise session. No frame MUST go to a service.

6.10.4 **[5.1.0]** The Inbox of 5.0.x has no table `items`. A database from 5.0.x MUST stop the start. The error names the table and points to the section "Upgrades" of the README. The base schema changes in place, and no migration exists.

### 6.11 Log, metrics and tests

6.11.1 **[5.1.0]** The log MUST have the lines `approval_requested`, `approval_answered`, `approval_consumed`, `approval_abandoned`, `approval_revived`, `approval_superseded`, `approval_expired`, `body_served`, `body_read`, `body_effect_mismatch` and `content_key_loaded`. No line MUST hold a body. The metrics have the prefix `portal_item_`.

6.11.2 **[5.1.0]** Each rule of time MUST use an injected clock. A test MUST simulate 90 days without a pause. The tests MUST run on SQLite and on PostgreSQL.

6.11.3 **[5.1.0]** A canary test MUST put one unique string in a command, an explanation, a comment and a turn. After a full cycle and the purge, the string MUST be absent from each table column, the replica and the backup. It MUST also be absent from the log, the traces and the metrics. It MUST be present in the decoded Noise frame and in the web page.

6.11.4 **[5.1.0]** A test with a service that records its input MUST run complete cycles with an explanation, a hint, a comment, a thread and several phones. The service MUST NOT receive any of these strings, a login, a device id or a response time.

### 6.12 Verification

Each row maps a requirement of the releases 5.1.0 and 5.2.0 to its lot and its test. The owner of the lot replaces the placeholder with the name of the test. The lots are in the plan of the work.

| Requirement | Release | Lot | Test |
|---|---|---|---|
| 3.1.5a | 5.1.0 | L3 | `proto::tests::the_kind_of_an_alert_is_alert_and_nothing_else`, `frames::the_golden_files_cover_every_frame_in_both_directions`, `old_hello_protocol_4_gets_4426` |
| 3.3.8a | 5.1.0 | L9 | `agent_channel::api_key_cannot_use_mcp_and_mcp_key_cannot_use_rest_alerts`, `mcp_contract::the_key_and_tool_cases_k01_k02_k03_r01_to_r09` (K01) |
| 3.6.1 | 5.1.0 | L5 | `an_item_reaches_each_phone_of_its_owner_that_is_not_revoked`, `create_idempotent_same_request_same_id` |
| 3.6.2 | 5.1.0 | L5 | `an_item_reaches_each_phone_of_its_owner_that_is_not_revoked`, `renewal_writes_at_most_once_per_lease_s_in_both_databases` |
| 3.6.3 | 5.1.0 | L5 | `items::tests::state_machine_every_state_times_event_table` |
| 3.6.4 | 5.1.0 | L5 | `cas_two_racers_one_answer`, `an_approval_is_consumed_once_released_and_lapses` |
| 3.6.5 | 5.1.0 | L5 | `answer_after_abandon_returns_abandoned`, `the_sweeper_abandons_after_lease_plus_grace_and_not_before`, `items::tests::abandon_after_lease_plus_grace_not_before` |
| 3.6.6 | 5.1.0 | L5, L7 | L5: `items::tests::waits_are_checked_and_never_changed`, `until_expires_and_never_abandons`. L7: `approval_api_tests::a02_lease_out_of_range_422_no_clamp`, `approval_api_tests::a02_lease_and_until_together_422`, `approval_api_tests::a10_intent_rules_explanation_one_shot` |
| 3.6.7 | 5.1.0 | L5 | `items::tests::lease_effective_is_three_times_lease_s`, `items::tests::max_lifetime_caps_lease_and_supersede_does_not_extend`, `ninety_days_are_simulated_without_a_pause_and_supersede_does_not_extend` |
| 3.6.8 | 5.1.0 | L5, L9 | L5: `items::tests::renewal_write_at_most_once_per_lease_s`, `renewal_writes_at_most_once_per_lease_s_in_both_databases`, `events_one_row_per_transition_none_per_renewal`. L9: `agent_channel::get_answer_renews_lease_without_budget_unit` (`get_answer`, `keepalive` and a read with `wait` renew, a read without `wait` does not, and no table gains a row) |
| 3.6.9 | 5.1.0 | L5 | `items::tests::abandon_after_lease_plus_grace_not_before`, `the_sweeper_abandons_after_lease_plus_grace_and_not_before` |
| 3.6.10 | 5.1.0 | L5 | `items::tests::until_reached_is_expired_neutral`, `until_expires_and_never_abandons`, `ninety_days_are_simulated_without_a_pause_and_supersede_does_not_extend` |
| 3.6.11 | 5.1.0 | L5 | `revive_in_window_max_three_notifications_once`, `revive_after_window_refused_new_request_needs_previous_id`, `items::tests::revive_needs_the_window_the_count_and_the_body`. L9: `agent_channel::keepalive_revives_in_the_window_and_gives_410_after` (the `410` and `revivable` false) |
| 3.6.12 | 5.1.0 | L5, L7 | L5: `supersede_race_one_winner_old_approval_cancelled`. L7: `approval_api_tests::supersedes_cancels_old_approval` |
| 3.6.13 | 5.1.0 | L5, L9 | L5: `the_cancel_of_a_key_and_of_an_owner_withdraws_the_open_items`. L9: `agent_channel::cancel_makes_pep_see_void` (the key cancels, the service reads `void`) |
| 3.6.14 | 5.1.0 | L5, L8 | L5: `cas_two_racers_one_answer`, `answer_effect_mismatch_state_unchanged`, `answer_after_abandon_returns_abandoned`. L8: `approve_on_phone_then_pdp_true_once`, `two_phones_race_one_accepted_other_settled`, `answer_after_abandon_gets_abandoned_and_settled` |
| 3.6.15 | 5.1.0 | L5, L8, L9 | L5: `the_thread_follows_the_flags_the_limits_and_the_pace_of_the_agent`. L8: `approval_api_tests::reply_moves_to_replied_and_agent_sees_user_message`, `custom_and_ask_back_are_refused_without_the_flags`. L9: to fill |
| 3.6.16 | 5.1.0 | L2 | `key_interaction_default_by_kind`, `config_interaction_values_closed`, `the_interaction_field_is_closed_and_an_absent_field_changes_nothing`, `grant_roundtrip_confirm_both_backends` |
| 3.6.17 | 5.1.0 | L7, L9 | L7: `approval_api_tests::a10_intent_rules_explanation_one_shot` (`422 interaction_not_allowed`). L9: `agent_channel::say_needs_allow_ask_back_and_conversational_key` (a `say` of a `one_shot` key: the tool is hidden at the endpoint, and `422 interaction_not_allowed` at the REST door); the refusal of a `reply` of a `one_shot` item is in L5 `the_thread_follows_the_flags_the_limits_and_the_pace_of_the_agent` |
| 3.6.18 | 5.1.0 | L5 | `caps_per_key_owner_total` |
| 3.6.19 | 5.1.0 | L5 | `body_purge_keeps_metadata_effect_and_events` |
| 3.6.20 | 5.1.0 | L5 | `outage_extends_open_leases_by_the_gap_no_false_abandon`, `degraded_storage_returns_storage_degraded_and_writes_nothing` |
| 3.6.21 | 5.2.0 | L13 | to fill |
| 3.6.22 | 5.2.0 | L13 | to fill |
| 3.6.23 | 5.2.0 | L13 | to fill |
| 3.6.24 | 5.2.0 | L13 | to fill |
| 3.6.25 | 5.2.0 | L13 | to fill |
| 3.7.1 | 5.1.0 | L2 | `confirm_true_when_key_or_owner_grant_has_it`, `superadmin_owner_uses_key_confirm_only`, `config_confirm_on_portal_service_stops_start`, `config_confirm_on_access_stops_start` |
| 3.7.2 | 5.1.0 | L6 | Conformance D76, D82, D86 to D88 (`conformance_d::the_cases_of_the_series_d_pass`), `pdp::approval_tests::confirm_with_client_without_approvals_flag_plain_false`, `pdp::approval_tests::answers_carry_only_listed_reasons`, `pdp::approval_tests::other_subject_resource_or_effect_not_applicable` |
| 3.7.3 | 5.1.0 | L6, L7 | L6: `pdp::approval::tests::the_evaluation_id_is_stateless_valid_on_another_instance_for_the_ttl_and_binds_each_fact`, `pdp::approval::tests::an_evaluation_id_survives_a_rotation_of_the_content_key_while_the_old_key_is_in_the_file`, `pdp::approval_tests::the_evaluation_id_of_an_answer_binds_the_question_and_works_on_another_instance`. L7: `approval_api_tests::a01_evaluation_id_required_other_subject_422`, `pdp::approval::tests::a_submission_opens_the_id_with_the_service_and_the_effect_and_gets_the_sealed_facts` |
| 3.7.4 | 5.1.0 | L3, L7 | L3: `frames::golden_approval_verifies_and_a_changed_op_does_not`. L7: `approval_api_tests::a11_ref_single_use_bound_to_key_expires` (the effect of a request with an explanation is the effect of `op`) |
| 3.7.5 | 5.1.0 | L3, L7 | L3: `effect::tests::effect_vectors_match`, `effect::tests::view_pointer_must_exist`, `effect::tests::effect_changes_with_service_action_resource_op_each`. L7: `approval_api_tests::a08_effect_mismatch_invalid_view_command_too_large` |
| 3.7.6 | 5.1.0 | L3, L7 | L3: `effect::tests::jcs_rejects_float_and_integer_above_2_53`. L7: `approval_api_tests::a08_effect_mismatch_invalid_view_command_too_large`, `approval_api::tests::a_bad_op_is_invalid_op_and_a_bad_envelope_is_invalid_argument` |
| 3.7.7 | 5.1.0 | L7 | `approval_api_tests::a08_effect_mismatch_invalid_view_command_too_large`, `approval_api_tests::a10_intent_rules_explanation_one_shot` |
| 3.7.8 | 5.1.0 | L7 | `approval_api_tests::a11_ref_single_use_bound_to_key_expires`, `approval_api_tests::intent_content_not_readable_by_pep`, `approval_api_tests::a10_intent_rules_explanation_one_shot` |
| 3.7.9 | 5.1.0 | L7 | `approval_api_tests::a01_idempotent_repost_same_id_no_renewal_no_body_swap`, `approval_api_tests::idempotence_under_concurrency_one_open_request_for_each_effect`, `approval_api_tests::a_unique_index_on_the_hash_refuses_a_second_open_request_and_frees_it_at_the_end` |
| 3.7.10 | 5.1.0 | L8 | to fill |
| 3.7.11 | 5.1.0 | L8 | `approval_api_tests::decline_with_comment_reaches_agent_never_pep`, `frames::tests::previous_and_follows_come_from_the_facts` |
| 3.7.12 | 5.1.0 | L8, L10 | L8: `a_phone_of_another_account_gets_nothing_and_cannot_answer`, `revoked_device_answer_refused_4403_and_no_state_change`. L10: to fill |
| 3.7.13 | 5.1.0 | L8 | `two_phones_race_one_accepted_other_settled`, `phone_enrolled_after_creation_receives_open_items`, `two_instances_each_flush_their_own_phone` |
| 3.7.14 | 5.1.0 | L6 | Conformance D77, D78, D81, `pdp::approval_tests::consume_takes_one_unit_once`, `pdp::approval_tests::budget_exceeded_keeps_approval_usable_release_path`, `pdp::approval_tests::two_resume_calls_race_one_true`, `pdp::approval_tests::hundred_concurrent_resumes_one_true`, `items_tests::concurrent_consumes_use_an_approval_once_and_status_consumes_nothing` |
| 3.7.15 | 5.1.0 | L6 | Conformance D80, `pdp::approval_tests::crash_between_consume_and_count_never_double_executes`, `pdp::approval_tests::abandoned_or_lapsed_reads_as_approval_expired`, `items_tests::an_approval_is_consumed_once_released_and_lapses` |
| 3.7.16 | 5.1.0 | L6 | Conformance D84, `pdp::approval_tests::revoked_key_or_removed_grant_after_approval_is_false`, `pdp::approval_tests::revoking_a_key_cancels_its_open_approvals` |
| 3.7.17 | 5.1.0 | L7 | `approval_api_tests::deny_cooldown_same_effect_only_and_not_after_abandon`, `approval_api_tests::mute_after_three_refusals_alerts_owner`. The button "refuse and mute the key for one hour" comes with the phone and the web |
| 3.7.18 | 5.1.0 | L7 | `approval_api_tests::a01_429_after_cap` |
| 3.7.19 | 5.1.0 | L7 | `approval_api_tests::the_verdict_follows_the_state_and_only_four_values_exist`, `approval_api_tests::a12_get_returns_only_verdict_fields` |
| 3.7.20 | 5.1.0 | L7, L12 | L7: `approval_api_tests::pep_observation_unit`, `approval_api_tests::a12_get_returns_only_verdict_fields`, `approval_api_tests::the_routes_of_the_api_answer_the_methods_and_the_unknown_paths_as_17_4_says`. L12: to fill (the full canary of the release) |
| 3.7.21 | 5.1.0 | L7 | `approval_api_tests::a03_renew_only_with_subject_of_requester` |
| 3.7.22 | 5.1.0 | L10, A2 | to fill |
| 3.7.23 | 5.1.0 | A2 | to fill |
| 3.7.24 | 5.1.0 | L10 | to fill |
| 3.7.25 | 5.1.0 | L10 | to fill |
| 3.7.26 | 5.1.0 | L10 | to fill |
| 3.7.27 | 5.1.0 | L10 | to fill |
| 3.8.1 | 5.1.0 | L9 | `mcp_contract::the_transport_cases_t01_to_t13` (T01 to T13: JSON only, no session, the versions), `agent_channel::the_endpoint_follows_the_path_of_the_file_and_the_block_gates_the_tools`, `config::tests::a_bad_key_of_the_block_mcp_or_a_contradiction_stops_the_start_with_the_key_the_range_and_the_value` (`keys.mcp = true`) |
| 3.8.2 | 5.1.0 | L9 | `agent_channel::rest_door_serves_the_same_core_with_its_statuses`, `get_answer_renews_lease_without_budget_unit` (the read with `wait` renews, the read without it does not) |
| 3.8.3 | 5.1.0 | L9 | `mcp_contract::the_transport_cases_t01_to_t13` (T09 to T12, the order of the checks), `the_key_and_tool_cases_k01_k02_k03_r01_to_r09` (K01, K02), `agent_channel::one_shot_key_tools_list_hides_say_and_supersede`, `unauthenticated_flood_limited_per_address`, `key_over_its_rate_gets_429_and_retry_after` |
| 3.8.4 | 5.1.0 | L9 | `agent_channel::wait_never_exceeds_max_wait`, `supersede_cancels_old_approval_and_new_waits` (the wait wakes at once), `get_answer_returns_comment_in_untrusted_with_intent` (a pending read is short), `config::tests::a_bad_key_of_the_block_mcp_or_a_contradiction_stops_the_start_with_the_key_the_range_and_the_value` (`max_wait` against `wait_max`) |
| 3.8.5 | 5.1.0 | L9 | `agent_channel::agent_cannot_read_or_speak_on_another_keys_item`, `api_key_cannot_use_mcp_and_mcp_key_cannot_use_rest_alerts`, `cancel_makes_pep_see_void` |
| 3.8.6 | 5.1.0 | L9 | `agent_channel::get_answer_returns_comment_in_untrusted_with_intent` (the notice first, the intent, no approver and no device), `the_texts_of_the_agent_never_reach_the_service_through_the_channel`, `mcp_tools::tests::the_notice_is_the_first_member_of_a_result_with_untrusted_text` |
| 3.8.7 | 5.1.0 | L8, L10, A2 | L8: `frames::tests::the_line_from_never_holds_text_of_an_agent`. L10, A2: to fill |
| 3.8.8 | 5.1.0 | L9 | `agent_channel::get_answer_renews_lease_without_budget_unit` (an item that the service made with `lease_s` and no intent), `approval_api_tests::pep_observation_unit` (L7) |
| 3.8.9 | 5.1.0 | L9 | `config::tests::the_block_mcp_has_the_documented_defaults_and_the_set_values_apply`, `a_bad_key_of_the_block_mcp_or_a_contradiction_stops_the_start_with_the_key_the_range_and_the_value` |
| 3.9.1 | 5.1.0 | L11 | to fill |
| 3.9.2 | 5.1.0 | L11 | to fill |
| 3.9.3 | 5.1.0 | L11 | to fill |
| 3.9.4 | 5.1.0 | L8, L11 | L8: `set_prefs_is_stored_and_comes_back_in_the_welcome`. L11: to fill |
| 3.9.5 | 5.1.0 | A3 | to fill |
| 3.9.6 | 5.1.0 | L11 | to fill |
| 3.10.1 | 5.1.0 | L3 | `frames::every_frame_rejects_unknown_field`, `invalid/client/answer-with-signature.json` in `frames::invalid_frames_are_refused` |
| 3.10.2 | 5.1.0 | L2 | `config_risk_key_is_refused_and_no_document_depends_on_a_risk_level` |
| 3.10.3 | 5.1.0 | L7 | L7: `approval_api_tests::no_phone_409`, `approval_api_tests::mute_after_three_refusals_alerts_owner` (the owner is the only recipient). L8 and L10 add the phone and the web screens |
| 3.10.4 | 5.1.0 | L12 | to fill |
| 3.10.5 | 5.1.0 | L4 | to fill |
| 3.10.6 | 5.1.0 | L10 | to fill |
| 4.9.2a | 5.1.0 | L5 | `degraded_storage_returns_storage_degraded_and_writes_nothing` |
| 4.11.1 | 5.1.0 | L5 | `an_item_reaches_each_phone_of_its_owner_that_is_not_revoked` |
| 4.11.2 | 5.1.0 | L8 | `phone_enrolled_after_creation_receives_open_items`, `offline_phone_receives_request_and_settled_on_reconnect`, `answer_after_abandon_gets_abandoned_and_settled` |
| 4.11.3 | 5.1.0 | L8 | `no_plaintext_in_alerts_table_after_cycle`, `frames::tests::an_approval_frame_carries_the_command_and_verifies` |
| 4.11.4 | 5.1.0 | A1 | to fill |
| 4.11.5 | 5.1.0 | A1, L8 | L8: `approve_on_phone_then_pdp_true_once` (the same `client_msg_id` again), `ack_by_id_stops_retransmission`. A1: to fill |
| 4.11.6 | 5.1.0 | L8 | `two_phones_race_one_accepted_other_settled`, `offline_phone_receives_request_and_settled_on_reconnect` |
| 4.11.7 | 5.1.0 | L8 | `effect_mismatch_refused`, `revoked_device_answer_refused_4403_and_no_state_change`, `custom_and_ask_back_are_refused_without_the_flags`, `answer_after_abandon_gets_abandoned_and_settled` |
| 4.11.8 | 5.1.0 | L8 | `offline_phone_receives_request_and_settled_on_reconnect`, `two_phones_race_one_accepted_other_settled` |
| 4.11.9 | 5.1.0 | L8 | `sync_reconciles_open_and_closed`, `phone_enrolled_after_creation_receives_open_items` |
| 4.11.10 | 5.1.0 | L8 | The answer of a phone calls `Items::answer`, which waits for the copy of the replica before it returns (test of L5: `canary_absent_in_every_column_and_in_replica_snapshot`). No separate test |
| 5.11.1 | 5.1.0 | L4 | `the_documents_say_what_the_seal_covers_and_does_not_cover` |
| 5.11.2 | 5.1.0 | L5 | `canary_absent_in_every_column_and_in_replica_snapshot` |
| 5.11.3 | 5.1.0 | L4 | `seal_open_round_trip`, `aad_binds_request_field_seq_and_effect` |
| 5.11.4 | 5.1.0 | L4, L5 | L4: `moved_sealed_value_does_not_open`; L5: `a_sealed_value_that_moves_does_not_open` |
| 5.11.5 | 5.1.0 | L5 | `canary_absent_in_every_column_and_in_replica_snapshot`, `an_item_reaches_each_phone_of_its_owner_that_is_not_revoked` |
| 5.11.6 | 5.1.0 | L4 | `plain_secret_in_file_stops_start`, `the_blocks_items_and_at_rest_give_typed_values_with_documented_defaults`, `two_keys_active_writes_all_read`, `active_key_unknown_stops_start` |
| 5.11.7 | 5.1.0 | L4, L5 | L4: `active_key_unknown_stops_start`, `duplicate_key_id_or_secret_stops_start`, `two_ids_with_one_key_stop_the_start`, `secret_not_32_bytes_or_all_equal_stops_start`, `wrong_key_same_id_fails_check_value`, `wrong_key_same_id_stops_the_start_and_the_log_names_the_keys`, `a_key_id_that_the_file_lost_is_reported_with_its_count`; L5: `orphan_body_kid_stops_start_with_count` |
| 5.11.8 | 5.1.0 | L4, L5 | L4: `content_key_command_never_overwrites`; L5: `orphan_body_kid_stops_start_with_count` (the function behind `content-purge`) |
| 5.11.9 | 5.2.0 | L15 | to fill |
| 5.11.10 | 5.1.0 | L5 | `body_purge_keeps_metadata_effect_and_events` |
| 5.11.11 | 5.1.0 | L12 | to fill |
| 5.11.12 | 5.1.0 | L4 | `the_documents_say_what_the_seal_covers_and_does_not_cover` |
| 5.11.13 | 5.1.0 | L4 | `the_documents_say_what_the_seal_covers_and_does_not_cover` |
| 5.11.14 | 5.1.0 | A2, L12 | to fill |
| 5.11.15 | 5.1.0 | L10 | to fill |
| 5.11.16 | 5.1.0 | L10 | to fill |
| 5.11.17 | 5.1.0 | L10 | to fill |
| 3.6.26 | 5.1.0 | L3b | `frames::markdown_and_language_members_are_kept_and_plain_is_a_valid_spelling`, `frames::new_invalid_frames_fail_for_the_named_reason`, `frames::tests::a_markdown_turn_of_the_agent_keeps_its_format`, `agent_channel::the_format_and_the_language_of_an_intent_reach_the_frame_of_the_phone`, `agent_channel::a_turn_of_the_agent_can_be_markdown_and_the_message_frame_says_so`, `FramesTest.theMarkdownAndLanguageMembersReadToTheTypedValues` |
| 3.6.27 | 5.1.0 | L3b | `render::markdown::tests::limits_make_the_text_plain`, `render::markdown::tests::autolinks_html_and_images_are_text`, `render::markdown::tests::tables_footnotes_and_rules_are_text`, `render::markdown::tests::entities_are_text`, `render::markdown::tests::random_markdown_never_panics_and_always_gives_a_tree`, `render::markdown::tests::nested_structures_keep_their_shape`, `markdown_html::the_xss_corpus_gives_only_allowed_tags_and_shows_the_source_as_text`, `markdown_html::bombs_and_long_texts_stay_inside_the_subset` |
| 3.6.28 | 5.1.0 | L3b | `render::markdown::tests::links_show_the_host_and_never_act`, `markdown_html::the_xss_corpus_gives_only_allowed_tags_and_shows_the_source_as_text`, `markdown_html::no_class_comes_from_the_text`, `markdown_html::bidi_and_zero_width_characters_are_visible_marks` |
| 3.6.29 | 5.2.0 | L3b | `frames::new_invalid_frames_fail_for_the_named_reason`, `frames::the_largest_form_fits_one_noise_message`, `frames::tests::a_stored_body_that_breaks_a_rule_of_the_form_gives_no_frame`, `FramesTest.theFormRulesAreRefusedWithTheNamedReason`, `FramesTest.theLargestFormFitsOneNoiseMessage` |
| 3.6.30 | 5.2.0 | L3b | `form::tests::custom_text_needs_allow_custom_and_is_exclusive`, `frames::tests::a_stored_body_that_breaks_a_rule_of_the_form_gives_no_frame` (a form on a `one_shot` key) |
| 3.6.31 | 5.2.0 | L3b | `form::tests::table_of_refusals`, `form::tests::a_complete_answer_passes`, `frames::tests::the_owner_answer_holds_the_answers_of_a_form`, `FramesTest.theGoldenAnswerOfAFormReadsAndWritesBack`. L13 wires the check into the engine. |
| 3.6.32 | 5.2.0 | L3b | `agent_channel::get_answer_returns_the_answers_of_a_form_in_untrusted` |
| 3.6.33 | 5.1.0 | L3b | `render::highlight::tests` (one test for each language, `unterminated_constructs_run_to_the_end`, `random_text_never_panics_and_spans_are_in_bounds`, `work_is_linear_on_hostile_input`), `markdown_html::a_command_keeps_its_characters_and_gets_spans`, `bridge::tests::markdown_and_highlight_bridges`, `lib::tests::each_native_function_of_the_app_has_its_jni_symbol` |
| 5.12.1 | 5.1.0 | L10, A2 | to fill |
| 5.12.2 | 5.1.0 | L10, A2 | to fill |
| 5.12.3 | 5.1.0 | L10 | to fill |
| 5.12.4 | 5.2.0 | L14, A4 | to fill |
| 6.9.1 | 5.1.0 | L4 | `no_default_for_mandatory_keys_message_names_key_range_value`, `the_blocks_items_and_at_rest_give_typed_values_with_documented_defaults` |
| 6.9.2 | 5.1.0 | L4 | `the_blocks_items_and_at_rest_give_typed_values_with_documented_defaults`, `no_default_for_mandatory_keys_message_names_key_range_value` |
| 6.9.3 | 5.1.0 | L6, L7 | L6: `the_block_approvals_gives_typed_values_with_documented_defaults_and_the_flag_of_a_client`, `a_contradiction_of_the_approvals_stops_the_start_with_the_key_the_range_and_the_value` (`consume_within`, `evaluation_ttl`, `deny_cooldown`, the blocks `pdp` and `keys.mcp`). L7: `config::tests::the_keys_of_the_approval_api_have_the_documented_defaults_and_the_set_values_apply`, `a_bad_key_of_the_approval_api_or_a_contradiction_stops_the_start_with_the_key_the_range_and_the_value` |
| 6.9.4 | 5.1.0 | L2, L6 | L2: `config_confirm_is_a_member_of_a_grant_object_with_the_default_false`. L6: `the_block_approvals_gives_typed_values_with_documented_defaults_and_the_flag_of_a_client`, `a_contradiction_of_the_approvals_stops_the_start_with_the_key_the_range_and_the_value` |
| 6.9.5 | 5.1.0 | L4, L6 | L4: `a_contradiction_of_the_items_stops_the_start_with_the_two_keys_and_values`; L6: `a_contradiction_of_the_approvals_stops_the_start_with_the_key_the_range_and_the_value` (`consume_within` above `max_lifetime` cannot happen with today's ranges: 1 hour at most against 1 day at least) |
| 6.9.6 | 5.1.0 | L4, L6, L7, L9 | L4: `the_digest_mode_decides_which_keys_apply`, `the_blocks_items_and_at_rest_need_each_other`; L6 has no contradiction in this list. L7: `config::tests::a_bad_key_of_the_approval_api_or_a_contradiction_stops_the_start_with_the_key_the_range_and_the_value` (`max_command_bytes` above `pdp.max_body_bytes`, `require_explanation` with `max_explanation_chars = 0`). L9: `config::tests::a_bad_key_of_the_block_mcp_or_a_contradiction_stops_the_start_with_the_key_the_range_and_the_value` (`inbox.mcp` without `keys.mcp = true`, `max_wait` above `wait_max`, the path of the metrics) |
| 6.9.7 | 5.1.0 | L4 | `print_config_masks_secret_shows_kid_and_source`, `plain_secret_in_file_stops_start` |
| 6.9.8 | 5.2.0 | L13 | to fill |
| 6.10.1 | 5.1.0 | L3 | `old_hello_protocol_4_gets_4426`, `frames::old_hello_and_old_ack_do_not_parse` |
| 6.10.2 | 5.1.0 | L3 | `frames::frames_round_trip_golden`, `frames::every_frame_rejects_unknown_field`, `frames::title_with_newline_or_control_rejected`. L8: `approve_on_phone_then_pdp_true_once`, `sync_reconciles_open_and_closed`, `frames::tests::a_digest_has_the_count_and_the_oldest_first_and_never_comes_empty` |
| 6.10.3 | 5.1.0 | L3 | `frames::request_fits_one_noise_message_at_max_sizes`, `frames::encode_refuses_a_frame_above_one_noise_message` |
| 6.10.4 | 5.1.0 | L5 | `old_inbox_schema_stops_start` |
| 6.11.1 | 5.1.0 | L5 | `the_journal_and_the_metrics_follow_the_names_of_the_specification` |
| 6.11.2 | 5.1.0 | L5 | `ninety_days_are_simulated_without_a_pause_and_supersede_does_not_extend`; each test of `items_tests.rs` runs on SQLite and on PostgreSQL |
| 6.11.3 | 5.1.0 | L12 | to fill |
| 6.11.4 | 5.1.0 | L12 | to fill |
| 8.2.1 | 5.1.0 | L12 | to fill |
| 8.2.2 | 5.2.0 | L15 | to fill |
| 8.2.3 | 5.1.0 | L12 | to fill |
| 8.2.4 | 5.1.0 | S1 | this document |
| 8.3.1 | 5.1.0 | L12 | to fill |
| 8.3.2 | 5.1.0 | L12 | to fill |
| 8.3.3 | 5.1.0 | L12 | to fill |
| 8.3.4 | 5.1.0 | L12 | to fill |
| 8.3.5 | 5.1.0 | L12 | to fill |


## 7. Integration

7.1 Grafana: a contact point of the type Webhook sends to `POST /api/v1/webhooks/alertmanager/<group>` with a bearer `api_` key. [`grafana/contact-point.yaml`](grafana/contact-point.yaml) is an example.

7.2 Alertmanager: a `webhook_configs` receiver does the same. [`alertmanager.yml`](alertmanager.yml) is an example.

## 8. Roadmap: steps and releases

The roadmap has three steps. Step 1 is version 5.0.x. Step 2 is not built. Step 3 is the items.

Release 5.1.0 has the approvals and the discussion. Release 5.2.0 has the questions. The requirements of step 3 are in 3.6 to 3.10, 4.11, 5.11 and 5.12. The decisions of the owner are final.

### 8.1 Step 2: agents send alerts (FUTURE)

8.1.1 **FUTURE.** An agent MUST send alerts through the REST route of 3.3, with an `api_` key of its own.

8.1.2 **FUTURE.** The MCP side MAY accept an alert from an `mcp_` key. It comes with the tools map: a tool `send_alert` maps to the action `alert.send`.

8.1.3 **FUTURE.** The zone **Sent** SHOULD show a webhook icon for an `api_` key and a robot icon for an `mcp_` key.

### 8.2 Step 3: approvals and questions

8.2.1 **[5.1.0]** Release 5.1.0 MUST hold the grant `confirm`, the approvals and the discussion. It MUST hold the agent channel with REST and `/mcp`, the sealed bodies, the preferences, the web pages and the phone screens. The key `interaction` and the variant `one_shot` MUST be in the same release.

8.2.2 **[5.2.0]** Release 5.2.0 MUST hold the questions, their web rows, the screens of the phone for questions, and the command `rekey`. It MUST add the actions `question.ask`, `question.read` and `question.cancel`, and the budget `questions`.

8.2.3 **[5.1.0]** The server and the app MUST ship together, because protocol 5 breaks protocol 4. The section "Upgrades" of the README MUST say three things. A new inbox database is needed. The app must be installed again. A new content key must live outside the data volume and outside the backup.

8.2.4 **[5.1.0]** The sketch of step 3 in the earlier text of this section is void. Its frames `question`, `answer`, `approval` and `decision` give way to the frames of 6.10. Its rule "a timeout means deny" gives way to 3.6.9 and 3.6.10. Its actions `approval.request`, `approval.decide` and `inbox_user` do not exist.

### 8.3 Out of scope: removed, not deferred

8.3.1 **[5.1.0]** The Inbox MUST NOT have end-to-end encryption, device keys, a key directory or browser keys.

8.3.2 **[5.1.0]** The Inbox MUST NOT send an item to a recipient other than the owner. This removes named accounts, groups, `question.ask_user`, `inbox_user` and `inbox_group` for items. It also removes two-person rules, explicit approvers, temporary delegation and escalation to a group.

8.3.3 **[5.1.0]** An approval MUST NOT give a right that stays. An approval for a person on a web page of a service is out of scope.

8.3.4 **[5.1.0]** The Inbox MUST NOT have a risk class or a signed proof from the phone.

8.3.5 **[5.1.0]** The Inbox MUST NOT support iOS, a push service of a third party, or an approval by elicitation of the MCP client.

### 8.4 Deferred

8.4.1 **FUTURE.** The server MAY drive the execution with MCP Tasks and the URL mode of elicitation, after release 5.2.0.

8.4.2 **FUTURE.** The Inbox MAY offer parallel alternatives and a floor `approval: "required"` after the first use of 5.1.0.

8.4.3 **FUTURE.** The items MAY get attachments and images. The content keys MAY come from a KMS or an HSM. These points are not scheduled.

8.4.4 **FUTURE.** Delegation, escalation, muted agents, a quorum and a vote need a new decision of the owner. So do a webhook to the agent and an adapter for the Claude Agent SDK.
