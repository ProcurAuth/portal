#!/bin/sh
# Builds the signed release APK: dist/inbox-<version>.apk
#
# Signing comes from INBOX_KEYSTORE, INBOX_KEYSTORE_PASSWORD, INBOX_KEY_ALIAS and
# INBOX_KEY_PASSWORD. If none is set and secrets/android-release.env exists, it is
# loaded. There is no unsigned or debug-signed fallback.
#
# Runs inside the inbox-android image (tools/android/Dockerfile). Outside of it,
# the script re-executes itself in that image. Set INBOX_IN_CONTAINER=1 to run
# directly (CI job already using the image).
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)

if [ -z "${INBOX_KEYSTORE:-}" ] && [ -f "$root/secrets/android-release.env" ]; then
  set -a; . "$root/secrets/android-release.env"; set +a
fi
for v in INBOX_KEYSTORE INBOX_KEYSTORE_PASSWORD INBOX_KEY_ALIAS INBOX_KEY_PASSWORD; do
  eval "val=\${$v:-}"
  if [ -z "$val" ]; then
    echo "error: $v is required (run tools/android/gen-keystore.sh once, or export the signing variables)" >&2
    exit 1
  fi
done
[ -f "$INBOX_KEYSTORE" ] || { echo "error: keystore $INBOX_KEYSTORE not found" >&2; exit 1; }

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\([^"]*\)".*/\1/p' "$root/Cargo.toml")
[ -n "$version" ] || { echo "error: cannot read workspace version from Cargo.toml" >&2; exit 1; }

if [ "${INBOX_IN_CONTAINER:-}" != 1 ]; then
  docker image inspect inbox-android >/dev/null 2>&1 || docker build -t inbox-android "$root/tools/android"
  for vol in inbox-gradle inbox-cargo; do
    docker volume inspect "$vol" >/dev/null 2>&1 || {
      docker volume create "$vol" >/dev/null
      docker run --rm -v "$vol:/v" inbox-android chown "$(id -u):$(id -g)" /v
    }
  done
  ks_dir=$(cd "$(dirname "$INBOX_KEYSTORE")" && pwd)
  ks_name=$(basename "$INBOX_KEYSTORE")
  tty=""; [ -t 0 ] && tty="-it"
  exec docker run --rm $tty -u "$(id -u):$(id -g)" \
    -v "$root:/src" -v "$ks_dir:/keystore:ro" \
    -v inbox-gradle:/gradle -v inbox-cargo:/cargo-registry \
    -e INBOX_IN_CONTAINER=1 -e HOME=/tmp -e GRADLE_OPTS=-Duser.home=/tmp \
    -e GRADLE_USER_HOME=/gradle \
    -e INBOX_KEYSTORE="/keystore/$ks_name" \
    -e INBOX_KEYSTORE_PASSWORD -e INBOX_KEY_ALIAS -e INBOX_KEY_PASSWORD \
    -e INBOX_GIT_SHA="${INBOX_GIT_SHA:-}" \
    -w /src inbox-android sh tools/android/build-apk.sh
fi

# ---- inside the build image
ndk_bin="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
api=26
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$ndk_bin/aarch64-linux-android$api-clang"
export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER="$ndk_bin/armv7a-linux-androideabi$api-clang"
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$ndk_bin/x86_64-linux-android$api-clang"
export CC_aarch64_linux_android="$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"
export CC_armv7_linux_androideabi="$CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER"
export CC_x86_64_linux_android="$CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER"
export AR="$ndk_bin/llvm-ar"
# The JNI bridge must be able to catch panics and raise Java exceptions.
export CARGO_PROFILE_RELEASE_PANIC=unwind
export CARGO_TARGET_DIR=/src/target-android
# Reuse the shared registry cache when mounted.
if [ -d /cargo-registry ] && [ -w /cargo-registry ]; then
  mkdir -p /cargo-registry/registry
  export CARGO_HOME=/tmp/cargo-home
  mkdir -p "$CARGO_HOME"
  ln -sfn /cargo-registry/registry "$CARGO_HOME/registry"
fi

libs=/src/android/app/src/main/jniLibs
for pair in aarch64-linux-android:arm64-v8a armv7-linux-androideabi:armeabi-v7a x86_64-linux-android:x86_64; do
  triple=${pair%%:*}
  abi=${pair##*:}
  echo "==> inbox-android for $abi"
  cargo build --locked --release -p inbox-android --target "$triple"
  mkdir -p "$libs/$abi"
  cp "$CARGO_TARGET_DIR/$triple/release/libinbox_android.so" "$libs/$abi/libinbox_android.so"
done

echo "==> gradle assembleRelease ($version)"
cd /src/android
gradle --no-daemon --console=plain -PinboxVersion="$version" assembleRelease

mkdir -p /src/dist
out="/src/dist/inbox-$version.apk"
cp app/build/outputs/apk/release/app-release.apk "$out"
"$ANDROID_HOME/build-tools/35.0.0/apksigner" verify --print-certs "$out" | head -3
sha256sum "$out"
