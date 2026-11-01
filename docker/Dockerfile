# syntax=docker/dockerfile:1
ARG RUST_VERSION=1.88
FROM rust:${RUST_VERSION}-bookworm AS builder

ARG TARGETARCH
ARG TARGETVARIANT
ARG PORTAL_VERSION=dev

WORKDIR /src

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY crates ./crates
COPY web ./web

RUN set -eux; \
    case "${TARGETARCH}/${TARGETVARIANT}" in \
      amd64/) rust_target='x86_64-unknown-linux-musl' ;; \
      arm64/) rust_target='aarch64-unknown-linux-musl' ;; \
      arm/v7) rust_target='armv7-unknown-linux-musleabihf' ;; \
      *) echo "unsupported Docker target architecture: TARGETARCH=${TARGETARCH} TARGETVARIANT=${TARGETVARIANT}" >&2; exit 1 ;; \
    esac; \
    rustup target add "$rust_target"; \
    cargo fetch --locked --target "$rust_target"; \
    PORTAL_RELEASE_VERSION="$PORTAL_VERSION" \
      cargo build --release --frozen --target "$rust_target"; \
    binary="target/${rust_target}/release/portal"; \
    test -x "$binary"; \
    "$binary" version; \
    mkdir -p /out; \
    cp "$binary" /out/portal

FROM scratch

COPY --from=builder /out/portal /portal

EXPOSE 4181
# The AuthZEN PDP of the services (config pdp block). Do not route it through Traefik.
EXPOSE 4182

HEALTHCHECK --interval=10s \
            --timeout=2s \
            --start-interval=1s \
            --start-period=10s \
            --retries=3 \
            CMD ["/portal", "healthcheck", "--config", "/etc/portal/config.hcl"]

ENTRYPOINT ["/portal"]
CMD ["run", "--config", "/etc/portal/config.hcl"]
