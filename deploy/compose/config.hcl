# Configuration of the portal for the Compose stack of the repository (docker-compose.yml).
# The stack runs the portal with the inbox and one PostgreSQL database.
# Check it with:  docker compose run --rm --no-deps portal check-config --config /etc/portal/config.hcl
#
# Every secret comes from a file under /run/secrets. `deploy/compose/init-secrets.sh` makes these files.
# The keys that this file does not set have documented defaults: see the configuration reference in docs/configuration.md.

server {
  listen_address  = "0.0.0.0:4181"
  name            = "Portal"
  # No proxy in front of this stack: the client address is the address of the TCP peer.
  trusted_proxies = []
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

# A new account has no grant. The superadmin gives grants in the settings pages. The mode "hybrid"
# also reads the blocks `user` of this file: the account "local" owns the API key of the inbox.
access {
  grants_mode = "hybrid"
}

# The kind `api` is on: the API of the inbox accepts API keys (`api_` and 49 characters). The kind
# `mcp` is off: this stack has no `identity` block and no `pdp` block, which the kind `mcp` needs.
keys {
  mcp = false
  api = true
}

# An account of the file owns the key. A key follows its owner: it can do only what both can do.
user "local" {
  grants = [
    "keys.own.use on service:portal/key_kind:api",
    "alert.send on service:portal/inbox_group:infra",
  ]
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
  server_key_file = "/run/secrets/inbox_server_key"

  devices {
    # The public base URL of the portal as a phone reaches it. It is in the enrollment code. The phones use the
    # port of the pages (4181), like the senders and the webhooks.
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

  group "infra" {
    name        = "Infra"
    description = "Test group of the local stack"
  }

  # A sender sends alerts with `Authorization: Bearer <key>`. The key is a file of the init script.
  api_key "local" {
    owner       = "local"
    secret_file = "/run/secrets/inbox_api_key"
    groups      = ["infra"]
  }
}
