# Screenshots of the documentation

This directory holds the scripts that make the screenshots in `docs/assets/screenshots/`. The pictures show the real product with demo data. Nothing in them is drawn by hand.

## What you need

- Docker. The scripts build local images only. They never push an image.
- Google Chrome. The default path is `/usr/bin/google-chrome`. Set `CHROME` for another path.
- Node.js 22 or later, and a network for the first `npm install`.
- Python 3 with the package Pillow.

## Make the pictures again

1. Run `tools/docs/screenshots/run.sh all`.
2. Wait. The first run builds the test binary and the portal image. This can take more than 20 minutes.
3. Read the pictures in `docs/assets/screenshots/`. Look at each one before you keep it.

Run `run.sh items`, `run.sh agents` or `run.sh demo` to make one group of pictures.

## How it works

| File | Role |
|---|---|
| `run.sh` | Runs the steps below and copies the pictures. |
| `test-server.sh` | Starts a test server of the repository in Docker. The tests `items_demo_for_the_eyes` and `agents_demo_for_the_eyes` give it demo data. |
| `make_plan.py` | Writes the list of the pages to capture, with the demo logins. |
| `capture.js` | Opens a session in Chrome and takes one picture of each page in the light theme and in the dark theme. |
| `seed-extra.sh` | Adds keys, sign-ins and alerts to the Docker demo. |
| `optimise.py` | Reduces each picture to a palette, so that each file stays under 250 KB. |

The pictures come from two sources.

- The Docker demo (`demo/up.sh`) gives the sign-in page, the launcher and the settings pages. It runs Traefik, the portal and example services.
- The test servers give the Inbox, the approval page and the agent pages. They hold approvals and agents in every state.

## Rules

- Use demo data only. A picture must show no real name, host, address or secret.
- Take the Inbox and approval pictures at once when the test server is ready. A request is marked as old after one minute.
- Do not edit a picture by hand. Run the scripts again.
- The scripts hold no secret. The demo passwords are made at run time by `demo/up.sh`.
- The Docker demo uses the subnet `203.0.113.0/24`. Set `DEMO_SUBNET` when this subnet is in use.

## The Android pictures

The pictures `android-*.png` come from the Android emulator in Docker (`tools/android/emulator.sh`) and the Docker demo. Take them by hand with `adb exec-out screencap -p`.
The steps are in `docs/development.md`.
