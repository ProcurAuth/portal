#!/bin/sh
# End-to-end check of the portal against the REAL reference PEP of the ProcurAuth repository (lot L7b).
# It builds a throwaway Docker stack: the portal (built from this checkout), the reference service (a real PEP), a tap on the
# link between them, a fake Docker socket for the discovery, and optionally PostgreSQL. It then runs two checks:
#   1. tests/pep_e2e.rs (a Rust agent and a Rust phone with Noise): the scenarios of CONF 3 (approve and execute one time, a
#      second call asks again, decline with a comment, abandon, renewal by get_action_status) and the PEP observation
#      (the service saw the verdict, the request id and the code only), and the agent canary of SPEC 3.10.4: two agents of one
#      key, a second agent for the same effect, and no token, label, client or id of an agent in any request, answer or log of
#      the service.
#   2. the series U of the ProcurAuth runner against the same reference service binary. The script shows the cases U30 and
#      U100 to U117 (the approvals), U118 to U127 (ProcurAuth 1.1) and each case that did not pass.
#      The runner is the PDP of its own cases. It cannot call the portal: see docs/inbox/CONTRIBUTING.md.
# Usage: tools/dev/pep-e2e.sh
# Docker only. The host needs Docker (with Compose), openssl and POSIX sh. The script opens ports on 127.0.0.1 only. It never
# touches another stack, it pushes no image, and it never writes in the ProcurAuth checkout (it is a read-only build context).
# PEP_E2E_PROCURAUTH   Path of the ProcurAuth checkout (default: ../procurauth or ../../procurauth of this checkout).
# PEP_E2E_BACKEND      sqlite (default) or postgres: the database of the portal.
# PEP_E2E_PORT_BASE    First of three ports on 127.0.0.1 (default 18400): the portal, a spare, the service.
# PEP_E2E_PROJECT      Compose project name (default pep-e2e-<PORTAL_SLOT or local>). The images and volumes use the slot too.
# PEP_E2E_KEEP_BUILD   1: keep the images and the build volume of the tests for the next run (default: remove them).
# PEP_E2E_KEEP         1: leave the stack and its files after the run, and print how to remove them.
# PEP_E2E_SKIP_UNIT    1: skip the Rust scenarios. PEP_E2E_SKIP_RUNNER=1: skip the series U.
# PEP_E2E_TEST_ARGS    More arguments for the test binary, for example a test name filter.
# PORTAL_SLOT          Suffix of the build volume (as in tools/dev/test.sh).
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
here="$root/tools/dev/pep-e2e"
slot=${PORTAL_SLOT:-local}
slot_lc=$(printf '%s' "$slot" | tr 'A-Z' 'a-z' | tr -c 'a-z0-9\n' '-')
project=${PEP_E2E_PROJECT:-pep-e2e-$slot_lc}
backend=${PEP_E2E_BACKEND:-sqlite}
base=${PEP_E2E_PORT_BASE:-18400}
port_portal=$base
port_service=$((base + 2))
portal_image="pep-e2e-portal:$slot_lc"
pa_image="pep-e2e-procurauth:$slot_lc"
sidecar_image=${PEP_E2E_SIDECAR_IMAGE:-python:3.13-alpine}
postgres_image=${PEP_E2E_POSTGRES_IMAGE:-postgres:18-alpine}
rust_image=${RUST_IMAGE:-rust:1.98}
build_volume="portal-target-pep-e2e-$slot_lc"
case "$backend" in sqlite | postgres) ;; *) echo "PEP_E2E_BACKEND must be sqlite or postgres" >&2; exit 2 ;; esac

pa=${PEP_E2E_PROCURAUTH:-}
if [ -z "$pa" ]; then
  for d in "$root/../procurauth" "$root/../../procurauth"; do
    [ -d "$d/crates/reference-service" ] && pa=$(cd "$d" && pwd) && break
  done
fi
[ -n "$pa" ] && [ -d "$pa/crates/reference-service" ] || { echo "ProcurAuth checkout not found: set PEP_E2E_PROCURAUTH" >&2; exit 2; }
docker info >/dev/null 2>&1 || { echo "Docker is not available" >&2; exit 2; }
for p in "$port_portal" "$port_service"; do
  # curl exits with 7 when nothing listens. Any other exit code means that a program answers on the port.
  rc=0
  curl -s -o /dev/null --connect-timeout 1 "http://127.0.0.1:$p/" || rc=$?
  [ "$rc" = 7 ] || { echo "port $p is in use: set PEP_E2E_PORT_BASE" >&2; exit 2; }
done

work=$(mktemp -d "${TMPDIR:-/tmp}/pep-e2e.XXXXXX")
chmod 0755 "$work"
status=0
dc() { docker compose -p "$project" -f "$here/compose.yml" "$@"; }
export PEP_E2E_DIR="$work" PEP_E2E_PORT_PORTAL="$port_portal" PEP_E2E_PORT_SERVICE="$port_service" \
  PEP_E2E_PA_IMAGE="$pa_image" PEP_E2E_PORTAL_IMAGE="$portal_image" PEP_E2E_SIDECAR_IMAGE="$sidecar_image" \
  PEP_E2E_POSTGRES_IMAGE="$postgres_image"

cleanup() {
  if [ "${PEP_E2E_KEEP:-0}" = 1 ]; then
    echo "The stack stays up (project $project). Files: $work"
    echo "Remove it with: docker compose -p $project -f $here/compose.yml down -v; rm -rf $work"
    return
  fi
  dc --profile postgres down -v --remove-orphans >/dev/null 2>&1 || true
  rm -rf "$work"
  if [ "${PEP_E2E_KEEP_BUILD:-0}" != 1 ]; then
    docker image rm "$portal_image" "$pa_image" >/dev/null 2>&1 || true
    docker volume rm "$build_volume" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------- images
echo "== build the portal image from this checkout"
version=$("$root/tools/version.sh")
sha=$(git -C "$root" rev-parse --short=12 HEAD 2>/dev/null || echo unknown)
docker build -q -f "$root/docker/Dockerfile" --target runtime --build-arg PORTAL_VERSION="$version" \
  --build-arg PORTAL_GIT_SHA="$sha" -t "$portal_image" "$root" >/dev/null
echo "== build the reference service and the runner (the ProcurAuth checkout is a read-only context)"
docker build -q -f "$here/Dockerfile.procurauth" -t "$pa_image" "$pa" >/dev/null

# ---------------------------------------------------------------- files
mkdir -p "$work/secrets" "$work/content" "$work/sock" "$work/tap" "$work/out"
chmod 0755 "$work/secrets" "$work/content"
chmod 0777 "$work/sock" "$work/tap" "$work/out"
random() { head -c 256 /dev/urandom | base64 | tr -d '=+/\n' | head -c "$1"; }
printf '%s' root > "$work/secrets/superadmin_login"
random 24 > "$work/secrets/superadmin_password"
random 48 > "$work/secrets/session_secret"
random 40 > "$work/secrets/pdp_token"
random 32 > "$work/secrets/db_password"
openssl genpkey -algorithm ed25519 -out "$work/secrets/identity_key" 2>/dev/null
portal_tool() { docker run --rm --network none -u "$(id -u):$(id -g)" -v "$work/secrets:/out" -v "$work/content:/content" "$portal_image" "$@"; }
portal_tool inbox keygen --out /out/inbox_server_key >/dev/null
# The content key is outside the data volume and outside the secrets of the database.
portal_tool inbox content-key --out /content/inbox_content_key >/dev/null

if [ "$backend" = postgres ]; then
  storage='postgres {
  host = "postgres"
  database = "portal"
  user = "portal"
  password_file = "/run/secrets/db_password"
  pool_size = 6
  connect_timeout = "5s"
}'
  pool='    pool_size = 6'
else
  storage='storage { database = "/var/lib/portal/portal.db" }'
  pool=''
fi
issuer="https://portal.pep-e2e.invalid"
cat > "$work/config.hcl" <<EOF
# Configuration of the throwaway stack of tools/dev/pep-e2e.sh. Every secret is a file under /run/secrets.
server {
  listen_address  = "0.0.0.0:4181"
  name            = "PEP e2e"
  trusted_proxies = []
}
authentication {
  superadmin_login_file    = "/run/secrets/superadmin_login"
  superadmin_password_file = "/run/secrets/superadmin_password"
  mfa                      = "off"
}
session {
  secret_mode = "provided"
  secret_file = "/run/secrets/session_secret"
}
$storage
access {
  grants_mode = "hybrid"
}
keys {
  mcp = true
  api = false
}
identity {
  issuer           = "$issuer"
  signing_key_file = "/run/secrets/identity_key"
  token_ttl        = "5m"
}
pdp {
  listen_address = "0.0.0.0:4182"
  # The service reaches the PDP through the tap, so the tap is the public address of the PDP (protocol 10.0 step 4).
  url            = "http://tap:4182"
  plain_http     = true
}
pdp_client "datastore" {
  token_file = "/run/secrets/pdp_token"
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
  server_key_file = "/run/secrets/inbox_server_key"
  devices {
    public_address        = "127.0.0.1:$port_portal"
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
$pool
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
  approvals {
    consume_within                = "5m"
    deny_cooldown                 = "30s"
    mute_after_refusals           = 0
    max_requests_per_key_per_hour = 500
  }
  mcp {}
  # The agents (lot G5): the key makes agents by its calls, and the canary test checks that the service never sees them.
  agents {}
  group "infra" {
    name        = "Infra"
    description = "Test group of the throwaway stack"
  }
}
EOF
docker run --rm --network none -u "$(id -u):$(id -g)" -v "$work/secrets:/run/secrets:ro" -v "$work/content:/run/content:ro" \
  -v "$work/config.hcl:/etc/portal/config.hcl:ro" "$portal_image" identity-public-key --config /etc/portal/config.hcl \
  > "$work/secrets/identity_public.jwks"
cat > "$work/secrets/service.json" <<EOF
{
  "listen": "0.0.0.0:8080",
  "service_id": "datastore",
  "token_header": "X-Identity-Token",
  "issuer": "$issuer",
  "max_token_age_seconds": 300,
  "clock_skew_seconds": 30,
  "jwks_file": "/run/secrets/identity_public.jwks",
  "cache_ttl_seconds": 0,
  "mcp_path": "/mcp",
  "pdp": {
    "url": "http://tap:4182",
    "credential_file": "/run/secrets/pdp_token",
    "allow_plain_http": true,
    "request_timeout_ms": 5000,
    "usage_wait_margin_ms": 1000
  }
}
EOF
chmod 0444 "$work"/secrets/* "$work/content/inbox_content_key" "$work/config.hcl"
docker run --rm --network none -u "$(id -u):$(id -g)" -v "$work/secrets:/run/secrets:ro" -v "$work/content:/run/content:ro" \
  -v "$work/config.hcl:/etc/portal/config.hcl:ro" "$portal_image" check-config --config /etc/portal/config.hcl >/dev/null \
  || { echo "The configuration of the stack is not valid" >&2; exit 1; }

# ---------------------------------------------------------------- stack
echo "== start the stack (project $project, ports $port_portal and $port_service, backend $backend)"
profile=""
[ "$backend" = postgres ] && profile="--profile postgres"
# shellcheck disable=SC2086
dc $profile up -d >/dev/null
i=0
until [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port_portal/healthz" 2>/dev/null)" = 200 ]; do
  i=$((i + 1))
  if [ "$i" -gt 90 ]; then
    echo "The portal is not ready" >&2
    dc logs --tail 40 portal service >&2 || true
    exit 1
  fi
  sleep 1
done

# ---------------------------------------------------------------- 1. scenarios with a Rust agent and a Rust phone
if [ "${PEP_E2E_SKIP_UNIT:-0}" != 1 ]; then
  echo "== scenarios: agent, real PEP, real PDP, phone (tests/pep_e2e.rs)"
  docker volume inspect "$build_volume" >/dev/null 2>&1 || docker volume create "$build_volume" >/dev/null
  docker volume inspect portal-cargo >/dev/null 2>&1 || docker volume create portal-cargo >/dev/null
  # shellcheck disable=SC2086
  docker run --rm --network host --ulimit nofile=1048576:1048576 \
    -v "$root":/src:ro -v "$build_volume":/target -v portal-cargo:/usr/local/cargo/registry -e CARGO_TARGET_DIR=/target \
    -v "$work":/pep-e2e \
    -e PEP_E2E=1 -e PEP_E2E_DIR=/pep-e2e -e PEP_E2E_PORTAL_PORT="$port_portal" -e PEP_E2E_SERVICE_PORT="$port_service" \
    -w /src "$rust_image" cargo test --locked --test pep_e2e -- --ignored --nocapture --test-threads=1 ${PEP_E2E_TEST_ARGS:-} \
    || status=1

  echo "== the canary strings must be in no log of the stack"
  if [ -s "$work/out/canaries.txt" ]; then
    for c in portal service tap sock; do
      dc logs --no-color "$c" > "$work/out/log-$c.txt" 2>&1 || true
    done
    hits=0
    while read -r canary; do
      [ -n "$canary" ] || continue
      for c in portal service tap sock; do
        if grep -q -- "$canary" "$work/out/log-$c.txt"; then
          echo "FAIL: a canary string is in the log of the container $c" >&2
          hits=1
        fi
      done
    done < "$work/out/canaries.txt"
    # The values that only the service must not see. The portal keeps the id of an agent in its own log (the field `agent`).
    if [ -s "$work/out/canaries-service.txt" ]; then
      while read -r canary; do
        [ -n "$canary" ] || continue
        for c in service tap sock; do
          if grep -q -- "$canary" "$work/out/log-$c.txt"; then
            echo "FAIL: an agent value is in the log of the container $c" >&2
            hits=1
          fi
        done
      done < "$work/out/canaries-service.txt"
    else
      echo "FAIL: the test wrote no agent values" >&2
      hits=1
    fi
    if [ "$hits" = 0 ]; then echo "ok: no canary string in the logs of the containers"; else status=1; fi
  else
    echo "FAIL: the test wrote no canary strings" >&2
    status=1
  fi
fi

# ---------------------------------------------------------------- 2. the series U against the real service binary
if [ "${PEP_E2E_SKIP_RUNNER:-0}" != 1 ]; then
  echo "== series U of the ProcurAuth runner against the reference service (the runner is the PDP of its cases)"
  # One container, no network: the runner is the client, the edge and the PDP. The same binary as in the stack.
  # shellcheck disable=SC2016
  docker run --rm --init --network none "$pa_image" bash -c '
    set -u
    cd /pa/crates/reference-service/examples
    PROCURAUTH_PDP_CREDENTIAL=$(tr -d "\n" < /pa/crates/access-conformance/examples/pdp-credential.txt)
    export PROCURAUTH_PDP_CREDENTIAL
    code=0
    reference-service conformance.json 2>/dev/null &
    pid=$!
    for _ in $(seq 1 100); do (exec 3<>/dev/tcp/127.0.0.1/8080) 2>/dev/null && break; sleep 0.1; done
    timeout 300 access-conformance --role pep --profile conformance-profile.json --url http://127.0.0.1:8080 --series U \
      > /tmp/u.out 2>/tmp/u.err || code=$?
    kill $pid 2>/dev/null
    echo "--- the cases of the approvals (U30, U100 to U117)"
    grep -E "^(U30|U10[0-9]|U11[0-7]) " /tmp/u.out | cut -c1-110
    echo "--- the cases of ProcurAuth 1.1 (U118 to U127)"
    grep -E "^(U11[89]|U12[0-7]) " /tmp/u.out | cut -c1-110
    echo "--- the other cases that did not pass"
    grep -E "^U[0-9]+ +(fail|warn|skip) " /tmp/u.out | cut -c1-110
    tail -n 1 /tmp/u.out
    [ "$code" = 0 ] || tail -n 20 /tmp/u.err
    exit $code
  ' || status=1
fi

if [ "$status" = 0 ]; then echo "== pep-e2e: all checks passed"; else echo "== pep-e2e: FAILED" >&2; fi
exit "$status"
