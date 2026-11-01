#!/bin/sh
# Build entry point of the portal. Everything runs in containers. The host needs Docker (BuildKit),
# POSIX sh and python3. No command pushes an image or a file to a registry.
#
#   tools/build.sh server   dist/portal_<version>_linux_<arch>         (static musl binary, checked)
#   tools/build.sh image    local image portal:<version> and portal:local
#   tools/build.sh apk      dist/inbox-<version>.apk                  (see tools/android)
#   tools/build.sh dist     dist/portal_<version>_linux_<arch>.tar.gz and dist/SHA256SUMS
#   tools/build.sh all      server, image, apk, dist
#
# The version is the field `version` of the root [package] in Cargo.toml (tools/version.sh).
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

version=$(tools/version.sh)
git_sha=$(git rev-parse --short=12 HEAD 2>/dev/null || echo unknown)
# Reproducible archives: fixed mtime (last commit), owner and compression header.
epoch=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || echo 0)}

case "$(uname -m)" in
  x86_64 | amd64) arch=amd64 ;;
  aarch64 | arm64) arch=arm64 ;;
  *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac

build_server() {
  out="$root/dist/.artifact-$arch"
  rm -rf "$out"
  mkdir -p dist
  docker build -f docker/Dockerfile --target artifact --build-arg PORTAL_VERSION="$version" --build-arg PORTAL_GIT_SHA="$git_sha" \
    --output "type=local,dest=$out" .
  python3 tools/release/check-static.py "$out/portal"
  mv "$out/portal" "dist/portal_${version}_linux_${arch}"
  rm -rf "$out"
  echo "dist/portal_${version}_linux_${arch}"
}

build_image() {
  docker build -f docker/Dockerfile --target runtime --build-arg PORTAL_VERSION="$version" --build-arg PORTAL_GIT_SHA="$git_sha" \
    -t "portal:$version" -t portal:local .
  echo "image portal:$version"
}

build_apk() {
  [ -x tools/android/build-apk.sh ] || { echo "tools/android/build-apk.sh not found" >&2; exit 1; }
  tools/android/build-apk.sh
}

build_dist() {
  mkdir -p dist
  found=0
  for bin in dist/portal_"${version}"_linux_*; do
    case "$bin" in *.tar.gz) continue ;; esac
    [ -f "$bin" ] || continue
    found=1
    name=$(basename "$bin")
    stage=$(mktemp -d)
    cp "$bin" "$stage/portal"
    chmod 0755 "$stage/portal"
    tar -C "$stage" --owner=0 --group=0 --numeric-owner --mtime="@$epoch" -cf - portal | gzip -9n > "dist/$name.tar.gz"
    rm -rf "$stage"
  done
  [ "$found" = 1 ] || { echo "no server binary in dist/, run: tools/build.sh server" >&2; exit 1; }
  (
    cd dist
    set -- ./portal_"${version}"_linux_*.tar.gz
    [ -f "./inbox-${version}.apk" ] && set -- "$@" "./inbox-${version}.apk"
    sha256sum -- "$@" | sed 's| \./| |' > SHA256SUMS
  )
  cat dist/SHA256SUMS
}

case "${1:-}" in
  server) build_server ;;
  image) build_image ;;
  apk) build_apk ;;
  dist) build_dist ;;
  all) build_server; build_image; build_apk; build_dist ;;
  *) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2 ;;
esac
