#!/bin/sh
# Prints the product version. The version is the field `version` of the root `[package]` in Cargo.toml.
# The field `version` of `[workspace.package]` (the crates of the phone) must be the same: the script stops if they differ.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
pkg=$(sed -n '/^\[package\]/,/^\[/s/^version *= *"\([^"]*\)".*/\1/p' "$root/Cargo.toml" | head -n1)
ws=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\([^"]*\)".*/\1/p' "$root/Cargo.toml" | head -n1)
[ -n "$pkg" ] || { echo "cannot read [package] version from Cargo.toml" >&2; exit 1; }
[ "$pkg" = "$ws" ] || { echo "version mismatch in Cargo.toml: [package] $pkg, [workspace.package] ${ws:-none}" >&2; exit 1; }
printf '%s\n' "$pkg"
