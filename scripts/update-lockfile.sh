#!/usr/bin/env bash
set -euo pipefail

# Intentional maintenance command. Run this only when Cargo.toml changes, review
# the resulting Cargo.lock diff, then commit it. Normal CI/release/build paths
# never regenerate dependency resolution.
docker run --rm \
  --volume "$PWD:/work" \
  --workdir /work \
  rust:1.98 \
  bash -euxo pipefail -c '
    cargo generate-lockfile
    cargo metadata --locked --no-deps --format-version 1 >/dev/null
  '
