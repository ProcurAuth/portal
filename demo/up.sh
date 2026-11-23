#!/usr/bin/env bash
# Starts the local demo of the portal with the inbox. Docker is the only tool that this script needs,
# with openssl for the keys and curl for the seed. Nothing is published to a registry: the portal
# image stays on this machine.
#
# Each password is random, the superadmin password too. The script writes them to .secrets/credentials.txt.
# This is for a demo on one machine only.
set -euo pipefail
cd "$(dirname "$0")"

secrets=.secrets
mkdir -p "$secrets"
chmod 700 "$secrets"

random() { openssl rand -base64 48 | tr -d '=+/\n' | head -c "$1"; }

echo "Building the portal image (local/portal:demo)..."
docker build -q -t local/portal:demo -f ../docker/Dockerfile --target runtime ..

umask 077
# Each file is made one time. A file that exists stays: delete it to make it again.
[ -f "$secrets/superadmin_login" ] || printf 'admin' > "$secrets/superadmin_login"
[ -f "$secrets/superadmin_password" ] || random 20 > "$secrets/superadmin_password"
for user in alice bob carol; do
  [ -f "$secrets/${user}_password" ] || random 20 > "$secrets/${user}_password"
done
# The key of the TOTP secrets: 32 random bytes as 64 hexadecimal characters.
[ -f "$secrets/totp_key" ] || openssl rand -hex 32 > "$secrets/totp_key"
[ -f "$secrets/pdp_token_notes" ] || random 43 > "$secrets/pdp_token_notes"
if [ ! -f "$secrets/identity_key.pem" ]; then
  openssl genpkey -algorithm ed25519 -out "$secrets/identity_key.pem" 2> /dev/null
  openssl pkey -in "$secrets/identity_key.pem" -pubout -out "$secrets/identity_pub.pem"
fi
# The Noise key of the inbox server and the token of the producer. A new key invalidates every phone.
inbox() { docker run --rm --network none -u "$(id -u):$(id -g)" -v "$PWD/$secrets:/out" local/portal:demo inbox "$@"; }
[ -f "$secrets/inbox_server_key" ] || inbox keygen --out /out/inbox_server_key > /dev/null
[ -f "$secrets/inbox_api_key" ] || inbox token --out /out/inbox_api_key > /dev/null
[ -f "$secrets/inbox_content_key" ] || inbox content-key --out /out/inbox_content_key > /dev/null 2>&1
{
  echo "URL:        http://localhost:8088/"
  echo "superadmin: admin / $(cat "$secrets/superadmin_password")"
  echo "alice:      alice / $(cat "$secrets/alice_password")"
  echo "bob:        bob / $(cat "$secrets/bob_password")"
  echo "carol:      carol / $(cat "$secrets/carol_password")"
} > "$secrets/credentials.txt"
# The service notes runs as another user, and it must read its files.
chmod 644 "$secrets"/*

docker compose up -d --build

# The portal opens its port when its catalogue is complete. Traefik then needs a moment to route to it.
code() { curl -s -o /dev/null -w '%{http_code}' "$1" || true; }
echo "Waiting for the portal..."
for _ in $(seq 1 120); do
  [ "$(code http://localhost:8091/healthz)" = 200 ] && [ "$(code http://localhost:8088/auth/login)" = 200 ] && break
  sleep 2
done
[ "$(code http://localhost:8088/auth/login)" = 200 ] \
  || { echo "The portal did not start. Read: docker logs portal-demo" >&2; exit 1; }

./seed.sh

echo
echo "The demo runs at http://localhost:8088/"
echo "The producers use the same address: POST http://localhost:8088/api/v1/alerts"
echo "The phones use the same port too. An emulator reaches it at http://172.17.0.1:8088"
echo "The logins and the passwords are in demo/$secrets/credentials.txt"
echo "Stop it with: docker compose -f demo/docker-compose.yml down -v"
