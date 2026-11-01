#!/usr/bin/env bash
# Seeds the demo through the pages of the portal, as the superadmin and as alice. Run by up.sh.
# The accounts are database accounts. The grants are the ones of the editor of the settings pages.
# It is safe to run again: a grant that is already set stays, and the key of alice is made one time.
#
#   alice   the grants of the demo on notes, files and wiki; group.subscribe on infra and critical,
#           alert.send and alert.read on infra; the rights to make and use a key
#   bob     the grants of the demo on notes and wiki; group.subscribe on infra
#   carol   an account without a grant
#   alice   a key `demo-producer` with alert.send on infra: .secrets/alice_key
set -euo pipefail
cd "$(dirname "$0")"
# shellcheck source=lib.sh
. ./lib.sh

admin=$(mktemp); alice=$(mktemp)
trap 'rm -f "$admin" "$alice"' EXIT
[ "$(login admin "$admin")" = 303 ] || { echo "seed: the superadmin cannot sign in" >&2; exit 1; }
origin=(-H "Origin: $base")

# add_user <login>: makes the account when it does not exist (a file account exists already).
add_user() {
  local page csrf pwfile
  if [ "$(status -b "$admin" "$base/auth/settings/users/$1")" = 200 ]; then return; fi
  page=$(curl -s -b "$admin" "$base/auth/settings?add=1")
  csrf=$(form_csrf "$page" /auth/users/add)
  pwfile=$(password_file "$1")
  curl -s -o /dev/null -b "$admin" "${origin[@]}" --data-urlencode "csrf=$csrf" \
    --data-urlencode "login=$1" --data-urlencode "password@$pwfile" "$base/auth/users/add"
  rm -f "$pwfile"
  echo "seed: account $1 made"
}

# give <login> <grant>...: sets the grants of the page of the account (a Save replaces them).
# A grant text can have limit values after a semicolon: "access on service:notes;requests.minute=5".
give() {
  local login=$1 page csrf args=() i=0 g l location
  shift
  for g in "$@"; do
    args+=(--data-urlencode "grant$i=${g%%;*}")
    if [ "$g" != "${g%%;*}" ]; then
      for l in $(printf '%s' "${g#*;}" | tr ';' ' '); do args+=(--data-urlencode "limit$i.$l"); done
    fi
    i=$((i + 1))
  done
  page=$(curl -s -b "$admin" "$base/auth/settings/users/$login")
  csrf=$(form_csrf "$page" /auth/users/update)
  location=$(curl -s -o /dev/null -D - -b "$admin" "${origin[@]}" --data-urlencode "csrf=$csrf" \
    --data-urlencode "login=$login" --data-urlencode grants=1 "${args[@]}" "$base/auth/users/update" \
    | tr -d '\r' | sed -n 's/^[Ll]ocation: //p')
  case "$location" in
    *s=user-updated | *s=unchanged) echo "seed: grants of $login ($location)" ;;
    *) echo "seed: grants of $login refused ($location)" >&2; exit 1 ;;
  esac
}

# rights <login> <action>...: sets the rights on keys (the section Keys of the page of the account).
rights() {
  local login=$1 page csrf args=() a location
  shift
  for a in "$@"; do args+=(--data-urlencode "p.$a=1"); done
  page=$(curl -s -b "$admin" "$base/auth/settings/users/$login")
  csrf=$(form_csrf "$page" /auth/users/portal-rights)
  location=$(curl -s -o /dev/null -D - -b "$admin" "${origin[@]}" --data-urlencode "csrf=$csrf" \
    --data-urlencode "login=$login" "${args[@]}" "$base/auth/users/portal-rights" | tr -d '\r' | sed -n 's/^[Ll]ocation: //p')
  echo "seed: rights on keys of $login ($location)"
}

add_user alice
add_user bob
add_user carol
give alice \
  "access on service:notes;requests.minute=5" \
  "notes.read on service:notes;max_results=2" \
  "notes.write on service:notes/notebook:work" \
  "notes.delete on service:notes/notebook:work" \
  "access on service:files" \
  "access on service:wiki" \
  "group.subscribe on service:inbox/group:infra" \
  "group.subscribe on service:inbox/group:critical" \
  "alert.send on service:inbox/group:infra" \
  "alert.read on service:inbox/group:infra"
rights alice keys.own.read keys.own.create keys.own.revoke keys.own.use
give bob \
  "access on service:notes" \
  "notes.read on service:notes/notebook:public" \
  "access on service:wiki" \
  "group.subscribe on service:inbox/group:infra"

# The key of alice: made on her page of keys, shown one time, kept in .secrets/alice_key.
[ "$(login alice "$alice")" = 303 ] || { echo "seed: alice cannot sign in" >&2; exit 1; }
if [ ! -s .secrets/alice_key ] || ! curl -s -b "$alice" "$base/auth/keys" | grep -q 'demo-producer'; then
  page=$(curl -s -b "$alice" "$base/auth/keys/new")
  csrf=$(form_csrf "$page" /auth/keys/create)
  created=$(curl -s -b "$alice" "${origin[@]}" --data-urlencode "csrf=$csrf" --data-urlencode "name=demo-producer" \
    --data-urlencode "expires_at=" --data-urlencode "grant0=alert.send on service:inbox/group:infra" \
    "$base/auth/keys/create")
  key=$(printf '%s' "$created" | sed -n 's/.*id="key-text"[^>]*value="\(mcp_[A-Za-z0-9]*\)".*/\1/p')
  case "$key" in
    mcp_*) (umask 077; printf '%s' "$key" > .secrets/alice_key); chmod 644 .secrets/alice_key
           echo "seed: key demo-producer made for alice (.secrets/alice_key)" ;;
    *) echo "seed: no key text in the answer" >&2; exit 1 ;;
  esac
fi
