# Helpers that seed.sh and smoke.sh share. Source this file. It reads the passwords in .secrets/credentials.txt.
# shellcheck shell=bash
base=${DEMO_BASE:-https://localhost:8088}
metrics=${DEMO_METRICS:-http://127.0.0.1:8091/metrics}
creds=.secrets/credentials.txt

# The demo certificate is self-signed: every call of curl in these scripts skips its check (-k).
# A real deployment uses a certificate that the client trusts and never uses -k.
curl() { command curl -k "$@"; }

# pw <login>: the password of an account of the demo.
pw() { if [ "$1" = admin ]; then grep '^superadmin:' "$creds"; else grep "^$1:" "$creds"; fi | sed 's/^[^/]*\/ //'; }

# password_file <login>: writes the password of an account to a new file that only this user reads, and prints its
# name. The password never goes on a command line: `ps` would show it. The caller deletes the file.
password_file() {
  local file
  file=$(umask 077; mktemp)
  printf '%s' "$(pw "$1")" > "$file"
  printf '%s' "$file"
}

# login <user> <cookie jar>: signs in and prints the HTTP status (303 when it works).
login() {
  local jar=$2 page csrf sig rd pwfile out
  page=$(curl -s -c "$jar" "$base/auth/login")
  csrf=$(printf '%s' "$page" | sed -n 's/.*name="csrf" value="\([^"]*\)".*/\1/p')
  sig=$(printf '%s' "$page" | sed -n 's/.*name="sig" value="\([^"]*\)".*/\1/p')
  rd=$(printf '%s' "$page" | sed -n 's/.*name="rd" value="\([^"]*\)".*/\1/p')
  pwfile=$(password_file "$1")
  out=$(curl -s -o /dev/null -w '%{http_code}' -b "$jar" -c "$jar" -H "Origin: $base" \
    --data-urlencode "csrf=$csrf" --data-urlencode "rd=$rd" --data-urlencode "sig=$sig" \
    --data-urlencode "login=$1" --data-urlencode "password@$pwfile" "$base/auth/login")
  rm -f "$pwfile"
  printf '%s' "$out"
}

status() { curl -s -o /dev/null -w '%{http_code}' "$@"; }

# form_csrf <page text> <form action>: the anti-CSRF token of the first form with this action.
form_csrf() {
  printf '%s' "$1" | grep -o "action=\"$2\"[^>]*><input type=\"hidden\" name=\"csrf\" value=\"[^\"]*\"" \
    | head -n 1 | sed 's/.*value="//; s/"$//'
}
