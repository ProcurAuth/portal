#!/bin/sh
# Checks the reads of the environment (tools/dev/check-env-reads.sh) and the format, and runs clippy on the whole
# workspace, in Docker. Nothing is written to the source tree.
# Usage: tools/dev/lint.sh
# PORTAL_SLOT: suffix of the Docker volume of the build output, as for tools/dev/test.sh.
# CARGO_BUILD_JOBS: when set, the number of parallel jobs of cargo in the container.
# To format the code, run `cargo fmt --all` in the same image (see CONTRIBUTING).
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
image=${RUST_IMAGE:-rust:1.98}
vol=portal-target${PORTAL_SLOT:+-$PORTAL_SLOT}
docker volume inspect "$vol" >/dev/null 2>&1 || docker volume create "$vol" >/dev/null
docker volume inspect portal-cargo >/dev/null 2>&1 || docker volume create portal-cargo >/dev/null
exec docker run --rm -v "$root":/src:ro -v "$vol":/target -v portal-cargo:/usr/local/cargo/registry \
  ${CARGO_BUILD_JOBS:+-e CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS} \
  -e CARGO_TARGET_DIR=/target -w /src "$image" sh -euc '
    sh tools/dev/check-env-reads.sh
    rustup component add rustfmt clippy >/dev/null 2>&1
    cargo fmt --all -- --check
    cargo clippy --locked --workspace --all-targets -- -D warnings
  '
