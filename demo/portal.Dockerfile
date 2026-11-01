# The portal for the local demo: a dynamic build on Debian. The image stays on this machine.
FROM rust:1.88-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY web ./web
RUN cargo build --release --locked && cp target/release/portal /portal

FROM debian:bookworm-slim
COPY --from=builder /portal /portal
EXPOSE 4181 4182
ENTRYPOINT ["/portal"]
CMD ["run", "--config", "/etc/portal/config.hcl"]
