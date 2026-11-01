#!/bin/sh
# Run cargo inside the pinned development image: tools/dev/cargo.sh <cargo args>
# INBOX_TARGET_DIR selects the target directory (default: target), so parallel
# builds do not wait on each other. INBOX_TEST_PG / INBOX_E2E_BACKEND (PostgreSQL tests) are passed through, and
# INBOX_DEV_NETWORK=host lets the container reach a database published on the host.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
docker image inspect inbox-dev >/dev/null 2>&1 || docker build -q -t inbox-dev "$root/tools/dev" >/dev/null
docker volume inspect inbox-cargo >/dev/null 2>&1 || {
  docker volume create inbox-cargo >/dev/null
  docker run --rm -v inbox-cargo:/cargo inbox-dev chown "$(id -u):$(id -g)" /cargo
}
tty=""; [ -t 0 ] && tty="-it"
exec docker run --rm $tty -u "$(id -u):$(id -g)" --network "${INBOX_DEV_NETWORK:-bridge}" \
  -v inbox-cargo:/cargo -v "$root:/src" -w /src \
  -e CARGO_TARGET_DIR="/src/${INBOX_TARGET_DIR:-target}" -e INBOX_TEST_PG -e INBOX_E2E_BACKEND inbox-dev cargo "$@"
