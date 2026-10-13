#!/bin/sh
# Builds the release payload on this machine, the same files the `release` workflow publishes, for when GitHub Actions is
# not available (no credit left, outage). Needs Docker with BuildKit, QEMU binfmt for arm64 (docker buildx ls shows
# linux/arm64), python3, git, and `gh` logged in for --publish. The APK is signed with secrets/android-release.env.
#
#   tools/release/local-release.sh [--publish] [--skip-apk] [--arch amd64|arm64 ...]
#
# Without --publish nothing leaves the machine: the files are left in dist/release-<version>/. The checks are those of the
# workflow: the tag must be vX.Y.Z[-suffix], equal to the workspace version, and point at HEAD; the tree must be clean.
# With --publish the tag must also be on origin, and the release is created from it (`gh release create --verify-tag`).
# Tests are not run here: run tools/dev/cargo.sh test first, as CI would.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

publish=0
apk=1
archs=""
while [ $# -gt 0 ]; do
  case "$1" in
    --publish) publish=1 ;;
    --skip-apk) apk=0 ;;
    --arch) shift; archs="$archs ${1:?--arch needs amd64 or arm64}" ;;
    -h | --help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
  shift
done
[ -n "$archs" ] || archs="amd64 arm64"
for a in $archs; do
  case "$a" in amd64 | arm64) ;; *) echo "unsupported arch $a" >&2; exit 2 ;; esac
done

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n1)
[ -n "$version" ] || { echo "cannot read workspace version from Cargo.toml" >&2; exit 1; }
tag="v$version"
printf '%s' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$' || { echo "invalid tag $tag" >&2; exit 1; }

head_sha=$(git rev-parse HEAD)
git rev-parse -q --verify "refs/tags/$tag" >/dev/null || { echo "tag $tag does not exist: git tag $tag" >&2; exit 1; }
[ "$(git rev-parse "$tag^{commit}")" = "$head_sha" ] || { echo "tag $tag does not point at HEAD" >&2; exit 1; }
[ -z "$(git status --porcelain --untracked-files=no)" ] || { echo "tracked files are modified: commit or stash first" >&2; exit 1; }
if [ "$publish" = 1 ]; then
  git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1 || { echo "tag $tag is not on origin: git push origin $tag" >&2; exit 1; }
  gh release view "$tag" >/dev/null 2>&1 && { echo "release $tag already exists" >&2; exit 1; }
fi

case "$version" in *-*) pre=--prerelease ;; *) pre="" ;; esac
epoch=$(git log -1 --format=%ct)
out="dist/release-$version"
rm -rf "$out"
mkdir -p "$out"

for arch in $archs; do
  echo "== server $arch"
  art="$out/.artifact-$arch"
  docker buildx build --platform "linux/$arch" --target artifact --build-arg INBOX_GIT_SHA="$head_sha" \
    --output "type=local,dest=$art" .
  python3 tools/release/check-static.py "$art/inbox"
  "$art/inbox" version
  stage=$(mktemp -d)
  install -m 0755 "$art/inbox" "$stage/inbox"
  tar -C "$stage" --owner=0 --group=0 --numeric-owner --mtime="@$epoch" -cf - inbox \
    | gzip -9n > "$out/inbox_${version}_linux_${arch}.tar.gz"
  rm -rf "$stage" "$art"
done

if [ "$apk" = 1 ]; then
  echo "== apk"
  tools/android/build-apk.sh
  [ -f "dist/inbox-$version.apk" ] || { echo "dist/inbox-$version.apk was not produced" >&2; exit 1; }
  cp "dist/inbox-$version.apk" "$out/"
fi

(cd "$out" && sha256sum -- * > SHA256SUMS && cat SHA256SUMS)

if [ "$publish" = 1 ]; then
  echo "== publish $tag"
  set -- "$out"/*.tar.gz "$out"/SHA256SUMS
  [ "$apk" = 1 ] && set -- "$@" "$out/inbox-$version.apk"
  # shellcheck disable=SC2086
  gh release create "$tag" --verify-tag --title "Inbox $tag" --generate-notes $pre "$@"
else
  echo "Built in $out (dry run: nothing published, add --publish to create the release)."
fi
