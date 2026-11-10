#!/bin/sh
# Check the editor of grants in a real browser (Playwright, in Docker). The check runs against a
# portal that already runs with the demo services (demo/up.sh). It never saves anything.
# With UI_CHECK_INBOX=1 it also checks the pages of the Inbox. Those checks send alerts: use a
# portal that is yours, not a shared demo.
# Usage: tools/dev/ui-check.sh
# UI_CHECK_BASE, UI_CHECK_LOGIN, UI_CHECK_PASSWORD, UI_CHECK_KEY_LOGIN, UI_CHECK_KEY_PASSWORD and
# UI_CHECK_SHOTS: see the header of tools/dev/ui-check.js. UI_CHECK_SHOTS must be a directory of the
# host: the script mounts it and gives the path in the container.
# UI_CHECK_SCRIPT: the script to run (default ui-check.js). ui-check-notify.js checks the notifications: see its header.
# ui-check-keep.js checks the box "Stay signed in" of the sign-in page and the session cookie: see its header.
# ui-check-login.js checks the sign-in page, its band and the second step: see its header. It reads UI_CHECK_HOST too.
# ui-check-confirm.js checks the lock of an approval in the editor and the interaction of a key. It WRITES: use a portal that is yours. See its header.
# ui-check-prefs.js checks the page Inbox > Preferences. It WRITES: start the server with the test `prefs_demo_for_the_eyes` of tests/prefs_e2e.rs (see the header of the script).
# ui-check-items.js checks the web pages of the items (open items, history, one request, limits). It WRITES: start the server with the test `items_demo_for_the_eyes` of tests/items_web_e2e.rs (see the header of the script). With the test `questions_demo_for_the_eyes` of tests/questions_web_e2e.rs it checks the web pages of the questions instead.
# ui-check-agents.js checks the web pages of the agents (groups, notice of a new agent, agent page, forms, History filter). It WRITES: start the server with the test `agents_demo_for_the_eyes` of tests/agents_web_e2e.rs (see the header of the script).
# ui-check-agents-enrolled.js checks the web pages of the mode `enrolled` (Add agent, the token once, Reset token, Also asked by). It WRITES: start the server with the test `agents_enrolled_demo_for_the_eyes` of tests/agents_web_e2e.rs (see the header of the script).
# ui-check-theme.js checks the theme selector of the launcher (the demo is fine: it only signs in): see its header.
# PLAYWRIGHT_IMAGE: the image of the browser (default mcr.microsoft.com/playwright:v1.49.1-noble).
# The container uses the network of the host, so the base URL can be localhost. The image needs
# access to the npm registry to install the `playwright` package of the same version.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
image=${PLAYWRIGHT_IMAGE:-mcr.microsoft.com/playwright:v1.49.1-noble}
shots=""
if [ -n "${UI_CHECK_SHOTS:-}" ]; then
  mkdir -p "$UI_CHECK_SHOTS"
  shots="-v $(cd "$UI_CHECK_SHOTS" && pwd):/shots"
fi
# shellcheck disable=SC2086
exec docker run --rm --network host -v "$here":/check:ro $shots \
  -e UI_CHECK_BASE -e UI_CHECK_LOGIN -e UI_CHECK_PASSWORD -e UI_CHECK_KEY_LOGIN -e UI_CHECK_KEY_PASSWORD -e UI_CHECK_INBOX -e UI_CHECK_BOB_PASSWORD -e UI_CHECK_CAROL_PASSWORD -e UI_CHECK_WIDE -e UI_CHECK_LIMITS_NAME -e UI_CHECK_SCRIPT -e UI_CHECK_TOKEN -e UI_CHECK_USER -e UI_CHECK_USER_PASSWORD -e UI_CHECK_ROOT_PASSWORD -e UI_CHECK_ONLY -e UI_CHECK_REMEMBER_TTL -e UI_CHECK_DAYS -e UI_CHECK_HOST -e UI_CHECK_BASE_REQUIRED -e UI_CHECK_IDS \
  ${UI_CHECK_SHOTS:+-e UI_CHECK_SHOTS=/shots} \
  -e NODE_PATH=/tmp/node_modules -w /tmp "$image" \
  sh -c 'npm install --silent --no-audit --no-fund --prefix /tmp playwright@1.49.1 && node /check/${UI_CHECK_SCRIPT:-ui-check.js}'
