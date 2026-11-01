# Configuration of the portal for the Compose stack of the repository (docker-compose.yml).
# The stack runs the portal with the inbox and one PostgreSQL database.
# Check it with:  docker compose run --rm --no-deps portal check-config --config /etc/portal/config.hcl
#
# Every secret comes from a file under /run/secrets. `deploy/compose/init-secrets.sh` makes these files.
# The keys that this file does not set have documented defaults: see the configuration reference in README.md.

server {
  listen_address  = "0.0.0.0:4181"
  # No proxy in front of this stack: the client address is the address of the TCP peer.
  trusted_proxies = []
}

authentication {
  superadmin_login_file    = "/run/secrets/superadmin_login"
  superadmin_password_file = "/run/secrets/superadmin_password"
}

# A new account has no grant. The superadmin gives grants in the settings pages.
access {
  grants_mode = "dynamic"
}

# PostgreSQL is required for several instances. It needs a signing key that all instances share.
session {
  secret_mode = "provided"
  secret_file = "/run/secrets/session_secret"
}

storage {
  retention_days = 90
}

postgres {
  host          = "postgres"
  database      = "portal"
  user          = "portal"
  password_file = "/run/secrets/db_password"
}

metrics {
  enabled = true
  path    = "/metrics"
}

logging {
  level = "info"
}

inbox {
  name            = "Inbox"
  server_key_file = "/run/secrets/inbox_server_key"

  devices {
    # The public base URL of the portal as a phone reaches it. It is in the enrollment code. The phones use the
    # port of the pages (4181), like the producers and the webhooks.
    # Change it when the phone is not on this host. An emulator reaches the host at 172.17.0.1:4181.
    public_address        = "127.0.0.1:4181"
    heartbeat_interval    = "20s"
    session_timeout       = "60s"
    resend_after          = "15s"
    max_inflight          = 32
    unreachable_after     = "2m"
    offline_warning_after = "3m"
  }

  storage {
    delivery_ttl             = "7d"
    alert_retention          = "30d"
    connection_log_retention = "90d"
  }

  group "infra" {
    name        = "Infra"
    description = "Test group of the local stack"
  }

  # A producer sends alerts with `Authorization: Bearer <token>`. The token is a file of the init script.
  producer "local" {
    token_file = "/run/secrets/inbox_producer_token"
    groups     = ["infra"]
  }
}
