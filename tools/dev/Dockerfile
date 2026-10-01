# Development and CI toolchain for the Rust workspace (server, core, wasm).
FROM rust:1.98-alpine3.24
RUN apk add --no-cache musl-dev gcc make perl git \
 && rustup component add clippy rustfmt \
 && rustup target add wasm32-unknown-unknown
ENV CARGO_HOME=/cargo
WORKDIR /src
