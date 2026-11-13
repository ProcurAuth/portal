#!/bin/sh
# Joint test of the REAL portal and the REAL ClickHouse Dash (audit item X-13). Nobody ran the two products together before.
# It builds a throwaway Docker stack: the portal (built from this checkout), ClickHouse Dash (the static binary that you name),
# a ClickHouse server, Traefik as the edge, a tap on the PDP link of Dash, and a fake Docker socket for the discovery.
# The portal is the PDP and the gate. A Python container then runs the scenarios and prints a PASS/FAIL table:
#   1 the actions document of Dash in the portal        7 the own confirm of a person
#   2 the metadata of the PDP, and a large batch        8 confirm on mcp.session, and the tools that never use it
#   3 a person opens a page through the gate            9 a ceiling above 1e9
#   4 an agent key calls the MCP tools                 10 a page that needs an approval (approval_cannot_wait)
#   5 the approval of a call (approve, deny, void)     11 the budget queries of Dash, counted by the portal
#   6 the rule of several permissions (export_query)   12 the wire summary (no 5xx, no 400 between Dash and the PDP)
# Usage: INTEROP_DASH_BIN=/path/to/chdash tools/dev/interop-dash.sh        (remove an old stack: tools/dev/interop-dash.sh clean)
# Docker only. The host needs Docker (with Compose), openssl and POSIX sh. The ports open on 127.0.0.1 only. The script never
# touches another container, network or volume, it never reads the Docker socket of the host, and it pushes no image.
# Exit code: 0 when each scenario passes, 1 when one fails, 2 when the setup fails. It takes about 90 seconds with a cached image.
# INTEROP_DASH_BIN     Path of the static binary of ClickHouse Dash (required). It is mounted read-only. Nothing is built from it.
# INTEROP_PORT_BASE    First of two ports on 127.0.0.1 (default 18500): the edge, the portal.
# INTEROP_CH_IMAGE     ClickHouse image (default clickhouse/clickhouse-server:26.8.34.13). Docker pulls it when it is not local.
# INTEROP_EXTRA_HOSTS  Extra hosts in the Dash configuration (default 30). 32 hosts make a navigation batch of 129 questions.
# INTEROP_KEEP         1: leave the stack up after the run, and print how to remove it.
# INTEROP_KEEP_BUILD   1: keep the portal image x13-portal:dev for the next run (default: remove it).
# INTEROP_ONLY         Run only these scenarios, for example "5,6" (the setup always runs).
# INTEROP_REPORT       A file path. The script copies the JSON report of the scenarios there.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
here="$root/tools/dev/interop-dash"
project=portal-x13
base=${INTEROP_PORT_BASE:-18500}
port_edge=$base
port_portal=$((base + 1))
portal_image="x13-portal:dev"
ch_image=${INTEROP_CH_IMAGE:-clickhouse/clickhouse-server:26.8.34.13}
sidecar_image=${INTEROP_SIDECAR_IMAGE:-python:3.13-alpine}
dash_image=${INTEROP_DASH_IMAGE:-alpine:3.22}
traefik_image=${INTEROP_TRAEFIK_IMAGE:-traefik:v3.6}
service_id=clickhouse-dash
extra_hosts=${INTEROP_EXTRA_HOSTS:-30}

dc() { docker compose -p "$project" -f "$here/compose.yml" --profile driver "$@"; }

if [ "${1:-}" = clean ]; then
  # The names are fixed and start with x13. Nothing else is touched.
  docker compose -p "$project" down -v --remove-orphans 2>/dev/null || true
  docker image rm "$portal_image" >/dev/null 2>&1 || true
  echo "clean: the project $project is removed"
  exit 0
fi

bin=${INTEROP_DASH_BIN:-}
[ -n "$bin" ] && [ -x "$bin" ] || { echo "INTEROP_DASH_BIN must name the executable static binary of ClickHouse Dash" >&2; exit 2; }
bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")
docker info >/dev/null 2>&1 || { echo "Docker is not available" >&2; exit 2; }
command -v openssl >/dev/null 2>&1 || { echo "openssl is not available" >&2; exit 2; }
for p in "$port_edge" "$port_portal"; do
  # curl exits with 7 when nothing listens. Any other exit code means that a program answers on the port.
  rc=0
  curl -s -o /dev/null --connect-timeout 1 "http://127.0.0.1:$p/" || rc=$?
  [ "$rc" = 7 ] || { echo "port $p is in use: set INTEROP_PORT_BASE" >&2; exit 2; }
done
# A name of the stack that exists already belongs to an earlier run: stop, so that no foreign object is replaced.
for c in x13-clickhouse x13-tap x13-sock x13-portal x13-dash x13-traefik x13-driver; do
  if docker container inspect "$c" >/dev/null 2>&1; then echo "the container $c exists: run tools/dev/interop-dash.sh clean" >&2; exit 2; fi
done
for n in x13-net; do
  if docker network inspect "$n" >/dev/null 2>&1; then echo "the network $n exists: run tools/dev/interop-dash.sh clean" >&2; exit 2; fi
done
for v in x13-ch-data x13-portal-data; do
  if docker volume inspect "$v" >/dev/null 2>&1; then echo "the volume $v exists: run tools/dev/interop-dash.sh clean" >&2; exit 2; fi
done

work=$(mktemp -d "${TMPDIR:-/tmp}/interop-dash.XXXXXX")
chmod 0755 "$work"
status=0
export X13_PROJECT="$project" X13_DIR="$work" X13_CH_IMAGE="$ch_image" X13_SIDECAR_IMAGE="$sidecar_image" \
  X13_DASH_IMAGE="$dash_image" X13_TRAEFIK_IMAGE="$traefik_image" X13_PORTAL_IMAGE="$portal_image" X13_DASH_BIN="$bin" \
  X13_PORT_EDGE="$port_edge" X13_PORT_PORTAL="$port_portal" X13_SERVICE_ID="$service_id" X13_ONLY="${INTEROP_ONLY:-}"
random() { head -c 256 /dev/urandom | base64 | tr -d '=+/\n' | head -c "$1"; }
X13_CH_ADMIN_PASSWORD=$(random 24)
export X13_CH_ADMIN_PASSWORD

cleanup() {
  if [ "${INTEROP_KEEP:-0}" = 1 ]; then
    echo "The stack stays up (project $project). Files: $work"
    echo "Remove it with: tools/dev/interop-dash.sh clean; rm -rf $work"
    return
  fi
  dc down -v --remove-orphans >/dev/null 2>&1 || true
  rm -rf "$work"
  if [ "${INTEROP_KEEP_BUILD:-0}" != 1 ]; then docker image rm "$portal_image" >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------- image of the portal
echo "== build the portal image from this checkout (no push)"
version=$("$root/tools/version.sh")
sha=$(git -C "$root" rev-parse --short=12 HEAD 2>/dev/null || echo unknown)
docker build -q -f "$root/docker/Dockerfile" --target runtime --build-arg PORTAL_VERSION="$version" \
  --build-arg PORTAL_GIT_SHA="$sha" -t "$portal_image" "$root" >/dev/null

# ---------------------------------------------------------------- files
# The directory "creds" holds the credential files of the throwaway stack. Every value is random and lives only in $work.
mkdir -p "$work/creds" "$work/content" "$work/sock" "$work/tap" "$work/out" "$work/logs" "$work/init"
chmod 0755 "$work/creds" "$work/content" "$work/init"
chmod 0777 "$work/sock" "$work/tap" "$work/out" "$work/logs"
printf '%s' root > "$work/creds/superadmin_login"
random 24 > "$work/creds/superadmin_password"
random 48 > "$work/creds/session_secret"
random 40 > "$work/creds/pdp_token"
openssl genpkey -algorithm ed25519 -out "$work/creds/identity_key" 2>/dev/null
portal_tool() { docker run --rm --network none -u "$(id -u):$(id -g)" -v "$work/creds:/out" -v "$work/content:/content" "$portal_image" "$@"; }
portal_tool inbox keygen --out /out/inbox_server_key >/dev/null
portal_tool inbox content-key --out /content/inbox_content_key >/dev/null
issuer="https://portal.x13.invalid"

cat > "$work/portal.hcl" <<EOF
# Configuration of the portal in the throwaway stack of tools/dev/interop-dash.sh. Every secret is a file under /run/creds.
server {
  listen_address  = "0.0.0.0:4181"
  name            = "Interop with Dash"
  # The private ranges of Docker: the only peers of the portal are the containers of this stack.
  trusted_proxies = ["172.16.0.0/12", "192.168.0.0/16", "10.0.0.0/8"]
}
authentication {
  superadmin_login_file    = "/run/creds/superadmin_login"
  superadmin_password_file = "/run/creds/superadmin_password"
  mfa                      = "off"
}
session {
  secret_mode = "provided"
  secret_file = "/run/creds/session_secret"
  bind        = "user_agent"
}
storage { database = "/var/lib/portal/portal.db" }
access {
  grants_mode     = "dynamic"
  actions_refresh = "10s"
}
keys {
  mcp = true
  api = false
}
identity {
  issuer           = "$issuer"
  signing_key_file = "/run/creds/identity_key"
  token_ttl        = "5m"
}
pdp {
  listen_address = "0.0.0.0:4182"
  # Dash reaches the PDP through the tap, so the tap is the public address of the PDP (protocol 10.0 step 4).
  url            = "http://tap:4182"
  plain_http     = true
}
# The label is the id of the service of Dash. It is the audience of its identity tokens.
pdp_client "$service_id" {
  token_file = "/run/creds/pdp_token"
}
discovery {
  docker_socket = "/sock/docker.sock"
  interval      = "2s"
}
logging {
  level = "info"
}
metrics {
  enabled = true
  path    = "/metrics"
}
inbox {
  server_key_file = "/run/creds/inbox_server_key"
  devices {
    public_address        = "http://127.0.0.1:$port_edge"
    heartbeat_interval    = "5s"
    session_timeout       = "120s"
    resend_after          = "1s"
    max_inflight          = 8
    unreachable_after     = "120s"
    offline_warning_after = "120s"
    max_devices           = 10
    max_auth_failures     = 10
    auth_failure_window   = "1m"
    max_request_bytes     = "64KiB"
  }
  storage {
    delivery_ttl             = "1h"
    alert_retention          = "1h"
    connection_log_retention = "1h"
  }
  items {
    max_lifetime       = "90d"
    max_open_per_key   = 20
    max_open_per_owner = 50
    max_open_total     = 1000
    retention          = "180d"
    body_purge_after   = "30d"
    audit_retention    = "365d"
    lease {
      min           = "5s"
      max           = "7d"
      grace         = "5s"
      revive_window = "7d"
    }
    digest { mode = "off" }
  }
  at_rest {
    active_key = "k1"
    key "k1" { secret_file = "/run/content/inbox_content_key" }
  }
  # The web approval is on (the default): the owner of a key answers on the page, with no phone.
  approvals {
    consume_within                = "8s"
    deny_cooldown                 = "20s"
    mute_after_refusals           = 0
    max_requests_per_key_per_hour = 500
    web_approval                  = true
  }
  group "infra" {
    name        = "Infra"
    description = "Test group of the throwaway stack"
  }
}
EOF
docker run --rm --network none -u "$(id -u):$(id -g)" -v "$work/creds:/run/creds:ro" -v "$work/content:/run/content:ro" \
  -v "$work/portal.hcl:/etc/portal/config.hcl:ro" "$portal_image" identity-public-key --config /etc/portal/config.hcl \
  > "$work/creds/identity_public.jwks"

# The accounts of ClickHouse. The names follow the Dash repository (docs/mcp.md, "The ClickHouse MCP user").
for u in runner system mcp mcp_ro1; do random 20 > "$work/creds/pw-$u"; done
cat > "$work/init/01-x13.sql" <<EOF
-- The throwaway server of tools/dev/interop-dash.sh. The marker database x13_ready comes last: the health check waits for it.
CREATE USER chdash_runner IDENTIFIED WITH plaintext_password BY '$(cat "$work/creds/pw-runner")';
CREATE USER chdash_system IDENTIFIED WITH plaintext_password BY '$(cat "$work/creds/pw-system")';
CREATE USER chdash_mcp IDENTIFIED WITH plaintext_password BY '$(cat "$work/creds/pw-mcp")';
CREATE SETTINGS PROFILE chdash_readonly1 SETTINGS readonly = 1;
CREATE USER chdash_mcp_ro1 IDENTIFIED WITH plaintext_password BY '$(cat "$work/creds/pw-mcp_ro1")' SETTINGS PROFILE 'chdash_readonly1';
CREATE DATABASE x13_data;
CREATE TABLE x13_data.events (n UInt64, label String) ENGINE = MergeTree ORDER BY n;
INSERT INTO x13_data.events SELECT number, concat('row-', toString(number)) FROM numbers(1000);
GRANT SELECT, INSERT, ALTER, CREATE, DROP, TRUNCATE, OPTIMIZE ON *.* TO chdash_runner;
GRANT SHOW DATABASES, SHOW TABLES, SHOW COLUMNS, SHOW DICTIONARIES ON *.* TO chdash_runner;
GRANT SELECT ON system.* TO chdash_system;
GRANT SHOW DATABASES, SHOW TABLES, SHOW COLUMNS, SHOW DICTIONARIES, KILL QUERY, SYSTEM FLUSH LOGS ON *.* TO chdash_system;
GRANT SELECT ON x13_data.* TO chdash_mcp;
GRANT SELECT ON system.databases TO chdash_mcp;
GRANT SELECT ON system.tables TO chdash_mcp;
GRANT SELECT ON system.columns TO chdash_mcp;
GRANT SELECT ON system.one TO chdash_mcp;
GRANT SELECT ON system.numbers TO chdash_mcp;
GRANT SELECT ON system.functions TO chdash_mcp;
GRANT SELECT ON system.data_skipping_indices TO chdash_mcp;
GRANT SELECT ON system.documentation TO chdash_mcp;
GRANT SELECT ON system.parts TO chdash_mcp;
GRANT SELECT ON x13_data.* TO chdash_mcp_ro1;
GRANT SELECT ON system.databases TO chdash_mcp_ro1;
GRANT SELECT ON system.tables TO chdash_mcp_ro1;
GRANT SELECT ON system.columns TO chdash_mcp_ro1;
GRANT SELECT ON system.one TO chdash_mcp_ro1;
GRANT SELECT ON system.numbers TO chdash_mcp_ro1;
GRANT SELECT ON system.functions TO chdash_mcp_ro1;
GRANT SELECT ON system.data_skipping_indices TO chdash_mcp_ro1;
GRANT SELECT ON system.documentation TO chdash_mcp_ro1;
GRANT SELECT ON system.parts TO chdash_mcp_ro1;
CREATE DATABASE x13_ready;
EOF
chmod 0444 "$work"/init/* "$work"/creds/* "$work/content/inbox_content_key" "$work/portal.hcl"

# The configuration of Dash. The host "local" has a normal MCP user. The host "ro1" has a user whose profile is readonly = 1.
# The extra hosts make a navigation batch of more than 100 questions: Dash must split it (access.md, "Decisions").
host_block() { # name mcp_user
  cat <<EOF
  host {
    name                 = "$1"
    runner_uri           = "clickhouse://chdash_runner@clickhouse:9000"
    runner_password_file = "/run/creds/pw-runner"
    system_uri           = "clickhouse://chdash_system@clickhouse:9000"
    system_password_file = "/run/creds/pw-system"
    mcp_uri              = "clickhouse://$2@clickhouse:9000"
    mcp_password_file    = "/run/creds/pw-$3"
  }
EOF
}
{
  cat <<EOF
# Configuration of ClickHouse Dash in the throwaway stack of tools/dev/interop-dash.sh (the identity mode of MCP).
server {
  listen_host = "0.0.0.0"
  listen_port = 8080
}
health {
  interval_ms = 1000
  timeout_ms  = 1000
}
access {
  mode = "authzen"
  identity {
    issuer           = "$issuer"
    audience         = "$service_id"
    public_keys_file = "/run/creds/identity_public.jwks"
    max_age          = "5m"
  }
  pdp {
    url           = "http://tap:4182"
    # The test network is closed: the PDP is in the same Docker network, without TLS.
    insecure_link = true
    token_file    = "/run/creds/pdp_token"
    timeout       = "2s"
    cache_ttl     = "1s"
  }
  approval {
    wait              = "10s"
    max_command_bytes = 4096
  }
}
mcp {
  enabled   = true
  auth_mode = "identity"
  # The scenarios send many requests from one address.
  unauthenticated_rate_limit_per_minute = 0
  # The bound of a call (default 1 GiB). A ceiling of 2 GiB in a grant is below it, so Dash applies the 2 GiB of the grant (scenario 9).
  max_memory_bytes = 4294967296
  usage {
    enabled  = true
    dir      = "/tmp"
    instance = "x13"
  }
}
# A read-only library: library.read is published, library.write is not.
query_library {
  enabled  = true
  file     = "/tmp/query_library.json"
  writable = false
}
clickhouse {
EOF
  host_block local chdash_mcp mcp
  host_block ro1 chdash_mcp_ro1 mcp_ro1
  i=1
  while [ "$i" -le "$extra_hosts" ]; do
    host_block "$(printf 'h%02d' "$i")" chdash_mcp mcp
    i=$((i + 1))
  done
  echo "}"
} > "$work/dash.hcl"
chmod 0444 "$work/dash.hcl"
docker run --rm --network none -v "$bin:/chdash:ro" -v "$work/dash.hcl:/etc/chdash/config.hcl:ro" -v "$work/creds:/run/creds:ro" \
  "$dash_image" /chdash --config /etc/chdash/config.hcl --print-config >"$work/out/dash-print-config.txt" 2>&1 \
  || { echo "The configuration of Dash is not valid" >&2; cat "$work/out/dash-print-config.txt" >&2; exit 2; }
docker run --rm --network none -u "$(id -u):$(id -g)" -v "$work/creds:/run/creds:ro" -v "$work/content:/run/content:ro" \
  -v "$work/portal.hcl:/etc/portal/config.hcl:ro" "$portal_image" check-config --config /etc/portal/config.hcl >/dev/null \
  || { echo "The configuration of the portal is not valid" >&2; exit 2; }

# Scenario 8 reads these two results. The same file with a user block: confirm on mcp.session must stop the start.
hcl_variant() { # name action
  sed 's/grants_mode     = "dynamic"/grants_mode     = "hybrid"/' "$work/portal.hcl" > "$work/portal-$1.hcl"
  cat >> "$work/portal-$1.hcl" <<EOF
user "zed" {
  grants = [
    "access on service:$service_id",
    { grant = "$2 on service:$service_id", confirm = true },
  ]
}
EOF
  chmod 0444 "$work/portal-$1.hcl"
  rc=0
  msg=$(docker run --rm --network none -u "$(id -u):$(id -g)" -v "$work/creds:/run/creds:ro" -v "$work/content:/run/content:ro" \
    -v "$work/portal-$1.hcl:/etc/portal/config.hcl:ro" "$portal_image" check-config --config /etc/portal/config.hcl 2>&1 >/dev/null) || rc=$?
  printf 'exit=%s\n%s\n' "$rc" "$msg" > "$work/out/hcl-confirm-$1.txt"
}
hcl_variant mcp-session mcp.session
hcl_variant ok query.run.read

# ---------------------------------------------------------------- stack
echo "== start the stack (project $project, edge on port $port_edge)"
dc up -d clickhouse tap sock portal dash traefik >/dev/null 2>&1 || { dc logs --tail 30 >&2; exit 2; }
i=0
until [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port_portal/healthz" 2>/dev/null)" = 200 ]; do
  i=$((i + 1))
  if [ "$i" -gt 120 ]; then echo "The portal is not ready" >&2; dc logs --tail 40 portal >&2 || true; exit 2; fi
  sleep 1
done
# The edge needs a moment to load its routes: it answers 404 until then.
i=0
until [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port_edge/auth/login" 2>/dev/null)" = 200 ]; do
  i=$((i + 1))
  if [ "$i" -gt 60 ]; then echo "The edge does not route to the portal" >&2; dc logs --tail 20 traefik >&2 || true; exit 2; fi
  sleep 1
done
i=0
until docker exec x13-dash wget -q -O /dev/null http://127.0.0.1:8080/readyz 2>/dev/null; do
  i=$((i + 1))
  if [ "$i" -gt 120 ]; then echo "Dash is not ready" >&2; dc logs --tail 40 dash clickhouse >&2 || true; exit 2; fi
  sleep 1
done

# ---------------------------------------------------------------- scenarios
echo "== scenarios (a Python container on the network of the stack)"
dc run --rm driver || status=$?
if [ "$status" = 0 ] || [ "$status" = 1 ]; then :; else status=2; fi
if [ -s "$work/out/report.json" ]; then cp "$work/out/report.json" "${INTEROP_REPORT:-/dev/null}" 2>/dev/null || true; fi
exit "$status"
