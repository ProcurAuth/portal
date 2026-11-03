#!/usr/bin/env bash
# Seeds the demo through the pages of the portal, as the superadmin and as alice. Run by up.sh.
# The accounts are database accounts. The grants are the ones of the editor of the settings pages.
# It is safe to run again: a grant that is already set stays, and the key of alice is made one time.
#
#   alice   the grants of the demo on notes, files and wiki; inbox.receive on infra and critical,
#           alert.send and alert.read on infra, alert.send_user (an alert to any account); the rights
#           to make and use the keys of both kinds
#   bob     the grants of the demo on notes and wiki; inbox.receive on infra
#   carol   an account without a grant
#   alice   an API key `demo-sender` with alert.send on infra: .secrets/alice_key
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
  local page csrf
  if [ "$(status -b "$admin" "$base/auth/settings/users/$1")" = 200 ]; then return; fi
  page=$(curl -s -b "$admin" "$base/auth/settings/new-user")
  csrf=$(form_csrf "$page" /auth/users/add)
  curl -s -o /dev/null -b "$admin" "${origin[@]}" --data-urlencode "csrf=$csrf" \
    --data-urlencode "login=$1" --data-urlencode "password=$(pw "$1")" "$base/auth/users/add"
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
  "inbox.receive on service:portal/inbox_group:infra" \
  "inbox.receive on service:portal/inbox_group:critical" \
  "alert.send on service:portal/inbox_group:infra" \
  "alert.send_user on service:portal" \
  "alert.read on service:portal/inbox_group:infra" \
  "keys.own.read on service:portal" \
  "keys.own.revoke on service:portal" \
  "keys.own.create on service:portal/key_kind:mcp" \
  "keys.own.use on service:portal/key_kind:mcp" \
  "keys.own.create on service:portal/key_kind:api" \
  "keys.own.use on service:portal/key_kind:api"
give bob \
  "access on service:notes" \
  "notes.read on service:notes/notebook:public" \
  "access on service:wiki" \
  "inbox.receive on service:portal/inbox_group:infra"

# The key of alice: made in the tab Access, shown one time, kept in .secrets/alice_key.
[ "$(login alice "$alice")" = 303 ] || { echo "seed: alice cannot sign in" >&2; exit 1; }
if [ ! -s .secrets/alice_key ] || ! curl -s -b "$alice" "$base/auth/settings" | grep -q 'demo-sender'; then
  page=$(curl -s -b "$alice" "$base/auth/keys/new?kind=api")
  csrf=$(form_csrf "$page" /auth/keys/create)
  created=$(curl -s -b "$alice" "${origin[@]}" --data-urlencode "csrf=$csrf" --data-urlencode "kind=api" --data-urlencode "name=demo-sender" \
    --data-urlencode "expires_at=" --data-urlencode "grant0=alert.send on service:portal/inbox_group:infra" \
    "$base/auth/keys/create")
  key=$(printf '%s' "$created" | sed -n 's/.*id="key-text"[^>]*value="\(api_[A-Za-z0-9]*\)".*/\1/p')
  case "$key" in
    api_*) (umask 077; printf '%s' "$key" > .secrets/alice_key); chmod 644 .secrets/alice_key
           echo "seed: key demo-sender made for alice (.secrets/alice_key)" ;;
    *) echo "seed: no key text in the answer" >&2; exit 1 ;;
  esac
fi
