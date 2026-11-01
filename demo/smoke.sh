#!/usr/bin/env bash
# Checks the demo from a terminal: sign-in, the gate, grants, ceiling, budget, keys and the inbox.
# Run it after ./up.sh. It reads the passwords in .secrets/credentials.txt.
# Rate budgets:
#   - The budget of alice on notes is 5 uses each minute: wait one minute before a second run.
#   - Each run makes one enrollment code for bob. A code lives 10 minutes, and an account may hold 5 codes
#     that are not used (inbox.enrollment.max_active_per_account). A sixth run in 10 minutes fails at the code.
#   - Each run sends 3 alerts to the groups infra and critical. The phones that are enrolled get them.
set -uo pipefail
cd "$(dirname "$0")"
# shellcheck source=lib.sh
. ./lib.sh
fail=0

check() { # name expected actual
  if [ "$2" = "$3" ]; then echo "ok    $1"; else echo "FAIL  $1: expected $2, got $3"; fail=1; fi
}

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

# ---- the inbox
# The phone port has no session: it serves the routes of the phones and the health routes only. The API of
# the producers is on the port of the pages, through Traefik without ForwardAuth. A producer proves itself
# with a bearer token or a key.
check "inbox: phone port /healthz" 200 "$(status "$devices/healthz")"
check "inbox: phone port /readyz" 200 "$(status "$devices/readyz")"
check "inbox: the API of the producers is not on the phone port" 404 "$(status "$devices/api/version")"
check "inbox: /readyz on the port of the pages" 200 "$(status "$base/readyz")"
check "inbox: /api/version on the port of the pages, no ForwardAuth" 200 "$(status "$base/api/version")"
token=$(cat .secrets/inbox_producer_token)
akey=$(cat .secrets/alice_key)
run=$(date +%s)
send() { # <token> <idempotency key> <group>: prints the HTTP status
  status -X POST "$base/api/v1/alerts" -H "Authorization: Bearer $1" -H "Idempotency-Key: $2" \
    -H 'Content-Type: application/json' -d "{\"group\":\"$3\",\"title\":\"Smoke test $run\"}"
}
check "inbox: alert with the token of the file (infra)" 201 "$(send "$token" "smoke-$run-a" infra)"
check "inbox: the same alert again is a duplicate" 200 "$(send "$token" "smoke-$run-a" infra)"
check "inbox: alert with the token of the file (critical)" 201 "$(send "$token" "smoke-$run-b" critical)"
check "inbox: a wrong token" 401 "$(send wrong "smoke-$run-c" infra)"
check "inbox: alert with the key of alice (infra)" 201 "$(send "$akey" "smoke-$run-d" infra)"
check "inbox: the key of alice cannot send to critical" 403 "$(send "$akey" "smoke-$run-e" critical)"
check "inbox: a key that does not exist" 401 "$(send mcp_000000000000000000000000000000000000000000000000000 "smoke-$run-f" infra)"

# The tab Inbox shows the pages that the permissions open. carol has no permission of the inbox.
carol=$(mktemp)
check "carol signs in" 303 "$(login carol "$carol")"
inbox=$base/auth/settings/inbox
check "inbox page: admin opens the overview" 200 "$(status -b "$admin" "$inbox")"
check "inbox page: admin opens the phones" 200 "$(status -b "$admin" "$inbox/devices")"
check "inbox page: alice opens the overview (alert.send)" 200 "$(status -b "$alice" "$inbox")"
check "inbox page: alice cannot open the phones of all (no device.read)" 403 "$(status -b "$alice" "$inbox/devices")"
check "inbox page: bob has no overview, the tab opens My phones" 303 "$(status -b "$bob" "$inbox")"
check "inbox page: bob opens My phones" 200 "$(status -b "$bob" "$inbox/phones")"
check "inbox page: carol cannot open the overview" 403 "$(status -b "$carol" "$inbox")"
check "inbox page: carol cannot open My phones" 403 "$(status -b "$carol" "$inbox/phones")"

# An enrollment code: the page of the account makes it and shows it one time.
page=$(curl -s -b "$bob" "$inbox/phones")
csrf=$(form_csrf "$page" /auth/settings/inbox/code)
answer=$(curl -s -b "$bob" -H "Origin: $base" --data-urlencode "csrf=$csrf" "$inbox/code")
case "$answer" in
  *inbox://enroll*) echo "ok    bob makes an enrollment code" ;;
  *) echo "FAIL  bob makes an enrollment code: no link in the answer"; fail=1 ;;
esac
check "inbox: carol cannot make a code" 403 "$(status -b "$carol" -H "Origin: $base" --data-urlencode "csrf=$csrf" "$inbox/code")"

series=$(curl -s "$metrics" | grep -c '^inbox_alerts_accepted_total')
[ "$series" -gt 0 ] && echo "ok    metrics: inbox_alerts_accepted_total has $series series" || { echo "FAIL  metrics: no inbox_alerts_accepted_total"; fail=1; }

rm -f "$alice" "$bob" "$admin" "$carol"
exit $fail
