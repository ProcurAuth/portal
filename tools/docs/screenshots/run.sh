#!/usr/bin/env bash
# Take the screenshots of the documentation again.
# Usage: tools/docs/screenshots/run.sh [items|agents|demo|all]
# The script needs Docker, Google Chrome, Node.js 22 or later, Python 3 with Pillow, and a network
# for the first npm install. It builds local images only. It never pushes an image.
# Environment:
#   CHROME   the path of the Chrome program (default /usr/bin/google-chrome)
#   WORK     a work directory (default: a new directory under /tmp)
#   OUT      the directory of the pictures (default docs/assets/screenshots)
#   PORTAL_SLOT  the suffix of the Docker volume of the test build (default shots)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
work=${WORK:-$(mktemp -d /tmp/portal-shots.XXXXXX)}
out=${OUT:-$root/docs/assets/screenshots}
what=${1:-all}
mkdir -p "$work" "$out"

# The driver of the browser. The package is installed in the work directory, not in the repository.
if [ ! -d "$work/node_modules/playwright-core" ]; then
  (cd "$work" && npm init -y > /dev/null && npm install --silent --no-audit --no-fund playwright-core@1.49.1)
fi
export NODE_PATH="$work/node_modules"
capture() { node "$here/capture.js" "$1" "$work/pictures"; }

# A test server of the repository, with demo data. The pictures come from it while it runs.
from_test_server() {
  local kind=$1 file=$2 name=$3 dir="$work/server-$1"
  "$here/test-server.sh" "$file" "$name" "$dir" > "$work/server-$kind.log" 2>&1 &
  local pid=$!
  # The test writes demo.json when the data is ready. The first request of the Inbox is "old" after one minute,
  # so take the pictures at once.
  for _ in $(seq 1 1800); do [ -s "$dir/demo.json" ] && break; kill -0 $pid 2> /dev/null || { cat "$work/server-$kind.log" >&2; exit 1; }; sleep 1; done
  python3 "$here/make_plan.py" "$kind" "$dir/demo.json" "$work/plan-$kind.json"
  capture "$work/plan-$kind.json"
  touch "$dir/stop"
  wait $pid || true
}

if [ "$what" = items ] || [ "$what" = all ]; then
  from_test_server items items_web_e2e items_demo_for_the_eyes
fi
if [ "$what" = agents ] || [ "$what" = all ]; then
  from_test_server agents agents_web_e2e agents_demo_for_the_eyes
fi
# The Docker demo. The files of the demo are not changed: a Compose override gives the network another subnet (the subnet
# of the demo can overlap with a network that exists) and a configuration with the block inbox.questions (for the phone).
start_demo() {
  local subnet=${DEMO_SUBNET:-203.0.113.0/24}
  sed -e "s|172.30.0.0/24|$subnet|" -e 's|^  group "infra" {|  questions {}\n\n  group "infra" {|' "$root/demo/portal.hcl" > "$work/portal.hcl"
  # The limiter of the sign-in allows 5 attempts in one minute by default. The scripts sign in more often than that.
  printf '\nsecurity {\n  login_attempts_per_ip = 100\n}\n' >> "$work/portal.hcl"
  cat > "$work/override.yml" <<YML
networks:
  demo:
    ipam:
      config: !override
        - subnet: $subnet
configs:
  portal_config:
    file: $work/portal.hcl
YML
  export COMPOSE_FILE="$root/demo/docker-compose.yml:$work/override.yml"
  "$root/demo/up.sh"
}

if [ "$what" = demo ] || [ "$what" = all ]; then
  start_demo
  trap 'docker compose -f "$root/demo/docker-compose.yml" down -v' EXIT
  "$here/seed-extra.sh"
  python3 "$here/make_plan.py" demo http://localhost:8088 "$work/plan-demo.json"
  capture "$work/plan-demo.json"
fi

python3 "$here/optimise.py" "$work/pictures"
cp "$work/pictures"/*.png "$out/"
echo "The pictures are in $out"
