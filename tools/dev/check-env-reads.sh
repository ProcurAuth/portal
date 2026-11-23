#!/bin/sh
# Checks that only src/config.rs reads the process environment, in the Rust code of src/ and crates/.
# Usage: tools/dev/check-env-reads.sh [ROOT]    (default ROOT: the root of this repository)
# The configuration file is the only source of the program. src/config.rs reads a variable only when the file names it
# with `<name>_env`. Test code can read the variables of the test scripts. These parts are test code:
#   - a file named tests.rs or *_tests.rs, and each file under a directory named tests;
#   - the lines of an inline module that `#[cfg(test)]` marks (`mod name {` ... `}` at the same indent, as rustfmt writes it;
#     a `// comment` can follow the brace on the first and on the last line);
#   - the file of an out-of-line module that `#[cfg(test)]` marks (`mod name;`).
# The script reads the files only. It needs sh, find, grep and awk. Exit status: 0 when no read is found, 1 when a read is found.
set -eu
root=${1:-$(cd "$(dirname "$0")/../.." && pwd)}
cd "$root"

# The calls that read or change the process environment, also through `use std::env::{...}` and a bare `var_os(`.
pattern='env::(var|vars|var_os|vars_os|set_var|remove_var)\b|env::\{[^}]*\b(var|vars|var_os|vars_os|set_var|remove_var)\b|\bvar_os\('

dirs=""
for d in src crates; do [ -d "$d" ] && dirs="$dirs $d"; done
[ -n "$dirs" ] || { echo "env-reads: no src/ or crates/ in $root" >&2; exit 1; }

# shellcheck disable=SC2086 # $dirs is a list of words.
all=$(find $dirs -type f -name '*.rs' ! -path '*/tests/*' ! -path '*/target/*' ! -name tests.rs ! -name '*_tests.rs' \
  ! -path src/config.rs | sort)
[ -n "$all" ] || { echo "env-reads: ok (no Rust file)"; exit 0; }

# The files of the out-of-line modules that #[cfg(test)] marks. A module `name` declared in a/b.rs lives in a/b/name.rs
# or a/b/name/mod.rs. A module declared in mod.rs, lib.rs or main.rs lives next to that file.
# shellcheck disable=SC2086 # $all is a list of paths without spaces.
testmods=$(awk '
  FNR == 1 { pending = 0 }
  /^[[:space:]]*#\[cfg\(test\)\]/ {
    pending = 1
    line = $0; sub(/^[[:space:]]*#\[cfg\(test\)\][[:space:]]*/, "", line)
    if (line == "") next
    $0 = line
  }
  pending && /^[[:space:]]*(#\[|\/\/)/ { next }
  pending {
    pending = 0
    if ($0 ~ /^[[:space:]]*(pub(\([a-z]+\))?[[:space:]]+)?mod[[:space:]]+[A-Za-z0-9_]+[[:space:]]*;/) {
      name = $0; sub(/^[[:space:]]*(pub(\([a-z]+\))?[[:space:]]+)?mod[[:space:]]+/, "", name); sub(/[[:space:]]*;.*/, "", name)
      dir = FILENAME; base = FILENAME; sub(/\/[^\/]*$/, "", dir); sub(/^.*\//, "", base)
      if (base != "mod.rs" && base != "lib.rs" && base != "main.rs") { stem = base; sub(/\.rs$/, "", stem); dir = dir "/" stem }
      print dir "/" name ".rs"; print dir "/" name "/mod.rs"
    }
  }
' $all)

files=$(printf '%s\n' "$all" | grep -vxF "${testmods:-/nothing}" || true)
[ -n "$files" ] || { echo "env-reads: ok (no file outside test code)"; exit 0; }

# The lines outside the inline #[cfg(test)] modules and outside the comments, as FILE:LINE:TEXT.
# shellcheck disable=SC2086 # $files is a list of paths without spaces.
hits=$(awk '
  FNR == 1 { pending = 0; skip = 0 }
  skip { if ($0 ~ end) skip = 0; next }
  /^[[:space:]]*#\[cfg\(test\)\]/ {
    pending = 1
    indent = $0; sub(/[^[:space:]].*$/, "", indent)
    line = $0; sub(/^[[:space:]]*#\[cfg\(test\)\][[:space:]]*/, "", line)
    if (line == "") next
    $0 = line
  }
  pending && /^[[:space:]]*(#\[|\/\/)/ { next }
  pending {
    pending = 0
    if ($0 ~ /^[[:space:]]*(pub(\([a-z]+\))?[[:space:]]+)?mod[[:space:]]+[A-Za-z0-9_]+[[:space:]]*\{[[:space:]]*(\/\/.*)?$/) {
      skip = 1; end = "^" indent "[}][[:space:]]*(//.*)?$"; next
    }
  }
  /^[[:space:]]*\/\// { next }
  { print FILENAME ":" FNR ":" $0 }
' $files | grep -E "$pattern" || true)

if [ -n "$hits" ]; then
  echo "env-reads: only src/config.rs may read an environment variable, outside the test code:" >&2
  printf '%s\n' "$hits" >&2
  exit 1
fi
echo "env-reads: ok"
