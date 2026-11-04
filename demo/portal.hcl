# Configuration of the portal for the local demo, with the inbox. The secrets come from files that up.sh writes.
server {
  listen_address  = "0.0.0.0:4181"
  name            = "Portal demo"
  # The Traefik container of the demo network. The portal then reads the client address from X-Forwarded-For.
  trusted_proxies = ["172.30.0.0/24"]
}

authentication {
  superadmin_login_file    = "/run/secrets/superadmin_login"
  superadmin_password_file = "/run/secrets/superadmin_password"
  # The second factor is optional: nobody has it until a person turns it on in the page Security. The sign-in of
  # the demo scripts does not change. up.sh makes the key.
  mfa                      = "optional"
  totp_key_file            = "/run/secrets/totp_key"
}

# The grants of the accounts of the database come from the settings pages. seed.sh sets those of alice
# and bob. The block `user` below is an account of the file: it owns the API key of the inbox.
access {
  grants_mode = "hybrid"
}

storage {
  database = "/var/lib/portal/portal.db"
}

session {
  secret_mode = "ephemeral"
  cookie_name = "__Host-portal"
  ttl         = "24h"
}

discovery {
  docker_socket = "/var/run/docker.sock"
  interval      = "5s"
}

identity {
  issuer          = "http://localhost:8088"
  signing_key_file = "/run/secrets/identity_key"
  token_ttl       = "5m"
}

pdp {
  listen_address = "0.0.0.0:4182"
  url            = "http://portal:4182"
  plain_http     = true
}

pdp_client "notes" {
  token_file = "/run/secrets/pdp_token_notes"
}

# Keys: the page /auth/keys. The kind `mcp` is for agents (the edge /auth/verify-key). The kind `api` is for
# services and scripts (the API of the inbox).
keys {
  mcp = true
  api = true
}

# The account of the file that owns the API key "local". A key follows its owner.
user "local" {
  grants = [
    "keys.own.use on service:portal/key_kind:api",
    "alert.send on service:portal/inbox_group:infra",
    "alert.send on service:portal/inbox_group:critical",
    "alert.send_user on service:portal",
  ]
}

metrics {
  enabled = true
  path    = "/metrics"
}

# The inbox of the portal. The pages, the senders and the phones use the same port (Traefik, 8088).
inbox {
  server_key_file = "/run/secrets/inbox_server_key"

  devices {
    # The public base URL of the portal as the phones reach it. The Android emulator reaches the host
    # at 172.17.0.1. The host reaches this address too.
    public_address        = "http://172.17.0.1:8088"
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
    description = "Alerts about the infrastructure."
  }

  # A critical group has no limit on alert.send.
  group "critical" {
    name        = "Critical"
    description = "Pages that must wake somebody."
    critical    = true
  }

  # One API key of the file. The key is a file that up.sh makes.
  api_key "local" {
    owner       = "local"
    secret_file = "/run/secrets/inbox_api_key"
    groups      = ["infra", "critical"]
    # The key can send an alert to an account too: the owner holds alert.send_user.
    direct_alerts = true
  }

  # The portal tells the group infra when a service stays unhealthy. 10 seconds is the shortest value.
  watch {
    group           = "infra"
    unhealthy_after = "10s"
  }
}
