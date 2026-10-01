#!/bin/sh
# Builds the admin-front WebAssembly module and installs it as web/inbox.wasm,
# which the server build (crates/inbox-server/build.rs) embeds.
#
# Equivalent command, for Dockerfiles and CI:
#   cargo build -p inbox-wasm --target wasm32-unknown-unknown --release
#   cp target/wasm32-unknown-unknown/release/inbox_wasm.wasm web/inbox.wasm
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
target=${INBOX_TARGET_DIR:-target}
INBOX_TARGET_DIR=$target "$root/tools/dev/cargo.sh" build -p inbox-wasm --target wasm32-unknown-unknown --release
cp "$root/$target/wasm32-unknown-unknown/release/inbox_wasm.wasm" "$root/web/inbox.wasm"
echo "web/inbox.wasm: $(wc -c < "$root/web/inbox.wasm") bytes"
