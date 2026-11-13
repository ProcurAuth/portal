#!/bin/sh
# Run the tests of the workspace in Docker, with a throwaway PostgreSQL.
# Usage: tools/dev/test.sh [cargo test arguments]
# PORTAL_SLOT: suffix of the Docker volume of the build output, so two worktrees do not share it.
# PROCURAUTH_CONFORMANCE: directory of the conformance data (default: ../procurauth/conformance). Without it, the series D and E skip.
# PROCURAUTH_CONFORMANCE_REQUIRED=1: a missing directory of the conformance data stops the script with an error, and the
# tests of the series D and E fail when they cannot read the data. CI sets it, so a run never passes without the cases.
# INBOX_E2E_BACKEND=postgres: run tests/inbox_e2e.rs on PostgreSQL instead of SQLite.
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
docker network inspect portal-net >/dev/null 2>&1 || docker network create portal-net >/dev/null
if ! docker ps --format '{{.Names}}' | grep -qx portal-pg; then
  docker rm -f portal-pg >/dev/null 2>&1 || true
  docker run -d --name portal-pg --network portal-net -e POSTGRES_PASSWORD=testonly -e POSTGRES_DB=portal_test --shm-size=1g \
    postgres:18 -c fsync=off -c max_connections=2000 -c shared_buffers=128MB >/dev/null
  for _ in $(seq 1 40); do docker exec portal-pg pg_isready -U postgres >/dev/null 2>&1 && break; sleep 1; done
fi
docker volume inspect portal-target${PORTAL_SLOT:+-$PORTAL_SLOT} >/dev/null 2>&1 || docker volume create portal-target${PORTAL_SLOT:+-$PORTAL_SLOT} >/dev/null
docker volume inspect portal-cargo >/dev/null 2>&1 || docker volume create portal-cargo >/dev/null
# The tests of the degraded mode need an IP address, not a name.
pgip=$(docker inspect -f '{{(index .NetworkSettings.Networks "portal-net").IPAddress}}' portal-pg)
confmount=""; [ -n "$conf" ] && confmount="-v $conf:/conformance:ro -e PROCURAUTH_CONFORMANCE=/conformance"
exec docker run --rm --ulimit nofile=1048576:1048576 --network portal-net $confmount \
  ${PROCURAUTH_CASES:+-e PROCURAUTH_CASES=$PROCURAUTH_CASES} \
  ${PROCURAUTH_CONFORMANCE_REQUIRED:+-e PROCURAUTH_CONFORMANCE_REQUIRED=$PROCURAUTH_CONFORMANCE_REQUIRED} \
  -v "$root":/src:ro -v portal-target${PORTAL_SLOT:+-$PORTAL_SLOT}:/target -v portal-cargo:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/target \
  -e PORTAL_TEST_POSTGRES=postgres://postgres:testonly@portal-pg/portal_test \
  -e INBOX_TEST_PG=$pgip:5432:postgres:testonly \
  ${INBOX_E2E_BACKEND:+-e INBOX_E2E_BACKEND=$INBOX_E2E_BACKEND} \
  -w /src "$image" cargo test --locked "$@"
