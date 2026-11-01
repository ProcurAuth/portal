# syntax=docker/dockerfile:1
#
# The portal image: one fully static musl binary on scratch, for amd64 and arm64.
# The build stays on this machine. No tool of this repository pushes an image to a registry.
#
#   docker build -t portal:local .                                  # runtime image
#   docker build --target artifact --output type=local,dest=out .   # the binary only (releases)
#
# Build arguments:
#   RUST_IMAGE        Alpine image of the pinned Rust toolchain. Alpine is musl: the binary is static.
#   PORTAL_VERSION    Version that `portal version` shows. Empty: the version of Cargo.toml.
#   PORTAL_GIT_SHA    Commit that the build comes from. `/api/version` of the portal shows it.
#
# Build for the other architecture with `docker buildx build --platform linux/arm64`. The compiler then
# runs on that architecture (a native builder or QEMU). There is no cross compilation.

ARG RUST_IMAGE=rust:1.98-alpine3.24

# ------------------------------------------------------------------ toolchain
# musl-dev, gcc, make and perl build the C code of SQLite and of aws-lc.
FROM ${RUST_IMAGE} AS toolchain
RUN apk add --no-cache musl-dev gcc make perl binutils
ENV CARGO_HOME=/usr/local/cargo \
    CARGO_TARGET_DIR=/src/target \
    CARGO_NET_RETRY=5 \
    CARGO_INCREMENTAL=0 \
    RUSTFLAGS="--remap-path-prefix=/src=/portal --remap-path-prefix=/usr/local/cargo=/cargo"
WORKDIR /src

# ------------------------------------------------------------------ dependencies
# Manifests and stub sources only: the layers of the dependencies stay valid when the sources change.
FROM toolchain AS deps
COPY Cargo.toml Cargo.lock ./
COPY crates/inbox-core/Cargo.toml crates/inbox-core/
COPY crates/inbox-android/Cargo.toml crates/inbox-android/
RUN set -eu; \
    mkdir -p src crates/inbox-core/src crates/inbox-android/src; \
    echo 'fn main() {}' > src/main.rs; \
    echo '' > crates/inbox-core/src/lib.rs; \
    echo '' > crates/inbox-android/src/lib.rs; \
    cargo fetch --locked; \
    cargo build --release --locked -p portal

# ------------------------------------------------------------------ build
FROM deps AS build
ARG PORTAL_VERSION=
ARG PORTAL_GIT_SHA=unknown
COPY src src
COPY crates crates
COPY web web
# COPY keeps the mtimes of the context, which are older than the stubs: force a rebuild of our crates.
RUN set -eu; \
    find src crates web -type f -exec touch {} +; \
    if [ -n "$PORTAL_VERSION" ]; then export PORTAL_RELEASE_VERSION="$PORTAL_VERSION"; fi; \
    PORTAL_GIT_SHA="$PORTAL_GIT_SHA" cargo build --release --locked -p portal; \
    bin=target/release/portal; \
    if readelf -l "$bin" | grep -q INTERP; then echo "portal has a program interpreter" >&2; exit 1; fi; \
    if readelf -d "$bin" 2>/dev/null | grep -q NEEDED; then echo "portal needs a shared library" >&2; exit 1; fi; \
    "$bin" version; \
    mkdir -p /out/rootfs/var/lib/portal /out/rootfs/tmp /out/rootfs/etc/portal; \
    chmod 1777 /out/rootfs/tmp; \
    install -m 0755 "$bin" /out/portal

# ------------------------------------------------------------------ artifact
FROM scratch AS artifact
COPY --from=build /out/portal /portal

# ------------------------------------------------------------------ runtime
# The root file system can be read only. The process writes to /tmp and to /var/lib/portal (SQLite) only.
FROM scratch AS runtime
COPY --from=build --chown=10001:10001 /out/rootfs/ /
COPY --from=build /out/portal /portal
USER 10001:10001
# 4181: the portal: the pages, and the API of the inbox for the producers and the webhooks. 4182: the AuthZEN PDP.
# 8080: the phone port of the inbox. Do not route 4182 through Traefik. Publish 8080 only when the inbox runs.
EXPOSE 4181 4182 8080
VOLUME ["/var/lib/portal"]
HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --start-interval=1s --retries=3 \
  CMD ["/portal", "healthcheck", "--config", "/etc/portal/config.hcl"]
ENTRYPOINT ["/portal"]
CMD ["run", "--config", "/etc/portal/config.hcl"]
