#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Generate THIRD-PARTY-NOTICES.md from `cargo metadata --locked --offline` and from the files of the repository.
# Use: tools/third-party.sh [OUTPUT_FILE]      (default: the standard output)
#
# The script runs `cargo metadata` in the Rust image of tools/dev/lint.sh, with the Docker volume portal-cargo
# as the cache of the crates. It needs no network. When a crate is not in the cache, run tools/dev/lint.sh or
# tools/dev/test.sh one time first. The host needs Docker and python3.
#
# The file has these parts:
#   - the file that the binary embeds from web/ (web/sprite.svg: Tabler Icons, with the notice of web/ICONS.md);
#   - the Rust crates of the server: the crates that the package `portal` needs on the two musl targets;
#   - the Rust crates of the Android app: the crates that the package `inbox-android` needs on the three Android targets;
#   - the Java libraries that android/app/build.gradle.kts declares with `implementation`.
# The crates that only the tests use (dev-dependencies) are not in the lists. Build dependencies are in the lists.
# Checks: a licence file under web/ or android/app/src/ stops the script with an error. A Java library that this
# script does not know also stops it. On an error, the script writes nothing.
# RUST_IMAGE: the Rust image (default rust:1.98, as tools/dev/lint.sh).
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
image=${RUST_IMAGE:-rust:1.98}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# A new copied file with its own licence needs its notice in this script.
# find writes to a file before sort: without pipefail, a find error in a pipe is lost. With set -e, it stops the script.
for d in web android/app/src; do
    [ -d "$d" ] || { echo "third-party.sh: the directory $d does not exist" >&2; exit 1; }
done
find web android/app/src -type f \( -iname 'LICENSE*' -o -iname 'LICENCE*' -o -iname 'COPYING*' -o -iname 'NOTICE*' -o -iname 'OFL*' \) \
    >"$tmp/licence-files.raw"
sort "$tmp/licence-files.raw" >"$tmp/licence-files.txt"
if [ -s "$tmp/licence-files.txt" ]; then
    echo "third-party.sh: no notice block for these files (add it to this script):" >&2
    cat "$tmp/licence-files.txt" >&2
    exit 1
fi
[ -f web/ICONS.md ] || { echo "third-party.sh: web/ICONS.md does not exist" >&2; exit 1; }

# One line of JSON for each target. The order of the lines is the order of the targets below.
targets="x86_64-unknown-linux-musl aarch64-unknown-linux-musl aarch64-linux-android armv7-linux-androideabi x86_64-linux-android"
docker volume inspect portal-cargo >/dev/null 2>&1 || docker volume create portal-cargo >/dev/null
docker run --rm -v "$root":/src:ro -v portal-cargo:/usr/local/cargo/registry -w /src "$image" sh -euc '
    for t in '"$targets"'; do
        cargo metadata --locked --offline --format-version 1 --filter-platform "$t"
        echo
    done
' >"$tmp/metadata.jsonl"

python3 -I - "$tmp/metadata.jsonl" "$targets" >"$tmp/result.md" <<'PY'
import json, re, sys

lines = [l for l in open(sys.argv[1], encoding="utf-8").read().splitlines() if l.strip()]
targets = sys.argv[2].split()
if len(lines) != len(targets):
    sys.exit("third-party.sh: cargo metadata gave %d documents for %d targets" % (len(lines), len(targets)))
docs = dict(zip(targets, (json.loads(l) for l in lines)))


def closure(doc, root_name):
    """Give the third-party packages that root_name needs, without the dev-dependencies."""
    members = set(doc["workspace_members"])
    pkgs = {p["id"]: p for p in doc["packages"]}
    nodes = {n["id"]: n for n in doc["resolve"]["nodes"]}
    roots = [i for i in members if pkgs[i]["name"] == root_name]
    if len(roots) != 1:
        sys.exit("third-party.sh: the workspace has no package %s" % root_name)
    seen, todo = set(), list(roots)
    while todo:
        i = todo.pop()
        if i in seen:
            continue
        seen.add(i)
        for dep in nodes[i]["deps"]:
            if any(k.get("kind") != "dev" for k in dep["dep_kinds"]):
                todo.append(dep["pkg"])
    return {i: pkgs[i] for i in seen if i not in members}


def crates(names, root_name):
    found = {}
    for t in names:
        for p in closure(docs[t], root_name).values():
            licence = p.get("license") or ("see the file %s of the crate" % p["license_file"] if p.get("license_file") else "not declared")
            found[(p["name"], p["version"])] = licence.replace("|", "\\|")
    return sorted(found.items(), key=lambda kv: (kv[0][0], [int(x) if x.isdigit() else x for x in re.split(r"[.+-]", kv[0][1])]))


def table(rows):
    out = ["| Crate | Version | Licence |", "| --- | --- | --- |"]
    out += ["| %s | %s | %s |" % (n, v, l) for (n, v), l in rows]
    return "\n".join(out)


icons = open("web/ICONS.md", encoding="utf-8").read()
m = re.search(r"Tabler Icons v([0-9]+(?:\.[0-9]+)*)", icons)
notice = re.search(r"## Origin and licence\n.*?```text\n(.*?)\n```", icons, re.S)
if not m or not notice or "MIT License" not in notice.group(1):
    sys.exit("third-party.sh: web/ICONS.md does not give the version and the MIT notice of Tabler Icons")
icons_version, icons_notice = m.group(1), notice.group(1)

# The licence of each Java library of the app. A library that is not in this table stops the script.
java_licences = {
    "com.squareup.okhttp3:okhttp": ("OkHttp", "Apache-2.0"),
    "androidx.biometric:biometric": ("AndroidX Biometric", "Apache-2.0"),
}
gradle = open("android/app/build.gradle.kts", encoding="utf-8").read()
java = []
for group, artifact, version in re.findall(r'^\s*implementation\("([^":]+):([^":]+):([^"]+)"\)', gradle, re.M):
    key = "%s:%s" % (group, artifact)
    if key not in java_licences:
        sys.exit("third-party.sh: no licence for the Java library %s (add it to this script)" % key)
    java.append((java_licences[key][0], "`%s`" % key, version, java_licences[key][1]))
if not java:
    sys.exit("third-party.sh: android/app/build.gradle.kts declares no Java library")

server = crates(targets[:2], "portal")
app = crates(targets[2:], "inbox-android")

print("""# Third-party notices

<!-- tools/third-party.sh generates this file from Cargo.lock, web/ICONS.md and android/app/build.gradle.kts. Do not edit it. -->

The portal is under the licence `MIT OR Apache-2.0` (see `LICENSE-MIT` and `LICENSE-APACHE`). The release also holds the
files, the Rust crates and the Java libraries below. Each one keeps its own licence.

## Files of the interface

The server binary embeds the file `web/sprite.svg`. The portal never loads it from another place. The directory
`web/icons-ui/` holds source copies of some of these icons. The binary does not hold them.

| Component | Version | Licence | Where |
| --- | --- | --- | --- |
| Tabler Icons (outline) | %s | MIT | `web/sprite.svg`, with the notice in `web/ICONS.md` |

### Tabler Icons

```text
%s
```

## C library of the server

The server binary is static. It links musl libc from the Alpine build image of `docker/Dockerfile`. musl is under
the licence MIT. Its notice is the file `COPYRIGHT` of musl, at https://musl.libc.org/.

## Rust crates of the server

Cargo builds the release binary `portal` from the crates below (`Cargo.lock` fixes each version). The list comes from
`cargo metadata` for the targets `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, without the crates that only
the tests use. Each crate gives its licence expression. When a crate offers a choice (`OR`), the choice is yours.

%s

## Android app

The app `portal-inbox-<version>.apk` holds the native library of the crate `inbox-android` and the Java libraries below.

### Rust crates of the app

The list comes from `cargo metadata` for the package `inbox-android` and the targets `aarch64-linux-android`,
`armv7-linux-androideabi` and `x86_64-linux-android`, without the crates that only the tests use.

%s

### Java libraries of the app

The file `android/app/build.gradle.kts` declares these libraries. Gradle adds the libraries that they need. The
repository has no Gradle wrapper: Gradle is in the image `inbox-android` (`tools/android/Dockerfile`). In this image,
the command `gradle :app:dependencies --configuration releaseRuntimeClasspath` in `android/` lists all of them.

| Library | Coordinates | Version | Licence |
| --- | --- | --- | --- |
%s""" % (
    icons_version,
    icons_notice,
    table(server),
    table(app),
    "\n".join("| %s | %s | %s | %s |" % row for row in java),
))
PY

if [ "$#" -ge 1 ]; then
    cp "$tmp/result.md" "$1"
else
    cat "$tmp/result.md"
fi
