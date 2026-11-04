# Configuration of the portal with the inbox. Check it with:  portal check-config --config <file>
#
# The inbox starts when the file has a block `inbox`. The delivery values have no default: the
# program stops at start if one is missing, misspelled or out of range. Durations accept the
# units ms, s, m, h and d. Secrets come from a file (`_file`) or from a variable (`_env`).
# The keys of the portal that this example does not set have documented defaults: see the
# configuration reference in README.md.

server {
  # The portal: the pages (behind Traefik ForwardAuth where needed), and the inbox: the API for the senders and
  # the webhooks (/api/v1/*) and the routes of the phones (/v1/enroll, /v1/session). The routes of the inbox have no
  # ForwardAuth. The account pages make the enrollment codes.
  listen_address  = "0.0.0.0:4181"
  # The name of this instance. The app shows it as the name of the server.
  name            = "Portal"
  # Peers whose X-Forwarded-For header the portal trusts, for the pages, the senders and the phones. Empty: none.
  trusted_proxies = ["127.0.0.1/32"]
}

authentication {
  superadmin_login_file    = "/run/secrets/superadmin_login"
  superadmin_password_file = "/run/secrets/superadmin_password"
}

session {
  secret_mode = "provided"
  secret_file = "/run/secrets/session_secret"

  # A session ends this time after the sign-in, also when it is in use (default "24h").
  # ttl = "24h"
  # The box "Keep me signed in": a session with the box lives for remember_ttl and has a persistent
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
user "grafana" {
  grants = [
    "keys.own.use on service:portal/key_kind:api",
    "alert.send on service:portal/inbox_group:infra",
  ]
}

# SQLite. Instead of this block, the block `postgres` below lets several active instances share one database.
storage {
  database = "/var/lib/portal/portal.db"
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
    # Defaults: max_devices = 1000, max_auth_failures = 5, auth_failure_window = "10m", max_request_bytes = "64KiB".
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
