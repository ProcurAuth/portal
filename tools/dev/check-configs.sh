#!/bin/sh
# Checks every configuration file of deploy/ with `portal check-config`, in Docker, with dummy secret files.
# Usage: tools/dev/check-configs.sh [image]    (default image: portal:local, built by tools/build.sh image)
# The dummy secrets live in a temporary directory that the script deletes. They never leave this machine.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
image=${1:-portal:local}
docker image inspect "$image" >/dev/null 2>&1 || { echo "image $image not found: run tools/build.sh image" >&2; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
secrets="$tmp/secrets"
mkdir -p "$secrets"
chmod 0755 "$tmp" "$secrets"
random() { head -c 256 /dev/urandom | base64 | tr -d '=+/\n' | head -c "$1"; }
printf '%s' admin > "$secrets/superadmin_login"
random 24 > "$secrets/superadmin_password"
random 48 > "$secrets/session_secret"
head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$secrets/totp_key"
random 32 > "$secrets/db_password"
openssl genpkey -algorithm ed25519 -out "$secrets/identity_key" 2>/dev/null
portal() { docker run --rm --network none -u "$(id -u):$(id -g)" -v "$secrets:/out" "$image" "$@" >/dev/null; }
portal inbox keygen --out /out/inbox_server_key
portal inbox content-key --out /out/inbox_content_key
for t in inbox_api_key inbox_alerting_key inbox_grafana_key; do
  portal inbox token --out "/out/$t"
done
chmod 0444 "$secrets"/*

status=0
for cfg in deploy/compose/config.hcl deploy/k3s/config.hcl deploy/config.example.hcl; do
  if out=$(docker run --rm --network none -u "$(id -u):$(id -g)" \
    -e KUBERNETES_SERVICE_HOST=10.43.0.1 -e KUBERNETES_SERVICE_PORT_HTTPS=443 \
    -v "$secrets:/run/secrets:ro" -v "$root/$cfg:/etc/portal/config.hcl:ro" \
    "$image" check-config --config /etc/portal/config.hcl 2>&1); then
    echo "ok     $cfg"
  else
    echo "FAILED $cfg" >&2
    printf '%s\n' "$out" | tail -5 >&2
    status=1
  fi
done
exit "$status"
