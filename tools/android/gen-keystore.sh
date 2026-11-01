#!/bin/sh
# Creates the release signing keystore once: secrets/android-release.jks and
# secrets/android-release.env (passwords). Back both up: losing them means the
# app can no longer be updated in place on phones.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$root/secrets"
chmod 700 "$root/secrets"
jks="$root/secrets/android-release.jks"
envf="$root/secrets/android-release.env"
if [ -e "$jks" ] || [ -e "$envf" ]; then
  echo "refusing to overwrite $jks / $envf" >&2
  exit 1
fi
docker image inspect inbox-android >/dev/null 2>&1 || docker build -q -t inbox-android "$root/tools/android" >/dev/null
pw=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
docker run --rm -u "$(id -u):$(id -g)" -v "$root/secrets:/out" inbox-android \
  keytool -genkeypair -noprompt -keystore /out/android-release.jks -storetype PKCS12 \
    -alias inbox -keyalg RSA -keysize 4096 -validity 10000 \
    -storepass "$pw" -keypass "$pw" -dname "CN=Inbox, O=Inbox"
umask 077
cat > "$envf" <<EOT
INBOX_KEYSTORE=$jks
INBOX_KEYSTORE_PASSWORD=$pw
INBOX_KEY_ALIAS=inbox
INBOX_KEY_PASSWORD=$pw
EOT
chmod 600 "$jks" "$envf"
echo "keystore: $jks"
echo "passwords: $envf"
