#!/usr/bin/env bash
# Run the ProcurAuth conformance series U (role PEP) against the demo service notes.
# Use: demo/conformance/run-conformance-u.sh [--no-build] [--refusal-cases] [--format text|json]
#          [--cases U04,U05,...]
#
#   --no-build      Do not build access-conformance. Use the program of the last build.
#   --refusal-cases Use notes-refusal-profile.json and run only the cases with the
#                   operation use_plain that expect a refusal. README.md tells why.
#
# The variable PROCURAUTH gives the checkout of the ProcurAuth repository. The default is
# the directory procurauth next to the portal checkout.
#
# The script starts notes in a container with no network (--network none). The runner
# access-conformance then runs in the network namespace of that container: the two share
# only the loopback address. The runner acts as the client, as the edge (it signs the
# identity tokens with the test key of ProcurAuth) and as the PDP (127.0.0.1:8181).
# notes never reaches a real portal. The exit code is the code of the runner.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
portal="$(cd "$here/../.." && pwd)"
procurauth="${PROCURAUTH:-$(cd "$portal/.." && pwd)/procurauth}"
notes_image="portal-demo-notes"

build=1
profile=notes-profile.json
cases=""
runner_args=()
while [ "$#" -gt 0 ]; do
    case "$1" in
        --no-build) build=0 ;;
        --refusal-cases)
            profile=notes-refusal-profile.json
            # The cases with use_plain whose steps expect only a refusal: HTTP 401, 403 or 503.
            # A use with a budget in place of use_plain does not change their result.
            refusal="U03,U06,U08,U12,U38,U41,U42,U43,U44,U46,U47,U48,U49,U50,U54,U55"
            refusal="$refusal,U60,U61,U64,U65,U66"
            cases="${cases:-$refusal}"
            ;;
        --cases)
            [ "$#" -ge 2 ] || { echo "The option $1 needs a value." >&2; exit 2; }
            cases="$2"
            shift
            ;;
        --format)
            [ "$#" -ge 2 ] || { echo "The option $1 needs a value." >&2; exit 2; }
            runner_args+=("$1" "$2")
            shift
            ;;
        *)
            echo "Use: $0 [--no-build] [--refusal-cases] [--format text|json] [--cases ID,ID,...]" >&2
            exit 2
            ;;
    esac
    shift
done
if [ -n "$cases" ]; then
    runner_args+=(--cases "$cases")
fi

if [ ! -f "$procurauth/scripts/docker-env.sh" ]; then
    echo "The ProcurAuth checkout is not at $procurauth. Set PROCURAUTH." >&2
    exit 2
fi
# This sets image, target_volume, uid and gid, as the scripts of ProcurAuth do.
# shellcheck source=/dev/null
source "$procurauth/scripts/docker-env.sh"
ensure_image

if [ "$build" -eq 1 ]; then
    echo "== build access-conformance" >&2
    "$procurauth/scripts/cargo.sh" build --release --locked -p access-conformance >&2
fi

# The image of the demo. It holds Python and the library cryptography. The run mounts the
# app.py of this checkout over the copy in the image, so a change needs no new image.
if ! docker image inspect "$notes_image" >/dev/null 2>&1; then
    echo "== build the image of notes" >&2
    docker compose -f "$portal/demo/docker-compose.yml" build notes >&2
fi

work="$(mktemp -d)"
name="notes-conformance-$$"
cleanup() {
    docker rm -f "$name" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT

# The public key of the test edge, for notes. ProcurAuth gives only the private key in PEM.
keys="$procurauth/crates/access-conformance/examples"
docker run --rm --network none --user "$uid:$gid" --volume "$keys:/keys:ro" "$notes_image" \
    python -I -c '
from cryptography.hazmat.primitives import serialization as s
key = s.load_pem_private_key(open("/keys/test-edge-ed25519.pem", "rb").read(), None)
print(key.public_key().public_bytes(s.Encoding.PEM, s.PublicFormat.SubjectPublicKeyInfo).decode(), end="")
' >"$work/edge-public.pem"
chmod 0644 "$work/edge-public.pem"

# notes, with no network. The values agree with notes-profile.json.
echo "== start notes" >&2
docker run --detach --rm --name "$name" --network none --read-only \
    --volume "$portal/demo/notes/app.py:/app/app.py:ro" \
    --volume "$portal/demo/notes/index.html:/app/index.html:ro" \
    --volume "$work/edge-public.pem:/run/conformance/edge-public.pem:ro" \
    --volume "$keys/pdp-credential.txt:/run/conformance/pdp-credential.txt:ro" \
    --env IDENTITY_ISSUER=https://edge.example \
    --env IDENTITY_PUBLIC_KEY_FILE=/run/conformance/edge-public.pem \
    --env PDP_TOKEN_FILE=/run/conformance/pdp-credential.txt \
    --env PDP_URL=http://127.0.0.1:8181 \
    "$notes_image" >/dev/null

# The runner, in the network namespace of notes. The profile reads the test keys of
# ProcurAuth at ../procurauth, so the two directories are next to each other in the container.
echo "== run the series U" >&2
set +e
timeout 900 docker run --rm --init --network "container:$name" \
    --user "$uid:$gid" \
    --volume "$here:/run/conformance/profile:ro" \
    --volume "$procurauth:/run/conformance/procurauth:ro" \
    --volume "$target_volume:/cargo-target:ro" \
    --env HOME=/tmp \
    "$image" timeout 840 bash -c '
for i in $(seq 1 100); do
    (exec 3<>/dev/tcp/127.0.0.1/8080) 2>/dev/null && break
    sleep 0.1
done
profile="$1"
shift
exec /cargo-target/release/access-conformance --role pep --series U \
    --profile "/run/conformance/profile/$profile" --url http://127.0.0.1:8080 "$@"
' runner "$profile" "${runner_args[@]}"
code=$?
set -e
if [ "$code" -eq 2 ]; then
    echo "--- log of notes" >&2
    docker logs --tail 40 "$name" >&2 || true
fi
exit "$code"
