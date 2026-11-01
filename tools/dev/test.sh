#!/bin/sh
# Run the tests of the workspace in Docker, with a throwaway PostgreSQL.
# Usage: tools/dev/test.sh [cargo test arguments]
# PROCURAUTH_CONFORMANCE: directory of the conformance data (default: ../procurauth/conformance).
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
conf=${PROCURAUTH_CONFORMANCE:-$(cd "$root/../procurauth/conformance" 2>/dev/null && pwd || true)}
image=${RUST_IMAGE:-rust:1.98}
docker network inspect portal-net >/dev/null 2>&1 || docker network create portal-net >/dev/null
if ! docker ps --format '{{.Names}}' | grep -qx portal-pg; then
  docker rm -f portal-pg >/dev/null 2>&1 || true
  docker run -d --name portal-pg --network portal-net -e POSTGRES_PASSWORD=testonly -e POSTGRES_DB=portal_test \
    postgres:18 -c fsync=off -c max_connections=2000 -c shared_buffers=128MB >/dev/null
  for _ in $(seq 1 40); do docker exec portal-pg pg_isready -U postgres >/dev/null 2>&1 && break; sleep 1; done
fi
docker volume inspect portal-target >/dev/null 2>&1 || docker volume create portal-target >/dev/null
docker volume inspect portal-cargo >/dev/null 2>&1 || docker volume create portal-cargo >/dev/null
# The tests of the degraded mode need an IP address, not a name.
pgip=$(docker inspect -f '{{(index .NetworkSettings.Networks "portal-net").IPAddress}}' portal-pg)
confmount=""; [ -n "$conf" ] && confmount="-v $conf:/conformance:ro -e PROCURAUTH_CONFORMANCE=/conformance"
exec docker run --rm --ulimit nofile=1048576:1048576 --network portal-net $confmount \
  ${PROCURAUTH_CASES:+-e PROCURAUTH_CASES=$PROCURAUTH_CASES} \
  -v "$root":/src:ro -v portal-target:/target -v portal-cargo:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/target \
  -e PORTAL_TEST_POSTGRES=postgres://postgres:testonly@portal-pg/portal_test \
  -e INBOX_TEST_PG=$pgip:5432:postgres:testonly \
  -w /src "$image" cargo test --locked "$@"
