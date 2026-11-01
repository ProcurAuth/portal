#!/usr/bin/env bash
# Starts the local demo of the portal with the inbox. Docker is the only tool that this script needs,
# with openssl for the keys and curl for the seed. Nothing is published to a registry: the portal
# image stays on this machine.
#
# The superadmin password is `test`. This is for a demo on one machine only.
#
# Traefik serves the pages over HTTPS with a self-signed certificate. This script makes the certificate in
# .secrets/ with openssl (subjectAltName localhost, 127.0.0.1 and 172.17.0.1, valid for 825 days). The browser
# shows a warning for it: see README.md. The portal does not know TLS.
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
[ -f "$secrets/superadmin_password" ] || printf 'test' > "$secrets/superadmin_password"
for user in alice bob carol; do
  [ -f "$secrets/${user}_password" ] || random 20 > "$secrets/${user}_password"
done
[ -f "$secrets/pdp_token_notes" ] || random 43 > "$secrets/pdp_token_notes"
if [ ! -f "$secrets/identity_key.pem" ]; then
  openssl genpkey -algorithm ed25519 -out "$secrets/identity_key.pem" 2> /dev/null
  openssl pkey -in "$secrets/identity_key.pem" -pubout -out "$secrets/identity_pub.pem"
fi
# The self-signed certificate of Traefik. A file that exists stays: delete tls.crt and tls.key to make them again.
if [ ! -f "$secrets/tls.crt" ] || [ ! -f "$secrets/tls.key" ]; then
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 825 \
    -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1,IP:172.17.0.1" \
    -keyout "$secrets/tls.key" -out "$secrets/tls.crt" 2> /dev/null
fi
# The Noise key of the inbox server and the token of the producer. A new key invalidates every phone.
inbox() { docker run --rm --network none -u "$(id -u):$(id -g)" -v "$PWD/$secrets:/out" local/portal:demo inbox "$@"; }
[ -f "$secrets/inbox_server_key" ] || inbox keygen --out /out/inbox_server_key > /dev/null
[ -f "$secrets/inbox_producer_token" ] || inbox token --out /out/inbox_producer_token > /dev/null
{
  echo "URL:        https://localhost:8088/  (self-signed certificate: curl -k, accept the warning in a browser)"
  echo "superadmin: admin / $(cat "$secrets/superadmin_password")"
  echo "alice:      alice / $(cat "$secrets/alice_password")"
  echo "bob:        bob / $(cat "$secrets/bob_password")"
  echo "carol:      carol / $(cat "$secrets/carol_password")"
} > "$secrets/credentials.txt"
# The service notes runs as another user, and it must read its files.
chmod 644 "$secrets"/*

docker compose up -d --build

# The portal opens its port when its catalogue is complete. Traefik then needs a moment to route to it.
code() { curl -sk -o /dev/null -w '%{http_code}' "$1" || true; }
echo "Waiting for the portal..."
for _ in $(seq 1 120); do
  [ "$(code http://localhost:8091/healthz)" = 200 ] && [ "$(code https://localhost:8088/auth/login)" = 200 ] && break
  sleep 2
done
[ "$(code https://localhost:8088/auth/login)" = 200 ] \
  || { echo "The portal did not start. Read: docker logs portal-demo" >&2; exit 1; }

./seed.sh

echo
echo "The demo runs at https://localhost:8088/ (self-signed certificate: use curl -k, or accept the warning)"
echo "The producers use the same address: POST https://localhost:8088/api/v1/alerts"
echo "The phones use the plain entrypoint 8090 (demo only). An emulator reaches it at http://172.17.0.1:8090"
echo "The logins and the passwords are in demo/$secrets/credentials.txt"
echo "Stop it with: docker compose -f demo/docker-compose.yml down -v"
