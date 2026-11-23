#!/bin/sh
# Builds the release payload on this machine, the same files the `release` workflow publishes, for when GitHub Actions is
# not available (no credit left, outage). Needs Docker with BuildKit, QEMU binfmt for arm64 (docker buildx ls shows
# linux/arm64), python3, git, and `gh` logged in for --publish. The APK is signed with secrets/android-release.env.
# No container image is built or pushed: a release holds binaries, the APK and SHA256SUMS only.
#
#   tools/release/local-release.sh [--publish] [--skip-apk] [--arch amd64|arm64 ...]
#
# Without --publish nothing leaves the machine: the files are left in dist/release-<version>/. The checks are those of the
# workflow: the tag must be vX.Y.Z[-suffix], equal to the product version (Cargo.toml), and point at HEAD; the tree must be clean.
# CHANGELOG.md must have a section for the version: it gives the notes (dist/release-<version>.notes.md), as in the workflow.
# With --publish the tag must also be on origin, and the release is created from it (`gh release create --verify-tag`).
# Tests are not run here: run tools/dev/test.sh --workspace first, as CI would.
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

version=$(tools/version.sh)
tag="v$version"
printf '%s' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$' || { echo "invalid tag $tag" >&2; exit 1; }

head_sha=$(git rev-parse HEAD)
# The commit that the binary shows (`git_sha` of /api/version): the same form as tools/build.sh and the workflow.
git_sha=$(git rev-parse --short=12 HEAD)
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
notes="$out.notes.md"
rm -rf "$out" "$notes"
mkdir -p "$out"
# The notes of the release are the section of the version in CHANGELOG.md. The file is not in $out: SHA256SUMS ignores it.
scripts/changelog-section.sh "$version" CHANGELOG.md > "$notes" || { echo "CHANGELOG.md has no section for $version" >&2; exit 1; }

for arch in $archs; do
  echo "== server $arch"
  art="$out/.artifact-$arch"
  docker buildx build -f docker/Dockerfile --platform "linux/$arch" --target artifact \
    --build-arg PORTAL_VERSION="$version" --build-arg PORTAL_GIT_SHA="$git_sha" \
    --output "type=local,dest=$art" .
  python3 tools/release/check-static.py "$art/portal"
  # An arm64 binary runs only on an arm64 host or with QEMU: the check is skipped when it fails to start.
  "$art/portal" version || echo "(binary not run on this host)"
  stage=$(mktemp -d)
  install -m 0755 "$art/portal" "$stage/portal"
  tar -C "$stage" --owner=0 --group=0 --numeric-owner --mtime="@$epoch" -cf - portal \
    | gzip -9n > "$out/portal_${version}_linux_${arch}.tar.gz"
  rm -rf "$stage" "$art"
done

if [ "$apk" = 1 ]; then
  echo "== apk"
  tools/android/build-apk.sh
  [ -f "dist/portal-inbox-$version.apk" ] || { echo "dist/portal-inbox-$version.apk was not produced" >&2; exit 1; }
  cp "dist/portal-inbox-$version.apk" "$out/"
fi

(cd "$out" && sha256sum -- * > SHA256SUMS && cat SHA256SUMS)

if [ "$publish" = 1 ]; then
  echo "== publish $tag"
  set -- "$out"/*.tar.gz "$out"/SHA256SUMS
  [ "$apk" = 1 ] && set -- "$@" "$out/portal-inbox-$version.apk"
  # shellcheck disable=SC2086
  gh release create "$tag" --verify-tag --title "portal $tag" --notes-file "$notes" $pre "$@"
else
  echo "Built in $out, notes in $notes (dry run: nothing published, add --publish to create the release)."
fi
