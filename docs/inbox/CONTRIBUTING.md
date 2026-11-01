# Contributing

- Code, comments and documentation are in English (`docs/SPEC.md` is the French
  specification).
- Keep patches minimal and focused; no unrelated reformatting.
- Update tests and docs with every behaviour change. Protocol changes update
  `docs/protocol.md` and bump `PROTOCOL_VERSION` when incompatible.
- Delivery guarantees are not negotiable: a change must never allow an accepted
  alert to be dropped or shown twice.
- New dependencies need a reason; prefer the standard library and the platform.
- Configuration stays strict: no implicit defaults, unknown fields rejected.

## Checks

Everything runs in containers (Docker only on the host):

```sh
tools/dev/cargo.sh fmt --all -- --check
tools/dev/cargo.sh clippy --locked -p inbox-core -p portal -p inbox-android --all-targets -- -D warnings
tools/dev/cargo.sh test --locked -p inbox-core -p portal -p inbox-android
tools/build.sh image
```

Pull requests describe what changed, why, and how it was validated.
