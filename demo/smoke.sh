#!/usr/bin/env bash
# Checks the demo from a terminal: sign-in, the gate, grants, ceiling, budget and keys.
# Run it after ./up.sh. It reads the passwords in .secrets/credentials.txt.
# The budget of alice is 5 uses each minute: wait one minute before a second run.
set -uo pipefail
cd "$(dirname "$0")"
base=http://localhost:8088
fail=0
creds=.secrets/credentials.txt
pw() { if [ "$1" = admin ]; then grep '^superadmin:' "$creds"; else grep "^$1:" "$creds"; fi | sed 's/^[^/]*\/ //'; }

check() { # name expected actual
  if [ "$2" = "$3" ]; then echo "ok    $1"; else echo "FAIL  $1: expected $2, got $3"; fail=1; fi
}

login() { # user jar
  local jar=$2 page csrf
  page=$(curl -s -c "$jar" "$base/auth/login")
  csrf=$(printf '%s' "$page" | sed -n 's/.*name="csrf" value="\([^"]*\)".*/\1/p')
  local sig rd
  sig=$(printf '%s' "$page" | sed -n 's/.*name="sig" value="\([^"]*\)".*/\1/p')
  rd=$(printf '%s' "$page" | sed -n 's/.*name="rd" value="\([^"]*\)".*/\1/p')
  curl -s -o /dev/null -w '%{http_code}' -b "$jar" -c "$jar" -H "Origin: $base" \
    --data-urlencode "csrf=$csrf" --data-urlencode "rd=$rd" --data-urlencode "sig=$sig" \
    --data-urlencode "login=$1" --data-urlencode "password=$(pw "$1")" "$base/auth/login"
}

status() { curl -s -o /dev/null -w '%{http_code}' "$@"; }

alice=$(mktemp); bob=$(mktemp)
check "alice signs in" 303 "$(login alice "$alice")"
check "bob signs in" 303 "$(login bob "$bob")"

check "no session: redirect to the login" 302 "$(status "$base/notes/")"
check "alice opens notes" 200 "$(status -b "$alice" "$base/notes/")"
check "alice opens files" 200 "$(status -b "$alice" "$base/files/")"
check "bob opens notes" 200 "$(status -b "$bob" "$base/notes/")"
check "bob cannot open files" 302 "$(status -b "$bob" "$base/files/")"

api=$base/notes/api
check "bob reads the public notebook" 200 "$(status -b "$bob" "$api/notebooks/public/notes")"
check "bob cannot read the work notebook" 403 "$(status -b "$bob" "$api/notebooks/work/notes")"
check "bob cannot write" 403 "$(status -b "$bob" -H 'Content-Type: application/json' -d '{"text":"x"}' "$api/notebooks/public/notes")"

shown=$(curl -s -b "$alice" "$api/notebooks/work/notes" | python3 -c 'import sys,json; print(len(json.load(sys.stdin)["notes"]))')
check "alice: the ceiling max_results cuts 3 notes to 2" 2 "$shown"
check "alice writes in work" 201 "$(status -b "$alice" -H 'Content-Type: application/json' -d '{"text":"from smoke"}' "$api/notebooks/work/notes")"
check "alice cannot write in public" 403 "$(status -b "$alice" -H 'Content-Type: application/json' -d '{"text":"x"}' "$api/notebooks/public/notes")"

# The budget is 5 uses each minute. The uses above counted: bob has none, alice has 3 (read, write, refused write is not counted).
codes=""
for _ in 1 2 3 4 5 6; do codes="$codes $(status -b "$alice" "$api/notebooks/public/notes")"; done
case "$codes" in *429*) echo "ok    alice: the budget gives 429 ($codes )";; *) echo "FAIL  alice: no 429 ($codes )"; fail=1;; esac
retry=$(curl -s -D - -o /dev/null -b "$alice" "$api/notebooks/public/notes" | grep -i '^retry-after:' | tr -d '\r')
[ -n "$retry" ] && echo "ok    the answer 429 has $retry" || { echo "FAIL  no Retry-After"; fail=1; }

check "bad key: 401" 401 "$(status -H 'Authorization: Bearer mcp_000000000000000000000000000000000000000000000000000' "$api/whoami")"
check "no key text format: the user route needs a session" 302 "$(status -H 'Authorization: Bearer abc' "$api/whoami")"

# Keys: the superadmin makes a key, and the key acts through the key route of Traefik.
admin=$(mktemp)
check "admin signs in" 303 "$(login admin "$admin")"
keys_page=$(curl -s -b "$admin" "$base/auth/keys/new")
csrf=$(printf '%s' "$keys_page" | sed -n 's/.*action="\/auth\/keys\/create"[^>]*><input type="hidden" name="csrf" value="\([^"]*\)".*/\1/p')
# The editor writes each ticked grant as a field `grant<n>` with its text. The form accepts the same fields from a script.
g_access=grant0
g_read=grant1
created=$(curl -s -b "$admin" -H "Origin: $base" --data-urlencode "csrf=$csrf" --data-urlencode "name=smoke" \
  --data-urlencode "expires_at=" --data-urlencode "$g_access=access on service:notes" \
  --data-urlencode "$g_read=notes.read on service:notes" "$base/auth/keys/create")
key=$(printf '%s' "$created" | sed -n 's/.*id="key-text"[^>]*value="\(mcp_[A-Za-z0-9]*\)".*/\1/p')
case "$key" in mcp_*) echo "ok    the superadmin made a key (53 characters: ${#key})";; *) echo "FAIL  no key text"; fail=1;; esac
auth="Authorization: Bearer $key"
check "key: whoami" 200 "$(status -H "$auth" "$api/whoami")"
sub=$(curl -s -H "$auth" "$api/whoami" | python3 -c 'import sys,json; print(json.load(sys.stdin)["claims"]["sub"][:4])')
check "key: the subject of the token is a key" "key:" "$sub"
check "key: read a notebook" 200 "$(status -H "$auth" "$api/notebooks/public/notes")"
check "key: cannot write (no grant)" 403 "$(status -H "$auth" -H 'Content-Type: application/json' -d '{"text":"x"}' "$api/notebooks/public/notes")"
check "a key opens no route of persons (files has no key route)" 302 "$(status -H "$auth" "$base/files/")"

rm -f "$alice" "$bob" "$admin"
exit $fail
