#!/bin/sh
# Makes the secret files of the Compose stack in deploy/compose/secrets/ (or in the directory of the first argument):
#
#   superadmin_login       login of the superadmin ("admin")
#   superadmin_password    random password of the superadmin
#   session_secret         signing key of the forms (all instances use the same key)
#   totp_key               key that encrypts the TOTP secrets (64 hexadecimal characters)
#   db_password            password of the PostgreSQL user
#   inbox_server_key       Noise key of the inbox server (portal inbox keygen)
#   inbox_api_key          bearer key of the sender "local" (portal inbox token)
#   inbox_content_key      key that seals the contents of the items (portal inbox content-key)
#
# An existing file is never replaced: delete it to make it again.
# A new inbox_server_key makes every enrolled phone and every enrollment code invalid: back it up.
# The content key stays in this directory, outside the volume of the database (postgres_data) and outside its backups.
# Back it up apart. A lost key makes the sealed contents unreadable.
# There is no shared device password. A phone enrolls with a code that a signed-in account makes.
#
# Environment:
#   PORTAL_IMAGE   image that makes the keys (default: portal:local). The script builds it when it is missing.
#   PORTAL_SUPERADMIN_LOGIN, PORTAL_SUPERADMIN_PASSWORD   use these values for the first run (optional)
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
dir=${1:-$here/secrets}
image=${PORTAL_IMAGE:-portal:local}

if ! docker image inspect "$image" >/dev/null 2>&1; then
  echo "image $image not found: building it" >&2
  docker build -f "$root/docker/Dockerfile" --target runtime -t "$image" "$root"
fi

umask 077
mkdir -p "$dir"
dir=$(cd "$dir" && pwd)
chmod 0700 "$dir"

# random <number of characters>: letters and digits from the system generator.
random() {
  head -c 256 /dev/urandom | base64 | tr -d '=+/\n' | head -c "$1"
}

# seal <name>: the container user must read the file, other users of the host must not reach the directory.
seal() {
  chmod 0444 "$dir/$1"
  echo "created $dir/$1"
}

portal() {
  docker run --rm --network none -u "$(id -u):$(id -g)" -v "$dir:/out" "$image" "$@"
}

if [ ! -f "$dir/superadmin_login" ]; then
  printf '%s' "${PORTAL_SUPERADMIN_LOGIN:-admin}" > "$dir/superadmin_login"
  seal superadmin_login
fi
if [ ! -f "$dir/superadmin_password" ]; then
  printf '%s' "${PORTAL_SUPERADMIN_PASSWORD:-$(random 24)}" > "$dir/superadmin_password"
  seal superadmin_password
fi
if [ ! -f "$dir/session_secret" ]; then
  random 48 > "$dir/session_secret"
  seal session_secret
fi
if [ ! -f "$dir/totp_key" ]; then
  head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$dir/totp_key"
  seal totp_key
fi
if [ ! -f "$dir/db_password" ]; then
  random 32 > "$dir/db_password"
  seal db_password
fi
if [ ! -f "$dir/inbox_server_key" ]; then
  portal inbox keygen --out /out/inbox_server_key
  seal inbox_server_key
fi
if [ ! -f "$dir/inbox_api_key" ]; then
  portal inbox token --out /out/inbox_api_key
  seal inbox_api_key
fi

if [ ! -f "$dir/inbox_content_key" ]; then
  portal inbox content-key --out /out/inbox_content_key
  seal inbox_content_key
fi

echo "secrets ready in $dir"
echo "sign in as the login in $dir/superadmin_login, with the password in $dir/superadmin_password"
