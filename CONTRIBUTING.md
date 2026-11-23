# Contributing to portal

Thank you for your interest. This page tells you how to report a problem, how to propose a change, and which checks a change must pass. Read the [code of conduct](CODE_OF_CONDUCT.md) first.

## Report a problem

- Open an issue with the form that fits: a [bug report](https://github.com/ProcurAuth/portal/issues/new?template=bug_report.yml) or a [feature request](https://github.com/ProcurAuth/portal/issues/new?template=feature_request.yml).
- Do not report a security problem in a public issue. Use the [private vulnerability report](https://github.com/ProcurAuth/portal/security/advisories/new). See [SECURITY.md](SECURITY.md).
- Do not paste a secret, a key, a cookie or a password. Replace real host names with `example.com`.

## Propose a change

1. Open an issue first for a large change, so that we can agree on the aim.
2. Fork the repository and make a branch.
3. Make the change. Keep it small and focused. One pull request has one purpose.
4. Run the checks below.
5. Open a pull request. The template lists what to confirm.

When you send a change, you agree that it is licensed under `MIT OR Apache-2.0`, as the rest of the project.

## Set up

Docker is the only tool that you need. Each script builds and runs in a container, and writes nothing to the source tree.

```sh
tools/dev/lint.sh                 # format check and clippy with warnings as errors
tools/dev/test.sh --workspace     # the tests, with a throwaway PostgreSQL
tools/dev/smoke.sh                # a Compose smoke test of the real image
```

The tests of the AuthZEN decision point and of the edge read the conformance cases of the [ProcurAuth](https://github.com/ProcurAuth/procurauth) repository. Clone it next to this repository, so that `../procurauth/conformance` exists. Set `PROCURAUTH_CONFORMANCE` to use another path. See [Development](docs/development.md) for all scripts, the variables and the Android tests.

To format the code, run `cargo fmt --all` in the same image as `tools/dev/lint.sh` (`rust:1.98`).

## Rules for the code

- The `portal` binary and the crate `inbox-core` have no `unsafe` Rust.
- The configuration is strict. A new key goes into the table in `src/config/keys.rs` with its type, its range, its default and its documentation. Do not read an environment variable that the file does not name. Do not change a value to make it fit. A secret accepts only `_env` and `_file`.
- Generate the configuration reference again after a change of the table: `portal config-reference`. Paste the result between the markers in `docs/configuration.md`. A test fails when the page and the table differ.
- A new behaviour needs a test. A security rule needs a test that shows the refusal.
- Never log a password, a key or a secret.
- Add one line to `CHANGELOG.md` under Unreleased for each change that a user can see.

## Rules for the documentation

The documents are in English, in ASD-STE100 Simplified Technical English. Write sentences of 20 words at most in a procedure, and 25 words at most elsewhere. Use the active voice. Do not use phrasal verbs.

- The Markdown files in `docs/` are the source. The files `docs/*.html` come from them. Do not edit an HTML file by hand.
- Build the site: `python3 tools/docs/build_site.py`. Check that it is up to date: `python3 tools/docs/build_site.py --check`.
- Check the links: `python3 tools/docs/check_links.py`.
- Check the language: `python3 tools/dev/ste-check.py docs/yourfile.md`.
- Use `example.com`, `203.0.113.x` and `host-1` in examples. Never write a real host name, address or secret.

## Release

A maintainer makes the release with `tools/release/local-release.sh`. A release holds binaries, the Android APK and a checksum file. It never holds a container image. See [Build and release](docs/build-and-release.md).
