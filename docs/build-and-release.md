# Build and release

This page tells how to build the portal, its image and the Android app, how the dependencies stay fixed, and how a release is made. A release ships binaries, the APK and the source. It never ships a container image.

## One version

The product has one version. It is the field `version` of the root `[package]` in `Cargo.toml`. The crates of the phone have the same version in `[workspace.package]`. The script `tools/version.sh` prints it and stops if the two differ. The binary, the Android app and the release tag use this version.

## Build commands

Everything is built in Docker. The host needs Docker with BuildKit, POSIX `sh` and `python3`. No command pushes an image or a file to a registry.

| Command | Result |
|---|---|
| `tools/build.sh server` | `dist/portal_<version>_linux_<arch>`: a static musl binary. The script checks it for dynamic links. |
| `tools/build.sh image` | The local images `portal:<version>` and `portal:local`. |
| `tools/build.sh apk` | `dist/portal-inbox-<version>.apk`, signed. See [Android signature](#android-signature). |
| `tools/build.sh dist` | `dist/portal_<version>_linux_<arch>.tar.gz` and `dist/SHA256SUMS`. The archives are reproducible. |
| `tools/build.sh all` | The four commands above. |

The `<arch>` is `amd64` or `arm64`, the architecture of the machine. The archives have a fixed owner, a fixed compression header and the time of the last commit as their date. The same binary therefore always gives the same archive. The file `SHA256SUMS` covers the archives and the APK.

### The image

The image is described by [`docker/Dockerfile`](../docker/Dockerfile). It builds on `rust:1.98-alpine3.24`, so the binary is static. The build stage refuses a binary that has a program interpreter or a shared library. The runtime stage is `scratch`, with the user `10001`, and it works with a read-only root file system. The image exposes the ports 4181 and 4182 and has the volume `/var/lib/portal`. Its `HEALTHCHECK` is `portal healthcheck`.

With a read-only root, mount a tmpfs of 16 MiB on `/tmp`, and mount a volume on `/var/lib/portal` when you use SQLite. The page [Operations](operations.md#container-and-read-only-root) shows the Compose form and the Kubernetes form of the repository files.

For the other architecture, use `docker buildx build --platform linux/arm64`. The build runs natively on that architecture, with a native builder or with QEMU. There is no cross compilation.

The image is for tests and for your own deployment. The repository publishes no image. No tool and no workflow of the repository authenticates to a registry or pushes to one. The job Policy of the workflow `ci.yml` fails when a workflow mentions a container registry.

The command `docker compose up` at the root of the repository starts the portal with the Inbox and PostgreSQL. The page [deploy/README.md](../deploy/README.md) describes the Compose stack and the K3s manifests.

## Reproducible dependencies

The file `Cargo.lock` is part of the source tree. Every build uses it: the Docker build and the test script call Cargo with `--locked`, so a build never changes the file. The dependencies of the root package have exact versions (`=x.y.z`) in `Cargo.toml`.

When you change a dependency on purpose, run the maintenance script:

```sh
scripts/update-lockfile.sh
```

The script runs `cargo generate-lockfile` in a `rust` container. Review the diff of `Cargo.lock` and commit it before you run the tests or make a release.

## Checks

| Check | Command |
|---|---|
| Format and clippy | `tools/dev/lint.sh` |
| All heavy checks | `tools/dev/heavy.sh` |
| Tests of the workspace | `tools/dev/test.sh --workspace --no-fail-fast` |
| Configuration files of `deploy/` | `tools/dev/check-configs.sh` |
| Binary, configuration files and Compose stack of the image | `tools/dev/check-image.sh` |
| Compose stack | `tools/dev/smoke.sh` |
| Documents | `python3 tools/dev/ste-check.py FILE` and `python3 tools/docs/check_links.py` |

[Development](development.md) describes each check, its variables and the other test scripts.

## Android signature

The release APK is always signed. The build has no unsigned fallback and no debug-signed fallback. The values for the signature come from four environment variables: `INBOX_KEYSTORE` (the path of the key store), `INBOX_KEYSTORE_PASSWORD`, `INBOX_KEY_ALIAS` and `INBOX_KEY_PASSWORD`.

The script `tools/android/gen-keystore.sh` makes a key store one time. It writes `secrets/android-release.jks` and the file `secrets/android-release.env` with the values. If `INBOX_KEYSTORE` is not set and `secrets/android-release.env` exists, `tools/android/build-apk.sh` loads that file.

The directory `secrets/` is not in git: `.gitignore` excludes `/secrets/`, `*.jks` and `*.keystore`, and `.dockerignore` excludes them from the build context. No release archive holds them. Keep a copy of the key store and its password in a safe place. If you lose them, the app cannot be updated in place on the phones that have it.

The build runs in the image `inbox-android` (`tools/android/Dockerfile`): JDK 17, the Android SDK and NDK, Gradle and the Rust targets. It builds the library `inbox-android` for three ABIs, runs the unit tests of the app and assembles the APK. The script then verifies the signature and prints the SHA-256 sum.

## Release

The release is made by `tools/release/local-release.sh` on a developer machine. The script builds the release files in Docker and, when you ask, creates the GitHub release.

1. Set the same version in `Cargo.toml` (both places). Update [CHANGELOG.md](../CHANGELOG.md).
2. Commit. Run `tools/dev/heavy.sh` (lint, tests and image). It runs `tools/dev/lint.sh` and `tools/dev/test.sh --workspace --no-fail-fast`. The release script does not run the tests.
3. Make the tag `vX.Y.Z` on the commit. The tag must be equal to the version of `Cargo.toml`, and it must point at `HEAD`.
4. Run `tools/release/local-release.sh`. Without `--publish`, nothing leaves the machine. The files stay in `dist/release-<version>/`.
5. Check the files and `SHA256SUMS`. Push the tag. Run the script again with `--publish`. It creates the release from the tag with `gh release create --verify-tag`.

The script needs Docker with BuildKit and QEMU binfmt for arm64, `python3`, `git`, and `gh` with a valid login for `--publish`. It stops when a tracked file is modified, when the tag is not valid or does not point at `HEAD`, or when the release exists. The options are `--publish`, `--skip-apk` and `--arch amd64|arm64`. A tag with a suffix (`v5.7.0-rc1`) makes a pre-release.

The release holds the static binaries for amd64 and arm64 as `portal_<version>_linux_<arch>.tar.gz`, the signed APK `portal-inbox-<version>.apk` and `SHA256SUMS`. It holds no image.

### Workflows

The directory `.github/workflows/` has four workflows. The logic of the local script and the logic of the release workflow are the same.

- `ci.yml` runs on each push to `main` and on each pull request. It is the fast path: each job takes a few seconds to a few tens of seconds. It checks that no workflow publishes an image. It checks the reads of the environment, the format and the syntax of the shell scripts. It does no build.
- `docs.yml` checks the documents and the site on each push to `main` and on each pull request.
- `nightly.yml` runs the heavy checks: clippy, the tests, the image, the configuration files, the Compose stack and the Android app with a throwaway key. It runs every night on `main`, and by hand on any branch. The tests use the conformance cases of the series D and E from a pinned commit of the [procurauth](https://github.com/ProcurAuth/procurauth) repository. The image stays on the runner and is never pushed. `tools/dev/heavy.sh` runs the same checks on your machine.
- `release.yml` starts on a tag `v*`. It checks that the tag equals the version of `Cargo.toml`. It runs `ci.yml` and `nightly.yml` as gates. It builds the binaries on an amd64 runner and on an arm64 runner. It builds the signed APK with secrets of the repository. It creates the release with the same files as the local script.

The local script is the release path that the project relies on. The workflows run the same steps when GitHub Actions is available.

## Check the result

After a build, check a binary with `python3 tools/release/check-static.py FILE`. It fails unless the ELF file has no program interpreter and no shared library. To check an archive, compare its sum with `SHA256SUMS`:

```sh
sha256sum -c SHA256SUMS --ignore-missing
```
