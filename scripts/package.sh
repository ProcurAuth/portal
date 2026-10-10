#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo "usage: $0 VERSION OS ARCH BINARY" >&2
  exit 2
fi

version="$1"
os="$2"
arch="$3"
binary="$4"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out_dir="$root/dist"
archive="$out_dir/portal_${version}_${os}_${arch}.tgz"

[ -f "$binary" ] || { echo "binary not found: $binary" >&2; exit 1; }
mkdir -p "$out_dir"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
install -m 0755 "$binary" "$tmp/portal"

tar --sort=name \
    --mtime='UTC 1970-01-01' \
    --owner=0 --group=0 --numeric-owner \
    --pax-option=delete=atime,delete=ctime \
    -C "$tmp" -cf - portal | gzip -n > "$archive"

printf '%s\n' "$archive"
