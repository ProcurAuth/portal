# Repository metadata

This file lists the settings of the GitHub repository that a file cannot set. A maintainer applies them with `gh`.

## About

Description (120 characters at most):

```text
Gate, keys, AuthZEN decision point and alert inbox for self-hosted services. One static Rust binary.
```

Website:

```text
https://procurauth.github.io/portal/
```

## Topics

```text
procurauth authzen forwardauth traefik authorization access-control pdp ai-agents mcp self-hosted homelab rust android alerting noise-protocol
```

## Commands

```sh
gh repo edit ProcurAuth/portal \
  --description "Gate, keys, AuthZEN decision point and alert inbox for self-hosted services. One static Rust binary." \
  --homepage "https://procurauth.github.io/portal/" \
  --add-topic procurauth --add-topic authzen --add-topic forwardauth --add-topic traefik \
  --add-topic authorization --add-topic access-control --add-topic pdp --add-topic ai-agents \
  --add-topic mcp --add-topic self-hosted --add-topic homelab --add-topic rust \
  --add-topic android --add-topic alerting --add-topic noise-protocol
```

## Other settings

- Enable GitHub Pages: source is the branch `main`, folder `/docs`.
- Enable the private reports of vulnerabilities (Settings, Code security).
- Social preview: upload `assets/brand/social-preview-1280x640.png` or a card that uses `docs/assets/banner.svg`.
- The local script `tools/release/local-release.sh` makes the release. See [Build and release](../docs/build-and-release.md).
