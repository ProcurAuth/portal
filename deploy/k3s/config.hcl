# Configuration of the portal with the inbox for K3s (ConfigMap portal-config). Two or more active instances share
# one PostgreSQL database. Check it with:  portal check-config --config deploy/k3s/config.hcl
# The check needs the files that the configuration names. Make dummy files for a check outside the cluster.
#
# Edit these values for your cluster before you apply the manifests:
#   server.trusted_proxies                                    the pod network (the pages, the producers and the phones)
#   identity.issuer                                           the base URL of the portal
#   inbox.devices.public_address                              the public base URL of the portal, as the phones reach it
#   postgres.host                                             the read-write Service of the database
#   the blocks group and api_key                              your alert groups and sources

server {
  listen_address  = "0.0.0.0:4181"
  name            = "Portal"
  # X-Forwarded-For counts only from a peer in these networks: the pod network, where the Traefik pods run.
  # A NetworkPolicy lets only the Traefik pods and the Prometheus pods reach this port.
  trusted_proxies = ["10.42.0.0/16"]
}

authentication {
  superadmin_login_file    = "/run/secrets/superadmin_login"
  superadmin_password_file = "/run/secrets/superadmin_password"
}

# "hybrid": the settings pages give the grants of the accounts of the database, and the blocks `user`
# of this file give the grants of the accounts of the file. The account "alerting" owns an API key.
access {
  grants_mode = "hybrid"
}

# Both kinds of key are on. The kind `mcp` needs the blocks `identity` and `pdp` below. The kind `api`
# needs the block `inbox` below.
keys {
  mcp = true
  api = true
}

# The account of the file that owns the API key "alerting". A key follows its owner.
user "alerting" {
  grants = [
    "keys.own.use on service:portal/key_kind:api",
    "alert.send on service:portal/inbox_group:infra",
  ]
}

# The identity token that /auth/verify gives to the service that a user opens.
identity {
  issuer           = "https://portal.example.org"
  signing_key_file = "/run/secrets/identity_key"
}

# The AuthZEN PDP of the services, on a second listener (Service portal-pdp). No IngressRoute names it.
# Each server that asks needs its own block, for example:
#   pdp_client "infra-example" { token_file = "/run/secrets/pdp_token_example" }
# Without a block pdp_client, the PDP refuses every request.
pdp {
  listen_address = "0.0.0.0:4182"
  url            = "http://portal-pdp.edge.svc.cluster.local:4182"
  # The link stays inside the cluster network, so it has no TLS.
  plain_http     = true
}

storage {
  retention_days = 90
}

# All instances sign the forms with the same key. PostgreSQL needs this mode.
session {
  secret_mode = "provided"
  secret_file = "/run/secrets/session_secret"
  bind        = "ip_user_agent"
}

# The sessions, the accounts, the history and the tables of the inbox are in this database.
# Plain TCP inside the cluster: the portal has no TLS stack for PostgreSQL.
postgres {
  host          = "postgresql-rw.data.svc.cluster.local"
  database      = "portal"
  user          = "portal"
  password_file = "/run/secrets/db_password"
}

metrics {
  enabled = true
  path    = "/metrics"
}

discovery {
  kubernetes          = true
  # The two variables that Kubernetes sets in every pod: the host and the HTTPS port of the API.
  kubernetes_host_env = "KUBERNETES_SERVICE_HOST"
  kubernetes_port_env = "KUBERNETES_SERVICE_PORT_HTTPS"
}

logging {
  level = "info"
}

inbox {
  server_key_file = "/run/secrets/inbox_server_key"

  devices {
    # The public base URL of the portal, as a phone reaches it. It is in the enrollment code. The phones use the
    # domain of the portal: Traefik sends /v1/ to the portal without ForwardAuth (IngressRoute portal-inbox) and
    # sets X-Forwarded-For to the real client address. Traefik runs in the pod network, which
    # server.trusted_proxies lists.
    public_address        = "https://portal.example.org"
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

  # While PostgreSQL has no writable primary, each instance keeps accepting alerts and copies them to the others.
  # The headless Service portal-peers lists the instances. Every request carries an HMAC.
  cluster {
    listen = "0.0.0.0:8082"
    peers  = ["portal-peers.edge.svc.cluster.local:8082"]
  }

  group "infra" {
    name        = "Infra"
    description = "Outages of the infrastructure"
  }

  # Alert sources: Alertmanager, Grafana, scripts. Make a key with `portal inbox token --out FILE`.
  # The key follows its owner, an account of the file (the block `user` above).
  api_key "alerting" {
    owner       = "alerting"
    secret_file = "/run/secrets/inbox_alerting_key"
    groups      = ["infra"]
  }

  # The portal alerts about itself when a service stays unhealthy or when sign-ins fail in a burst.
  watch {
    group = "infra"
  }
}
