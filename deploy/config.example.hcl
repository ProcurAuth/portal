# Configuration of the portal with the inbox. Check it with:  portal check-config --config <file>
#
# The inbox starts when the file has a block `inbox`. The delivery values have no default: the
# program stops at start if one is missing, misspelled or out of range. Durations accept the
# units ms, s, m, h and d. Secrets come from a file (`_file`) or from a variable (`_env`).
# The keys of the portal that this example does not set have documented defaults: see the
# configuration reference in docs/configuration.md.

server {
  # The portal: the pages (behind Traefik ForwardAuth where needed), and the inbox: the API for the senders and
  # the webhooks (/api/v1/*) and the routes of the phones (/v1/enroll, /v1/session). The routes of the inbox have no
  # ForwardAuth. The account pages make the enrollment codes.
  listen_address  = "0.0.0.0:4181"
  # The name of this instance. The app shows it as the name of the server. The browser tab shows it at the end of the
  # title of the pages after sign-in. No page body shows it.
  name            = "Portal"
  # Peers whose X-Forwarded-For header the portal trusts, for the pages, the senders and the phones. Empty: none.
  trusted_proxies = ["127.0.0.1/32"]
}

authentication {
  superadmin_login_file    = "/run/secrets/superadmin_login"
  superadmin_password_file = "/run/secrets/superadmin_password"

  # The second factor (TOTP, RFC 6238). "off": no second factor. "optional" (default): each account chooses to turn it on
  # in the page Security. "required": each account, the superadmin too, must enrol at the next sign-in.
  # Any value other than "off" needs the key that encrypts the TOTP secrets in the database: 64 hexadecimal
  # characters, made with `openssl rand -hex 32`. All instances use the same key.
  # mfa = "optional"
  totp_key_file = "/run/secrets/totp_key"
}

session {
  secret_mode = "provided"
  secret_file = "/run/secrets/session_secret"

  # A session ends this time after the sign-in, also when it is in use (default "24h").
  # ttl = "24h"
  # The box "Remember me": a session with the box lives for remember_ttl and has a persistent
  # cookie. With remember = false, the form has no box. A change of the client address ends the use
  # of a session when bind is "ip_user_agent" (default). "user_agent" is the usual choice for phones.
  # remember              = true
  # remember_ttl          = "30d"
  # remember_idle_timeout = "7d"
}

# "hybrid": the settings pages give the grants of the accounts of the database, and the blocks `user` of
# this file give the grants of the accounts of the file. The account "grafana" owns an API key.
access {
  grants_mode = "hybrid"
}

# Keys. The kind `api` is for services, webhooks and scripts: the API of the inbox accepts it. It needs the
# block `inbox`. The kind `mcp` is for agents: the key edge accepts it. It needs the blocks `identity` and
# `pdp`, so this example turns it off. A block with no kind on stops the program.
keys {
  mcp = false
  api = true
}

# The account of the file that owns the API key "grafana". A key follows its owner: it can do only what the
# key and its owner can both do. The owner needs `keys.own.use` on the kind, and `alert.send` on each group.
# To let a key alert an account, give the owner `alert.send_user on service:portal` and set `direct_alerts = true`.
# A grant of a service can ask for an approval before each use: write { grant = "<action> on service:<id>", confirm = true }.
# A grant of the service `portal` or of the action `access` cannot do this.
user "grafana" {
  grants = [
    "keys.own.use on service:portal/key_kind:api",
    "alert.send on service:portal/inbox_group:infra",
  ]
}

# An external service: a tile of the launcher that opens a link. The server never requests the link and does not protect
# it. The label is the display name, and the id is `ext-` and its slug: "Team wiki" gives `ext-team-wiki`. An account sees
# the tile when it holds `access on service:ext-team-wiki`: give it in a block `user`, or in the settings pages.
# href is mandatory: an absolute http or https URL, with a DNS name, an IPv4 address or an IPv6 address in square brackets as
# its host. A link that does not follow this rule stops the start. description is optional (200 characters at most).
# The own icon is optional. Give at most one of `icon` (a data URI) and `icon_path` (the path of a PNG, JPEG, WebP, GIF or
# SVG file of 32 KiB at most, which the server reads at start). The server serves the image itself, to a signed-in session.
# A service of the file wins over a service of the database with the same id, and the settings pages cannot change it.
external_service "Team wiki" {
  href        = "https://wiki.example.org/"
  description = "The wiki of the team"
  # icon_path = "/run/secrets/wiki_icon.svg"
  # icon      = "data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciLz4="
}

# SQLite. Instead of this block, the block `postgres` below lets several active instances share one database.
storage {
  database = "/var/lib/portal/portal.db"
}

ui {
  # The icon of the browser tab. "default" (default): the built-in logo, on the pages after sign-in only.
  # "none": no page after sign-in links an icon, and the icon route gives the answer of an unknown path. A page
  # before sign-in never has an icon: it has the empty icon.
  # favicon = "default"

  # true (default): a tile of the launcher opens its service in a new browser tab. false: the same tab.
  # The value applies to every service.
  # services_new_tab = true
}

logging {
  level = "info"
}

inbox {
  # Made once with `portal inbox keygen --out FILE`. Replacing it invalidates every phone and every enrollment code.
  server_key_file = "/run/secrets/inbox_server_key"

  devices {
    # The public base URL of the portal, as the phones reach it from outside. It is the `s` of the enrollment
    # code: host:port, a domain, or an http(s) URL with an optional path prefix. The phones add /v1/enroll and
    # /v1/session. The portal has no base path: a proxy that publishes it under a prefix removes the prefix.
    # The client address of a phone comes from server.trusted_proxies.
    public_address  = "inbox.example.org"
    heartbeat_interval    = "20s"
    session_timeout       = "60s"
    resend_after          = "15s"
    max_inflight          = 32
    unreachable_after     = "2m"
    offline_warning_after = "3m"
    # Defaults: max_devices = 1000, max_per_account = 10 (at most max_devices), max_auth_failures = 5,
    # auth_failure_window = "10m", max_handshakes_per_address = 16, max_request_bytes = "64KiB".
    max_devices = 1000
  }

  # Optional. The defaults are shown.
  enrollment {
    # A code works one time and lives this long after an account made it.
    code_ttl               = "10m"
    # The most codes of one account that are not used and not expired.
    max_active_per_account = 5
  }

  storage {
    # An alert that a phone did not receive within this time is not sent to it any more.
    delivery_ttl             = "7d"
    # History (delivered and expired alerts) is deleted after this time. It is at least delivery_ttl.
    alert_retention          = "30d"
    connection_log_retention = "90d"
  }

  # The items are the approvals and the questions that an agent sends to a person. The limits have no default:
  # the portal stops at start when one is absent. The values are the recommended ones.
  items {
    max_lifetime       = "90d"   # the longest life of a chain of requests
    max_open_per_key   = 5
    max_open_per_owner = 20
    max_open_total     = 1000
    retention          = "180d"  # at least max_lifetime + lease.revive_window
    body_purge_after   = "30d"   # the sealed contents are erased at this age
    audit_retention    = "365d"  # at least retention

    lease {
      min           = "30s"
      max           = "7d"
      grace         = "2m"
      revive_window = "7d"
    }

    digest {
      mode     = "daily"
      at       = "08:30"
      timezone = "UTC"
    }
  }

  # The portal seals the contents of the items with these keys. The key file is outside the data volume and outside
  # the backups of the database: a copy of the database alone shows no content. Make it once with
  # `portal inbox content-key --out FILE`. Keep a copy of it apart. A lost key makes the sealed contents unreadable.
  at_rest {
    active_key = "k1"
    key "k1" {
      secret_file = "/run/secrets/inbox_content_key"
    }
  }

  # Optional: the approval of an action. A grant with `confirm = true` then needs the approval of the owner of the
  # key. It needs keys.mcp = true and the blocks identity and pdp, which this example does not have. Each service
  # that uses the PDP can ask for approvals: its block pdp_client has no flag for it. Defaults: evaluation_ttl = "5m", deny_cooldown = "5m".
  # The same block sets the limits of the approval API of the services: max_command_bytes = "16KiB",
  # max_explanation_chars = 2000, require_explanation = false, max_comment_chars = 2000, intent_ttl = "10m",
  # max_requests_per_key_per_hour = 20, mute_after_refusals = 3, mute_for = "1h", web_approval = true.
  # approvals {
  #   consume_within = "5m"   # the time after the decision in which the approval can be used
  # }

  # Optional: the MCP endpoint of the agents (`/mcp`), with the tools approval_intent, ask, get_answer, keepalive, say,
  # supersede and cancel. It needs the block items and keys.mcp = true, which this example does not have. Defaults:
  # path = "/mcp", max_wait = "25s" (at most items.wait_max), rate_limit_per_minute = 120,
  # max_unauthenticated_per_minute = 10, max_body_bytes = "64KiB". `allowed_origins` and `allowed_client_cidrs` are
  # lists; without them, no browser origin passes and each client address passes.
  # mcp {
  # }

  # Optional: the agents of a key. The portal gives each agent a token and shows the items by agent. It needs the blocks
  # items and at_rest. Without this block, each key is one agent. Defaults: max_per_key = 16, new_per_hour = 6,
  # lock_new = false (true: the owner must allow a new agent before it can get an approval), idle_after = "14d",
  # purge_after = "365d". A call over a limit goes to the bucket "Unattributed" and the owner gets an alert.
  # agents {
  # }

  # Optional: the questions that an agent asks to the owner of its key (the tool `ask` and POST /api/v1/questions).
  # It needs the blocks items and at_rest. A key needs the permission question.ask. Default: max_options = 8 (2 to 8).
  # questions {
  # }

  group "infra" {
    name        = "Infra prod"
    description = "Production outages"
  }

  group "oncall" {
    name        = "On call"
    description = "Pages for the on-call engineer"
    critical    = true   # a limit on alert.send never applies to this group
  }

  # Optional: alert sources. A key is made with `portal inbox token --out FILE`. A key of the file works
  # when the database is away: the start decides its groups from the grants of its owner.
  api_key "grafana" {
    owner                 = "grafana"
    secret_file           = "/run/secrets/inbox_grafana_key"
    groups                = ["infra"]
    # direct_alerts       = false   # true: the key can send an alert to any account
    # interaction         = "one_shot"   # or "conversational": what the key can do when the server asks a person
    webhook_send_resolved = false
  }

  # Optional: the portal sends an alert about itself to this group. A service of the launcher that
  # stays unhealthy raises one. A burst of refused sign-ins raises one. Each value shown is the default,
  # except `group`, which has no default.
  # watch {
  #   group             = "infra"
  #   service_unhealthy = true
  #   unhealthy_after   = "2m"
  #   sign_in_burst     = true
  #   sign_in_failures  = 10
  #   sign_in_window    = "5m"
  #   repeat_after      = "1h"
  #   recovered         = true
  # }

  # Optional, with SQLite only: standby replication to a directory that the standby instances share
  # (a ReadWriteMany volume).
  # replica {
  #   dir               = "/replica"
  #   snapshot_interval = "10s"
  #   lease_ttl         = "30s"
  #   sync_timeout      = "5s"
  #   sync_alerts       = true
  # }

  # Optional, with PostgreSQL only: the instances copy to each other the alerts that they accept while
  # PostgreSQL has no writable primary. `peers` names the instances, for example a headless Service.
  # cluster {
  #   listen = "0.0.0.0:8082"
  #   peers  = ["inbox-peers.apps.svc.cluster.local:8082"]
  # }
}

# Optional, instead of the block `storage`: PostgreSQL, shared by any number of active instances (plain TCP, no TLS).
# postgres {
#   host            = "postgres.internal"
#   port            = 5432
#   database        = "portal"
#   user            = "portal"
#   password_file   = "/run/secrets/db_password"
#   pool_size       = 8
#   connect_timeout = "5s"
# }
