#!/bin/sh
# Runs the heavy checks on this machine. The nightly workflow (.github/workflows/nightly.yml) runs the same checks.
# Usage: tools/dev/heavy.sh [lint] [tests] [image] [android]    (default: lint tests image; no name means these three)
#   lint     the reads of the environment, the format and clippy    (tools/dev/lint.sh)
#   tests    the tests of the workspace, then the end-to-end tests of the Inbox on PostgreSQL   (tools/dev/test.sh)
#   image    the image, its binary, the configuration files and the Compose stack   (tools/build.sh image, tools/dev/check-image.sh)
#   android  the Android app with a throwaway signing key (tools/android/build-apk.sh)
# Each check runs in Docker and writes nothing to the source tree. Set PROCURAUTH_CONFORMANCE to the directory
# conformance/ of the procurauth checkout, or clone procurauth next to this repository. PROCURAUTH_CONFORMANCE_REQUIRED=1
# is set here, as in CI. Set CARGO_BUILD_JOBS to limit the jobs of cargo. The script pushes nothing to a registry.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
[ "$#" -gt 0 ] || set -- lint tests image
for step in "$@"; do
  case "$step" in lint | tests | image | android) ;; *) echo "heavy.sh: unknown check $step" >&2; exit 2 ;; esac
done
for step in "$@"; do
  echo "=== $step"
  case "$step" in
    lint) tools/dev/lint.sh ;;
    tests)
      export PROCURAUTH_CONFORMANCE_REQUIRED=1
      tools/dev/test.sh --workspace --no-fail-fast
      INBOX_E2E_BACKEND=postgres tools/dev/test.sh --test inbox_e2e
      ;;
    image)
      tools/build.sh image
      tools/dev/check-image.sh
      ;;
    android)
      tmp=$(mktemp -d)
      trap 'rm -rf "$tmp"' EXIT
      docker image inspect inbox-android >/dev/null 2>&1 || docker build -q -t inbox-android tools/android >/dev/null
      docker run --rm -u "$(id -u):$(id -g)" -v "$tmp:/out" inbox-android keytool -genkeypair -noprompt \
        -keystore /out/ci.jks -storepass ci-store-pass -keypass ci-store-pass -alias ci -keyalg RSA -keysize 2048 \
        -validity 1 -dname "CN=Inbox CI" >/dev/null 2>&1
      INBOX_KEYSTORE=$tmp/ci.jks INBOX_KEYSTORE_PASSWORD=ci-store-pass INBOX_KEY_ALIAS=ci INBOX_KEY_PASSWORD=ci-store-pass \
        tools/android/build-apk.sh
      ;;
  esac
done
echo "heavy checks passed: $*"
