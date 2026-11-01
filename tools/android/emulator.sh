#!/bin/sh
# Test the APK without a phone: an Android 14 emulator in Docker, shown in the
# browser (noVNC), with the APK installed.
#
#   tools/android/emulator.sh start [apk]   boot, install, print the URL
#   tools/android/emulator.sh install [apk] reinstall the APK (keeps app data)
#   tools/android/emulator.sh logs          app logs (logcat, Inbox only)
#   tools/android/emulator.sh stop          remove the emulator
#
# Requires /dev/kvm. The emulator reaches the host through the Docker bridge
# gateway (printed by `start`, usually 172.17.0.1): set `inbox.devices.public_address`
# to <gateway>:<port> of a inbox published on the host, make an enrollment code on the
# page "My phones", and open the link `inbox://enroll?...` on the phone (for example
# `adb shell am start -a android.intent.action.VIEW -d '<link>'`). The host network is not used: the
# emulator's virtual display would clash with the desktop's X server.
set -eu

IMAGE="budtmo/docker-android:emulator_14.0@sha256:1bffbbed5d530284dd355eb73e863411b4a761bc55a00fa1e49fe4a013b6b24a"
NAME="inbox-emulator"
WEB_PORT=6080
root=$(cd "$(dirname "$0")/../.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)

apk_path() {
  apk="${1:-$root/dist/inbox-$version.apk}"
  [ -f "$apk" ] || { echo "APK not found: $apk (build it: tools/android/build-apk.sh)" >&2; exit 1; }
}

adb() {
  docker exec "$NAME" adb "$@"
}

wait_boot() {
  printf 'booting emulator'
  i=0
  until [ "$(adb shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ]; do
    i=$((i + 1))
    [ "$i" -le 180 ] || { echo; echo "emulator did not boot within 6 minutes" >&2; exit 1; }
    printf '.'
    sleep 2
  done
  echo " ok"
}

install_apk() {
  docker cp "$apk" "$NAME:/tmp/inbox.apk"
  adb install -r -g /tmp/inbox.apk >/dev/null
  # Permissions a user grants on the onboarding screen.
  adb shell appops set app.inbox USE_FULL_SCREEN_INTENT allow >/dev/null 2>&1 || true
  adb shell cmd notification allow_dnd app.inbox >/dev/null 2>&1 || true
  adb shell dumpsys deviceidle whitelist +app.inbox >/dev/null 2>&1 || true
  adb shell appops set app.inbox SYSTEM_ALERT_WINDOW allow >/dev/null 2>&1 || true
  adb shell monkey -p app.inbox -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1
  echo "installed $(basename "$apk")"
}

case "${1:-}" in
  start)
    apk_path "${2:-}"
    [ -e /dev/kvm ] || { echo "/dev/kvm is required" >&2; exit 1; }
    if ! docker ps --format '{{.Names}}' | grep -qx "$NAME"; then
      docker rm -f "$NAME" >/dev/null 2>&1 || true
      docker run -d --name "$NAME" --device /dev/kvm -p "127.0.0.1:$WEB_PORT:$WEB_PORT" \
        -e EMULATOR_DEVICE="Samsung Galaxy S10" -e WEB_VNC=true -e WEB_VNC_PORT="$WEB_PORT" \
        "$IMAGE" >/dev/null
    fi
    wait_boot
    install_apk
    echo
    echo "Emulator screen: http://localhost:$WEB_PORT"
    gateway=$(docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}')
    echo "The host is $gateway for the phone. Make an enrollment code on the page \"My phones\", then open its link on the phone."
    ;;
  install)
    apk_path "${2:-}"
    install_apk
    ;;
  logs)
    pid=$(adb shell pidof app.inbox | tr -d '\r')
    [ -n "$pid" ] || { echo "app not running" >&2; exit 1; }
    adb logcat --pid="$pid"
    ;;
  stop)
    docker rm -f "$NAME" >/dev/null && echo "emulator removed"
    ;;
  *)
    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
