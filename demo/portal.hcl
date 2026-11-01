# Configuration of the portal for the local demo. The secrets come from files that up.sh writes.
server {
  listen_address  = "0.0.0.0:4181"
  # The Traefik container of the demo network. The portal then reads the client address from X-Forwarded-For.
  trusted_proxies = ["172.30.0.0/24"]
}

authentication {
  superadmin_login_file    = "/run/secrets/superadmin_login"
  superadmin_password_file = "/run/secrets/superadmin_password"
}

# The grants of alice and bob come from this file. The grants of the other users come from the settings pages.
access {
  grants_mode = "hybrid"
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

# Keys: the page /auth/keys and the edge /auth/verify-key.
keys {}

# alice can do much in the service notes. Her budget is 5 uses each minute, and one read gives 2 notes at most.
user "alice" {
  password_hash_file = "/run/secrets/alice_hash"
  grants = [
    { grant = "access on service:notes", limits = { requests = [{ amount = 5, window = "minute" }] } },
    { grant = "notes.read on service:notes", limits = { max_results = 2 } },
    "notes.write on service:notes/notebook:work",
    "notes.delete on service:notes/notebook:work",
    "access on service:files",
    "access on service:wiki",
  ]
}

# bob can read the public notebook only.
user "bob" {
  password_hash_file = "/run/secrets/bob_hash"
  grants = [
    "access on service:notes",
    "notes.read on service:notes/notebook:public",
    "access on service:wiki",
  ]
}
