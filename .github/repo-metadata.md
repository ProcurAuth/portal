# Repository metadata

This file lists the settings that a maintainer applies on GitHub with `gh`. The repository files do not apply them.

## About

- Description (100 characters): `Gate, keys, AuthZEN decision point and alert inbox for self-hosted services. One static Rust binary.`
- Website: `https://procurauth.github.io/portal/`
- Social preview image: upload `docs/assets/og.png` (1200 by 630 pixels).

## Topics

`procurauth`, `authzen`, `forwardauth`, `traefik`, `authorization`, `access-control`, `pdp`, `ai-agents`, `mcp`, `self-hosted`, `homelab`, `rust`, `android`, `alerting`, `noise-protocol`

## Settings

- Pages: serve the branch `main`, from the folder `/docs`.
- Enable the private reports of vulnerabilities (Settings, Code security). `SECURITY.md` and `CODE_OF_CONDUCT.md` link to them.
- Enable the alerts of Dependabot. `.github/dependabot.yml` updates the actions, the Cargo and Gradle dependencies and the Docker base images.
- Protect `main`: require these checks before a merge: `Policy (no image is published)`, `Rules, format and shell syntax` and `docs`. Do not require the jobs of `nightly.yml`, because they do not run on a pull request.
- Do not publish a container image. A release holds binaries, the Android APK and a checksum file only.
- The local script `tools/release/local-release.sh` makes the release. Read [Build and release](../docs/build-and-release.md).

## Labels

Create these labels. The issue forms use them: `bug`, `enhancement`, `documentation`.

## Commands

```sh
gh repo edit ProcurAuth/portal \
  --description "Gate, keys, AuthZEN decision point and alert inbox for self-hosted services. One static Rust binary." \
  --homepage "https://procurauth.github.io/portal/" \
  --add-topic procurauth --add-topic authzen --add-topic forwardauth --add-topic traefik \
  --add-topic authorization --add-topic access-control --add-topic pdp --add-topic ai-agents \
  --add-topic mcp --add-topic self-hosted --add-topic homelab --add-topic rust \
  --add-topic android --add-topic alerting --add-topic noise-protocol
for label in bug enhancement documentation; do gh label create "$label" --repo ProcurAuth/portal || true; done
```
