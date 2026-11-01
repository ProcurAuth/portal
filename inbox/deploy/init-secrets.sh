#!/bin/sh
# Creates the secrets used by docker-compose.yml in deploy/secrets/:
#   server_key         OPAQUE server setup + Noise static key (inbox keygen)
#   device_credential  shared device password record   (inbox credential)
#   alerting_token     producer API / webhook token     (inbox token)
#
# The admin front has no password: it is protected by where it listens (see
# docker-compose.yml, bound to localhost) and by admin.allowed_hosts.
#
# Existing files are never overwritten: delete one explicitly to regenerate it.
# Replacing server_key invalidates every enrolled device and the device credential.
#
# Non-interactive use: INBOX_DEVICE_PASSWORD.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
dir="$here/secrets"
image="inbox:${INBOX_IMAGE_TAG:-local}"

if ! docker image inspect "$image" >/dev/null 2>&1; then
  echo "building $image ..."
  docker compose -f "$here/docker-compose.yml" build inbox
fi

mkdir -p "$dir"
chmod 0700 "$dir"

inbox() {
  docker run --rm -i --network none -u "$(id -u):$(id -g)" -v "$dir:/out" "$image" "$@"
}

ask() { # ask <label> <env var value>
  if [ -n "$2" ]; then
    printf '%s' "$2"
    return
  fi
  [ -t 0 ] || { echo "$1 password required (set it in the environment)" >&2; exit 1; }
  stty -echo
  printf '%s password: ' "$1" >&2; read -r first; printf '\n' >&2
  printf 'again: ' >&2; read -r second; printf '\n' >&2
  stty echo
  [ "$first" = "$second" ] || { echo "passwords differ" >&2; exit 1; }
  printf '%s' "$first"
}

seal() {
  chmod 0444 "$dir/$1"
  echo "created secrets/$1"
}

if [ ! -f "$dir/server_key" ]; then
  inbox keygen --out /out/server_key
  seal server_key
fi

if [ ! -f "$dir/device_credential" ]; then
  pw=$(ask "device (shared by every phone, >= 8 characters)" "${INBOX_DEVICE_PASSWORD:-}")
  printf '%s\n' "$pw" | inbox credential --server-key /out/server_key \
    --out /out/device_credential --password-stdin
  seal device_credential
fi

if [ ! -f "$dir/alerting_token" ]; then
  inbox token --out /out/alerting_token
  seal alerting_token
fi

echo "secrets ready in $dir (back up server_key: losing it means re-enrolling every phone)"
