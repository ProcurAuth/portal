# syntax=docker/dockerfile:1
#
# Inbox server image: one fully static musl binary on scratch.
#
#   docker build -t inbox .                                    # runtime image
#   docker build --target artifact --output type=local,dest=out .  # binary only (releases)
#
# The admin front (web/, plain HTML/CSS/JS) is embedded in the binary at build
# time; the build context needs no pre-built artifact.

ARG RUST_IMAGE=rust:1.98-alpine3.24

# ------------------------------------------------------------------ toolchain
FROM ${RUST_IMAGE} AS toolchain
RUN apk add --no-cache musl-dev gcc make perl binutils
ENV CARGO_HOME=/usr/local/cargo \
    CARGO_TARGET_DIR=/src/target \
    CARGO_NET_RETRY=5
WORKDIR /src

# ------------------------------------------------------------------ dependencies
# Manifests and stub sources only, so dependency layers survive source changes.
FROM toolchain AS deps
COPY Cargo.toml Cargo.lock ./
COPY crates/inbox-core/Cargo.toml crates/inbox-core/
COPY crates/inbox-server/Cargo.toml crates/inbox-server/build.rs crates/inbox-server/
COPY crates/inbox-android/Cargo.toml crates/inbox-android/
RUN set -eu; \
    for c in inbox-core inbox-android; do \
      mkdir -p crates/$c/src && echo '' > crates/$c/src/lib.rs; \
    done; \
    mkdir -p crates/inbox-server/src web \
 && echo 'fn main() {}' > crates/inbox-server/src/main.rs \
 && touch web/index.html web/app.js web/style.css \
 && cargo fetch --locked \
 && cargo build --release --locked -p inbox-server

# ------------------------------------------------------------------ build
FROM deps AS build
ARG INBOX_GIT_SHA=unknown
ENV INBOX_GIT_SHA=${INBOX_GIT_SHA}
COPY crates crates
COPY web web
COPY config.example.hcl ./
# COPY keeps the context mtimes, which are older than the stubs: force a rebuild.
RUN set -eu; \
    find crates web -type f -exec touch {} +; \
    cargo build --release --locked -p inbox-server; \
    bin=target/release/inbox; \
    if readelf -l "$bin" | grep -q INTERP; then echo "inbox is dynamically linked" >&2; exit 1; fi; \
    if readelf -d "$bin" 2>/dev/null | grep -q NEEDED; then echo "inbox has shared library dependencies" >&2; exit 1; fi; \
    "$bin" version; \
    mkdir -p /out/rootfs/var/lib/inbox /out/rootfs/tmp /out/rootfs/etc/inbox; \
    chmod 1777 /out/rootfs/tmp; \
    install -m 0755 "$bin" /out/inbox

# ------------------------------------------------------------------ artifact
FROM scratch AS artifact
COPY --from=build /out/inbox /inbox

# ------------------------------------------------------------------ runtime
FROM scratch AS runtime
COPY --from=build --chown=10001:10001 /out/rootfs/ /
COPY --from=build /out/inbox /inbox
USER 10001:10001
EXPOSE 8080 8081
VOLUME ["/var/lib/inbox"]
HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=3 \
  CMD ["/inbox", "healthcheck", "--config", "/etc/inbox/config.hcl"]
ENTRYPOINT ["/inbox"]
CMD ["serve", "--config", "/etc/inbox/config.hcl"]
