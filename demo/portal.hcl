# Configuration of the portal for the local demo, with the inbox. The secrets come from files that up.sh writes.
server {
  listen_address  = "0.0.0.0:4181"
  # The Traefik container of the demo network, and no other address. The portal reads X-Forwarded-For and
  # X-Forwarded-Proto from this peer only. Traefik ends TLS: a request that it announces as https is secure.
  # The portal itself does not know TLS. It never trusts the gateway of the network: a request from the host
  # to the published port 8091 is plain HTTP and not secure.
  trusted_proxies = ["172.30.0.2/32"]
}

authentication {
  superadmin_login_file    = "/run/secrets/superadmin_login"
  superadmin_password_file = "/run/secrets/superadmin_password"
}

# The grants of every account come from the settings pages. seed.sh sets those of alice and bob.
access {
  grants_mode = "dynamic"
}

storage {
  database = "/var/lib/portal/portal.db"
}

session {
  secret_mode = "ephemeral"
  cookie_name = "__Host-portal"
  ttl         = "8h"
}

discovery {
  docker_socket = "/var/run/docker.sock"
  interval      = "5s"
}

identity {
  issuer          = "https://localhost:8088"
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

# Keys: the page /auth/keys and the edge /auth/verify-key.
keys {}

metrics {
  enabled = true
  path    = "/metrics"
}

# The inbox of the portal. The pages and the producers use Traefik on 8088 (HTTPS). The phones use the plain
# entrypoint 8090 in this demo, because the emulator does not trust the self-signed certificate.
inbox {
  name            = "Inbox demo"
  server_key_file = "/run/secrets/inbox_server_key"

  devices {
    # The public base URL of the portal as the phones reach it: the plain entrypoint `web-phones`. The phone routes
    # carry no password and need no secure transport (Noise protocol). The Android emulator reaches the host
    # at 172.17.0.1. A real deployment uses the HTTPS address of the pages here.
    public_address        = "http://172.17.0.1:8090"
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

  # One producer with a token. The token is a file that up.sh makes.
  producer "local" {
    token_file = "/run/secrets/inbox_producer_token"
    groups     = ["infra", "critical"]
  }

  # The portal tells the group infra when a service stays unhealthy. 10 seconds is the shortest value.
  portal_alerts {
    group           = "infra"
    unhealthy_after = "10s"
  }
}
