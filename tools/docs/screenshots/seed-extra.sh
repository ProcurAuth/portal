#!/usr/bin/env bash
# Add demo data for the screenshots to a running demo (demo/up.sh): three keys of alice with limits,
# some sign-ins of the other accounts, a few refused sign-ins, and alerts from the sender key.
# The script reads the passwords in demo/.secrets/credentials.txt. It changes the demo only.
# It is safe to run again: a key that exists stays.
set -euo pipefail
cd "$(dirname "$0")/../../../demo"
# shellcheck source=../../../demo/lib.sh
. ./lib.sh

alice=$(mktemp); bob=$(mktemp); carol=$(mktemp); admin=$(mktemp)
trap 'rm -f "$alice" "$bob" "$carol" "$admin"' EXIT
origin=(-H "Origin: $base")

# bad_login <user> <password>: a refused sign-in.
bad_login() {
  local jar page csrf sig rd token
  jar=$(mktemp)
  page=$(curl -s -c "$jar" "$base/auth/login")
  csrf=$(field "$page" csrf); sig=$(field "$page" sig); rd=$(field "$page" rd); token=$(field "$page" token)
  sleep 1.2
  curl -s -o /dev/null -b "$jar" -c "$jar" "${origin[@]}" --data-urlencode "csrf=$csrf" --data-urlencode "rd=$rd" \
    --data-urlencode "sig=$sig" --data-urlencode "token=$token" --data-urlencode "login=$1" --data-urlencode "password=$2" "$base/auth/login"
  rm -f "$jar"
}

# make_key <jar> <kind> <name> <grant>...: makes a key. A grant can have limit values after a semicolon.
make_key() {
  local jar=$1 kind=$2 name=$3 page csrf args=() i=0 g l
  shift 3
  if curl -s -b "$jar" "$base/auth/settings" | grep -q ">$name<"; then return; fi
  for g in "$@"; do
    args+=(--data-urlencode "grant$i=${g%%;*}")
    if [ "$g" != "${g%%;*}" ]; then
      for l in $(printf '%s' "${g#*;}" | tr ';' ' '); do args+=(--data-urlencode "limit$i.$l"); done
    fi
    i=$((i + 1))
  done
  page=$(curl -s -b "$jar" "$base/auth/keys/new?kind=$kind")
  csrf=$(form_csrf "$page" /auth/keys/create)
  curl -s -o /dev/null -b "$jar" "${origin[@]}" --data-urlencode "csrf=$csrf" --data-urlencode "kind=$kind" \
    --data-urlencode "name=$name" --data-urlencode "expires_at=" "${args[@]}" "$base/auth/keys/create"
  echo "seed-extra: key $name made"
}

[ "$(login alice "$alice")" = 303 ] || { echo "seed-extra: alice cannot sign in" >&2; exit 1; }
# The superadmin owns the key ops-agent: the page of a key opens for its owner only.
[ "$(login admin "$admin")" = 303 ] || { echo "seed-extra: admin cannot sign in" >&2; exit 1; }
make_key "$admin" mcp ops-agent \
  "access on service:notes;requests.minute=3" \
  "notes.read on service:notes/notebook:work;max_results=1" \
  "notes.write on service:notes/notebook:work"
make_key "$alice" mcp wiki-reader "access on service:wiki"
make_key "$alice" api grafana-alerts "alert.send on service:portal/inbox_group:infra"

# The id of the key ops-agent: make_plan.py needs it for the page of the key.
curl -s -b "$admin" "$base/auth/settings" | grep -o 'name="id\.[0-9a-f]*" value="1" aria-label="Select ops-agent"' \
  | sed 's/^name="id\.\([0-9a-f]*\)".*/\1/' > .secrets/shots_key_id

# Sign-ins of other accounts and some refused sign-ins, for the history page.
login bob "$bob" > /dev/null
login carol "$carol" > /dev/null
bad_login alice wrong-password
bad_login mallory wrong-password
bad_login bob wrong-password
login alice "$alice" > /dev/null

# Alerts from the sender key of the file, for the inbox of the phone.
key=$(cat .secrets/inbox_api_key)
for t in "Disk 86 % on db-2" "Backup finished" "Certificate renews in 9 days"; do
  curl -s -o /dev/null -X POST "$base/api/v1/alerts" -H "Authorization: Bearer $key" -H "Idempotency-Key: shots-$(printf '%s' "$t" | tr -c 'a-z0-9' '-')" \
    -H "Content-Type: application/json" -d "{\"group\":\"infra\",\"title\":\"$t\"}"
done
echo "seed-extra: done"
