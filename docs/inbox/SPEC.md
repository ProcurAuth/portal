# Inbox requirements

This document states what the Inbox must do and why. The Inbox is a feature of the portal. The code is the reference for details. [The design page](../design/inbox.md) gives the reasons for the main choices.

The words MUST, MUST NOT, SHOULD and MAY are the key words of RFC 2119 and RFC 8174.

A requirement has one of two states. A requirement without a mark is in force since version 5.0.0. A requirement with a mark of a release, for example **[5.1.0]** or **[5.5.0]**, is in force since that release. A requirement that the code does not implement has the line "Status: not implemented in 5.6.1". Section 8 lists the scope.

Each requirement has an id and a testable verb. Section 6.12 gives the tests for the requirements of the releases 5.1.0 to 5.5.0. The ids of the requirements stay the same between versions, and the code and the tests cite them.

## 1. Purpose

1.1 The Inbox sends alerts to Android phones. An alert can be vital. Each subscribed phone MUST get each alert, and MUST show it one time.

1.2 This holds when the phone was off, in airplane mode or disconnected when the alert came. It also holds when the app was stopped.

1.3 The Inbox is part of the portal. One account of the portal gives one identity and one set of permissions for the launcher, the keys and the Inbox.

## 2. Scope

| In scope | Out of scope |
|---|---|
| The Inbox feature of the binary `portal`. | A standalone alert server binary. |
| The Android app Portal Inbox. It receives alerts and items, and the owner answers the items on it. | iOS. It cannot keep a connection in the background without the push service of Apple. |
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

3.1.4 An alert has no human acknowledgement. The phone sends only an automatic receipt after it stores the alert. Without the receipt, the server cannot send the alert again, and the guarantee of delivery fails.

3.1.5 Each alert MUST have the kind `alert`. The wire frame and the stored alert carry it in the field `kind`.

3.1.5a **[5.1.0]** Protocol 5 has the message kinds `alert`, `question`, `approval`, `turn` and `digest`. A parser of protocol 5 MUST refuse any other value. The rule of one-way messages (3.1.4) is true for `alert` only. A phone of protocol 4 MUST get the close code 4426.

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

3.4.1 The page of the Inbox is the tab Inbox of the settings of the portal, at `/auth/settings/inbox`. It needs a session of the portal and the permissions of the portal features. The pages of the requests are under it (3.7.26).

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

An approval asks the owner of a key to say yes or no to one exact action of an agent. The portal is the gate. The service stays without state and learns only the verdict. Release: 5.1.0.

3.7.1 **[5.1.0]** A grant MAY carry `confirm`. For a key, the decision asks for an approval when a grant of the key or of its owner has `confirm`. For an owner who is the superadmin, only the `confirm` of the key counts.

3.7.1a **[5.5.0]** The `confirm` of a grant of a person MUST have no effect on a question of that person (protocol 17.8). It applies to the keys of the person. The approval API MUST never get a person as the subject of a request.

3.7.1b **[5.5.0]** A file with `confirm` on a grant of the service `portal`, of `access` or of `mcp.session` MUST stop at start. A form MUST refuse it. The action `mcp.session` has no effect to approve, and a method of this kind cannot wait. The PDP MUST ignore `confirm` on `access` and on `mcp.session` in the data.

3.7.2 **[5.3.0]** When `confirm` applies and no usable approval exists, the PDP MUST answer `false` with the reason `approval_required` and an `evaluation_id`. The answer MUST NOT take a unit of a budget. This holds for each action of each service with a known document, when the block `inbox.approvals` exists. Without the block, the PDP MUST answer a plain `false`.

3.7.2a **[5.3.0]** Each action of each service MAY be gated by a grant with `confirm`, except `access`, `mcp.session` and the actions of the service `portal`. The service `portal` is the scope of the functions of the portal. It is not a service of protocol 1.3: it has no PEP, no document at `/.well-known/procurauth/actions` and no identity token (protocol 1.3a). The file and the document of actions MUST have no flag and no member for it. The portal MUST ignore a member `approval` in an action. A service MUST handle `approval_required` for all its actions.

3.7.2c **[5.5.0]** The approval follows the action of the use of an operation alone (protocol 5.5a, 17.7). The PDP MUST ignore `confirm` for a question with `context.permission_only` equal to `true`. It MUST answer HTTP 400 for this member with `context.usage`, with `context.approval` or with another value, and `invalid_request` in a batch item.

3.7.2b **[5.3.0]** Any token of a block `pdp_client` MAY use the approval API when the block `inbox.approvals` exists. A request with no valid token of a block `pdp_client` MUST get `401` with the code `unauthorized` and the header `WWW-Authenticate: Bearer realm="portal-pdp"`. A valid token and no block `inbox.approvals` MUST get `403` with the code `forbidden`. The sealed `evaluation_id` MUST bind a submission to the service, the subject, the action, the resource and the effect of the answer `approval_required`. A submission for another action, resource or effect MUST get `422 invalid_evaluation`. The body of a submission MUST NOT name the action.

3.7.3 **[5.1.0]** The `evaluation_id` MUST be stateless. It MUST hold a key id, the time of issue and an HMAC. The HMAC covers the service, the subject, the action, the resource, the effect and the time. It MUST also hold the subject, the action and the resource, sealed with the content key, because a submission does not have them. It MUST be valid for `inbox.approvals.evaluation_ttl`. A submission without a valid `evaluation_id` for the same subject, action and resource MUST be refused.

3.7.4 **[5.1.0]** A message of approval has two parts. The command is the text of the service, made of `op` and `view`, and is bound to the effect hash. The explanation is the text of the agent, and nothing binds it. The explanation MUST NOT enter `effect`, `shown_effect` or the local hash of the phone.

3.7.5 **[5.1.0]** The effect hash is `"sha256:"` and the hex of the SHA-256 of the RFC 8785 canonical form of `{"v":1,"service","action","resource","op"}`. The portal MUST compute it again at the entry. A different value MUST get `422 effect_mismatch`. A pointer of `view` that does not exist in `op` MUST get `422 invalid_view`.

3.7.6 **[5.1.0]** The value `op` MAY hold objects, arrays, strings, booleans, null and integers between -(2^53-1) and 2^53-1. A float or a larger integer MUST get `422 invalid_op`. The PEP and the PDP MUST apply this rule.

3.7.7 **[5.1.0]** A command larger than `max_command_bytes` MUST get `413 command_too_large`. An explanation longer than `max_explanation_chars` MUST get `422 explanation_too_long`. No limit MUST truncate a text. With `require_explanation`, a request without an explanation MUST get `422 explanation_required`.

3.7.8 **[5.1.0]** Before a service submits, the agent MAY ask the portal for an intent with `approval_intent`. The call MUST return an opaque `approval_ref`. The reference MUST work one time, belong to the key of the agent, and expire after `intent_ttl`. An unknown, used, expired or foreign reference MUST get `invalid_argument`.

3.7.9 **[5.1.0]** The portal MUST keep at most one open request for each hash of service, subject, action, path and effect. A second submission MUST return the same request id. It MUST NOT renew the lease and MUST NOT replace the body.

3.7.10 **[5.1.0]** An `accept` MUST be refused when `shown_effect` differs from the effect, with the status `effect_mismatch`. An `accept` MUST NOT carry a text. A `decline` MAY carry a text with the intent `comment` or `change_request`, but only when `allow_custom` is true. Otherwise the status is `custom_not_allowed`. The text is at most `max_comment_chars`.

3.7.11 **[5.1.0]** The owner MUST NOT edit the command. A change request refuses the first request. The agent MAY then make a new call with another command. This makes a new effect hash and a new request that links to the old one with `previous_id`.

3.7.12 **[5.1.0]** Only the owner of the requester key MAY answer. The owner answers from a phone that is not revoked, or from a web session when `web_approval` is true. The Inbox MUST refuse an answer from an `mcp_` key, an `api_` key, a session that the superadmin opens as another account, and a service.

3.7.13 **[5.1.0]** The Inbox MUST send a request to all phones of the owner that are not revoked. The first valid answer wins. The other phones MUST get `settled`. An owner with no enrolled phone MUST get `409 no_device` on the submission to the approval API. A direct alert to an account with no phone keeps `409 no_phone`.

3.7.14 **[5.1.0]** A resume call with `usage` and the same `effect` MUST consume an approval that is `answered` with `accept`. The consumption is one compare-and-set of `consumed_at`, and it works only up to `consume_before`.

3.7.14a **[5.5.0]** The PDP looks at the approval, counts the unit, and then consumes the approval (protocol 17.2). The answer is `true` only after the consumption. When the budget refuses or the count fails, the PDP MUST leave the approval as it was. When the consumption fails or loses a race, the PDP MUST give the units back and MUST NOT answer `true`.

3.7.15 **[5.1.0]** One approval MUST give one execution at most. The value `consume_before` is the time of the decision plus the larger of `consume_within` and `lease_s`. After it, the approval is `lapsed`, and the service gets `approval_expired`.

3.7.15a **[5.5.0]** A crash or a failure between the count and the consumption MUST leave the approval usable. It MAY leave one unit counted. The PDP MUST NOT lose an approval because of a failure of its own. It MUST NOT give two executions.

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

3.7.26 **[5.3.0]** The page of the Inbox MUST show a panel "Open items" above the phones only when an item waits. It MUST have a zone "History" with filters. The matrix of the editor MUST have the columns Read and Cancel. It MUST have no column that decides. A lock icon MUST mark each action of a service other than `portal`, except `access`.

3.7.27 **[5.1.0]** The pages of the items MUST work without script. They MUST carry `Cache-Control: no-store`, and MUST check the CSRF token and the origin on an answer and on a reply. An owner MUST see only the items of the owner. With `web_approval` false, the page MUST list the items and MUST NOT let the owner decide.

3.7.28 **[5.1.0]** The page of the Inbox MUST show the alerts first, then the panel "Open items", then the phones and the connections. A link to the History MUST be in the bar of the page. The tab MUST show the count of the requests that wait.

3.7.29 **[5.1.0]** The page "Inbox limits" MUST show the value and the source of each key of the blocks `inbox.items`, `inbox.approvals` and `inbox.at_rest`. The source is `file`, `default`, `env:NAME` or `file:PATH`. The page MUST be read only. It MUST show a secret key by its id and its source, never by its value. It needs `approval.read`.

3.7.30 **[5.1.0]** The copy button of the command MUST copy the exact bytes of the command. A code block of the explanation MUST use colors that are different from the colors of the command. The buttons that need script MUST be hidden when the script does not run.

3.7.31 **[5.1.0]** The panel "Open items" MUST show one group for each agent that has an open request. A group MUST name the agent with text of the server: the name that the owner gave, else "<key name> #<number>". The `new` agents MUST come first. The other groups MUST follow by the time of their oldest request. The bucket of a key MUST come last. A key that has no agent MUST have one group with the name of the key and no state mark.

3.7.32 **[5.1.0]** The page MUST show one notice for each `new` agent, below the alerts and above the panel. The notice MUST have forms to name the agent, to make it `known`, to block it and to merge it. It MUST show what the agent said in a box with the label "Not verified". With `inbox.agents.lock_new` false, the page MUST NOT show a locked row. With `lock_new` true, the row and the page of the request MUST say why Approve is off. The server MUST refuse `accept` with the status `409`, and a decline MUST stay open.

3.7.33 **[5.1.0]** The page of one agent (`/agents/{id}`) and the list of the agents (`/agents`) MUST be for the owner only. An agent of another owner MUST get `404`. A form `rename`, `allow`, `block`, `unblock` or `merge` MUST be a POST with the token `csrf` and the `Origin`. It MUST change nothing for an agent of another owner and for a session that the superadmin opens as the owner. A name that is not valid or that is taken MUST get `422`, and an action in a wrong state MUST get `409`. The pages of the agents MUST work without script and MUST carry `Cache-Control: no-store`.

3.7.34 **[5.1.0]** The page of one request MUST show the box "From agent" before the command. The name, the key, the state and the owner MUST be text of the server. What the agent says, its client and its address range MUST be in a box with a dashed edge and the label "Not verified". They MUST NOT be a title. The box MUST warn when the text of the agent is the name of a known agent of the key. A viewer that is not the owner MUST see the name of the agent and nothing that the agent said.

3.7.35 **[5.1.0]** The History MUST have the filter `agent=<id>` and a column that names the agent, with a link to the filter. A merged agent MUST give its target. The filter MUST NOT show the name of an agent of another owner.

3.7.36 **[5.1.0]** The phone MUST keep one row for each portal and each agent. The key of the row MUST have the id of the portal. The phone MUST ignore a frame `agent` with a lower `rev`. It MUST send the receipt after the durable write, also for a frame that it ignores.

3.7.37 **[5.1.0]** The phone MUST follow `merged_into` until it finds an agent that is not merged. It MUST count the open items of an agent itself, because `open` and `last_seen_ms` are snapshots. It MUST take the name of an item from its table of agents, so a rename shows on old items at once. The `from.label` of the frame is only the fallback.

3.7.38 **[5.1.0]** The frame `sync` of the phone MUST list the agents that it holds with their `rev`. The list MUST have 200 agents at most and MUST NOT repeat an agent. When the phone holds more, the live agents MUST go first. The member MUST be absent when the phone holds no agent.

3.7.39 **[5.1.0]** The section "To answer" of the phone MUST have one group for each agent of each portal. The groups of two portals MUST stay apart. The new agents MUST come first, then the groups by their oldest open item, and the bucket last. In a group, the oldest item MUST come first. The section "Alerts" MUST stay above. A fold MUST stay on the phone, and a folded group MUST keep its count.

3.7.40 **[5.1.0]** The phone MUST show one card "New agent" for each agent in the state `new`, above "To answer" and below the alerts. The card MUST name the key and the first call. It MUST have the box "Observed, not verified" and the sentence that anyone with the key can send these texts. It MUST have a name field, **Name and allow**, **Allow as** with the name of the portal, **Block** and a merge. The lock line MUST show only when `lock_accept` is true.

3.7.41 **[5.1.0]** The merge of the card MUST offer only the live agents of the same key and the same portal. The hint `similar` MAY come first. A card without the hint MUST work. The card MUST warn when what the agent says equals the name of a known agent of the key. The fold of 3.8.15 decides the equality. The card MUST also warn when the text looks like the name with look-alike letters.

3.7.42 **[5.1.0]** A tap on the header of a group, or on **More**, MUST open the screen of the agent. The screen MUST show the name, the key, the state, the last call and the counts. It MUST show the box "Observed, not verified", the open requests, and the actions that are possible in the state of the agent. The list **Agents** of a portal MUST show also the blocked agents, so the owner can unblock them.

3.7.43 **[5.1.0]** The phone MUST send each action on an agent as the frame `agent_do` through its outbox. It MUST send the frame again after each `welcome` until the `agent_result` comes. It MUST show one line for each status of the closed list. It MUST NOT ask for a biometric check. It MUST ask for a confirmation before a block.

3.7.44 **[5.1.0]** The approval screen MUST have the box "From agent" under the title. The box MUST show the name, the state and the key as text of the portal. It MUST show what the agent says in a dashed line marked "Not verified". The state MUST come from the table of agents, so an agent that becomes `known` changes the box with no new request.

3.7.45 **[5.1.0]** For a new agent with `lock_accept` true, the approval screen MUST lock **Approve** and MUST say why in the bottom bar. **Decline** MUST stay on. The answer `agent_locked` MUST keep the item open, with a text that tells the owner to make the agent `known` first.

3.7.46 **[5.1.0]** A new agent that the phone did not hold MUST add a count and a line to the one notification. The line has the words "new agent on" and the key name. It MUST NOT make a second notification. It MUST alert once for the agent, and MUST NOT alert again for a new `rev`. The lock screen MUST show the count only. The Decline action of a single approval MUST be off while a new agent waits.

3.7.47 **[5.1.0]** The phone MUST put each text that an agent or its client sent through the path of untrusted text. These texts are what it says, its client, its address range and `agent_label`. A hidden, bidirectional or look-alike character MUST show as a mark. Such a text MUST NOT be a title or a name. The resources of the agent screens MUST have ASCII characters only.

3.7.48 **[5.1.0]** The list of the agents (`/agents`) MUST have the form "Add agent" for the owner of a key in the mode `enrolled`. The form MUST have the fields `key` and `name`. A list with no such key MUST NOT have the form. The page of an agent that the owner made, on a key in this mode, MUST have the button "Reset token". The page MUST say that the old token stops at once.

3.7.49 **[5.1.0]** The forms `enroll` and `reset` MUST need the token `csrf`, the right `Origin` and the owner. A session that the superadmin opens as another account MUST do nothing. A key that does not exist, a key of another owner and a key of another mode MUST get the same answer (422). An agent of another owner MUST get the same answer as an unknown agent. Each success MUST answer 200 with the token once, in a box with a copy button. It MUST show the header line `Agent-Token: agt_...` and name the argument `agent_token`.

The answer MUST have `Cache-Control: no-store` and MUST NOT put the token in a URL. No other page, event, log line, metric or frame MUST hold the token. The page text MUST NOT hold the name of the product.

3.7.50 **[5.1.0]** The phone MUST NOT enroll an agent. The frame `agent_do` MUST have no operation for it. The web is the only place where an owner makes an agent or resets a token.

3.7.51 **[5.1.0]** The page of a request MUST show the line "Also asked by" to the owner. Other agents asked for the same effect (3.8.18). The line MUST list their names as text of the portal, with a link to each agent page. It MUST NOT show any text of an agent. It MUST be absent when no other agent asked.

3.7.60 **[5.2.0]** The phone MUST open a question on its own screen, for the owner only. The screen MUST open from the list "To answer", from the list "Done" and from the screen of an agent. It MUST have the box "From agent" under the title. It MUST show the command above the explanation. The Copy button of the command MUST copy the exact text. The explanation MUST show in a dashed zone with the label "Not checked".

3.7.61 **[5.2.0]** The question screen MUST take the answer in the way that `input` says. A `choice` is one option of a list. With `multiple`, it is a set of boxes that follows `min` and `max`. A `confirm` is the two options Yes and No, and a `text` is a field. A `form` is one to four such questions, and it needs one answer for each. A `none` message MUST have no field and no Send button, and it MUST offer **Decline to answer** only.

3.7.62 **[5.2.0]** One pure function MUST decide the rows under the options from `allow_custom`, `allow_ask_back`, the type of `input` and the interaction. A question with no flag MUST have no row. `allow_custom` MUST add the row "Other answer" with a field. `allow_ask_back` MUST add the row "Ask the agent a question", and the row MUST open the discussion. A `text` question MUST have no row "Other answer". A combination that the protocol forbids MUST show no row.

3.7.63 **[5.2.0]** The free answer MUST end the question as the answer. With `multiple`, it MUST exclude the boxes. A tick MUST clear the free answer, and the free answer MUST clear the ticks. The numbers `min` and `max` MUST count the ticks only. Each free field and each `text` field MUST show the warning "Do not enter a password or a secret". The field MUST stop at 2000 characters, and the phone MUST NOT change the text.

3.7.64 **[5.2.0]** A request of a `one_shot` key MUST show the header "Automatic system (no conversation)". It MUST show the suggestion and its confidence above the options, as text that is not checked. It MUST say "last computed" in the place of "agent active". It MUST have no row "Other answer", no row "Ask the agent", no field and no link to a discussion. The suggestion MUST NOT tick an option.

3.7.65 **[5.2.0]** Send MUST stay off until each question has an answer that the server accepts. The phone MUST use the rules of the server for this check. A text of only spaces MUST NOT be an answer. Send MUST ask for the biometric check or the device credential, and MUST be off when the phone has none. **Decline to answer** and the row "Ask the agent" MUST NOT ask for it.

3.7.66 **[5.2.0]** The frame `answer` of a question MUST have `choice` for the ticks, in the order of the options. It MUST have `text` for a free answer or a `text` question, and `answers` for a form. A decline MUST have no text and no choice. The frame MUST go through the outbox with a `client_msg_id`. The phone MUST send it again after each `welcome`. It MUST do so until the `answer_result` or the `settled` frame closes the entry.

3.7.67 **[5.2.0]** For a new agent with `lock_accept` true, the question screen MUST disable Send and say why in the bottom bar. The server refuses `accept` for each item of such an agent (3.8.16). **Decline to answer** and the row "Ask the agent" MUST stay on. The answer `agent_locked` MUST keep the item open.

3.7.68 **[5.2.0]** The phone MUST show how a question ended: answered, declined, abandoned, replaced, expired or cancelled. An abandoned question MUST say that it is not a decline. An answer that came late MUST say "Too late" and that the answer was not used. The phone MUST show what the owner chose, or that the owner declined, only for an answer that it sent. For another answer it MUST say who answered. The frame `settled` has no such value.

3.7.69 **[5.2.0]** A question MUST NOT post its own notification. It MUST count in the one notification. The resources of the question screens MUST have ASCII characters only. A text of the agent MUST show its hidden characters as marks. The screen MUST have a label for a screen reader on the header, each field, each row and Send.

3.7.70 **[5.2.0]** The web MUST show a question on its own page, `/auth/settings/inbox/items/{id}`, for the owner only. The panel "Open items" MUST list the questions with the approvals, in one group for each agent. The panel MUST be absent when nothing waits, and the alerts MUST stay above it. The page MUST have the box "From agent" under the title. It MUST show the command above the explanation. The explanation MUST show in a dashed zone with the label "Not checked".

3.7.71 **[5.2.0]** The page MUST work without script. The page MUST take the answer in the way that `input` says. A `choice` is a list of radio buttons, or a list of boxes with `multiple` that follows `min` and `max`. A `confirm` is Yes and No, and a `text` is a field. A `form` is one to four such blocks, and it needs one answer for each. A `none` message MUST have no field and no Send button, and it MUST offer **Decline to answer** only.

3.7.72 **[5.2.0]** The rows under the options MUST follow the table of 3.7.62. One function decides them from the flags, the type of `input` and the interaction, and a question with no flag MUST have no row. `allow_custom` MUST add the row "Other answer" with a field. `allow_ask_back` MUST add the row "Ask the agent" with its own field and button. A combination that the protocol forbids MUST show no row. The page MUST show no option as ticked, also when the question has a `hint`.

3.7.73 **[5.2.0]** The free answer MUST end the question as the answer, and it MUST exclude the ticks. A text with a tick MUST be a mistake that the page names. Each free field and each `text` field MUST show the warning "Do not enter a password or a secret". A free text MUST have 1 to 2000 characters, and a text of only spaces MUST NOT be an answer. A mistake MUST give the page again with the status `422`, and the page MUST keep the text and the ticks that the owner typed. The page MUST NOT change a text.

3.7.74 **[5.2.0]** A question of a `one_shot` key MUST show the header "Automatic system (no conversation)" and the words "Last computed". It MUST show the `hint` and its confidence above the options, as text that is not checked. It MUST have no field, no row "Other answer", no row "Ask the agent" and no control of a discussion. The server MUST refuse a `reply` that a forged form sends.

3.7.75 **[5.2.0]** The form that answers or declines, `POST /auth/settings/inbox/items/question`, MUST need the session of the owner, the token `csrf` and the right `Origin`. A session that the superadmin opens as another account MUST answer nothing (`403`). An account that does not own the question MUST get the answer of an unknown question. The form MUST need no biometric. A field that the form never makes MUST give `400`. A decline MUST carry no content.

3.7.76 **[5.2.0]** The web answer MUST use the same engine as the phone. `Items::answer` MUST run the check of the answer (3.11.11) and the compare-and-set on the state. An answer for a closed question MUST give `409` and the page of the question with a notice, and it MUST change nothing. An answer that does not fit the question MUST give `422`. For a new agent with `lock_new` true, the server MUST refuse the answer with `409`. The page MUST show the lock line, disable Send, and keep **Decline to answer** and the row "Ask the agent" on.

3.7.77 **[5.2.0]** The page MUST show how a question ended: answered, declined, abandoned, replaced, expired or cancelled. An abandoned question MUST say that it is not a decline. A replaced question MUST link to the new one. An answered question MUST show to the owner what was chosen. It MUST also show the choices that were not ticked, the free text and the messages of the discussion. The History MUST list the questions with the chip "Answered".

3.7.78 **[5.2.0]** A page of a question MUST have `Cache-Control: no-store`. A text of an agent MUST show each hidden character as a mark, and the page MUST NOT write it as markup. The page MUST NOT show an agent token or the name of the product. An account with `question.read` MUST read the metadata only. An account with `question.cancel` MUST see the button to cancel an open question. The route of the administrator MUST read a body only for the superadmin and only with `inbox.at_rest.admin_body_read`.

### 3.8 Agent channel

The agent talks to the portal for everything except the command. The portal has one engine behind two doors, `/mcp` and REST. The text of the agent is untrusted. Release: 5.1.0.

3.8.1 **[5.1.0]** The portal MUST serve the endpoint `inbox.mcp.path` (default `/mcp`) with JSON only, no session and no SSE. It MUST need `keys.mcp = true`. The tools are `approval_intent`, `get_answer`, `keepalive`, `say`, `supersede` and `cancel`. The tool `ask` arrives in 5.2.0.

3.8.2 **[5.1.0]** The REST routes are `POST /api/v1/approval-intents`, `GET` and `DELETE /api/v1/items/{id}`, and `POST` to `/api/v1/items/{id}/keepalive`, `/turns` and `/supersede`. A `GET` with `?wait=` renews the lease. The routes for questions arrive in 5.2.0. The routes have no alias.

3.8.3 **[5.1.0]** The MCP endpoint MUST check the origin, the client address, the key, the rate and the size of the body, in this order. A call with no key MUST get `401` with an empty body. `tools/list` MUST depend on the key `interaction`. A `one_shot` key MUST NOT see `say`.

3.8.4 **[5.1.0]** A call MUST NOT wait more than `inbox.mcp.max_wait`. The value of `max_wait` MUST NOT pass `inbox.items.wait_max`. The value `inbox.items.wait_max` has the default 25 seconds and the maximum 60 seconds. A read with no answer SHOULD return fewer than 100 tokens.

3.8.5 **[5.1.0]** A key reads an item and speaks on it only when `items.requester_key_id` is its own id. No grant is needed. A call for another item MUST get `not_found`. An `mcp_` key and an `api_` key MUST NOT use each other's door (3.3.8a).

3.8.6 **[5.1.0]** A text that a third party wrote MUST sit in an `untrusted` object, with a `data_notice`. This holds for the comment, the change request and the text of a question. The result of `get_answer` MUST NOT hold the identity or the device of the approver.

3.8.7 **[5.1.0]** The screen MUST show the line "from" (key name, owner, interaction) that the portal builds. A label that the agent gives is a subtitle. The screen MUST show the fixed banner "written by an agent". The explanation MUST carry the label "Text of the agent, not verified".

3.8.8 **[5.1.0]** An agent with no channel to the portal MUST get the verdict only. It has no explanation, no flag and no thread. It sets the lease with `lease_s` or `until` at the service. The screen then says "No explanation given by the agent".

3.8.9 **[5.1.0]** The block `inbox.mcp` MUST give the path, the maximum wait, `allowed_origins` and `allowed_client_cidrs`. It MUST also give the rate limits and the size of the body. A contradiction in the block MUST stop the start (6.9.6).

An agent is one program, or one conversation of a model, that asks. Several agents can share one key. The portal gives each agent a token, and the owner gives it a name. The requirements 3.8.10 to 3.8.22 are the rules for the agents of a key. They hold for `approval_intent` now and for `ask` in 5.2.0.

3.8.10 **[5.1.0]** The portal MUST give a token that has the prefix `agt_` and 43 more characters, and the database MUST keep only its SHA-256 digest. The token MUST show once, in the result of the call that made the agent. A call MAY send the token in the header `Agent-Token` or in the argument `agent_token`. Two values that differ, in the header and the argument or in two headers, MUST get `422 agent_token_conflict`. An empty value MUST be the same as no token. A token in a query string MUST get `400 invalid_query`.

3.8.11 **[5.1.0]** A call that makes an item MUST be the only call that makes an agent. A read, a `say`, a `cancel` and a `supersede` MUST NOT make an agent or a row. A token that is unknown, of another key or of another owner MUST get the same error `422 agent_unknown`. A text that is not in the form of a token MUST get this error with no read of the database.

3.8.12 **[5.1.0]** A read (`get_answer`, `keepalive`) MUST need the key only. A token on a read MUST be the agent of the item, else `403 agent_mismatch`. A write (`say`, `cancel`, `supersede`) on the item of a real agent MUST need the token (`422 agent_token_required`). The token MUST be the agent of the item. A write on an item of the bucket MUST need the key only.

3.8.13 **[5.1.0]** The limits of a key (`max_per_key`, `new_per_hour`, the rate, the mute and the deny time) MUST be per key, never per agent. A call over `max_per_key` or `new_per_hour` MUST NOT fail. It MUST go to the bucket, with `agent_status` `unattributed` and no token. The portal MUST send one alert each hour and each key to the owner. The portal MUST count the limits in the transaction that inserts the agent.

3.8.14 **[5.1.0]** Each key MUST have one bucket, the pseudo agent "Unattributed". The bucket MUST have no token. The owner MUST NOT name it, merge it, block it or lock it. A submission of a service with no `approval_ref` MUST go to the bucket of its key. The mode `key` MUST put all items in the bucket.

3.8.15 **[5.1.0]** Only an action of the owner MUST set the name of an agent. A name MUST have 1 to 32 characters, no control character, no format character, no line separator and no white space at its ends. The portal MUST refuse a bad name and MUST NOT clean it. Two names of one key MUST differ after NFKC and lower case. Until the owner names an agent, the portal MUST show "<key name> #<ordinal>". The ordinal MUST be per key and MUST NOT be given twice.

3.8.16 **[5.1.0]** With `inbox.agents.lock_new` true, the server MUST refuse `accept` for an item of a `new` agent until the agent is `known`. The refusal MUST write nothing. A decline, a reply and a name MUST stay open. With `lock_new` false, the server MUST NOT lock the items of a new agent, and the owner MUST still get a notice. An item of the bucket MUST never be locked.

3.8.17 **[5.1.0]** `previous_id` of `approval_intent` MUST name an item of the agent that the call gets, else `403 agent_mismatch`. A call that makes a new agent MUST NOT have a `previous_id`. `supersedes` of a submission MUST name an item of the same agent, else `422 invalid_argument`. The database transaction of the write MUST check the agent, so a race cannot cross two agents.

3.8.18 **[5.1.0]** The service MUST NOT see the token, the id, the label or the hints of an agent. `approval_intent` MUST keep the agent in the row of the intent, and a submission that names the `approval_ref` MUST give its item to that agent. A blocked agent MUST have no live reference, so a submission that names one MUST get `422 invalid_argument`. When another agent asks for an effect that has an open request, the portal MUST give that request. The item MUST keep its first agent. The portal MUST write the event `also_requested` once for each other agent.

3.8.19 **[5.1.0]** A key MUST have one mode, and the three modes are `key`, `discover` and `enrolled`. The mode `key` MUST ignore the token. The mode `enrolled` MUST refuse a call that makes an item and has no token (`422 agent_token_required`). Without the block `inbox.agents`, each key MUST act in the mode `key`. The form of a key MUST have the field `agents` only with this block. A bad value MUST get `bad_agents`, and the default MUST be `discover` for the kind `mcp` and `key` for the kind `api`.

3.8.20 **[5.1.0]** The documents MUST say that a group of agents is not a sandbox. The agents of one key share the key. An approval stays bound to its effect and not to an agent. Another agent of the key can run the same effect with the same arguments.

3.8.21 **[5.1.0]** An action of the owner on an agent (`rename`, `allow`, `block`, `unblock` and `merge`) MUST need the owner of the agent. An action on an agent of another owner MUST get the same answer as an unknown agent. Each action MUST be idempotent, MUST write one row in `agent_events` and MUST raise `rev`. A block MUST cancel the open items of the agent with the reason `agent_blocked` and MUST make its live intents dead. A merge MUST stay inside one key. It MUST move the items and the intents, and the token of the merged agent MUST give the target.

3.8.22 **[5.1.0]** The portal MUST seal the hints of an agent (what it says, its client and its address range) with the content keys. An event of an agent MUST hold ids and numbers and MUST NOT hold a text that an agent sent. The sweeper MUST erase an agent that has no item, no live intent, no merged alias and no call for `inbox.agents.purge_after`. The command `content-purge` MUST erase the hints of a lost key. A change of the client, of the `User-Agent` or of the address range MUST NOT raise `rev`. It MUST NOT make an alert or a frame, unless it has an event of the agent.

3.8.23 **[5.1.0]** The agent channel MUST give to the engine the address range, the `User-Agent` and the client name of the call. The address MUST be the client address after `server.trusted_proxies`, cut to 24 bits for IPv4 and to 48 bits for IPv6. The client name MUST be `clientInfo` when the call has it, and else the `User-Agent`. The channel MUST cut each hint to its limit and MUST replace each control, format and line separator character with U+FFFD. `approval_intent` MUST keep `agent_label` in the sealed body of the item, with the explanation. The log line of an MCP call MUST have the field `agent` with the id, and no log, metric or trace MUST hold a token.

3.8.24 **[5.1.0]** The portal MUST refuse a text of an agent that holds a live token of the same owner, with or without the prefix `agt_`. The texts are `explanation`, `agent_label` and the message of `say`. The error MUST be `422 agent_token_in_text`, MUST NOT be retryable and MUST name no value. The refusal MUST come before any write, so it makes no agent, no intent and no item. A blocked or merged agent MUST count too. The check MUST read the database only when a text has a run of 43 characters of the token alphabet.

3.8.25 **[5.1.0]** The frame `agent` and the first-use notice of a `new` agent MUST name a similar known agent. The known agent MUST have the same key and the same owner. It MUST say the same text, or have the same client and the same address range. The fold of 3.8.15 MUST decide the equality of the texts, and a match on the text MUST win. The notice MUST say that hints can be copied. The portal MUST NOT merge two agents by itself, and it MUST NOT pick a blocked or merged agent.

3.8.26 **[5.1.0]** In the mode `enrolled`, only the owner of the key MUST make an agent, with the name that the owner gives. The agent MUST be `known` and its origin MUST be `owner`. It MUST NOT pass through the state `new`, and the portal MUST NOT write a first-use notice. The phones MUST get the frame `agent`, as for each change. The name MUST follow 3.8.15. The key MUST NOT have more than `max_per_key` agents.

The portal MUST refuse the action for a key of another mode.

3.8.27 **[5.1.0]** The portal MUST give the token of an enrolled agent once, and MUST keep only its SHA-256 digest. A reset of the token MUST replace the digest, MUST raise `rev` and MUST write the event `token_reset`. The old token MUST give `agent_unknown` at once. The event MUST hold no secret. A reset MUST be for an agent that the owner made, on a key in the mode `enrolled`. The portal MUST refuse a reset for the bucket, for a merged agent and for an agent that a call made.

### 3.9 Preferences

Each account has two preferences: the digest and the quiet hours. They are in the database. Release: 5.1.0.

3.9.1 **[5.1.0]** The Inbox MUST keep the preferences of an account in the table `account_prefs`. They override the defaults of `inbox.items.digest`. They MUST NOT exceed a limit of the file. Only the account itself MAY change them, never a session that the superadmin opens as the account.

3.9.2 **[5.1.0]** The digest mode is `off`, `daily` or `every`. `daily` needs a time `HH:MM` and a time zone. `every` needs a period from 1 hour to 7 days. A name that is not an IANA time zone MUST get `422` on the page and the error `invalid_prefs` on the phone. The server MUST store nothing and MUST NOT change the value, also not the capital letters. The start MUST check `inbox.items.digest.timezone` in the same time zone database.

3.9.3 **[5.1.0]** The Inbox MUST send one frame `digest` for each owner at each due time, even with several instances. The frame holds the count, the age of the oldest item and the ids. It MUST NOT hold a body. An empty digest MUST NOT be sent.

3.9.4 **[5.1.0]** A phone MAY set quiet hours with `set_prefs`. The server MUST sync them in the frame `welcome`. A request that arrives inside the quiet hours MUST be delivered with the flag `quiet`. The phone does not play a sound in this period, except for the urgency `high` when the preference `high_rings` is true.

3.9.5 **[5.1.0]** The lock screen option is local to the phone. The default is off. With it off, the notification MUST use the private visibility and a generic text.

3.9.6 **[5.1.0]** Delegation, escalation and muted agents are not part of this version. They have no requirement.

3.9.7 **[5.1.0]** The server MUST read the time zones from a database that is inside the binary. It MUST NOT read the zone files of the host. The crate of the database MUST be for the server only: the phone and `inbox-core` MUST NOT use it.

3.9.8 **[5.1.0]** A `daily` digest MUST be due once for each local day at `HH:MM` of its time zone, also across a change of summer time. An `every` digest MUST be due at the multiples of its period since the Unix epoch. The server MUST NOT send a due time that is more than 15 minutes old. It MUST NOT send a digest for an owner that had no open item at the due time. All times MUST come from an injected clock.

### 3.10 Product choices on items

Each rule below is a product choice. It has a requirement that a test can check.

3.10.1 **[5.1.0]** **No signature of the phone.** An `answer` frame MUST NOT have a signature field. The proof of an approval is an authenticated session and one accepted answer that is bound to the effect hash. A parser MUST refuse an unknown field.

3.10.2 **[5.1.0]** **No risk class.** No key, grant, action document or screen MUST depend on a risk level. The closed schema of the configuration MUST refuse a key `risk`.

3.10.3 **[5.1.0]** **The owner only.** The owner of the requester key is the only recipient and the only approver. The Inbox MUST NOT have a named account, a group, a delegation, an escalation or a two-person rule for items.

3.10.4 **[5.1.0]** **The service learns only the verdict.** The service MUST receive nothing that 3.7.20 lists. A test with a service that records its input MUST fail when one of these strings reaches it.

3.10.5 **[5.1.0]** **No end-to-end encryption.** The portal reads each body. It seals the body at rest (5.11). The specification MUST say this plainly, in 5.11.1.

3.10.6 **[5.1.0]** **Plain text, and safe Markdown for the explanation.** The command, the comments of the owner and the turns of the owner are plain text. The explanation of an agent is plain text unless the agent sets `explanation_format` to `markdown` (3.6.26). No screen MUST render HTML, an image, an active link or a script from any text.

### 3.11 Questions

3.11.1 **[5.2.0]** With the block `inbox.questions`, an agent MUST be able to ask a question with the tool `ask` and with `POST /api/v1/questions`. The key MUST hold `question.ask`. The owner of the key MUST be the only recipient, and a question MUST NOT have a field `to`. Without the block, the tool and the route MUST NOT exist.

3.11.2 **[5.2.0]** The flags `allow_custom` and `allow_ask_back` MUST be `false` by default. The portal MUST refuse with `422 invalid_flags` a `text` input with `allow_custom` and a `none` input with a flag. It MUST refuse a `form` with `allow_custom` and no `choice` question. The other types MUST take every pair of flags. The portal MUST NOT correct a flag. With `multiple` and `allow_custom`, a free text MUST exclude the ticks, and `min` and `max` MUST apply to the ticks only.

3.11.3 **[5.2.0]** A key that is `one_shot` MUST ask a `choice` or a `confirm` with no flag. A flag, or another type of input, MUST give `422 interaction_not_allowed`. The key MUST NOT see `say`, and a `reply` of the owner MUST be refused for its items. The key MUST see `ask` and `supersede`.

3.11.4 **[5.2.0]** The portal MUST refuse a value that is out of its limit and MUST NOT cut it. The title has 128 characters on one line. The explanation has 2000 characters, and the command has 16 KiB. A choice has 2 to `inbox.questions.max_options` options. The portal MUST refuse a control character other than the line end and the tab, and a bidirectional control, in `command` with `422 invalid_command_chars`. It MUST accept a zero-width character and keep it in the frame.

3.11.5 **[5.2.0]** The answer MUST be the member `answer` with `by`, `action`, `choice`, `asked_at`, `answered_at` and `age_at_answer_s`. A free text, and the answers of a form, MUST be in `answer.untrusted`. A text answer MUST have `choice` equal to `[]`. The result MUST start with `data_notice`.

3.11.6 **[5.2.0]** The portal MUST find a repeat of `ask` by the `idempotency_key` of the key. A repeat MUST give the same item and MUST renew the lease. The same key with another question MUST give `409 idempotency_conflict`. The portal MUST compare the unsealed bodies.

3.11.7 **[5.2.0]** A key with `allow_ask_back` MUST be able to talk with `say`. A turn MUST have 4000 characters at most. A stale `expect_seq` MUST give `409 stale_thread`. Two turns of an agent MUST be 2 seconds apart, and a thread MUST have `max_turns` turns at most.

3.11.8 **[5.2.0]** The portal MUST add the actions `question.ask`, `question.read` and `question.cancel`, and the budget `questions`. A key MUST hold `question.ask` to ask. A key MUST hold `question.cancel` to withdraw a question. An `ask`, a `say` on a question and a `supersede` MUST take one unit of the budget. A repeat of a call, a renewal of a lease and a read MUST take none.

3.11.9 **[5.2.0]** The portal MUST refuse a question with `422 agent_token_in_text` when a text holds a live token of an agent of the owner. The texts are the title, the explanation, the command, the labels and descriptions of the options, the placeholder, the text of a form and `agent_label`. The refusal MUST come before any write.

3.11.10 **[5.2.0]** A question MUST belong to an agent, as 3.8.10 to 3.8.17 say. A write on the question of a real agent MUST need its token. A read MUST go by the key.

3.11.11 **[5.2.0]** The server MUST check the answer of the owner against the question, for a phone and for the web page. It MUST refuse a text when `allow_custom` is false. It MUST refuse an option that does not exist, a wrong number of ticks and an answer that does not fit a form. A refusal MUST write nothing. For a phone, the refusal MUST be an `answer_result` with the status `custom_not_allowed`, or with the status `closed` and no state.

3.11.12 **[5.2.0]** A question MUST NOT pass through a service. No approval, no intent and no evaluation MUST exist for it. No log line, metric or error of a question MUST hold a text of the question, and no table MUST hold it in clear text.

3.11.13 **[5.2.0]** The list of the tools MUST show `ask` only when the block is on and the key holds `question.ask`. A key that does not see a tool MUST get the error of a tool that does not exist.

3.11.14 **[5.2.0]** The block `inbox.questions` MUST need `inbox.items` and `inbox.at_rest`. It MUST have the key `max_options`, from 2 to 8, with the default 8. The configuration MUST be closed and MUST NOT change a value.

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

5.10 The portal has no TLS. TLS stops at a reverse proxy. The portal MUST NOT trust a forwarded header from a peer that is not in `server.trusted_proxies`.

The details are in [`SECURITY.md`](SECURITY.md) and [`protocol.md`](protocol.md).

### 5.11 At rest

The portal seals the body of an item before it writes it. This is not end-to-end encryption. Release: 5.1.0.

5.11.1 **[5.1.0]** The portal reads each body. It receives the text in clear on an authenticated channel, seals it, writes only the sealed value, and unseals it to serve the owner. This specification and the documentation of the Inbox MUST say that this is not end-to-end encryption.

5.11.2 **[5.1.0]** The portal MUST seal the command, the view, the explanation, the hint, the turns and the comment of the owner. For a question it MUST also seal the title, the options, the flags and the answer. The seal uses AES-256-GCM with a random 12-byte nonce.

5.11.3 **[5.1.0]** A sealed value is `0x01`, the length of the key id, the key id, the nonce, the cipher text and the tag. The sub-key is the HMAC-SHA-256 of the key with the label `portal-content/seal/v1`. The additional data is length-prefixed and holds `portal-content/v1`, the request id, the field, the sequence number and the effect hash if it exists.

5.11.4 **[5.1.0]** A sealed value that moves to another row, field or effect MUST NOT open. A reader that unseals a command MUST compute the effect hash again. On a difference, it MUST serve nothing and MUST write the log line `body_effect_mismatch`.

5.11.5 **[5.1.0]** These data stay in clear in their columns: the request id, the service, the action, the resource path, the subject and the owner. The effect, the code, the state, the dates, the lease and the decision stay in clear too. So do the device or session of the answer, the size of the sealed body and the key id. The portal MUST NOT write a title in clear for an item.

5.11.6 **[5.1.0]** The block `inbox.at_rest` MUST have `active_key` and 1 to 8 blocks `key "<id>"`. An id has 1 to 16 characters from `a-z`, `0-9`, `-` and `_`. A key holds exactly one of `secret_env` and `secret_file`, with 32 random bytes in base64. A secret in clear in the file MUST stop the start.

5.11.7 **[5.1.0]** At start, the portal MUST stop in each of these cases. The error names the id and the count of rows. For the last case, the error MUST name two commands. `portal inbox rekey` is for an operator who still has the secret. `portal inbox content-purge` is for a lost key.

- `active_key` names no block.
- Two blocks have the same id or the same secret.
- A secret has a wrong form.
- The check value in the table `content_key_check` differs for the same id.
- A sealed row uses a key id that the file does not hold.

5.11.8 **[5.1.0]** The command `portal inbox content-key` MUST make a new key and MUST NOT overwrite a file. The command `portal inbox content-purge --key <id>` MUST erase the bodies of a lost key and keep the metadata. The operator MUST keep the keys apart from the database and from the backups.

5.11.9 **[5.2.0]** The command `portal inbox rekey` MUST seal again the rows of an old key with the active key. It MUST be idempotent, work in batches and resume after a stop. With SQLite it MUST refuse to run while an instance runs. With PostgreSQL it MUST run while the portal serves. It MUST visit each sealed value: the bodies of the items, the turns, the intents of the agents and the hints of the agents.

5.11.10 **[5.1.0]** After `body_purge_after`, the portal MUST erase the sealed value of a final item. The metadata, the effect hash and the audit MUST stay. The phone keeps its copy until its own `local_retention`. The `welcome` frame carries `body_purge_after_ms` as the default.

5.11.11 **[5.1.0]** The portal MUST NOT write a body in the log, a trace, a metric, the spool, the replica or a backup in clear. A debug print MUST hide the body. A page with a body MUST carry `Cache-Control: no-store`. A canary test MUST check each of these places.

5.11.12 **[5.1.0]** The Inbox chapter and the [security model of the Inbox](SECURITY.md) MUST say what the protection covers. It covers a leak of the database, a backup, a copy of the replica, a disk image, and a database administrator without the key. It covers an attacker who can write in the database.

5.11.13 **[5.1.0]** The same documents MUST say what the protection does not cover. It does not cover a person who has the key and the process, or a portal that is compromised while it runs. It does not cover a compromised host of the service. It does not cover a stolen phone or browser, or a key that leaves with the database. It does not cover the plain HTTP hop from the proxy to the portal. The operator MUST limit the PDP listener, which has no TLS, with a network policy or a private network.

5.11.14 **[5.1.0]** A portal that is dishonest MAY show one text and consume another effect. The Inbox does not prevent this. It makes it detectable. The phone MUST keep the command and its hash that it showed. The service logs the effect that it ran. The web page gives no independent proof.

5.11.15 **[5.1.0]** The switch `inbox.at_rest.admin_body_read` has the default `false`. With `false`, the portal MUST have no route that reads a body for an administrator, and the route MUST answer `404`. With `true`, only the superadmin MAY read a body, with `GET /auth/settings/inbox/items/{id}/body`.

5.11.16 **[5.1.0]** No separate right reads a body. The right `approval.read` MUST give metadata only. A session that the superadmin opens as another account MUST NOT read a body and MUST NOT answer.

5.11.17 **[5.1.0]** When `admin_body_read` is true, each read MUST write one row `body_read` in `item_events`. The row holds the login, the time and the request id, and never the body. The portal MUST count the reads in `portal_item_body_reads_total`. This trace does not stop a person who has the key and the database, because that person reads outside the application.

5.11.18 **[5.2.0]** The command `rekey` MUST keep the associated data of each value, so each seal and each effect check stays valid. It MUST write a row only when the row still holds the value that the command read. A writer that changed the row first keeps its value. The command MUST seal no value twice and MUST lose no row if it stops.

5.11.19 **[5.2.0]** The command `rekey` MUST report the number of rows moved for each table and the rows that remain on each old key. It MUST drop the check value of a key only when no row uses the key. It MUST leave a row that does not open as it is, and it MUST name the table and the id of this row. It MUST NOT write a content to the log, to the report or to an error.

5.11.20 **[5.2.0]** At start, the portal MUST write the log event `content_key_rows_to_rekey` for a key that is not active and still seals rows. The event holds the key id and the count. An `evaluation_id` names the key that made it. The PDP MUST check it with that key until its time ends, so the operator removes an old key after `evaluation_ttl`. No row holds an `evaluation_id` that needs an old key.

### 5.12 Text that comes from an agent

The agent is not trusted. Its text is shown with care. Release: 5.1.0.

5.12.1 **[5.1.0]** The screen MUST show the command first, in a fixed font, with a copy button, the age and the short hash. The explanation MUST come below it, in another style, with the label "Text of the agent, not verified". The text of the explanation MUST NOT be copyable in one gesture. A code block of the explanation MAY have its own copy button. The explanation MUST NOT hold an active link.

5.12.2 **[5.1.0]** The screen MUST show a control character, a bidirectional control and a zero-width character as a visible mark, for example `<U+202E>`. It MUST show the count of non-ASCII characters. The portal MUST NOT remove such a character from the explanation, the comments, the turns or the command of an approval.

5.12.3 **[5.1.0]** The page and the phone MUST show a text as plain text, except an explanation with the format `markdown` (3.6.27). The web page MUST escape every text, and it MUST write only the tags of the closed list of 3.6.28. The explanation longer than 400 characters MUST fold, with an action "Show all". The screen MUST state that the explanation does not change what the service runs.

5.12.4 **[5.2.0]** A screen with a free text field MUST warn: "Do not enter a password or a secret. An agent reads this answer."

5.12.6 **[5.1.0]** The text that an agent says about itself (`agent_label`), its client and its address range are not verified. A screen MUST show them in a box marked "Not verified" and MUST NOT use them as the title of an agent. The portal MUST refuse a label that has a control character or a format character. A screen MUST warn the owner when the label of a new agent equals the name of a known agent of the same key. The fold of 3.8.15 decides the equality. The error code MUST be `invalid_agent_label`, and a refused label MUST make no agent and no intent.

## 6. Technical requirements

6.1 The Inbox is a module of the one Rust binary `portal`. It uses Hyper for HTTP. It has no embedded front.

6.2 The configuration follows the rules of the portal. It is one HCL file with a closed schema. An error stops the program at start. The program does not coerce a value.

A secret uses `_env` or `_file`. The command `portal check-config` checks the file. The delivery values (`heartbeat_interval`, `session_timeout`, `resend_after`, `max_inflight`, `unreachable_after`, `offline_warning_after`, `delivery_ttl`, `alert_retention`, `connection_log_retention`) MUST have no default. A wrong default could hide an alert.

6.3 The Inbox uses the database of the portal. It has its own chain of migrations. Each chain MUST refuse a schema that is newer than the binary.

6.4 The protocol and the cryptography of the phones are in the crate `inbox-core`, with no I/O. The server and the app share it. The cryptography exists once.

6.5 The metrics of the Inbox have the prefix `portal_inbox_`. The metrics of the alerts of the portal have the prefix `portal_watch_`. They are in the Prometheus text of the portal. The log is the JSON log of the portal.

6.6 The build uses Docker only. There is one static musl image. No command publishes an image to a registry. A release holds binaries, the signed APK and `SHA256SUMS`.

6.7 The tests use the emulator and the end-to-end tests of the binary. No physical phone is needed.

6.8 The permissions of the Inbox are on the one internal scope of the portal, the service `portal`. The actions are `inbox.receive`, `alert.send`, `alert.send_user`, `alert.read`, `phone.read`, `phone.revoke`, `phone.log`, `approval.read`, `approval.cancel`, `question.ask`, `question.read` and `question.cancel`. `inbox.receive`, `alert.send` and `alert.read` apply to the resource type `inbox_group`. The other actions apply to the service.

### 6.9 Configuration of items

The blocks `inbox.items`, `inbox.at_rest`, `inbox.approvals` and `inbox.mcp` follow 6.2. The reference of the keys comes from the table of keys in the code. The values below are examples. The file has no default for a key that says "required".

6.9.1 **[5.1.0]** The block `inbox.items` MUST have these required keys: `max_lifetime`, `max_open_per_key`, `max_open_per_owner`, `max_open_total`, `lease.min`, `lease.max`, `lease.grace`, `lease.revive_window`, `retention`, `body_purge_after`, `audit_retention` and `digest.mode`. The examples use 90d, 5, 20, a total, 30s, 7d, 2m, 7d, 180d, 30d, 365d and `daily`.

6.9.2 **[5.1.0]** These keys have a default: `lease.max_revivals` 3, `max_chain` 10, `max_turns` 40, `stale_after` 24h, `max_body_bytes` 16KiB and `wait_max` 25s. The ranges are in the table of keys. A value outside its range MUST stop the start. The error names the key, the range and the value.

6.9.3 **[5.1.0]** The block `inbox.approvals` MUST have `consume_within`. The block has these defaults: `evaluation_ttl` 5m, `intent_ttl` 10m, `max_command_bytes` 16KiB, `max_explanation_chars` 2000 and `require_explanation` false. It has `max_comment_chars` 2000, `max_requests_per_key_per_hour` 20, `deny_cooldown` 5m, `mute_after_refusals` 3, `mute_for` 1h and `web_approval` true. The block MUST need `inbox.items`, `inbox.at_rest`, `pdp` and `keys.mcp = true`.

6.9.4 **[5.3.0]** The block `pdp_client` MUST NOT have the key `approvals`. A file with this key MUST stop the start with the error of an unknown key, and the error MUST name the key. A grant object has `confirm`, with the default `false`.

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

6.9.8 **[5.2.0]** The block `inbox.questions` MUST activate the questions. It needs `inbox.items` and `inbox.at_rest`. It has the key `max_options` with the range 2 to 8 and the default 8.

6.9.9 **[5.1.0]** The block `inbox.agents` is optional. Its keys have the ranges and the defaults of the table of keys: `max_per_key` 16, `new_per_hour` 6, `lock_new` false, `idle_after` 14d and `purge_after` 365d. The block MUST need `inbox.items` and `inbox.at_rest`. `idle_after` above `purge_after` MUST stop the start. The error names the key, the range and the value.

### 6.10 Protocol 5

6.10.1 **[5.1.0]** `PROTOCOL_VERSION` MUST be 5. The server MUST close the session of an older app with the code 4426. The protocol 5 has no compatibility with protocol 4.

6.10.2 **[5.1.0]** The server sends `request`, `message`, `settled`, `digest`, `answer_result`, `agent`, `agent_result` and `welcome`. The phone sends `answer`, `reply`, `refresh`, `sync`, `set_prefs`, `agent_do` and `ack`. The field of `ack` that names the message is `id`. Each parser MUST refuse an unknown field.

6.10.3 **[5.1.0]** A frame MUST fit in one Noise message. The protocol MUST add no new cryptography. The body goes in clear inside the Noise session. No frame MUST go to a service.

6.10.4 **[5.1.0]** The Inbox of 5.0.x has no table `items`, and a build of 5.1.0 before the agents has no table `agents`. A database of this kind MUST stop the start. The error names the table and points to the section "Upgrades" of [Operations](../operations.md#upgrades). The base schema changes in place, and no migration exists.

6.10.5 **[5.1.0]** The frame `request` MUST name the agent of the item in `from`: `agent_id`, `agent_kind` and `agent_state`. The value of `from.label` MUST be the name that the owner gave to the agent, else the key name and the number of the agent. No member of `from` MUST hold text that an agent sent. The text that an agent says about itself MUST be in `agent_label`, outside `from`. A parser MUST refuse such a text with a control or format character.

6.10.6 **[5.1.0]** After each change that raises the `rev` of an agent, the server MUST write a message for each device of the owner. It MUST send the frame `agent` from the queue, with a receipt. The frame MUST hold the state of the agent when the server sends it. The server MUST withdraw a message of an older `rev` when a newer message waits. The server MUST NOT send the frame to a device of another owner. A phone MUST ignore a frame with a lower `rev`.

6.10.7 **[5.1.0]** The frame `sync` MAY list the agents that the phone holds, with their `rev`. The server MUST send the frames `agent` that the phone lacks or holds in an older `rev`. It MUST NOT send an idle, `blocked` or `merged` agent that the phone does not hold. It MUST send no agent of another owner.

6.10.8 **[5.1.0]** The frame `agent_do` MUST do one of `rename`, `allow`, `block`, `unblock` and `merge` through the engine of the agents. The server MUST answer `agent_result` with one status of the closed list. A refusal MUST change nothing, and an agent of another owner MUST get `not_found`. Each action MUST be idempotent. A server without the block `inbox.agents` MUST answer `not_enabled`. The phone MUST NOT ask for a biometric check.

6.10.9 **[5.1.0]** The status `agent_locked` of `answer_result` MUST tell that the agent of the item is `new` and that the server locks the new agents. The item MUST stay open, and nothing MUST be written. With `lock_new` false, the server MUST NOT answer this status, and the frame `agent` MUST have `lock_accept` false.

### 6.11 Log, metrics and tests

6.11.1 **[5.1.0]** The log MUST have the lines `approval_requested`, `approval_answered`, `approval_consumed`, `approval_abandoned`, `approval_revived`, `approval_superseded`, `approval_expired`, `body_served`, `body_read`, `body_effect_mismatch` and `content_key_loaded`. No line MUST hold a body. The metrics have the prefix `portal_item_`.

6.11.2 **[5.1.0]** Each rule of time MUST use an injected clock. A test MUST simulate 90 days without a pause. The tests MUST run on SQLite and on PostgreSQL.

6.11.3 **[5.1.0]** A canary test MUST put one unique string in a command, an explanation, a comment and a turn. After a full cycle and the purge, the string MUST be absent from each table column, the replica and the backup. It MUST also be absent from the log, the traces and the metrics. It MUST be present in the decoded Noise frame and in the web page.

6.11.4 **[5.1.0]** A test with a service that records its input MUST run complete cycles with an explanation, a hint, a comment, a thread and several phones. The service MUST NOT receive any of these strings, a login, a device id or a response time.

### 6.12 Verification

Each row maps a requirement of the releases 5.1.0 to 5.5.0 to the tests that check it. A name with `::` is a Rust test: the part before the last `::` is a module or a test file. A name with a dot is a Kotlin test class and method. A row with the words "Scope rule" or "Documentation rule" has no automated test.

| Requirement | Release | Tests |
|---|---|---|
| 3.1.5a | 5.1.0 | `proto::tests::the_kind_of_an_alert_is_alert_and_nothing_else`, `frames::the_golden_files_cover_every_frame_in_both_directions`, `old_hello_protocol_4_gets_4426` |
| 3.3.8a | 5.1.0 | `agent_channel::api_key_cannot_use_mcp_and_mcp_key_cannot_use_rest_alerts`, `mcp_contract::the_key_and_tool_cases_k01_k02_k03_r01_to_r09` (K01) |
| 3.6.1 | 5.1.0 | `an_item_reaches_each_phone_of_its_owner_that_is_not_revoked`, `create_idempotent_same_request_same_id` |
| 3.6.2 | 5.1.0 | `an_item_reaches_each_phone_of_its_owner_that_is_not_revoked`, `renewal_writes_at_most_once_per_lease_s_in_both_databases` |
| 3.6.3 | 5.1.0 | `items::tests::state_machine_every_state_times_event_table` |
| 3.6.4 | 5.1.0 | `cas_two_racers_one_answer`, `an_approval_is_consumed_once_and_lapses` |
| 3.6.5 | 5.1.0 | `answer_after_abandon_returns_abandoned`, `the_sweeper_abandons_after_lease_plus_grace_and_not_before`, `items::tests::abandon_after_lease_plus_grace_not_before` |
| 3.6.6 | 5.1.0 | `items::tests::waits_are_checked_and_never_changed`, `until_expires_and_never_abandons`. `approval_api_tests::a02_lease_out_of_range_422_no_clamp`, `approval_api_tests::a02_lease_and_until_together_422`, `approval_api_tests::a10_intent_rules_explanation_one_shot` |
| 3.6.7 | 5.1.0 | `items::tests::lease_effective_is_three_times_lease_s`, `items::tests::max_lifetime_caps_lease_and_supersede_does_not_extend`, `ninety_days_are_simulated_without_a_pause_and_supersede_does_not_extend` |
| 3.6.8 | 5.1.0 | `items::tests::renewal_write_at_most_once_per_lease_s`, `renewal_writes_at_most_once_per_lease_s_in_both_databases`, `events_one_row_per_transition_none_per_renewal`. `agent_channel::get_answer_renews_lease_without_budget_unit` (`get_answer`, `keepalive` and a read with `wait` renew, a read without `wait` does not, and no table gains a row) |
| 3.6.9 | 5.1.0 | `items::tests::abandon_after_lease_plus_grace_not_before`, `the_sweeper_abandons_after_lease_plus_grace_and_not_before` |
| 3.6.10 | 5.1.0 | `items::tests::until_reached_is_expired_neutral`, `until_expires_and_never_abandons`, `ninety_days_are_simulated_without_a_pause_and_supersede_does_not_extend` |
| 3.6.11 | 5.1.0 | `revive_in_window_max_three_notifications_once`, `revive_after_window_refused_new_request_needs_previous_id`, `items::tests::revive_needs_the_window_the_count_and_the_body`. `agent_channel::keepalive_revives_in_the_window_and_gives_410_after` (the `410` and `revivable` false) |
| 3.6.12 | 5.1.0 | `supersede_race_one_winner_old_approval_cancelled`. `approval_api_tests::supersedes_cancels_old_approval` |
| 3.6.13 | 5.1.0 | `the_cancel_of_a_key_and_of_an_owner_withdraws_the_open_items`. `agent_channel::cancel_makes_pep_see_void` (the key cancels, the service reads `void`) |
| 3.6.14 | 5.1.0 | `cas_two_racers_one_answer`, `answer_effect_mismatch_state_unchanged`, `answer_after_abandon_returns_abandoned`. `approve_on_phone_then_pdp_true_once`, `two_phones_race_one_accepted_other_settled`, `answer_after_abandon_gets_abandoned_and_settled` |
| 3.6.15 | 5.1.0 | `the_thread_follows_the_flags_the_limits_and_the_pace_of_the_agent`. `approval_api_tests::reply_moves_to_replied_and_agent_sees_user_message`, `custom_and_ask_back_are_refused_without_the_flags` |
| 3.6.16 | 5.1.0 | `key_interaction_default_by_kind`, `config_interaction_values_closed`, `the_interaction_field_is_closed_and_an_absent_field_changes_nothing`, `grant_roundtrip_confirm_both_backends` |
| 3.6.17 | 5.1.0 | `approval_api_tests::a10_intent_rules_explanation_one_shot` (`422 interaction_not_allowed`). `agent_channel::say_needs_allow_ask_back_and_conversational_key`: a `say` of a `one_shot` key. The tool is hidden at the endpoint, and the REST door answers `422 interaction_not_allowed`. `the_thread_follows_the_flags_the_limits_and_the_pace_of_the_agent` tests the refusal of a `reply` of a `one_shot` item. |
| 3.6.18 | 5.1.0 | `caps_per_key_owner_total` |
| 3.6.19 | 5.1.0 | `body_purge_keeps_metadata_effect_and_events` |
| 3.6.20 | 5.1.0 | `outage_extends_open_leases_by_the_gap_no_false_abandon`, `degraded_storage_returns_storage_degraded_and_writes_nothing` |
| 3.6.21 | 5.2.0 | `question_tests::ask_makes_a_pending_question_and_the_answer_reaches_the_agent_in_the_documented_form`, `question_tests::invalid_questions_are_refused_with_their_codes_and_create_nothing`, `questions::tests::the_limits_refuse_and_never_cut`, `questions::tests::the_defaults_of_the_bounds_and_the_body_form`, `questions_e2e::the_phone_gets_a_question_and_answers_a_choice_a_custom_text_a_decline_and_asks_back`. `items_load::a_huge_explanation_options_and_forms_are_refused_before_storage` |
| 3.6.22 | 5.2.0 | `questions::tests::flags_matrix_all_valid_combinations_per_type`, `questions::tests::one_shot_with_any_flag_interaction_not_allowed`, `question_tests::multiple_with_custom_is_exclusive`, `question_tests::one_shot_key_asks_a_closed_choice_and_gets_interaction_not_allowed_for_a_flag`, `question_tests::a_text_without_allow_custom_and_a_wrong_answer_are_refused_and_write_nothing` |
| 3.6.23 | 5.2.0 | `questions::tests::command_with_bidi_or_control_refused`, `questions::tests::command_zero_width_accepted_and_flagged_in_frame`, `questions::tests::the_limits_refuse_and_never_cut`, `questions_e2e::answers_that_do_not_fit_the_question_get_a_result_and_change_nothing`. `items_load::a_flood_of_answers_from_a_phone_is_bounded` |
| 3.6.24 | 5.2.0 | `question_tests::text_answer_arrives_as_choice_empty_plus_untrusted_text`, `question_tests::a_repeat_of_an_answered_question_gives_the_answer`, `question_tests::ask_blocks_at_most_wait_s_then_returns_handle`, `question_tests::jev_style_rest_loop_get_wait_keeps_lease_600s`, `questions_e2e::the_phone_gets_a_question_and_answers_a_choice_a_custom_text_a_decline_and_asks_back` |
| 3.6.25 | 5.2.0 | `question_tests::question_budget_counts_ask_say_supersede_not_renewals`, `question_tests::the_tool_ask_needs_the_block_and_the_permission_and_tools_list_follows`, `question_tests::a_cancel_needs_the_permission_and_a_withdrawn_question_is_cancelled`, `question_tests::question_never_passes_through_a_service`. `items_load::a_flood_of_asks_hits_the_budget_questions_and_writes_nothing`, `items_load::many_open_questions_per_key_stop_at_max_open_per_key`, `items_load::a_flood_of_asks_with_and_without_agent_tokens_keeps_rows_events_and_frames_bounded`, `items_load::canary_for_question_bodies_in_every_place` (no service sees a question), `items_load::the_web_forms_of_a_question_are_bounded_and_owner_only` |
| 3.7.1 | 5.1.0 | `confirm_true_when_key_or_owner_grant_has_it`, `superadmin_owner_uses_key_confirm_only`, `config_confirm_on_portal_service_stops_start`, `config_confirm_on_access_stops_start`, `grants::tests::confirm_is_refused_on_the_service_portal_on_access_and_on_mcp_session`, `pdp::approval_tests::the_confirm_of_a_person_is_ignored_for_the_person_and_applies_to_the_keys_of_the_person`, `pdp::approval_tests::the_confirm_of_a_grant_of_mcp_session_has_no_effect` |
| 3.7.2 | 5.3.0 | Conformance D76, D82, D86 to D88 (`conformance_d::the_cases_of_the_series_d_pass`), `pdp::approval_tests::confirm_without_the_block_approvals_plain_false`, `approval_api_tests::a01_a_service_with_an_unknown_or_bad_document_gets_a_plain_false`, `pdp::approval_tests::answers_carry_only_listed_reasons`, `pdp::approval_tests::other_subject_resource_or_effect_not_applicable` |
| 3.7.2a | 5.3.0 | `approval_api_tests::a01_every_action_of_a_service_can_ask_for_an_approval`, `actions::tests::the_member_approval_of_an_action_is_ignored_with_any_value`, `key_forms::tests::a_grant_of_a_form_asks_for_an_approval_on_any_action_of_a_service_but_not_on_access_or_portal`, `ui::grant_editor::tests::each_action_of_a_service_gets_the_lock_but_access_and_the_portal_do_not` |
| 3.7.2b | 5.3.0 | Conformance D97, `approval_api_tests::a01_without_the_block_approvals_the_api_gives_403`, `approval_api_tests::a01_an_unknown_credential_gets_401_on_each_route`, `approval_api_tests::a01_the_evaluation_id_opens_only_the_request_that_the_pdp_asked_for`, `pdp::approval::tests::a_submission_opens_the_id_with_the_service_and_the_effect_and_gets_the_sealed_facts` |
| 3.7.3 | 5.1.0 | `pdp::approval::tests::the_evaluation_id_is_stateless_valid_on_another_instance_for_the_ttl_and_binds_each_fact`, `pdp::approval::tests::an_evaluation_id_survives_a_rotation_of_the_content_key_while_the_old_key_is_in_the_file`, `pdp::approval_tests::the_evaluation_id_of_an_answer_binds_the_question_and_works_on_another_instance`. `approval_api_tests::a01_evaluation_id_required_other_subject_422`, `pdp::approval::tests::a_submission_opens_the_id_with_the_service_and_the_effect_and_gets_the_sealed_facts` |
| 3.7.4 | 5.1.0 | `frames::golden_approval_verifies_and_a_changed_op_does_not`. `approval_api_tests::a11_ref_single_use_bound_to_key_expires` (the effect of a request with an explanation is the effect of `op`) |
| 3.7.5 | 5.1.0 | `effect::tests::effect_vectors_match`, `effect::tests::view_pointer_must_exist`, `effect::tests::effect_changes_with_service_action_resource_op_each`. `approval_api_tests::a08_effect_mismatch_invalid_view_command_too_large`. `pep_e2e::pep_end_to_end` (the effect of the real PEP is the effect of the portal and of the phone) |
| 3.7.6 | 5.1.0 | `effect::tests::jcs_rejects_float_and_integer_above_2_53`. `approval_api_tests::a08_effect_mismatch_invalid_view_command_too_large`, `approval_api::tests::a_bad_op_is_invalid_op_and_a_bad_envelope_is_invalid_argument` |
| 3.7.7 | 5.1.0 | `approval_api_tests::a08_effect_mismatch_invalid_view_command_too_large`, `approval_api_tests::a10_intent_rules_explanation_one_shot` |
| 3.7.8 | 5.1.0 | `approval_api_tests::a11_ref_single_use_bound_to_key_expires`, `approval_api_tests::intent_content_not_readable_by_pep`, `approval_api_tests::a10_intent_rules_explanation_one_shot` |
| 3.7.9 | 5.1.0 | `approval_api_tests::a01_idempotent_repost_same_id_no_renewal_no_body_swap`, `approval_api_tests::idempotence_under_concurrency_one_open_request_for_each_effect`, `approval_api_tests::a_unique_index_on_the_hash_refuses_a_second_open_request_and_frees_it_at_the_end`. `pep_e2e::pep_end_to_end` (scenario the real PEP resubmits while the request is pending and gets the same id and code) |
| 3.7.10 | 5.1.0 | `effect_mismatch_refused`, `custom_and_ask_back_are_refused_without_the_flags`, `approval_api_tests::decline_with_comment_reaches_agent_never_pep` |
| 3.7.11 | 5.1.0 | `approval_api_tests::decline_with_comment_reaches_agent_never_pep`, `frames::tests::previous_and_follows_come_from_the_facts` |
| 3.7.12 | 5.1.0 | `a_phone_of_another_account_gets_nothing_and_cannot_answer`, `revoked_device_answer_refused_4403_and_no_state_change`. `items_web_e2e::impersonated_session_cannot_answer`, `owner_sees_only_own_items`, `web_approval_false_lists_only` |
| 3.7.13 | 5.1.0 | `two_phones_race_one_accepted_other_settled`, `phone_enrolled_after_creation_receives_open_items`, `two_instances_each_flush_their_own_phone` |
| 3.7.14 | 5.1.0 | Conformance D77, D78, D81, `pdp::approval_tests::consume_takes_one_unit_once`, `pdp::approval_tests::budget_exceeded_keeps_approval_usable`, `pdp::approval_tests::a_failed_consumption_gives_the_units_back_and_leaves_the_approval_usable`, `pdp::approval_tests::a_race_for_one_approval_counts_one_use`, `pdp::approval_tests::two_resume_calls_race_one_true`, `pdp::approval_tests::hundred_concurrent_resumes_one_true`, `items_tests::concurrent_consumes_use_an_approval_once_and_status_consumes_nothing` |
| 3.7.15 | 5.1.0 | Conformance D80, `pdp::approval_tests::crash_between_count_and_consume_never_loses_the_approval_and_never_double_executes`, `pdp::approval_tests::abandoned_or_lapsed_reads_as_approval_expired`, `items_tests::an_approval_is_consumed_once_and_lapses` |
| 3.7.16 | 5.1.0 | Conformance D84, `pdp::approval_tests::revoked_key_or_removed_grant_after_approval_is_false`, `pdp::approval_tests::revoking_a_key_cancels_its_open_approvals` |
| 3.7.17 | 5.1.0 | `approval_api_tests::deny_cooldown_same_effect_only_and_not_after_abandon`, `approval_api_tests::mute_after_three_refusals_alerts_owner`. The button "refuse and mute the key for one hour" comes with the phone and the web |
| 3.7.18 | 5.1.0 | `approval_api_tests::a01_429_after_cap` |
| 3.7.19 | 5.1.0 | `approval_api_tests::the_verdict_follows_the_state_and_only_four_values_exist`, `approval_api_tests::a12_get_returns_only_verdict_fields` |
| 3.7.20 | 5.1.0 | `approval_api_tests::pep_observation_unit`, `approval_api_tests::a12_get_returns_only_verdict_fields`, `approval_api::tests::the_routes_of_the_api_are_the_three_of_17_4` `pep_e2e::pep_end_to_end` (ignored; run by `tools/dev/pep-e2e.sh` with the real reference PEP: the tap shows what the service received) |
| 3.7.21 | 5.1.0 | `approval_api_tests::a03_renew_only_with_subject_of_requester` |
| 3.7.22 | 5.1.0 | `items_web_e2e::stale_request_needs_checkbox`, `ui::items::tests::an_old_request_has_the_banner_and_the_required_box_and_a_new_one_has_neither` |
| 3.7.23 | 5.1.0 | `alert.AttentionTest`, `alert.AttentionCenterTest` (Kotlin unit tests of the notification) |
| 3.7.24 | 5.1.0 | `portal_actions::tests::the_document_has_the_keys_and_the_inbox_in_one_scope`, `items_web_e2e::approval_read_right_shows_metadata_not_body`, `an_account_with_approval_cancel_cancels_an_open_request_and_nobody_else_does` |
| 3.7.25 | 5.1.0 | `items_web_e2e::history_filters_chains_and_hides_the_open_items` (the side box says it from the hash), `ui::items::tests::the_command_is_first_and_the_copy_text_is_the_exact_bytes` |
| 3.7.26 | 5.3.0 | `ui::grant_editor::tests::each_action_of_a_service_gets_the_lock_but_access_and_the_portal_do_not`.  `items_web_e2e::open_items_zone_renders_with_javascript_disabled`, `history_filters_chains_and_hides_the_open_items`; `tools/dev/ui-check-items.js` |
| 3.7.27 | 5.1.0 | `items_web_e2e::no_store_header_on_pages_with_bodies`, `csrf_and_origin_required_on_answer_and_reply`, `owner_sees_only_own_items`, `web_approval_false_lists_only`, `web_and_phone_answer_race_one_accepted` |
| 3.7.28 | 5.1.0 | `items_web_e2e::alerts_come_before_the_open_items_and_the_phones_after`; `tools/dev/ui-check-items.js` |
| 3.7.29 | 5.1.0 | `items_web_e2e::the_limits_page_shows_the_values_the_sources_and_no_secret`, `ui::items::tests::the_limits_page_hides_a_secret_and_shows_the_source` |
| 3.7.30 | 5.1.0 | `ui::items::tests::the_command_is_first_and_the_copy_text_is_the_exact_bytes`, `items_web_e2e::xss_bidi_control_zero_width_rendered_visible`; `tools/dev/ui-check-items.js` (copy, colors) |
| 3.7.31 | 5.1.0 | `ui::items::tests::groups_order_new_first_then_oldest_bucket_last`, `agents_web_e2e::two_agents_with_one_key_have_two_groups_new_first_bucket_last`, `a_key_without_agents_is_one_group_with_the_name_of_the_key`; `tools/dev/ui-check-agents.js`. The agents of these tests come from the real channel (`agents_web_e2e::trio`). The first call has no token, and the next calls have the token. The owner uses the forms. |
| 3.7.32 | 5.1.0 | `agents_web_e2e::the_first_use_notice_names_the_choices_and_has_no_locked_row_by_default`, `with_lock_new_the_row_the_page_and_the_server_refuse_approve_and_decline_stays_open`; `ui::agents::tests::the_notice_names_the_choices_and_every_form_has_the_token_and_the_agent`, `the_notice_says_locked_only_with_the_switch_and_the_default_has_no_locked_row`; `tools/dev/ui-check-agents.js` (both modes) `agents_web_e2e::the_first_use_notice_names_a_similar_known_agent_and_nothing_merges` |
| 3.7.33 | 5.1.0 | `agents_web_e2e::owner_sees_only_own_agents_and_forms_refuse_foreign_agents`, `forms_need_csrf_and_origin_and_an_impersonated_session_does_nothing`, `rename_allow_block_unblock_merge_work_and_refuse_what_they_must`, `a_blocked_agent_loses_its_open_requests_and_can_be_unblocked`, `pages_of_the_agents_never_name_the_product_and_carry_no_store`; `ui::agents::tests::the_agent_page_has_the_forms_of_its_state` |
| 3.7.34 | 5.1.0 | `ui::items::tests::the_approval_page_has_the_box_from_agent_and_the_lock_only_with_the_switch`, `agents_web_e2e::a_text_that_folds_to_the_name_of_a_known_agent_is_flagged`; `tools/dev/ui-check-agents.js` |
| 3.7.35 | 5.1.0 | `ui::items::tests::the_history_names_the_agent_with_a_link_and_the_filter_has_a_chip`, `agents_web_e2e::history_filter_by_agent_and_the_agent_column` |
| 3.7.36 | 5.1.0 | `data.AgentTest`: `anAgentFrameIsKeptAndAcknowledged`, `aLowerRevIsIgnoredButStillAcknowledged`, `aHigherRevReplacesTheRowAndKeepsTheNotifiedFlag`, `theSameRevRenewsTheSnapshotOfOpenAndLastCallWithNoEvent`, `theSameDeliveryTwiceChangesNothingAndIsAcknowledgedAgain`, `twoPortalsHoldAgentsWithTheSameIdApart` |
| 3.7.37 | 5.1.0 | `data.AgentTest`: `aMergedAgentFollowsMergedIntoUntilAnAgentThatIsNotMerged`, `aChainWithAMissingTargetEndsAtTheTargetIdWithNoRow`, `aLoopInTheChainStopsAndNeverHangs`, `theItemsOfAMergedAgentCountForTheTarget`, `theLabelComesFromTheTableAndARenameShowsOnOldItemsAtOnce`, `theLabelOfTheFrameIsTheFallbackWhenTheAgentIsNotHeld`, `theLabelOfAnItemOfAMergedAgentIsTheNameOfTheTarget`, `theLabelLookupUsesThePortalOfTheItem`; `ui.AgentGroupsTest.theCountIsMadeByThePhoneNotByTheSnapshotOfTheFrame` |
| 3.7.38 | 5.1.0 | `data.AgentTest`: `theSyncFrameListsTheAgentsThatThePhoneHoldsWithTheirRev`, `theSyncFrameHasNoMemberAgentsWhenThePhoneHoldsNone`, `theSyncListHasAtMost200AgentsTheLiveOnesAndTheRecentOnesFirst` |
| 3.7.39 | 5.1.0 | `ui.AgentGroupsTest`: `theGroupsComeByTheirOldestOpenItem`, `insideAGroupTheOldestIsFirst`, `newAgentsComeFirstThenTheOldestGroupsAndTheBucketIsLast`, `twoNewAgentsKeepTheOrderOfTheirOldestItem`, `groupsOfTwoPortalsNeverMergeEvenWithTheSameAgentId`, `aFoldedGroupShowsNoRowButKeepsItsCount`, `theFoldingOfOnePortalDoesNotFoldTheSameAgentIdOfAnother`, `aRowUnderTheHeaderDoesNotRepeatTheNameOfTheAgent`; `ui.AgentCollapseTest` (all); `ui.ItemListOrderTest`: `theAlertsSectionIsAlwaysTheFirstOne`, `theCardsOfTheNewAgentsSitBetweenTheAlertsAndTheItems` |
| 3.7.40 | 5.1.0 | `ui.NewAgentCardTest`: `aNewAgentHasACardWithTheKeyInTheTitleAndTheFirstCall`, `theCountOfWaitingRequestsIsMadeByThePhone`, `anAgentThatIsNotNewAndABucketHaveNoCard`, `theCardsFollowTheFilterOfThePortals`, `theObservedBoxHoldsTheHintsAndTheValuesAreTheTextOfAClient`, `theLockLineShowsOnlyWithLockAccept`, `theNameOfTheFormIsCheckedBeforeItGoesOut`; `ui.AgentGroupsTest.theLockLineShowsOnlyWhenTheServerLocksTheNewAgent`; the real screens: to check on a phone (M-G4-1) |
| 3.7.41 | 5.1.0 | `ui.NewAgentCardTest`: `aTextThatEqualsTheNameOfAKnownAgentOfTheKeyGivesTheCopyWarning`, `aLookAlikeLetterGivesTheLookAlikeWarning`, `aNameOfAnotherKeyOrAnotherPortalIsNotCompared`, `theSimilarHintOfThePortalIsShownWithTheCopyNoteAndNeverMerges`, `withNoSimilarHintTheMergeButtonOpensAPickerOfTheKnownAgents`, `withNoKnownAgentOfTheKeyThereIsNoMergeButton`, `theAgentThatLooksLikeComesFirstInThePicker`; `data.AgentTest.theMergeTargetsAreTheLiveAgentsOfTheSameKeyAndPortalTheKnownFirst` |
| 3.7.42 | 5.1.0 | `ui.AgentBoxesTest`: `theAgentScreenCountsOpenAndAnsweredItemsItself`, `theAgentScreenOffersTheActionsOfTheStateWithoutThePendingOnes`, `aBlockedAgentCanBeUnblockedAndTheBucketHasNoActionAndNoHints`, `aMergedAgentNamesItsTargetAndHasNoAction`, `theObservedRowsOfTheAgentScreenHaveTheDetails`; `WordingAndManifestTest.onlyTheLinkScreenIsExportedAndReenrollIsNot` (the activity is not exported); the real screens: M-G4-2 |
| 3.7.43 | 5.1.0 | `data.AgentActionsTest`: `allowWithAName`, `renameBlockUnblockAndMergeMakeTheirFrames`, `theFrameIsSavedInTheOutboxWithTheAgentIdAndTheTypeAgentDo`, `anActionThatTheStateDoesNotAllowIsRefusedBeforeAnyFrame`, `aMergeNeedsALiveAgentOfTheSameKeyAndPortal`, `aBadNameIsRefusedAndNeverCleaned`, `theSameActionThatAlreadyWaitsIsNotQueuedTwice`, `theResultClosesTheEntryAndGivesTheEventWithTheLine`, `aResultForNoEntryOrAnEntryThatIsNotAnAgentActionChangesNothing`, `everyStatusOfTheClosedListHasItsOwnLine`, `theOutboxFrameGoesOutAgainAfterTheNextWelcome`, `anAgentActionIsNeverAnAnswerOfAnItem`; `service.AgentNoticesTest`; the sheet of the block: M-G4-2 |
| 3.7.44 | 5.1.0 | `ui.AgentBoxesTest`: `theBoxHoldsTheNameTheStateTheKeyAndWhatTheAgentSays`, `theStateComesFromTheTableSoAnAllowedAgentIsNotNewAnyMore`, `anAgentThatThePhoneDoesNotHoldFallsBackToTheFrameAndIsNotLocked`, `aRequestThatSaysAnotherNameThanTheTableIsShownToo`, `theBucketHasNoSaysAndNoState`, `aNewAgentThatSaysTheNameOfAKnownAgentGetsTheWarning`; the real screen: M-G4-3 |
| 3.7.45 | 5.1.0 | `ui.AgentBoxesTest`: `approveIsLockedForANewAgentAndDeclineStaysOn`, `approveWorksAgainWhenTheAgentIsAllowed`, `theLockOfTheAgentCountsBeforeTheReadGate`, `withTheSwitchOffTheNewAgentIsNotLocked`; `data.AgentActionsTest.theLockedAnswerKeepsTheItemOpenWithAClearText` |
| 3.7.46 | 5.1.0 | `alert.AttentionAgentsTest`: all tests (`aNewAgentIsACountAndALineOfThePortal`, `theLockScreenHasTheCountAndNoKeyName`, `theNewAgentUsesTheOneIdAndTheOneTagAndNeverASecondNotification`, `theSameAgentAndTheSameStateNeverAlertAgain`, `anAgentThatThePhoneAlreadyShowedIsNotNewAfterARestart`, `theDeclineActionIsOfferedForOneApprovalOnlyWithNoNewAgent`); `data.AgentTest.aNewAgentThatThePhoneDidNotHoldGivesTheEventOnce`; the real notification: M-G4-4 |
| 3.7.47 | 5.1.0 | `ui.AgentBoxesTest`: `aLookAlikeLetterAndAHiddenCharacterShowAsMarks`, `theQuotedTextKeepsItsSpansInPlace`, `theFixedSentencesAreAscii`; `ui.NewAgentCardTest.theTextOfTheClientIsNeverCopiedIntoAWarningOrATitle`; `ui.AgentGroupsTest.aHiddenCharacterInANameShowsAsAMark`; `WordingAndManifestTest.theTextsOfTheAgentsAreAsciiOnly` |
| 3.7.48 | 5.1.0 | `ui::agents::tests::the_list_has_the_form_add_agent_only_with_a_key_in_the_mode_enrolled`, `the_box_reset_token_shows_only_when_the_page_says_so_and_it_warns_the_old_token_stops`, `agents_web_e2e::a_key_that_is_not_enrolled_has_no_form_add_agent_and_no_reset`, `enrolled_cycle_enroll_call_with_and_without_the_token_reset_old_token_dead_new_works` |
| 3.7.49 | 5.1.0 | `ui::agents::tests::the_token_page_shows_the_token_and_the_header_line_with_copy_buttons_and_says_one_time`, `no_page_of_the_agents_names_the_product`. `agents_web_e2e::enrolled_cycle_enroll_call_with_and_without_the_token_reset_old_token_dead_new_works`: `no-store`, and the token is on no other page, in no row and in no log line. `enroll_and_reset_need_csrf_origin_the_owner_and_a_key_in_the_mode_enrolled`: csrf, origin, impersonation, foreign key, foreign agent, other mode, names and cap. `the_service_at_the_end_of_the_chain_never_sees_the_token_of_an_enrolled_agent`. `items_e2e::an_enrolled_agent_reaches_the_phone_as_known_and_a_reset_raises_its_rev`: no frame holds a token. |
| 3.7.50 | 5.1.0 | `frames::agent_do_has_no_operation_to_enroll_or_to_reset_a_token` (crate `inbox-core`), `docs/inbox/agents.md` section "Enrolled mode" |
| 3.7.51 | 5.1.0 | `ui::agents::tests::the_line_also_asked_by_names_the_other_agents_with_text_of_the_server`, `agents_web_e2e::also_asked_by_names_the_other_agents_that_asked_the_same_effect_with_text_of_the_server` |
| 3.7.60 | 5.2.0 | `ui.QuestionScreensTest`: `aQuestionOpensTheQuestionScreenAndAnApprovalOpensTheApprovalScreen`, `ui.ItemListOrderTest`: `aQuestionRowShowsTheKindOfAnswerAndOpensItsScreen`, `ui.QuestionHistoryTest`: `theDoneRowOfAQuestionUsesTheOwnAnswer`. The zones are the code of `ExplanationZone` and `QuestionActivity` (check on a device) |
| 3.7.61 | 5.2.0 | `data.AnswerFrameBuildTest`: `aConfirmAnswerIsYesOrNo`, `aTextAnswerCarriesTheText`, `aMessageWithNoAnswerHasNoSheet`, `aFormAnswerHasOneEntryForEachQuestion`; `ui.QuestionStateTest`: `aMessageWithNoAnswerShowsOnlyDecline` |
| 3.7.62 | 5.2.0 | `data.FlagsMatrixRowsTest`: `aChoiceFollowsTheFourRowsOfTheMatrix`, `aTextQuestionHasNoOtherRowBecauseItsFieldIsTheAnswer`, `everyCombinationGivesAnAnswerAndAnInvalidOneNeverShowsARow`, `aFrameGivesTheSameRowsAsItsFlags`; `ui.QuestionStateTest`: `aClosedQuestionHasNoEscapeRowAndOnlyDecline` |
| 3.7.63 | 5.2.0 | `data.MultipleWithCustomExclusiveTest`: all tests; `ui.QuestionStateTest`: `theWordsOfTheSecretWarningAreFixed`; `data.AnswerFrameBuildTest`: `theLimitsOfTheTextAreThoseOfTheServer`, `theTextIsNotChangedAndATextOfOnlySpacesIsNoAnswer` |
| 3.7.64 | 5.2.0 | `ui.OneShotScreenStateTest`: all tests |
| 3.7.65 | 5.2.0 | `ui.QuestionStateTest`: `sendIsOffUntilTheAnswerIsValidAndThenAsksForTheFingerprint`, `withoutAScreenLockSendStaysOffAndSaysWhy`; `ui.QuestionHistoryTest`: `thePromptTitleIsOneLineOfAtMostEightyCharacters`; `data.AnswerFrameBuildTest`: `theSheetRefusesWhatValidateFormAnswerRefuses`. The prompt is a check on a device |
| 3.7.66 | 5.2.0 | `data.AnswerFrameBuildTest`: `aChoiceAnswerCarriesTheIdOfTheOption`, `aCustomAnswerCarriesTheTextAndNoChoice`, `aDeclineCarriesNoChoiceNoTextNoIntent`; `data.QuestionOutboxTest`: `theAnswerIsSentAgainAfterEachReconnectionUntilTheResultArrives`, `aSettledFrameClosesTheAnswerThatWaits` |
| 3.7.67 | 5.2.0 | `ui.QuestionStateTest`: `aNewAgentThatTheServerLocksTurnsSendOffButKeepsDeclineAndAsk` |
| 3.7.68 | 5.2.0 | `ui.QuestionHistoryTest`: `anAnswerOfThisPhoneShowsWhatWasChosen`, `aDeclineOfThisPhoneIsDeclinedNotAnswered`, `anAnswerFromTheWebOrAnotherPhoneNamesWhoAnsweredButNotWhat`, `anAbandonedQuestionTellsThatItIsNotADecline`, `aLateAnswerGetsTheWordsOfMock6b`, `anExpiredQuestionGivesTheReasonAndNoDecision` |
| 3.7.69 | 5.2.0 | `WordingAndManifestTest`: `theTextsOfTheQuestionsAreAsciiOnly`, `theQuestionScreenIsNotExportedAndHasNoIntentFilter`; `ui.QuestionScreensTest`: `theHeaderSaysTheKindTheAgentTheAgesAndTheNumberOfOptions`, `theSendButtonNamesThePortalAndSaysWhyItIsOff`; `ui.QuestionStateTest`: `theHiddenCharactersOfTheAgentShowAsMarksInTheSenderLineAndTheSuggestion`; the notification rule is in `alert.AttentionCenterTest` and `alert.AttentionDigestTest`, which count a question in the one notification |
| 3.7.70 | 5.2.0 | `questions_web_e2e::the_page_of_a_choice_has_the_boxes_the_zones_and_no_tick`, `the_panel_lists_the_questions_by_agent_and_is_hidden_when_nothing_waits`; `ui::question_page::tests::the_title_is_escaped_and_no_option_is_ticked`; ui-check `ui-check-items.js` with `questions_demo_for_the_eyes` (390 and 1440 px, dark and light) |
| 3.7.71 | 5.2.0 | `questions_web_e2e::a_choice_a_confirm_a_multiple_choice_and_a_custom_text_reach_the_agent`, `a_form_has_blocks_a_counter_per_question_and_an_answered_view`, `a_decline_carries_nothing_and_the_agent_reads_declined`; `ui::questions::tests::a_form_needs_one_answer_for_each_block`, `a_checkbox_follows_min_and_max_and_the_ticks_keep_the_order_of_the_options`, `a_confirm_gives_yes_or_no_and_a_text_question_gives_the_text`, `a_message_with_no_answer_has_no_sheet` |
| 3.7.72 | 5.2.0 | `questions_web_e2e::rows_follow_flags_matrix_web` (the 40 combinations, 17 valid); `ui::questions::tests::the_four_rows_of_the_matrix_for_a_choice_and_a_confirm`, `seventeen_of_the_forty_combinations_are_valid_and_the_others_show_no_row`, `a_text_question_has_no_other_row_and_a_form_needs_a_choice_for_it`; `questions_web_e2e::hint_shown_above_options` |
| 3.7.73 | 5.2.0 | `questions_web_e2e::secret_warning_present`, `a_text_over_two_thousand_characters_is_refused_with_a_visible_error_and_the_text_is_kept`, `a_wrong_choice_gets_the_page_again_with_the_ticks_and_the_text_that_were_typed`; `ui::questions::tests::other_and_the_ticks_exclude_each_other_and_a_text_with_a_tick_is_not_sent`, `a_text_over_the_limit_is_refused_and_stays_in_the_draft`; `ui::question_page::tests::a_mistake_of_a_block_is_shown_and_the_typed_text_is_kept_not_marked` |
| 3.7.74 | 5.2.0 | `questions_web_e2e::one_shot_page_has_no_chat_controls`, `hint_shown_above_options` |
| 3.7.75 | 5.2.0 | `questions_web_e2e::text_answer_post_requires_csrf_origin`, `a_foreign_owner_is_answered_as_an_unknown_question_and_an_impersonated_session_does_nothing` |
| 3.7.76 | 5.2.0 | `questions_web_e2e::an_answer_on_a_closed_item_gives_a_409_page`, `with_lock_new_the_page_refuses_the_answer_and_decline_and_ask_stay_open`; `ui::question_page::tests::a_new_agent_with_the_lock_turns_send_off_and_keeps_decline_and_ask` |
| 3.7.77 | 5.2.0 | `questions_web_e2e::the_history_lists_the_states_of_the_questions_with_their_own_words`, `a_form_has_blocks_a_counter_per_question_and_an_answered_view`; `ui::question_page::tests::the_closed_states_have_their_words_and_no_field`, `an_answered_form_shows_each_block_the_free_text_and_what_was_not_chosen`, `the_words_of_a_row_say_the_kind_of_answer_and_the_discussion` |
| 3.7.78 | 5.2.0 | `questions_web_e2e::hidden_and_bidi_characters_of_the_agent_are_marked_and_markup_is_text`, `the_agent_token_is_never_in_a_page_and_no_page_names_the_product`, `an_account_with_question_read_sees_the_metadata_and_one_with_question_cancel_cancels`, `the_body_of_a_question_is_read_by_the_superadmin_only_behind_the_switch`, `the_page_of_a_choice_has_the_boxes_the_zones_and_no_tick` (no-store) |
| 3.8.1 | 5.1.0 | `mcp_contract::the_transport_cases_t01_to_t13` (T01 to T13: JSON only, no session, the versions), `agent_channel::the_endpoint_follows_the_path_of_the_file_and_the_block_gates_the_tools`, `config::tests::a_bad_key_of_the_block_mcp_or_a_contradiction_stops_the_start_with_the_key_the_range_and_the_value` (`keys.mcp = true`) |
| 3.8.2 | 5.1.0 | `agent_channel::rest_door_serves_the_same_core_with_its_statuses`, `get_answer_renews_lease_without_budget_unit` (the read with `wait` renews, the read without it does not) |
| 3.8.3 | 5.1.0 | `mcp_contract::the_transport_cases_t01_to_t13` (T09 to T12, the order of the checks), `the_key_and_tool_cases_k01_k02_k03_r01_to_r09` (K01, K02), `agent_channel::one_shot_key_tools_list_hides_say_and_supersede`, `unauthenticated_flood_limited_per_address`, `key_over_its_rate_gets_429_and_retry_after` |
| 3.8.4 | 5.1.0 | `agent_channel::wait_never_exceeds_max_wait`, `supersede_cancels_old_approval_and_new_waits` (the wait wakes at once), `get_answer_returns_comment_in_untrusted_with_intent` (a pending read is short), `config::tests::a_bad_key_of_the_block_mcp_or_a_contradiction_stops_the_start_with_the_key_the_range_and_the_value` (`max_wait` against `wait_max`) |
| 3.8.5 | 5.1.0 | `agent_channel::agent_cannot_read_or_speak_on_another_keys_item`, `api_key_cannot_use_mcp_and_mcp_key_cannot_use_rest_alerts`, `cancel_makes_pep_see_void` |
| 3.8.6 | 5.1.0 | `agent_channel::get_answer_returns_comment_in_untrusted_with_intent` (the notice first, the intent, no approver and no device), `the_texts_of_the_agent_never_reach_the_service_through_the_channel`, `mcp_tools::tests::the_notice_is_the_first_member_of_a_result_with_untrusted_text` |
| 3.8.7 | 5.1.0 | `frames::tests::the_line_from_never_holds_text_of_an_agent`. `items_web_e2e::command_above_explanation_and_explanation_labelled_unverified` (the line from has the name of the key and the owner) |
| 3.8.8 | 5.1.0 | `agent_channel::get_answer_renews_lease_without_budget_unit` (an item that the service made with `lease_s` and no intent), `approval_api_tests::pep_observation_unit` |
| 3.8.9 | 5.1.0 | `config::tests::the_block_mcp_has_the_documented_defaults_and_the_set_values_apply`, `a_bad_key_of_the_block_mcp_or_a_contradiction_stops_the_start_with_the_key_the_range_and_the_value` |
| 3.8.10 | 5.1.0 | `agents::tests::token_has_prefix_and_43_chars_and_only_digest_is_stored`, `a_text_of_another_form_is_not_a_token`. `agent_channel::who::two_agents_with_one_key_have_two_groups`, `token_in_header_and_in_argument_must_agree`, `rest_get_with_token_in_query_is_refused`, `agent_token_never_in_results_after_the_first`, `mcp_tools::tests::bind_agrees_the_header_and_the_argument_and_checks_the_label`, `the_transport_gives_the_token_of_the_header_and_bounded_hints`, `mcp_contract::the_agent_cases_a01_to_a10` (A03, A04) |
| 3.8.11 | 5.1.0 | `agents_tests::no_token_on_a_read_never_creates_an_agent`, `resolve_unknown_foreign_key_and_foreign_owner_token_give_the_same_error`. `agent_channel::who::unknown_token_is_agent_unknown_and_the_same_for_every_cause`, `agent_without_token_makes_a_new_agent_each_time_until_the_cap`, `blocked_agent_gets_agent_blocked_on_every_tool` |
| 3.8.12 | 5.1.0 | `agents::tests::binding_a_read_goes_by_the_key_and_a_write_goes_by_the_agent`, `agents_tests::per_key_reads_vs_per_agent_writes_at_engine_level`. `agent_channel::who::read_without_token_is_allowed_read_with_wrong_token_is_agent_mismatch`, `per_key_reads_vs_per_agent_writes` |
| 3.8.13 | 5.1.0 | `agents_tests::cap_and_rate_are_counted_in_the_transaction_two_writers`, `over_cap_goes_to_bucket_without_token_and_alerts_once_per_hour`, `config::tests::agents_block_closed_schema_and_defaults`. `agent_channel::who::agent_without_token_makes_a_new_agent_each_time_until_the_cap`: the call over the cap succeeds with `unattributed` and no token, on both doors. `mcp_contract::the_agent_cases_a01_to_a10` (A06). `agent_channel::who::flood_of_calls_with_no_token_keeps_the_cap_one_alert_each_hour_and_the_rows_bounded`, `flood_against_new_per_hour_goes_to_the_bucket_and_the_next_hour_makes_at_most_the_rate`, `flood_of_tokens_of_a_blocked_agent_and_of_unknown_tokens_writes_nothing`. In these tests, the rows of the agents, the events, the frames and the counters stay bounded, and the alert comes once each hour. |
| 3.8.14 | 5.1.0 | `agents_tests::resolve_mode_key_returns_bucket_and_ignores_token`, `bucket_cannot_be_named_merged_or_blocked`, `approval_api_tests::agent_binding::submission_without_intent_goes_to_the_bucket` |
| 3.8.15 | 5.1.0 | `agents::tests::name_rules_refuse_control_bidi_zero_width_and_never_clean`, `agents_tests::name_unique_on_nfkc_lowercase_inside_the_key_only`, `ordinal_is_per_key_and_never_reused` |
| 3.8.16 | 5.1.0 | `agents_tests::accept_of_a_new_agent_is_agent_locked_but_decline_and_reply_work`, `lock_new_false_lets_a_new_agent_be_approved`. `frames::agent_locked_is_a_status_of_an_answer`, `items_e2e::a_new_agent_is_locked_for_accept_until_the_owner_allows_it`, `a_new_agent_is_not_locked_when_lock_new_is_false`. `agents_web_e2e::with_lock_new_the_row_the_page_and_the_server_refuse_approve_and_decline_stays_open` (the web form gets `409`). `ui.AgentBoxesTest.approveIsLockedForANewAgentAndDeclineStaysOn`, `withTheSwitchOffTheNewAgentIsNotLocked`, `data.AgentActionsTest.theLockedAnswerKeepsTheItemOpenWithAClearText` |
| 3.8.17 | 5.1.0 | `agents_tests::a_chain_stays_inside_one_agent`, `approval_api_tests::agent_binding::supersedes_across_agents_is_invalid_argument`, `previous_id_of_another_agent_is_refused`. `agent_channel::who::previous_id_of_another_agent_is_refused_on_the_channel` |
| 3.8.18 | 5.1.0 | `approval_api_tests::agent_binding::intent_with_token_gives_the_item_to_that_agent`: no answer to the service holds the token or the id. `blocked_agent_intent_ref_is_dead`, `same_effect_by_a_second_agent_returns_the_first_item_and_records_also_requested`. `agent_channel::who::service_never_sees_agent_token_agent_label_or_hints`: the token, the label, the `User-Agent` and the address, through the path of the PEP. `agent_binding::same_effect_by_a_second_agent_returns_the_first_item_and_records_also_requested` also reads the agents with `Items::also_requested_by`. In step SA of `pep_e2e::pep_end_to_end` (`tools/dev/pep-e2e.sh`), a second agent asks for the same effect, and the service gets the same request. The token, the label, the client and the id of both agents are in no request, answer, identity token, metric or log of the stack. |
| 3.8.19 | 5.1.0 | `agents_tests::without_the_block_every_mode_is_the_mode_key`, `resolve_mode_key_returns_bucket_and_ignores_token`, `no_token_on_a_read_never_creates_an_agent` (the mode `enrolled`), `agents::tests::modes_and_states_have_one_text_each`. `agent_channel::who::enrolled_mode_refuses_a_call_without_token`, `a_key_of_the_mode_key_and_a_file_without_the_block_give_the_bucket`, `the_authorizer_gives_the_mode_of_a_key_to_the_frames`, `keys::tests::the_mode_of_the_agents_has_one_name_each_and_a_default_by_kind`, `key_forms::tests::key_form_agents_field_is_closed_and_an_absent_field_changes_nothing`, `ui::keys::tests::the_form_of_a_key_has_the_choice_of_the_agents_only_when_the_file_has_the_block`, `store::tests::grant_roundtrip_confirm_both_backends` (the mode of the agents of a key, on both databases), `sqlite::tests::a_file_made_before_the_agents_is_refused_with_a_message_that_says_why`, `mcp_contract::the_agent_cases_a01_to_a10`, `a_file_without_the_block_agents_has_no_field_and_refuses_it` |
| 3.8.20 | 5.1.0 | `docs/inbox/agents.md`, sections "Who is the agent" and "The grouping is not a sandbox", and `mcp_tools::tests::the_guide_names_the_codes_of_the_agents_and_says_the_grouping_is_not_a_sandbox` |
| 3.8.21 | 5.1.0 | `agents_tests::the_actions_are_for_the_owner_and_each_one_is_idempotent`, `merge_moves_items_and_intents_and_keeps_the_alias`, `merge_across_keys_refused`, `block_cancels_open_items_with_reason_agent_blocked_and_kills_intents`, `resolve_blocked_merged_follows_alias_one_hop`. `agents_web_e2e::rename_allow_block_unblock_merge_work_and_refuse_what_they_must`, `owner_sees_only_own_agents_and_forms_refuse_foreign_agents` (the web forms). `items_e2e::two_agents_of_one_key_with_real_tokens_reach_the_phones_and_agent_do_changes_them`: `rename`, `allow`, `block`, `unblock` and `merge` from two phones, with the tokens of the real channel. |
| 3.8.22 | 5.1.0 | `agents_tests::the_hints_are_sealed_updated_when_they_change_and_never_in_an_event`, `rekey_and_key_purge_visit_the_hints`, `sweeper_purges_only_unreferenced_idle_agents` |
| 3.8.23 | 5.1.0 | `agent_channel::who::hints_recorded`, `mcp_tools::tests::the_transport_gives_the_token_of_the_header_and_bounded_hints`, `mcp::tests::the_client_is_read_from_the_meta_of_the_new_line_then_from_initialize`, `mcp_contract::the_agent_cases_a01_to_a10` (A10, the log) |
| 3.8.24 | 5.1.0 | `agents::tests::the_candidates_of_a_text_are_the_windows_of_43_characters_of_the_alphabet`, `agent_channel::who::explanation_with_a_live_token_is_refused` (the prefix, bare, glued, the label, `say`, a second agent, a blocked agent, the REST door), `mcp_tools::tests::the_codes_of_the_agents_have_the_status_of_the_table_and_a_fixed_message` |
| 3.8.25 | 5.1.0 | `agents::tests::similar_names_a_known_agent_with_the_same_says_or_the_same_client_and_net`, `frames::tests::the_frame_of_a_new_agent_names_a_similar_known_agent_and_no_other_frame_does`, `agent_frames::a_new_agent_that_looks_like_a_known_one_carries_similar_and_nothing_merges`, `ui::agents::tests::the_notice_names_a_similar_agent_with_the_sentence_that_hints_can_be_copied` |
| 3.8.26 | 5.1.0 | `agents_tests::enroll_returns_the_token_once_and_the_database_keeps_only_the_digest`, `enroll_uses_the_name_rules_of_a_rename_and_the_ordinal`, `enroll_needs_the_mode_enrolled_and_the_cap_of_the_key_applies`, `a_call_without_a_valid_token_on_an_enrolled_key_is_refused_and_makes_nothing`, `enroll_and_reset_write_a_notice_for_the_phones_and_the_metrics_carry_no_secret`; `agents_web_e2e::enrolled_cycle_enroll_call_with_and_without_the_token_reset_old_token_dead_new_works` (known, no first-use notice, no lock); `items_e2e::an_enrolled_agent_reaches_the_phone_as_known_and_a_reset_raises_its_rev` |
| 3.8.27 | 5.1.0 | `agents_tests::reset_kills_the_old_token_at_once_raises_rev_and_the_event_has_no_secret`, `reset_refuses_the_bucket_a_call_agent_and_a_merged_agent`, `the_debug_text_of_an_enrolled_agent_hides_the_token`; `agents_web_e2e::enrolled_cycle_enroll_call_with_and_without_the_token_reset_old_token_dead_new_works` |
| 3.9.1 | 5.1.0 | `items_tests::prefs_override_file_defaults_never_a_cap`, `prefs_e2e::prefs_page_refused_for_impersonated_session`, `prefs_e2e::set_prefs_from_phone_synced_to_page` |
| 3.9.2 | 5.1.0 | `prefs_e2e::bad_timezone_name_refused_422_no_coercion`, `ui::prefs::tests::a_bad_value_is_the_error_with_its_field_and_nothing_is_changed`, `config::tests::the_digest_mode_decides_which_keys_apply` (a zone that the database does not hold) |
| 3.9.3 | 5.1.0 | `items_tests::digest_two_instances_one_frame`, `items_tests::digest_empty_not_sent`, `approval_api_tests::prefs::digest_row_becomes_a_frame_with_numbers_only` |
| 3.9.4 | 5.1.0 | `set_prefs_is_stored_and_comes_back_in_the_welcome`. `approval_api_tests::prefs::quiet_flag_set_in_window_request_still_delivered`, `prefs_e2e::quiet_flag_set_in_window_request_still_delivered`, `prefs_e2e::set_prefs_from_phone_synced_to_page` |
| 3.9.5 | 5.1.0 | `alert.AttentionTest` (the lock screen shows the count and no text) |
| 3.9.6 | 5.1.0 | `prefs_e2e::prefs_page_refused_for_impersonated_session` (the page has no row of delegation, escalation or muted agents) |
| 3.9.7 | 5.1.0 | `prefs::tests::zones_come_from_the_embedded_database_with_their_exact_name` (the crate `jiff` is not in `inbox-core`, so the phone build does not use it) |
| 3.9.8 | 5.1.0 | `items_tests::daily_digest_once_at_local_time_across_dst_change`, `items_tests::digest_every_n_hours`, `prefs::tests::a_daily_due_time_follows_the_local_clock_across_the_spring_change`, `prefs::tests::a_time_inside_the_gap_is_due_after_the_gap_and_a_time_that_repeats_is_due_once`, `prefs::tests::a_due_time_that_is_too_old_is_not_sent` |
| 3.10.1 | 5.1.0 | `frames::every_frame_rejects_unknown_field`, `invalid/client/answer-with-signature.json` in `frames::invalid_frames_are_refused` |
| 3.10.2 | 5.1.0 | `config_risk_key_is_refused_and_no_document_depends_on_a_risk_level` |
| 3.10.3 | 5.1.0 | `approval_api_tests::no_phone_409`, `approval_api_tests::mute_after_three_refusals_alerts_owner` (the owner is the only recipient). The phone and the web tests of 3.7.12 add the screens |
| 3.10.4 | 5.1.0 | `approval_api_tests::pep_observation_unit`. `agent_channel::who::service_never_sees_agent_token_agent_label_or_hints`. `tools/dev/pep-e2e.sh` with the real reference PEP: `pep_e2e::pep_end_to_end` and the scan of the logs of the containers `service`, `tap` and `sock` |
| 3.10.5 | 5.1.0 | `content_box::tests::the_documents_say_what_the_seal_covers_and_does_not_cover` |
| 3.10.6 | 5.1.0 | `items_web_e2e::xss_bidi_control_zero_width_rendered_visible`, `ui::markdown_html::tests` |
| 3.11.1 | 5.2.0 | `agent_channel::questions::ask_makes_a_pending_question_and_the_answer_reaches_the_agent_in_the_documented_form`, `the_tool_ask_needs_the_block_and_the_permission_and_tools_list_follows`, `the_rest_routes_of_the_questions_make_read_renew_replace_and_withdraw`; `questions_e2e::the_phone_gets_a_question_and_answers_a_choice_a_custom_text_a_decline_and_asks_back` |
| 3.11.2 | 5.2.0 | `questions::tests::flags_matrix_all_valid_combinations_per_type`, `invalid_flag_combinations_422`; `agent_channel::questions::invalid_questions_are_refused_with_their_codes_and_create_nothing`, `multiple_with_custom_is_exclusive` |
| 3.11.3 | 5.2.0 | `questions::tests::one_shot_with_any_flag_interaction_not_allowed`; `agent_channel::questions::one_shot_key_asks_a_closed_choice_and_gets_interaction_not_allowed_for_a_flag`, `reply_refused_for_one_shot`, `jev_style_rest_loop_get_wait_keeps_lease_600s`; `questions_e2e::a_one_shot_question_is_closed_on_the_phone_and_a_reply_is_refused` |
| 3.11.4 | 5.2.0 | `questions::tests::command_with_bidi_or_control_refused`, `command_zero_width_accepted_and_flagged_in_frame`, `the_limits_refuse_and_never_cut`, `the_defaults_of_the_bounds_and_the_body_form` |
| 3.11.5 | 5.2.0 | `agent_channel::questions::text_answer_arrives_as_choice_empty_plus_untrusted_text`, `a_form_answer_is_checked_and_reaches_the_agent_inside_the_answer`, `ask_makes_a_pending_question_and_the_answer_reaches_the_agent_in_the_documented_form` |
| 3.11.6 | 5.2.0 | `agent_channel::questions::idempotency_conflict_detected`, `a_repeat_of_an_answered_question_gives_the_answer`, `ask_blocks_at_most_wait_s_then_returns_handle` |
| 3.11.7 | 5.2.0 | `agent_channel::questions::ask_with_allow_ask_back_runs_the_thread_replied_say_pending`; `questions_e2e::the_phone_gets_a_question_and_answers_a_choice_a_custom_text_a_decline_and_asks_back` |
| 3.11.8 | 5.2.0 | `agent_channel::questions::question_budget_counts_ask_say_supersede_not_renewals`, `a_cancel_needs_the_permission_and_a_withdrawn_question_is_cancelled`; `portal_actions::tests::the_document_has_the_keys_and_the_inbox_in_one_scope`, `the_actions_that_a_key_can_hold_follow_its_kind` |
| 3.11.9 | 5.2.0 | `agent_channel::questions::token_in_text_refused_for_every_question_field` |
| 3.11.10 | 5.2.0 | `agent_channel::questions::two_agents_of_one_key_ask_and_write_only_on_their_own_questions` |
| 3.11.11 | 5.2.0 | `questions::tests::the_check_of_an_answer_follows_the_input_and_the_flags`, `a_form_answer_goes_through_the_form_validator`; `agent_channel::questions::a_text_without_allow_custom_and_a_wrong_answer_are_refused_and_write_nothing`; `questions_e2e::answers_that_do_not_fit_the_question_get_a_result_and_change_nothing` |
| 3.11.12 | 5.2.0 | `agent_channel::questions::question_never_passes_through_a_service`; `questions_e2e::canary_for_question_bodies` |
| 3.11.13 | 5.2.0 | `agent_channel::questions::the_tool_ask_needs_the_block_and_the_permission_and_tools_list_follows`, `one_shot_key_asks_a_closed_choice_and_gets_interaction_not_allowed_for_a_flag` |
| 3.11.14 | 5.2.0 | `config::tests::questions_block_closed_schema_and_defaults`, `questions_block_needs_items_and_at_rest`, `the_configuration_reference_in_the_docs_is_the_generated_one` |
| 4.9.2a | 5.1.0 | `degraded_storage_returns_storage_degraded_and_writes_nothing` |
| 4.11.1 | 5.1.0 | `an_item_reaches_each_phone_of_its_owner_that_is_not_revoked` |
| 4.11.2 | 5.1.0 | `phone_enrolled_after_creation_receives_open_items`, `offline_phone_receives_request_and_settled_on_reconnect`, `answer_after_abandon_gets_abandoned_and_settled` |
| 4.11.3 | 5.1.0 | `no_plaintext_in_alerts_table_after_cycle`, `frames::tests::an_approval_frame_carries_the_command_and_verifies` |
| 4.11.4 | 5.1.0 | `ack_by_id_stops_retransmission`, `data.ItemEngineTest` (Kotlin). The order store then `ack` has no separate test. |
| 4.11.5 | 5.1.0 | `approve_on_phone_then_pdp_true_once` (the same `client_msg_id` again), `ack_by_id_stops_retransmission` |
| 4.11.6 | 5.1.0 | `two_phones_race_one_accepted_other_settled`, `offline_phone_receives_request_and_settled_on_reconnect` |
| 4.11.7 | 5.1.0 | `effect_mismatch_refused`, `revoked_device_answer_refused_4403_and_no_state_change`, `custom_and_ask_back_are_refused_without_the_flags`, `answer_after_abandon_gets_abandoned_and_settled` |
| 4.11.8 | 5.1.0 | `offline_phone_receives_request_and_settled_on_reconnect`, `two_phones_race_one_accepted_other_settled` |
| 4.11.9 | 5.1.0 | `sync_reconciles_open_and_closed`, `phone_enrolled_after_creation_receives_open_items` |
| 4.11.10 | 5.1.0 | The answer of a phone calls `Items::answer`, which waits for the copy of the replica before it returns (test of `canary_absent_in_every_column_and_in_replica_snapshot`). No separate test |
| 5.11.1 | 5.1.0 | `the_documents_say_what_the_seal_covers_and_does_not_cover` |
| 5.11.2 | 5.1.0 | `canary_absent_in_every_column_and_in_replica_snapshot` |
| 5.11.3 | 5.1.0 | `seal_open_round_trip`, `aad_binds_request_field_seq_and_effect` |
| 5.11.4 | 5.1.0 | `moved_sealed_value_does_not_open`; `a_sealed_value_that_moves_does_not_open` |
| 5.11.5 | 5.1.0 | `canary_absent_in_every_column_and_in_replica_snapshot`, `an_item_reaches_each_phone_of_its_owner_that_is_not_revoked` |
| 5.11.6 | 5.1.0 | `plain_secret_in_file_stops_start`, `the_blocks_items_and_at_rest_give_typed_values_with_documented_defaults`, `two_keys_active_writes_all_read`, `active_key_unknown_stops_start` |
| 5.11.7 | 5.1.0 | `active_key_unknown_stops_start`, `duplicate_key_id_or_secret_stops_start`, `two_ids_with_one_key_stop_the_start`, `secret_not_32_bytes_or_all_equal_stops_start`, `wrong_key_same_id_fails_check_value`, `wrong_key_same_id_stops_the_start_and_the_log_names_the_keys`, `a_key_id_that_the_file_lost_is_reported_with_its_count`; `orphan_body_kid_stops_start_with_count`. `rekey_tests::removed_key_with_rows_stops_start_until_rekey_complete`, `items_e2e::rekey_command_rotates_the_content_key_with_the_binary` (the message names `rekey` and `content-purge`) |
| 5.11.8 | 5.1.0 | `content_key_command_never_overwrites`; `orphan_body_kid_stops_start_with_count` (the function behind `content-purge`) |
| 5.11.9 | 5.2.0 | `rekey_tests::rekey_moves_rows_to_active_key_resumes_after_kill` (the four tables, a stop in the middle, a second run), `rekey_tests::rekey_refuses_sqlite_while_running`, `rekey_tests::rekey_from_one_key_leaves_the_others_and_refuses_bad_ids`, `items_e2e::rekey_command_rotates_the_content_key_with_the_binary` (the binary: refusal with SQLite, online with PostgreSQL) |
| 5.11.10 | 5.1.0 | `body_purge_keeps_metadata_effect_and_events` |
| 5.11.11 | 5.1.0 | `items_load::canary_for_question_bodies_in_every_place` (log, metrics, tables with the alerts, errors, the PDP and the requests of the portal to a service), `questions_e2e::canary_for_question_bodies` |
| 5.11.12 | 5.1.0 | `the_documents_say_what_the_seal_covers_and_does_not_cover` |
| 5.11.13 | 5.1.0 | `the_documents_say_what_the_seal_covers_and_does_not_cover` |
| 5.11.14 | 5.1.0 | `data.ShownEffectTest`, `data.ItemEngineTest` (Kotlin unit tests of the shown effect and its hash) |
| 5.11.15 | 5.1.0 | `items_web_e2e::admin_body_route_404_when_switch_false` |
| 5.11.16 | 5.1.0 | `items_web_e2e::admin_body_read_true_superadmin_only_writes_audit_and_counter_no_body`, `approval_read_right_shows_metadata_not_body`, `impersonated_session_cannot_answer` |
| 5.11.17 | 5.1.0 | `items_web_e2e::admin_body_read_true_superadmin_only_writes_audit_and_counter_no_body` |
| 5.11.18 | 5.2.0 | `rekey_tests::rekey_moves_rows_to_active_key_resumes_after_kill`, `rekey_tests::rekey_does_not_overwrite_a_row_that_a_writer_changed` |
| 5.11.19 | 5.2.0 | `rekey_tests::rekey_reports_a_row_that_does_not_open_and_leaves_it`, `rekey_tests::the_report_says_what_moved_and_what_is_left`, `items_e2e::rekey_command_rotates_the_content_key_with_the_binary` (the report holds no content) |
| 5.11.20 | 5.2.0 | `rekey_tests::removed_key_with_rows_stops_start_until_rekey_complete` (the rows that wait), `items_e2e::rekey_command_rotates_the_content_key_with_the_binary` (the log event), `pdp::approval::tests::an_evaluation_id_survives_a_rotation_of_the_content_key_while_the_old_key_is_in_the_file` |
| 3.6.26 | 5.1.0 | `frames::markdown_and_language_members_are_kept_and_plain_is_a_valid_spelling`, `frames::new_invalid_frames_fail_for_the_named_reason`, `frames::tests::a_markdown_turn_of_the_agent_keeps_its_format`, `agent_channel::the_format_and_the_language_of_an_intent_reach_the_frame_of_the_phone`, `agent_channel::a_turn_of_the_agent_can_be_markdown_and_the_message_frame_says_so`, `FramesTest.theMarkdownAndLanguageMembersReadToTheTypedValues` |
| 3.6.27 | 5.1.0 | `render::markdown::tests::limits_make_the_text_plain`, `render::markdown::tests::autolinks_html_and_images_are_text`, `render::markdown::tests::tables_footnotes_and_rules_are_text`, `render::markdown::tests::entities_are_text`, `render::markdown::tests::random_markdown_never_panics_and_always_gives_a_tree`, `render::markdown::tests::nested_structures_keep_their_shape`, `markdown_html::the_xss_corpus_gives_only_allowed_tags_and_shows_the_source_as_text`, `markdown_html::bombs_and_long_texts_stay_inside_the_subset` |
| 3.6.28 | 5.1.0 | `render::markdown::tests::links_show_the_host_and_never_act`, `markdown_html::the_xss_corpus_gives_only_allowed_tags_and_shows_the_source_as_text`, `markdown_html::no_class_comes_from_the_text`, `markdown_html::bidi_and_zero_width_characters_are_visible_marks` |
| 3.6.29 | 5.2.0 | `frames::new_invalid_frames_fail_for_the_named_reason`, `frames::the_largest_form_fits_one_noise_message`, `frames::tests::a_stored_body_that_breaks_a_rule_of_the_form_gives_no_frame`, `FramesTest.theFormRulesAreRefusedWithTheNamedReason`, `FramesTest.theLargestFormFitsOneNoiseMessage` |
| 3.6.30 | 5.2.0 | `form::tests::custom_text_needs_allow_custom_and_is_exclusive`, `frames::tests::a_stored_body_that_breaks_a_rule_of_the_form_gives_no_frame` (a form on a `one_shot` key) |
| 3.6.31 | 5.2.0 | `form::tests::table_of_refusals`, `form::tests::a_complete_answer_passes`, `frames::tests::the_owner_answer_holds_the_answers_of_a_form`, `FramesTest.theGoldenAnswerOfAFormReadsAndWritesBack`. The engine calls the check: `question_tests::invalid_questions_are_refused_with_their_codes_and_create_nothing` |
| 3.6.32 | 5.2.0 | `agent_channel::get_answer_returns_the_answers_of_a_form_in_untrusted` |
| 3.6.33 | 5.1.0 | `render::highlight::tests` (one test for each language, `unterminated_constructs_run_to_the_end`, `random_text_never_panics_and_spans_are_in_bounds`, `work_is_linear_on_hostile_input`), `markdown_html::a_command_keeps_its_characters_and_gets_spans`, `bridge::tests::markdown_and_highlight_bridges`, `lib::tests::each_native_function_of_the_app_has_its_jni_symbol` |
| 5.12.1 | 5.1.0 | `items_web_e2e::command_above_explanation_and_explanation_labelled_unverified`; `tools/dev/ui-check-items.js` |
| 5.12.2 | 5.1.0 | `items_web_e2e::xss_bidi_control_zero_width_rendered_visible`, `ui::markdown_html::tests::a_command_has_a_span_for_each_line_and_marks_the_hidden_and_look_alike_characters` |
| 5.12.3 | 5.1.0 | `items_web_e2e::xss_bidi_control_zero_width_rendered_visible`, `ui::items::tests::the_explanation_folds_after_400_characters_and_markdown_has_its_tag` |
| 5.12.4 | 5.2.0 | `questions_web_e2e::secret_warning_present` (web). The screens of the phone have no separate test. |
| 5.12.6 | 5.1.0 | `agents::tests::says_with_hidden_characters_is_refused_and_equal_to_a_name_is_flagged`, `agents_tests::the_hints_are_sealed_updated_when_they_change_and_never_in_an_event`. `agent_channel::who::label_injection_refused`, `hints_recorded`, `mcp_contract::the_agent_cases_a01_to_a10` (A05). `ui::agents::tests::the_text_of_the_agent_is_in_the_dashed_box_escaped_and_marked`, `agents_web_e2e::hints_are_escaped_and_label_injection_is_rendered_as_text`, `a_text_that_folds_to_the_name_of_a_known_agent_is_flagged`. `ui.AgentBoxesTest.aLookAlikeLetterAndAHiddenCharacterShowAsMarks`, `ui.NewAgentCardTest.aTextThatEqualsTheNameOfAKnownAgentOfTheKeyGivesTheCopyWarning`, `aLookAlikeLetterGivesTheLookAlikeWarning`, `theTextOfTheClientIsNeverCopiedIntoAWarningOrATitle` |
| 6.9.1 | 5.1.0 | `no_default_for_mandatory_keys_message_names_key_range_value`, `the_blocks_items_and_at_rest_give_typed_values_with_documented_defaults` |
| 6.9.2 | 5.1.0 | `the_blocks_items_and_at_rest_give_typed_values_with_documented_defaults`, `no_default_for_mandatory_keys_message_names_key_range_value` |
| 6.9.3 | 5.1.0 | `the_block_approvals_gives_typed_values_with_documented_defaults`, `a_contradiction_of_the_approvals_stops_the_start_with_the_key_the_range_and_the_value` (`consume_within`, `evaluation_ttl`, `deny_cooldown`, the blocks `pdp` and `keys.mcp`). `config::tests::the_keys_of_the_approval_api_have_the_documented_defaults_and_the_set_values_apply`, `a_bad_key_of_the_approval_api_or_a_contradiction_stops_the_start_with_the_key_the_range_and_the_value` |
| 6.9.4 | 5.3.0 | `config_confirm_is_a_member_of_a_grant_object_with_the_default_false`. `the_removed_key_approvals_of_a_pdp_client_stops_the_start_with_its_name` |
| 6.9.5 | 5.1.0 | `a_contradiction_of_the_items_stops_the_start_with_the_two_keys_and_values`; `a_contradiction_of_the_approvals_stops_the_start_with_the_key_the_range_and_the_value` (`consume_within` above `max_lifetime` cannot happen with today's ranges: 1 hour at most against 1 day at least) |
| 6.9.6 | 5.1.0 | `the_digest_mode_decides_which_keys_apply`, `the_blocks_items_and_at_rest_need_each_other`. `config::tests::a_bad_key_of_the_approval_api_or_a_contradiction_stops_the_start_with_the_key_the_range_and_the_value` (`max_command_bytes` above `pdp.max_body_bytes`, `require_explanation` with `max_explanation_chars = 0`). `config::tests::a_bad_key_of_the_block_mcp_or_a_contradiction_stops_the_start_with_the_key_the_range_and_the_value` (`inbox.mcp` without `keys.mcp = true`, `max_wait` above `wait_max`, the path of the metrics) |
| 6.9.7 | 5.1.0 | `print_config_masks_secret_shows_kid_and_source`, `plain_secret_in_file_stops_start` |
| 6.9.8 | 5.2.0 | `config::tests::questions_block_closed_schema_and_defaults`, `config::tests::questions_block_needs_items_and_at_rest` |
| 6.9.9 | 5.1.0 | `config::tests::agents_block_closed_schema_and_defaults`, `idle_after_above_purge_after_stops_the_start`, `agents_block_needs_items_and_at_rest` |
| 6.10.1 | 5.1.0 | `old_hello_protocol_4_gets_4426`, `frames::old_hello_and_old_ack_do_not_parse` |
| 6.10.2 | 5.1.0 | `frames::frames_round_trip_golden`, `frames::every_frame_rejects_unknown_field`, `frames::title_with_newline_or_control_rejected`. `approve_on_phone_then_pdp_true_once`, `sync_reconciles_open_and_closed`, `frames::tests::a_digest_has_the_count_and_the_oldest_first_and_never_comes_empty` |
| 6.10.3 | 5.1.0 | `frames::request_fits_one_noise_message_at_max_sizes`, `frames::encode_refuses_a_frame_above_one_noise_message` |
| 6.10.4 | 5.1.0 | `old_inbox_schema_stops_start`. `agents_tests::base_database_without_agents_table_stops_the_start` |
| 6.10.5 | 5.1.0 | `frames::request_from_has_agent_members_and_refuses_unknown`, `agent_label_limits_are_exact_and_it_refuses_hidden_characters`, `frames::tests::the_line_from_never_holds_text_of_an_agent`, `the_text_that_an_agent_says_reaches_the_frame_only_when_the_frame_carries_it`, `agent_frames::the_agent_frame_goes_to_each_device_of_the_owner_and_to_nobody_else`, `items_e2e::requests_name_their_agent_and_sync_brings_the_agents_the_phone_lacks`. Kotlin: `FramesTest.theRequestFromHasTheAgentMembersAndRefusesUnknown`, `theTextThatAnAgentSaysIsOneLineWithoutHiddenCharacters`. `ui.AgentGroupsTest.anAgentThatThePhoneDoesNotHoldUsesTheFrameOfTheItem`, `aRenameChangesTheNameOfTheGroupAtOnce`, `data.AgentTest.theLabelComesFromTheTableAndARenameShowsOnOldItemsAtOnce` |
| 6.10.6 | 5.1.0 | `frames::agent_frame_roundtrip_values_and_rules`, `agent_text_and_number_limits_are_exact`, `agent_invalid_frames_fail_for_the_named_reason`, `frames::tests::the_agent_frame_is_portal_text_and_the_hints_of_the_row`, `agent_frames::a_change_raises_the_rev_and_the_phone_gets_one_frame_with_the_state_of_now`, `a_merge_sends_the_frame_of_the_agent_and_the_frame_of_the_target`, `items_e2e::agent_do_changes_the_agent_for_every_phone_of_the_owner_and_each_refusal_changes_nothing`. Kotlin: `FramesTest.theAgentFramesReadToTheTypedValues`, `theAgentFrameRulesAndLimitsAreExact`, `theLargestAgentFrameFitsOneNoiseMessage`, `theAgentFixturesAreRefusedWithTheNamedRule`. `data.AgentTest.aLowerRevIsIgnoredButStillAcknowledged`, `theSameDeliveryTwiceChangesNothingAndIsAcknowledgedAgain`, `twoPortalsHoldAgentsWithTheSameIdApart`. `agent_frames::a_new_agent_that_looks_like_a_known_one_carries_similar_and_nothing_merges`, `items_e2e::two_agents_of_one_key_with_real_tokens_reach_the_phones_and_agent_do_changes_them` |
| 6.10.7 | 5.1.0 | `frames::sync_with_agents`, `agent_frames::sync_brings_the_agents_that_the_phone_lacks`, `items_e2e::requests_name_their_agent_and_sync_brings_the_agents_the_phone_lacks`. Kotlin: `FramesTest.syncHasTheAgentsThatTheDeviceHoldsAndTheLimitsAreExact`. `data.AgentTest.theSyncFrameListsTheAgentsThatThePhoneHoldsWithTheirRev`, `theSyncListHasAtMost200AgentsTheLiveOnesAndTheRecentOnesFirst` |
| 6.10.8 | 5.1.0 | `frames::agent_do_ops_and_limits`, `agent_result_status_closed`, `agent_frames::agent_do_answers_with_a_closed_list_of_statuses_and_never_for_another_owner`, `agent_do_without_the_block_agents_is_not_enabled`, `items_e2e::a_malformed_agent_do_closes_the_session`, `without_the_block_agents_the_request_names_the_bucket_and_agent_do_is_not_enabled`. Kotlin: `FramesTest.agentDoHasFiveOpsAndEachTakesItsMembers`, `theAgentResultStatusIsClosedAndTheFrameHasNoDeliveryId`. `data.AgentActionsTest.theResultClosesTheEntryAndGivesTheEventWithTheLine`, `everyStatusOfTheClosedListHasItsOwnLine`, `anActionThatTheStateDoesNotAllowIsRefusedBeforeAnyFrame` (no biometric check: `AgentCommands` has no gate) |
| 6.10.9 | 5.1.0 | `frames::agent_locked_is_a_status_of_an_answer`, `agent_frames::lock_new_false_the_default_gives_the_frame_without_the_lock_and_the_same_fields`, `items_e2e::a_new_agent_is_locked_for_accept_until_the_owner_allows_it`, `a_new_agent_is_not_locked_when_lock_new_is_false`, `a_phone_of_another_account_learns_nothing_of_the_lock`. `data.AgentActionsTest.theLockedAnswerKeepsTheItemOpenWithAClearText`, `ui.AgentBoxesTest.approveIsLockedForANewAgentAndDeclineStaysOn` |
| 6.11.1 | 5.1.0 | `the_journal_and_the_metrics_follow_the_names_of_the_specification` |
| 6.11.2 | 5.1.0 | `ninety_days_are_simulated_without_a_pause_and_supersede_does_not_extend`; each test of `items_tests.rs` runs on SQLite and on PostgreSQL |
| 6.11.3 | 5.1.0 | `canary_absent_in_every_column_and_in_replica_snapshot`, `items_load::canary_for_question_bodies_in_every_place`, `questions_e2e::canary_for_question_bodies` |
| 6.11.4 | 5.1.0 | `pep_e2e::pep_end_to_end` (the service records its input; `tools/dev/pep-e2e.sh`) |
| 8.2.1 | 5.1.0 | Scope rule. The tests of 3.6 to 3.10 cover each part. |
| 8.2.2 | 5.2.0 | Scope rule. The tests of 3.6.21 to 3.6.25, 3.7.60 to 3.7.78 and 5.11.9 to 5.11.20 cover the parts. The abuse and load tests are `items_load`. |
| 8.2.3 | 5.1.0 | Documentation rule. No test. |
| 8.2.4 | 5.1.0 | Scope rule. No test. |
| 8.3.1 | 5.1.0 | Scope rule. No test: the code has no such feature. |
| 8.3.2 | 5.1.0 | Scope rule. No test: the code has no such feature. |
| 8.3.3 | 5.1.0 | Scope rule. No test: the code has no such feature. |
| 8.3.4 | 5.1.0 | Scope rule. No test: the code has no such feature. |
| 8.3.5 | 5.1.0 | Scope rule. No test: the code has no such feature. |


## 7. Integration

7.1 Grafana: a contact point of the type Webhook sends to `POST /api/v1/webhooks/alertmanager/<group>` with a bearer `api_` key. [`grafana/contact-point.yaml`](grafana/contact-point.yaml) is an example.

7.2 Alertmanager: a `webhook_configs` receiver does the same. [`alertmanager.yml`](alertmanager.yml) is an example.

## 8. Scope and status

The Inbox has two groups of features. The alerts exist since version 5.0.0. The items (approvals and questions) exist since the releases 5.1.0 to 5.5.1.

Release 5.1.0 has the approvals and the discussion. Release 5.2.0 has the questions. Release 5.3.0 gives the approval to each service. The releases 5.4.0 to 5.5.1 align the portal with the [ProcurAuth specification](https://procurauth.github.io/procurauth/specification.html). The requirements of the items are in 3.6 to 3.10, 4.11, 5.11 and 5.12.

### 8.1 Alerts from agents (not implemented)

An agent can send alerts today with an `api_` key of its own, through the route of 3.3, as any sender does. The requirements 8.1.2 and 8.1.3 are not implemented.

8.1.1 An agent MUST send alerts through the REST route of 3.3, with an `api_` key of its own. No new code is needed: the route of 3.3 already accepts the key.

8.1.2 The MCP side MAY accept an alert from an `mcp_` key. A tool `send_alert` would map to the action `alert.send`. Status: not implemented in 5.6.1.

8.1.3 The zone **Sent** SHOULD show a webhook icon for an `api_` key and a robot icon for an `mcp_` key. Status: not implemented in 5.6.1.

### 8.2 Items: approvals and questions (implemented)

8.2.1 **[5.1.0]** Release 5.1.0 MUST hold the grant `confirm`, the approvals and the discussion. It MUST hold the agent channel with REST and `/mcp`, the sealed bodies, the preferences, the web pages and the phone screens. The key `interaction` and the variant `one_shot` MUST be in the same release.

8.2.2 **[5.2.0]** Release 5.2.0 MUST hold the questions, their web rows, the screens of the phone for questions, and the command `rekey`. It MUST add the actions `question.ask`, `question.read` and `question.cancel`, and the budget `questions`.

8.2.3 **[5.1.0]** The server and the app MUST ship together, because protocol 5 breaks protocol 4. The section "Upgrades" of [Operations](../operations.md#upgrades) MUST say three things. A new inbox database is needed. The app must be installed again. A new content key must live outside the data volume and outside the backup.

8.2.4 **[5.1.0]** The frames of protocol 5 are the frames of 6.10. A timeout never means deny: 3.6.9 and 3.6.10 give the rule. The portal has no actions `approval.request` and `approval.decide`, and no resource type `inbox_user`.

### 8.3 Out of scope

8.3.1 **[5.1.0]** The Inbox MUST NOT have end-to-end encryption, device keys, a key directory or browser keys.

8.3.2 **[5.1.0]** The Inbox MUST NOT send an item to a recipient other than the owner. This excludes named accounts, groups, `question.ask_user`, `inbox_user` and `inbox_group` for items. It also excludes two-person rules, explicit approvers, temporary delegation and escalation to a group.

8.3.3 **[5.1.0]** An approval MUST NOT give a right that stays. An approval for a person on a web page of a service is out of scope.

8.3.4 **[5.1.0]** The Inbox MUST NOT have a risk class or a signed proof from the phone.

8.3.5 **[5.1.0]** The Inbox MUST NOT support iOS, a push service of a third party, or an approval by elicitation of the MCP client.

### 8.4 Not implemented

The requirements of this section describe possible features. No release has them.

8.4.1 The server MAY drive the execution with MCP Tasks and the URL mode of elicitation. Status: not implemented in 5.6.1.

8.4.2 The Inbox MAY offer parallel alternatives and a floor `approval: "required"`. Status: not implemented in 5.6.1.

8.4.3 The items MAY get attachments and images. The content keys MAY come from a KMS or an HSM. Status: not implemented in 5.6.1.

8.4.4 Delegation, escalation, muted agents, a quorum and a vote are not part of the product. A webhook to the agent and an adapter for an agent SDK are not part of it either. Status: not implemented in 5.6.1.
