# Contribute to portal

Thank you for your interest. This document tells how to report a problem and how to propose a change. It also lists the checks that a change must pass.

## Ask a question or report a defect

Open an issue with [the issue forms](https://github.com/ProcurAuth/portal/issues/new/choose). Give the version (`portal version`), the area, and the output of `portal check-config`. Remove each secret and each real host name. Use `example.com` in place of a real host name.

Do not report a vulnerability in an issue. Use the [private vulnerability report](https://github.com/ProcurAuth/portal/security/advisories/new). Read [SECURITY.md](SECURITY.md).

## Change the code

1. Open an issue first for a large change or a new feature. State the problem and the idea.
2. Fork the repository and make a branch. One pull request has one purpose.
3. Write a test for each change of behavior. A security rule needs a test that shows the refusal.
4. Run the checks below.

Docker is the only tool for the code. Each script builds and runs in a container, and writes nothing to the source tree.

```sh
tools/dev/lint.sh                 # format check and clippy with warnings as errors
tools/dev/test.sh --workspace     # the tests, with a throwaway PostgreSQL
tools/dev/smoke.sh                # a Compose smoke test of the real image
```

The checks of the documents need Python 3 only. They run on the host and write no file.

```sh
python3 -I scripts/check-docs.py  # the site, the links and the ASD-STE100 rules
```

The tests of the AuthZEN decision point and of the edge read the conformance cases of the [ProcurAuth](https://github.com/ProcurAuth/procurauth) repository. Clone it next to this repository, so that `../procurauth/conformance` exists. Set `PROCURAUTH_CONFORMANCE` to use another path. [Development](docs/development.md) lists all scripts, the variables and the Android tests.

To format the code, run `cargo fmt --all` in the same image as `tools/dev/lint.sh` (`rust:1.98`).

## Rules of the code

- The `portal` binary and the crate `inbox-core` have no `unsafe` Rust.
- The configuration is strict. A new key goes into the table in `src/config/keys.rs` with its type, its range, its default and its documentation.
- The program reads an environment variable only when the configuration file names it. It never changes a value to make it fit. A secret accepts only `_env` and `_file`.
- After a change of the table, make the configuration reference again with `portal config-reference`. Paste the result between the markers in `docs/configuration.md`. A test fails when the page and the table differ.
- A new behavior needs a test. Never write a password, a key or a secret to the log.
- Add one line to `CHANGELOG.md` under "Unreleased" for each change that a user can see.

## Rules for the documents

- Write in ASD-STE100 Simplified Technical English. A procedural sentence has 20 words at most. A descriptive sentence has 25 words at most. A paragraph has six sentences at most. Use the active voice and the imperative in a procedure.
- The text must describe the real behavior of the code. Check the code, not the old text.
- The Markdown files in `docs/` are the source. The files `docs/*.html` come from them. Do not edit an HTML file by hand.
- Build the site with `python3 tools/docs/build_site.py`. It runs on the host and writes the files `docs/*.html`. Then run `python3 -I scripts/check-docs.py`. It checks the site, the links and the language.
- Check the language of one file with `python3 -I tools/dev/ste-check.py docs/yourfile.md`.
- Use `example.com`, the address range `203.0.113.0/24` and `host-1` in examples. Never write a real host name, a real address or a real secret.

## Pull requests

Open a pull request against `main`. Complete the template. The CI must pass. When you send a change, you agree that it is licensed under `MIT OR Apache-2.0`, as the rest of the project.

## Release

A maintainer makes the release with `tools/release/local-release.sh`. A release holds binaries, the Android APK and a checksum file. It never holds a container image. Read [Build and release](docs/build-and-release.md).

## Code of conduct

This project follows [the code of conduct](CODE_OF_CONDUCT.md).
