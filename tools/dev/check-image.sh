#!/bin/sh
# Checks the local image portal:local: the binary is static, the image runs, the configuration files of deploy/ pass
# `portal check-config`, and the Compose stack answers (tools/dev/smoke.sh). The image comes from `tools/build.sh image`.
# Usage: tools/dev/check-image.sh [image]    (default image: portal:local)
# The script pushes nothing. It writes only to a temporary directory that it deletes.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
image=${1:-portal:local}
docker image inspect "$image" >/dev/null 2>&1 || { echo "image $image not found: run tools/build.sh image" >&2; exit 1; }
tmp=$(mktemp -d)
name=portal-binary-$$
cleanup() { docker rm -f "$name" >/dev/null 2>&1 || true; rm -rf "$tmp"; }
trap cleanup EXIT
echo "== the binary is static"
docker create --name "$name" "$image" >/dev/null
docker cp "$name:/portal" "$tmp/portal"
docker rm "$name" >/dev/null
python3 "$root/tools/release/check-static.py" "$tmp/portal"
docker run --rm "$image" version
echo "== the configuration files of deploy/"
"$root/tools/dev/check-configs.sh" "$image"
echo "== the Compose stack"
"$root/tools/dev/smoke.sh" "$image"
