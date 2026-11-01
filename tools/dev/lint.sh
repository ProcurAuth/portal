#!/bin/sh
# Checks the format and runs clippy on the whole workspace, in Docker. Nothing is written to the source tree.
# Usage: tools/dev/lint.sh
# PORTAL_SLOT: suffix of the Docker volume of the build output, as for tools/dev/test.sh.
# To format the code, run `cargo fmt --all` in the same image (see CONTRIBUTING).
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
image=${RUST_IMAGE:-rust:1.98}
vol=portal-target${PORTAL_SLOT:+-$PORTAL_SLOT}
docker volume inspect "$vol" >/dev/null 2>&1 || docker volume create "$vol" >/dev/null
docker volume inspect portal-cargo >/dev/null 2>&1 || docker volume create portal-cargo >/dev/null
exec docker run --rm -v "$root":/src:ro -v "$vol":/target -v portal-cargo:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/target -w /src "$image" sh -euc '
    rustup component add rustfmt clippy >/dev/null 2>&1
    cargo fmt --all -- --check
    cargo clippy --locked --workspace --all-targets -- -D warnings
  '
