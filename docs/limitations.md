# Limitations

This page lists what portal does not do, and the limits that its design sets. Each item gives the fact, its consequence and a way to work with it. A limit that is only a default value is in [Configuration](configuration.md), not here.

## Platforms and transport

- **The Inbox is for Android only.** iOS cannot keep a connection in the background without the push service of Apple. The portal uses no push service of a third party. Use the browser or a sender that your platform supports.
- **The portal has no TLS.** Its listeners speak plain HTTP. Noise protects the session of a phone. An API key travels in clear text on plain HTTP. Put a TLS proxy in front of the portal for the senders, and use an HTTPS `inbox.devices.public_address`. See [Traefik and TLS](traefik.md).
- **The Android app accepts a plain `http://` address.** It also accepts an `https://` address, and it checks the certificate with the trust store of the phone. The app has no network security configuration and no certificate code of its own. The manifest allows clear text for each address, because the portal is often on a private network. Enroll a phone only with an HTTPS address when the path crosses an untrusted network.
- **The release has no container image.** It has static binaries for amd64 and arm64, and the APK. No image is published. Build the image on your host with `tools/build.sh image`. See [Build and release](build-and-release.md).

## Several instances and counters

- **With SQLite, only one instance is active.** A standby needs the block `inbox.replica`. For active instances side by side, use PostgreSQL. See [Several instances](several-instances.md).
- **Each instance has its own rate limits and its own counter of refused sign-ins.** With N instances, a client has up to N times the budget. A burst on several instances can stay under the threshold of the alert of the portal. The counters of the wrong second-factor codes of an account are in the database, so they are shared.
- **An alert that the Inbox accepts while the database has no writable primary exists only in memory.** The instances copy it to each other. If every instance that holds it stops before the database is back, the Inbox loses the alert. The sender already got `201`. See [Inbox](inbox/README.md).

## Authentication

- **The second factor is a TOTP code.** A person can give the code to a false page, as a password. The portal has no WebAuthn (passkeys), which stops this. The portal also has no fresh second factor before a sensitive action, such as the creation of a key or the impersonation of an account.
- **An impersonated session can make an enrollment code for the account that it shows.** Only the superadmin opens such a session, and the superadmin can already see the account. Keep the credentials of the superadmin as strict as an account that holds every right.

## Inbox

- **The phone settings `alarm`, `ring` and `notify` are local to the phone.** The server does not know them. It cannot show or change them.
- **The enrollment code is text.** The page shows no QR code.
- **The permissions `inbox.receive`, `alert.send` and `alert.read` are given one group at a time.** No permission covers all groups. Give the permission for each group.
- **Alerts are one way.** No person acknowledges an alert. Agents, questions and approvals use the items of the Inbox (see [Inbox: agents](inbox/agents.md)). An `mcp_` key cannot send an alert, and there is no tool `send_alert`.

## Settings pages

- **The pages that draw the matrices need script.** Without script, the pages list the grants as text. The same holds for the editor of grants.
- **The count of the states in the zone Sent comes from the 500 newest alerts of the period.** The count shows `500+` when older alerts exist. The graph counts every alert.
