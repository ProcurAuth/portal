#!/usr/bin/env bash
# Starts the local demo of the portal. Docker is the only tool that this script needs, with openssl for the keys.
# Nothing is published to a registry: the portal image stays on this machine.
set -euo pipefail
cd "$(dirname "$0")"

secrets=.secrets
mkdir -p "$secrets"
chmod 700 "$secrets"

random() { openssl rand -base64 48 | tr -d '=+/\n' | head -c "$1"; }

echo "Building the portal image (local/portal:demo)..."
docker build -q -t local/portal:demo -f portal.Dockerfile ..

if [ ! -f "$secrets/credentials.txt" ]; then
  umask 077
  printf 'admin' > "$secrets/superadmin_login"
  random 24 > "$secrets/superadmin_password"
  random 20 > "$secrets/alice_password"
  random 20 > "$secrets/bob_password"
  random 43 > "$secrets/pdp_token_notes"
  openssl genpkey -algorithm ed25519 -out "$secrets/identity_key.pem" 2> /dev/null
  openssl pkey -in "$secrets/identity_key.pem" -pubout -out "$secrets/identity_pub.pem"
  for user in alice bob; do
    docker run --rm -i local/portal:demo hash-password < "$secrets/${user}_password" > "$secrets/${user}_hash"
  done
  {
    echo "URL:        http://localhost:8088/"
    echo "superadmin: admin / $(cat "$secrets/superadmin_password")"
    echo "alice:      alice / $(cat "$secrets/alice_password")"
    echo "bob:        bob / $(cat "$secrets/bob_password")"
  } > "$secrets/credentials.txt"
  # The service notes runs as another user, and it must read its two files.
  chmod 644 "$secrets"/* 
fi

docker compose up -d --build
echo
echo "The demo runs at http://localhost:8088/"
echo "The logins and the passwords are in demo/$secrets/credentials.txt"
echo "Stop it with: docker compose -f demo/docker-compose.yml down -v"
