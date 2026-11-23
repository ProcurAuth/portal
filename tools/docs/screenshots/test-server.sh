#!/bin/sh
# Start a test server with demo data, for the screenshots. The server comes from a test of the repository
# that is meant for a look at the pages. It listens on 127.0.0.1 in a Docker container that shares the network of the host.
# Usage: test-server.sh TEST_FILE TEST_NAME OUT_DIR
#   TEST_FILE  the name of the test file in tests/ without .rs (for example items_web_e2e)
#   TEST_NAME  the name of the test (for example items_demo_for_the_eyes)
#   OUT_DIR    a directory of the host. The test writes demo.json there (the port and the logins).
#              Create the file OUT_DIR/stop to stop the server.
# PORTAL_SLOT: the suffix of the Docker volume of the build output (default shots).
set -eu
root=$(cd "$(dirname "$0")/../../.." && pwd)
file=$1; name=$2; out=$3
mkdir -p "$out"; rm -f "$out/stop" "$out/demo.json"
slot=${PORTAL_SLOT:-shots}
image=${RUST_IMAGE:-rust:1.98}
docker volume inspect "portal-target-$slot" >/dev/null 2>&1 || docker volume create "portal-target-$slot" >/dev/null
docker volume inspect portal-cargo >/dev/null 2>&1 || docker volume create portal-cargo >/dev/null
exec docker run --rm --name "portal-shots-$slot" --network host -v "$root":/src:ro -v "portal-target-$slot":/target \
  -v portal-cargo:/usr/local/cargo/registry -v "$(cd "$out" && pwd)":/out -e CARGO_TARGET_DIR=/target -e INBOX_DEMO_OUT=/out \
  ${INBOX_DEMO_LOCK:+-e INBOX_DEMO_LOCK=$INBOX_DEMO_LOCK} \
  -w /src "$image" cargo test --locked --test "$file" "$name" -- --ignored --nocapture
