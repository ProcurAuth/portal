# Helpers that seed.sh and smoke.sh share. Source this file. It reads the passwords in .secrets/credentials.txt.
# shellcheck shell=bash
base=${DEMO_BASE:-http://localhost:8088}
metrics=${DEMO_METRICS:-http://127.0.0.1:8091/metrics}
creds=.secrets/credentials.txt

# pw <login>: the password of an account of the demo.
pw() { if [ "$1" = admin ]; then grep '^superadmin:' "$creds"; else grep "^$1:" "$creds"; fi | sed 's/^[^/]*\/ //'; }

# login <user> <cookie jar>: signs in and prints the HTTP status (303 when it works).
login() {
  local jar=$2 page csrf sig rd token
  page=$(curl -s -c "$jar" "$base/auth/login")
  csrf=$(printf '%s' "$page" | sed -n 's/.*name="csrf" value="\([^"]*\)".*/\1/p')
  sig=$(printf '%s' "$page" | sed -n 's/.*name="sig" value="\([^"]*\)".*/\1/p')
  rd=$(printf '%s' "$page" | sed -n 's/.*name="rd" value="\([^"]*\)".*/\1/p')
  token=$(printf '%s' "$page" | sed -n 's/.*name="token" value="\([^"]*\)".*/\1/p')
  # The form has a minimum time between its display and its submit.
  sleep 1.2
  curl -s -o /dev/null -w '%{http_code}' -b "$jar" -c "$jar" -H "Origin: $base" \
    --data-urlencode "csrf=$csrf" --data-urlencode "rd=$rd" --data-urlencode "sig=$sig" --data-urlencode "token=$token" \
    --data-urlencode "login=$1" --data-urlencode "password=$(pw "$1")" "$base/auth/login"
}

status() { curl -s -o /dev/null -w '%{http_code}' "$@"; }

# form_csrf <page text> <form action>: the anti-CSRF token of the first form with this action.
form_csrf() {
  printf '%s' "$1" | grep -o "action=\"$2\"[^>]*><input type=\"hidden\" name=\"csrf\" value=\"[^\"]*\"" \
    | head -n 1 | sed 's/.*value="//; s/"$//'
}
