#!/bin/sh
# Run the tests of the workspace in Docker, with a throwaway PostgreSQL.
# Usage: tools/dev/test.sh [cargo test arguments]
# PORTAL_SLOT: suffix of the Docker volume of the build output, so two worktrees do not share it.
# PROCURAUTH_CONFORMANCE: directory of the conformance data (default: ../procurauth/conformance). Without it, the series D and E skip.
# PROCURAUTH_CONFORMANCE_REQUIRED=1: a missing directory of the conformance data stops the script with an error, and the
# tests of the series D and E fail when they cannot read the data. CI sets it, so a run never passes without the cases.
# INBOX_E2E_BACKEND=postgres: run tests/inbox_e2e.rs on PostgreSQL instead of SQLite.
# PORTAL_KEEP_PG=1: keep the container portal-pg and the network portal-net at exit. Without it, the script removes at
# exit the container and the network that this run created, also after an error, when no other container uses
# portal-net. A portal-pg that is active at start (an earlier run kept it) is used again and stays.
# Signals: a trap runs only after the foreground docker run returns. After INT or TERM, the test container continues
# to its end; the removal and the exit occur when it stops.
# Exit status: the status of cargo test; 130 after INT and 143 after TERM; not 0 when a step of the set-up fails.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
conf=${PROCURAUTH_CONFORMANCE:-$(cd "$root/../procurauth/conformance" 2>/dev/null && pwd || true)}
if [ "${PROCURAUTH_CONFORMANCE_REQUIRED:-}" = 1 ]; then
  if [ -z "$conf" ] || [ ! -d "$conf/cases/D" ] || [ ! -d "$conf/cases/E" ]; then
    echo "error: PROCURAUTH_CONFORMANCE_REQUIRED=1 and the directory of the conformance data is missing or has no cases/D and cases/E: set PROCURAUTH_CONFORMANCE to the directory conformance/ of the procurauth checkout" >&2
    exit 1
  fi
fi
image=${RUST_IMAGE:-rust:1.98}
# The digest of postgres:18 on the day of the pin. Change the two parts together.
pgimage=postgres:18@sha256:edb0c08dc33befd0262ab3b5cf1c4f3f8cfd8b09e88c05b45adee29d6761ddb6
# 1 when this run created the container portal-pg or the network portal-net. The cleanup removes only these.
made_pg=0
made_net=0
cleanup() {
  [ "${PORTAL_KEEP_PG:-}" = 1 ] && return 0
  [ "$made_pg" = 1 ] || [ "$made_net" = 1 ] || return 0
  # A run of another worktree can use the same database: its test container is on portal-net.
  others=$(docker ps -q --filter network=portal-net 2>/dev/null | grep -vxF "$(docker ps -q --filter name='^portal-pg$' 2>/dev/null)" || true)
  if [ -n "$others" ]; then
    echo "test.sh: other containers use portal-net: portal-pg and portal-net stay" >&2
    return 0
  fi
  if [ "$made_pg" = 1 ]; then docker rm -f portal-pg >/dev/null 2>&1 || true; fi
  if [ "$made_net" = 1 ]; then docker network rm portal-net >/dev/null 2>&1 || true; fi
  return 0
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
if ! docker network inspect portal-net >/dev/null 2>&1; then
  docker network create portal-net >/dev/null
  made_net=1
fi
if ! docker ps --format '{{.Names}}' | grep -qx portal-pg; then
  docker rm -f portal-pg >/dev/null 2>&1 || true
  docker run -d --name portal-pg --network portal-net -e POSTGRES_PASSWORD=testonly -e POSTGRES_DB=portal_test --shm-size=1g \
    "$pgimage" -c fsync=off -c max_connections=2000 -c shared_buffers=128MB >/dev/null
  made_pg=1
  for _ in $(seq 1 40); do docker exec portal-pg pg_isready -U postgres >/dev/null 2>&1 && break; sleep 1; done
fi
docker volume inspect portal-target${PORTAL_SLOT:+-$PORTAL_SLOT} >/dev/null 2>&1 || docker volume create portal-target${PORTAL_SLOT:+-$PORTAL_SLOT} >/dev/null
docker volume inspect portal-cargo >/dev/null 2>&1 || docker volume create portal-cargo >/dev/null
# The tests of the degraded mode need an IP address, not a name.
pgip=$(docker inspect -f '{{(index .NetworkSettings.Networks "portal-net").IPAddress}}' portal-pg)
confmount=""; [ -n "$conf" ] && confmount="-v $conf:/conformance:ro -e PROCURAUTH_CONFORMANCE=/conformance"
status=0
docker run --rm --ulimit nofile=1048576:1048576 --network portal-net $confmount \
  ${PROCURAUTH_CASES:+-e PROCURAUTH_CASES=$PROCURAUTH_CASES} \
  ${PROCURAUTH_CONFORMANCE_REQUIRED:+-e PROCURAUTH_CONFORMANCE_REQUIRED=$PROCURAUTH_CONFORMANCE_REQUIRED} \
  -v "$root":/src:ro -v portal-target${PORTAL_SLOT:+-$PORTAL_SLOT}:/target -v portal-cargo:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/target \
  -e PORTAL_TEST_POSTGRES=postgres://postgres:testonly@portal-pg/portal_test \
  -e INBOX_TEST_PG=$pgip:5432:postgres:testonly \
  ${INBOX_E2E_BACKEND:+-e INBOX_E2E_BACKEND=$INBOX_E2E_BACKEND} \
  -w /src "$image" cargo test --locked "$@" || status=$?
exit "$status"
