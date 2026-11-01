# Configuration of the portal with the inbox. Check it with:  portal check-config --config <file>
#
# The inbox starts when the file has a block `inbox`. The delivery values have no default: the
# program stops at start if one is missing, misspelled or out of range. Durations accept the
# units ms, s, m, h and d. Secrets come from a file (`_file`) or from a variable (`_env`).
# The keys of the portal that this example does not set have documented defaults: see the
# configuration reference in README.md.

server {
  # The pages of the portal, behind Traefik ForwardAuth. The account pages make the enrollment codes.
  listen_address  = "0.0.0.0:4181"
  # Peers whose X-Forwarded-For / X-Real-IP headers the portal trusts. Empty: none.
  trusted_proxies = ["127.0.0.1/32"]
}

authentication {
  superadmin_login_file    = "/run/secrets/superadmin_login"
  superadmin_password_file = "/run/secrets/superadmin_password"
}

session {
  secret_mode = "provided"
  secret_file = "/run/secrets/session_secret"
}

# SQLite. Instead of this block, the block `postgres` below lets several active instances share one database.
storage {
  database = "/var/lib/portal/portal.db"
}

logging {
  level = "info"
}

inbox {
  # Shown in the app.
  name            = "Inbox"
  # Made once with `portal inbox keygen --out FILE`. Replacing it invalidates every phone and every enrollment code.
  server_key_file = "/run/secrets/inbox_server_key"

  devices {
    # Phones (/v1/enroll, /v1/session), producer API, webhooks, /healthz, /readyz.
    listen          = "0.0.0.0:8080"
    # The address that phones use to reach this listener, as they see it from outside. It is the `s` of the
    # enrollment code: host:port, a domain, or an http(s) URL.
    public_address  = "inbox.example.org"
    # Peers whose X-Forwarded-For / X-Real-IP headers the inbox trusts. Empty: none.
    trusted_proxies = ["127.0.0.1/32"]
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
  }

  # Optional: alert sources. A token is made with `portal inbox token --out FILE`.
  producer "grafana" {
    token_file            = "/run/secrets/inbox_grafana_token"
    groups                = ["infra"]
    webhook_send_resolved = false
  }

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
