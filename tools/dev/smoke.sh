#!/bin/sh
# Smoke test of the Compose stack: portal, PostgreSQL and inbox. It runs the image on this machine only.
# Usage: tools/dev/smoke.sh [image]    (default image: portal:local, built by tools/build.sh image)
# The script makes the secrets in a temporary directory, starts the stack under the project name portal-smoke
# on high ports, checks it, and always deletes the stack and its volumes.
# Checks:
#   /healthz, /readyz and /api/version of the portal answer 200
#   a producer token is accepted at POST /api/v1/alerts of the portal, on the port of the pages (201), a wrong token is not (401)
#   the routes of the phones are on the same port: a plain GET of /v1/session is 400, and an upgrade gives 101
#   the metrics of the portal show portal_inbox_* series
#   an account that is not signed in cannot read the admin calls of the inbox (401)
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
image=${1:-portal:local}
tag=${image#*:}
export PORTAL_IMAGE_TAG=$tag
export PORTAL_PORT=${PORTAL_SMOKE_PORT:-14181}

tmp=$(mktemp -d)
export PORTAL_SECRETS_DIR="$tmp/secrets"
compose() { docker compose -p portal-smoke --project-directory "$root" -f "$root/docker-compose.yml" "$@"; }
cleanup() {
  status=$?
  if [ "$status" -ne 0 ]; then compose logs --no-color --tail 60 portal >&2 || true; fi
  compose down -v >/dev/null 2>&1 || true
  rm -rf "$tmp"
  [ "$status" -eq 0 ] && echo "smoke test passed" || echo "smoke test FAILED" >&2
  exit "$status"
}
trap cleanup EXIT

PORTAL_IMAGE="$image" deploy/compose/init-secrets.sh "$PORTAL_SECRETS_DIR" >/dev/null
# The compose file builds the image only when it is missing, so it uses the image that was given.
compose up -d --no-build --wait --wait-timeout 120

check() { # check <label> <expected status> <actual status>
  if [ "$2" != "$3" ]; then echo "FAIL $1: expected $2, got $3" >&2; exit 1; fi
  echo "ok   $1 ($3)"
}
code() { curl -s -o /dev/null -w '%{http_code}' "$@"; }
pages="http://127.0.0.1:$PORTAL_PORT"

check "portal /healthz" 200 "$(code "$pages/healthz")"
check "portal /readyz" 200 "$(code "$pages/readyz")"
check "portal /api/version" 200 "$(code "$pages/api/version")"
check "phones: a plain GET of /v1/session reaches the inbox" 400 "$(code "$pages/v1/session")"
check "phones: a WebSocket upgrade of /v1/session" 101 "$(code --max-time 2 \
  -H 'Connection: Upgrade' -H 'Upgrade: websocket' -H 'Sec-WebSocket-Version: 13' \
  -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' "$pages/v1/session" || true)"

token=$(cat "$PORTAL_SECRETS_DIR/inbox_api_key")
key="smoke-$(date +%s)"
body='{"group":"infra","title":"Smoke test"}'
check "alert with the producer token" 201 "$(code -X POST "$pages/api/v1/alerts" \
  -H "Authorization: Bearer $token" -H "Idempotency-Key: $key" -H 'Content-Type: application/json' -d "$body")"
check "same alert again (idempotent)" 200 "$(code -X POST "$pages/api/v1/alerts" \
  -H "Authorization: Bearer $token" -H "Idempotency-Key: $key" -H 'Content-Type: application/json' -d "$body")"
check "alert with a wrong token" 401 "$(code -X POST "$pages/api/v1/alerts" \
  -H "Authorization: Bearer wrong" -H "Idempotency-Key: $key-2" -H 'Content-Type: application/json' -d "$body")"
check "admin call without a session" 401 "$(code "$pages/auth/inbox/api/overview")"

metrics=$(curl -s "$pages/metrics")
printf '%s\n' "$metrics" | grep -q '^portal_inbox_' || { echo "FAIL: /metrics has no portal_inbox_ series" >&2; exit 1; }
echo "ok   /metrics has portal_inbox_ series ($(printf '%s\n' "$metrics" | grep -c '^portal_inbox_') lines)"
