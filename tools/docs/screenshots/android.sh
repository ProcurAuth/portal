#!/usr/bin/env bash
# Take screenshots of the Android app Portal Inbox in the emulator of tools/android/emulator.sh.
# Usage: tools/docs/screenshots/android.sh NAME
# The script saves the screen of the emulator as docs/assets/screenshots/android-NAME.png, reduced to 540 pixels wide.
# Steps to get the two pictures of the documentation (the Docker demo must run, see run.sh):
#   1. tools/android/emulator.sh start
#   2. In the tab Inbox of the demo as admin, press "Add a phone". Copy the link portal-inbox://enroll?...
#   3. adb shell am start -a android.intent.action.VIEW -d 'LINK'
#      (use: docker exec inbox-emulator adb shell ...). Name the phone, tick the box, press "Add portal".
#   4. In the settings of the portal on the phone, set the groups Infra and Critical to "Notif".
#   5. Send alerts with the key demo/.secrets/inbox_api_key (see demo/README.md), and one question from an API key that
#      holds question.ask (POST /api/v1/questions with a command and "input": {"type": "confirm"}).
#   6. Run this script on the list (NAME=alerts) and on the open question (NAME=request).
# The address that the app shows in some screens is the Docker bridge address of the host. Do not use a screen that shows it.
set -euo pipefail
name=${1:?usage: android.sh NAME}
root=$(cd "$(dirname "$0")/../../.." && pwd)
out=${OUT:-$root/docs/assets/screenshots}
tmp=$(mktemp)
docker exec inbox-emulator adb exec-out screencap -p > "$tmp"
python3 - "$tmp" "$out/android-$name.png" <<'PY'
import sys
from PIL import Image
image = Image.open(sys.argv[1]).convert("RGB")
width = 540
image = image.resize((width, round(image.height * width / image.width)), Image.Resampling.LANCZOS)
image.quantize(colors=256, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE).save(sys.argv[2], optimize=True)
PY
rm -f "$tmp"
echo "wrote $out/android-$name.png"
